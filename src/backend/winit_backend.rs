//! Winit 后端 — 嵌套在现有桌面中运行 (开发模式)

use std::sync::Arc;
use std::time::Duration;

use smithay::{
    backend::{
        renderer::{
            damage::OutputDamageTracker,
            gles::GlesRenderer,
        },
        winit::{self, WinitEvent},
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale as OutputScale, Subpixel},
    reexports::calloop::EventLoop,
    utils::Transform,
    wayland::socket::ListeningSocketSource,
};

use tracing::{error, info, warn};

use crate::compositor::{ClientState, LingxiState};
use crate::renderer::scene::{self, CursorData};
use crate::renderer::shaders::LingxiShaders;
use smithay::wayland::compositor::CompositorClientState;

/// Winit 后端事件循环数据
pub struct WinitData {
    pub state: LingxiState,
    pub display: smithay::reexports::wayland_server::Display<LingxiState>,
}

/// 运行 winit 后端
pub fn run(config: crate::config::LingxiConfig) {
    let mut event_loop: EventLoop<WinitData> =
        EventLoop::try_new().expect("Failed to create event loop");
    let loop_signal = event_loop.get_signal();

    let display = smithay::reexports::wayland_server::Display::<LingxiState>::new()
        .expect("Failed to create display");

    let mut state = LingxiState::new(&display, loop_signal, config);
    info!("合成器状态初始化完成");

    // 初始化 Winit 后端
    let (mut backend, mut winit_event_loop) =
        winit::init::<GlesRenderer>().expect("Failed to initialize winit backend");

    let win_size = backend.window_size();
    let mode = OutputMode {
        size: win_size.into(),
        refresh: 60_000,
    };

    let output = Output::new(
        "lingxi-winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "lingxi".to_string(),
            model: "winit".to_string(),
        },
    );
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        Some(OutputScale::Integer(1)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    output.create_global::<LingxiState>(&display.handle());
    state.space.map_output(&output, (0, 0));

    // 注册 linux-dmabuf 协议
    {
        let renderer = backend.renderer();
        let dmabuf_formats = renderer.egl_context().dmabuf_render_formats().clone();
        let dmabuf_global = state.dmabuf_state.create_global::<LingxiState>(
            &display.handle(),
            dmabuf_formats,
        );
        state.dmabuf_global = Some(dmabuf_global);
    }

    info!("✅ Winit 后端已初始化 ({}x{})", win_size.w, win_size.h);

    // 编译自定义 shader (圆角/阴影)；失败则降级
    let shaders = LingxiShaders::compile(backend.renderer());
    if shaders.is_none() {
        error!("自定义 shader 编译失败，特效已禁用 (圆角/阴影)");
    }

    // 加载光标
    let cursor: CursorData = scene::create_cursor_data(state.config.general.cursor_size);

    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    // 注册 Wayland socket
    let listening_socket = ListeningSocketSource::new_auto()
        .expect("Failed to create listening socket");
    let socket_name = listening_socket.socket_name().to_os_string();
    info!("🔌 Wayland socket: {:?}", socket_name);

    event_loop
        .handle()
        .insert_source(listening_socket, |client_stream, _, data| {
            data.display
                .handle()
                .insert_client(
                    client_stream,
                    Arc::new(ClientState {
                        compositor_state: CompositorClientState::default(),
                    }),
                )
                .expect("Failed to insert client");
        })
        .expect("Failed to register listening socket");

    info!("🚀 进入主事件循环 (winit 模式)...");
    info!(
        "💡 运行: WAYLAND_DISPLAY={:?} cosmic-term",
        socket_name
    );

    // 启动 autostart 服务 (壁纸 swaybg / 状态栏 waybar / 通知 mako)
    crate::backend::drm_backend::spawn_autostart_pub();

    let mut data = WinitData { state, display };
    let mut running = true;

    while running {
        let mut input_events = Vec::new();

        let status = winit_event_loop.dispatch_new_events(|event| match event {
            WinitEvent::Resized { size, .. } => {
                let new_mode = OutputMode {
                    size: size.into(),
                    refresh: 60_000,
                };
                output.change_current_state(Some(new_mode), None, None, None);
            }
            WinitEvent::Input(event) => {
                input_events.push(event);
            }
            WinitEvent::Focus(_) => {}
            WinitEvent::Redraw => {}
            WinitEvent::CloseRequested => {
                info!("收到关闭请求，退出");
                running = false;
            }
        });

        if matches!(
            status,
            smithay::reexports::winit::platform::pump_events::PumpStatus::Exit(_)
        ) {
            break;
        }

        for event in input_events {
            data.state.handle_input(event);
        }

        // 刷新 Space 状态
        data.state.space.refresh();
        // 清理已关闭的 popup
        data.state.popup_manager.cleanup();
        // 重新 arrange 所有 outputs 的 layer map (见 drm_backend 同注释)
        for output in data.state.space.outputs().cloned().collect::<Vec<_>>() {
            let _ = smithay::desktop::layer_map_for_output(&output).arrange();
        }

        // 推进动画
        data.state.tick_animations();

        // 按需渲染 (脏标记或动画进行中才重绘)
        let animating = data.state.animations.has_active_animations();
        if data.state.needs_render || animating {
            let render_ok = {
                let (renderer, mut framebuffer) = match backend.bind() {
                    Ok(r) => r,
                    Err(e) => {
                        error!("bind 失败: {}", e);
                        continue;
                    }
                };

                let elements = scene::build_scene(
                    renderer,
                    &shaders,
                    &data.state,
                    &output,
                    Some(&cursor),
                );

                damage_tracker
                    .render_output(renderer, &mut framebuffer, 0, &elements, [0.1, 0.1, 0.1, 1.0])
                    .is_ok()
            };
            if render_ok {
                if let Err(e) = backend.submit(None) {
                    warn!("submit 失败: {}", e);
                }
            }
            data.state.needs_render = false;

            // 发送 frame callback
            scene::send_frames(&data.state, &output);
        }

        // Dispatch clients
        data.display
            .dispatch_clients(&mut data.state)
            .expect("dispatch_clients failed");
        data.display
            .flush_clients()
            .expect("flush_clients failed");

        event_loop
            .dispatch(Some(Duration::from_millis(1)), &mut data)
            .expect("event loop dispatch failed");
    }

    info!("灵犀 compositor 已退出 (winit)");
}
