//! DRM/KMS 后端 — 直接在 TTY 运行 (生产模式)
//!
//! 架构: libseat (session) → udev (设备发现) → DRM (GPU) → libinput (输入)

use std::sync::Arc;
use std::time::Duration;

use smithay::{
    backend::{
        allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, GbmBufferedSurface},
        egl::{EGLContext, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            damage::OutputDamageTracker,
            gles::GlesRenderer,
            Bind,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{self, UdevBackend, UdevEvent},
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale as OutputScale, Subpixel},
    reexports::{
        calloop::EventLoop,
        drm::control::{connector, crtc, ModeTypeFlags},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::Display,
    },
    utils::{DeviceFd, Transform},
    wayland::socket::ListeningSocketSource,
};

use crate::renderer::shaders::LingxiShaders;

use tracing::{error, info};

use crate::compositor::{ClientState, LingxiState};
use smithay::wayland::compositor::CompositorClientState;

/// DRM 后端事件循环数据
pub struct DrmData {
    pub state: LingxiState,
    pub display: Display<LingxiState>,
    pub session: LibSeatSession,
    pub gpu_node: DrmNode,
    pub drm_device: DrmDevice,
    pub gbm: GbmDevice<DrmDeviceFd>,
    pub renderer: GlesRenderer,
    pub shaders: Option<LingxiShaders>,
    pub surfaces: Vec<OutputSurface>,
    pub cursor: CursorData,
}

/// 光标数据 (复用共享 scene 模块的定义)
use crate::renderer::scene::{self, CursorData};

/// 每个输出的渲染表面
pub struct OutputSurface {
    pub output: Output,
    pub crtc: crtc::Handle,
    pub surface: GbmBufferedSurface<GbmAllocator<DrmDeviceFd>, ()>,
    pub damage_tracker: OutputDamageTracker,
}

/// 运行 DRM 后端
pub fn run(config: crate::config::LingxiConfig) {
    eprintln!("[DRM] >>> run() 进入");
    info!("启动 DRM/KMS 后端 (直接 TTY 模式)");

    // 1. 创建事件循环
    let mut event_loop: EventLoop<DrmData> =
        EventLoop::try_new().expect("Failed to create event loop");
    let loop_handle = event_loop.handle();
    let loop_signal = event_loop.get_signal();
    eprintln!("[DRM] 事件循环已创建");

    // 2. 初始化 session (libseat)
    eprintln!("[DRM] 正在创建 libseat session...");
    let (mut session, session_notifier) = match LibSeatSession::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[DRM] libseat 失败: {}", e);
            error!("无法创建 libseat session: {}", e);
            error!("DRM 模式需要从 TTY 直接运行 (Ctrl+Alt+F3 切换到 TTY)");
            error!("如果在桌面环境中，请使用: lingxi --winit");
            std::process::exit(1);
        }
    };
    eprintln!("[DRM] libseat OK (seat: {})", session.seat());
    info!("libseat session 已创建 (seat: {})", session.seat());

    loop_handle
        .insert_source(session_notifier, |event, _, _data| match event {
            SessionEvent::PauseSession => {
                info!("会话暂停 (VT 切换)");
            }
            SessionEvent::ActivateSession => {
                info!("会话恢复");
            }
        })
        .expect("Failed to register session notifier");

    // 3. 创建 Wayland Display
    let display = Display::<LingxiState>::new().expect("Failed to create display");
    let mut state = LingxiState::new(&display, loop_signal.clone(), config);
    eprintln!("[DRM] Wayland Display + State 已创建");

    // (dmabuf global 在 renderer 创建后注册 — 见下方)

    // 4. 找到主 GPU
    let primary_gpu_path = match udev::primary_gpu(session.seat()) {
        Ok(Some(p)) => p,
        Ok(None) => {
            tracing::error!("[DRM] 未找到主 GPU, 退出");
            std::process::exit(1);
        }
        Err(e) => {
            tracing::error!("[DRM] 查找主 GPU 失败: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("[DRM] 主 GPU: {:?}", primary_gpu_path);
    info!("主 GPU: {:?}", primary_gpu_path);

    let gpu_node = DrmNode::from_path(&primary_gpu_path)
        .expect("Failed to create DRM node");

    // 5. 打开 GPU
    let gpu_fd = session
        .open(&primary_gpu_path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY)
        .expect("Failed to open GPU device");
    let device_fd = DrmDeviceFd::new(DeviceFd::from(gpu_fd));
    eprintln!("[DRM] GPU fd 已打开");

    let (mut drm_device, drm_notifier) =
        DrmDevice::new(device_fd.clone(), true).expect("Failed to create DRM device");

    // GBM
    let gbm = GbmDevice::new(device_fd.clone()).expect("Failed to create GBM device");

    // EGL + GLES
    eprintln!("[DRM] 正在初始化 EGL...");
    let egl_display =
        unsafe { EGLDisplay::new(gbm.clone()) }.expect("Failed to create EGL display");
    let egl_context = EGLContext::new(&egl_display).expect("Failed to create EGL context");
    let mut renderer =
        unsafe { GlesRenderer::new(egl_context) }.expect("Failed to create renderer");
    eprintln!("[DRM] EGL + GLES 初始化完成");

    // 编译灵犀自定义 shader (圆角/阴影/模糊)；失败则降级为无特效
    let shaders = LingxiShaders::compile(&mut renderer);
    if shaders.is_none() {
        error!("自定义 shader 编译失败，特效已禁用 (圆角/阴影)");
    }

    info!("GPU 初始化完成: DRM + GBM + EGL + GLES");

    // 注册 linux-dmabuf 协议 (让 GPU 客户端能分配 buffer)
    let dmabuf_formats = renderer.egl_context().dmabuf_render_formats().clone();
    let dmabuf_global = state.dmabuf_state.create_global::<LingxiState>(
        &display.handle(),
        dmabuf_formats,
    );
    state.dmabuf_global = Some(dmabuf_global);
    eprintln!("[DRM] linux-dmabuf 协议已注册");

    // 6. 扫描连接器，创建输出表面
    let mut surfaces = Vec::new();
    {
        use smithay::reexports::drm::control::Device;

        let res = drm_device.resource_handles().expect("Failed to get DRM resources");

        for &conn in res.connectors() {
            let connector_info = match drm_device.get_connector(conn, true) {
                Ok(info) => info,
                Err(_) => continue,
            };

            if connector_info.state() != connector::State::Connected {
                continue;
            }

            let mode = connector_info
                .modes()
                .iter()
                .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                .or_else(|| connector_info.modes().first())
                .copied();

            let mode = match mode {
                Some(m) => m,
                None => continue,
            };

            // 找到 CRTC
            let crtc = connector_info
                .current_encoder()
                .and_then(|e| drm_device.get_encoder(e).ok())
                .and_then(|enc| enc.crtc())
                .or_else(|| res.crtcs().first().copied());

            let crtc = match crtc {
                Some(c) => c,
                None => continue,
            };

            info!(
                "发现输出: {}x{}@{}Hz (crtc: {:?})",
                mode.size().0,
                mode.size().1,
                mode.vrefresh(),
                crtc
            );

            // 创建 Smithay Output
            let output = Output::new(
                format!("lingxi-drm-{}", surfaces.len()),
                PhysicalProperties {
                    size: (
                        connector_info.size().unwrap_or((0, 0)).0 as i32,
                        connector_info.size().unwrap_or((0, 0)).1 as i32,
                    ).into(),
                    subpixel: Subpixel::Unknown,
                    make: "lingxi".to_string(),
                    model: "DRM".to_string(),
                },
            );

            let output_mode = OutputMode {
                size: (mode.size().0 as i32, mode.size().1 as i32).into(),
                refresh: (mode.vrefresh() * 1000) as i32,
            };

            output.change_current_state(
                Some(output_mode),
                Some(Transform::Normal),
                Some(OutputScale::Integer(1)),
                Some((0, 0).into()),
            );
            output.set_preferred(output_mode);
            output.create_global::<LingxiState>(&display.handle());
            state.space.map_output(&output, (0, 0));

            // 创建 DRM surface + GBM allocator
            let drm_surface = match drm_device.create_surface(crtc, mode, &[conn]) {
                Ok(s) => s,
                Err(e) => {
                    error!("创建 DRM surface 失败: {}", e);
                    continue;
                }
            };

            let allocator = GbmAllocator::new(
                gbm.clone(),
                GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
            );

            // 获取渲染器支持的格式
            let renderer_formats = renderer.egl_context().dmabuf_render_formats().clone();
            let color_formats = &[
                smithay::reexports::drm::buffer::DrmFourcc::Argb8888,
                smithay::reexports::drm::buffer::DrmFourcc::Xrgb8888,
            ];

            let gbm_surface = match GbmBufferedSurface::new(
                drm_surface,
                allocator,
                color_formats,
                renderer_formats,
            ) {
                Ok(s) => s,
                Err(e) => {
                    error!("创建 GBM surface 失败: {}", e);
                    continue;
                }
            };

            let damage_tracker = OutputDamageTracker::from_output(&output);

            surfaces.push(OutputSurface {
                output,
                crtc,
                surface: gbm_surface,
                damage_tracker,
            });
        }
    }

    info!("共 {} 个输出已就绪", surfaces.len());
    if surfaces.is_empty() {
        error!("没有找到可用输出！");
        std::process::exit(1);
    }

    // 注册 DRM 事件 (VBlank)
    loop_handle
        .insert_source(drm_notifier, |event, _, data| {
            match event {
                DrmEvent::VBlank(crtc) => {
                    // Page flip 完成 — 告知 GBM surface 可以开始下一帧
                    for surface in &mut data.surfaces {
                        if surface.crtc == crtc {
                            if let Err(e) = surface.surface.frame_submitted() {
                                tracing::warn!("frame_submitted 失败: {}", e);
                            }
                        }
                    }
                }
                DrmEvent::Error(e) => {
                    error!("DRM 错误: {}", e);
                }
            }
        })
        .expect("Failed to register DRM events");

    // 7. 初始化 libinput
    let mut libinput_context =
        Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
    libinput_context
        .udev_assign_seat(&session.seat())
        .expect("Failed to assign seat to libinput");

    let libinput_backend = LibinputInputBackend::new(libinput_context.clone());
    loop_handle
        .insert_source(libinput_backend, |event, _, data| {
            data.state.handle_input(event);
        })
        .expect("Failed to register libinput");

    info!("libinput 已初始化");

    // 8. udev 热插拔
    let udev_backend = UdevBackend::new(session.seat()).expect("Failed to create udev backend");
    loop_handle
        .insert_source(udev_backend, |event, _, _data| match event {
            UdevEvent::Added { device_id: _, path } => {
                info!("设备连接: {:?}", path);
            }
            UdevEvent::Changed { device_id: _ } => {}
            UdevEvent::Removed { device_id } => {
                info!("设备移除: {}", device_id);
            }
        })
        .expect("Failed to register udev");

    // 9. Wayland socket
    let listening_socket = ListeningSocketSource::new_auto()
        .expect("Failed to create listening socket");
    let socket_name = listening_socket.socket_name().to_os_string();
    info!("Wayland socket: {:?}", socket_name);
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket_name); }

    loop_handle
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
        .expect("Failed to register socket");

    // 10. 启动 autostart 服务 (壁纸 swaybg / 状态栏 waybar / 通知 mako)
    spawn_autostart();

    // 不用 Timer，直接在主循环里渲染
    eprintln!("[DRM] 进入主循环");
    info!("DRM 模式运行中!");
    info!("Super+Enter=终端, Super+Q=关窗, Super+Shift+Q=退出");

    let cursor = scene::create_cursor_data(state.config.general.cursor_size);

    let mut data = DrmData {
        state,
        display,
        session,
        gpu_node,
        drm_device,
        gbm,
        renderer,
        shaders,
        surfaces,
        cursor,
    };

    // 初次渲染 + 立即 frame_submitted (首帧是 modeset，没有 VBlank 事件)
    eprintln!("[DRM] 初次渲染...");
    render_all(&mut data.renderer, &data.shaders, &mut data.surfaces, &data.state, &data.cursor);
    for surface in &mut data.surfaces {
        let _ = surface.surface.frame_submitted();
    }
    eprintln!("[DRM] 初次渲染完成，进入事件循环");

    let mut frame_count: u64 = 0;
    loop {
        // 1. Dispatch Wayland clients (出错不 panic, 避免单客户端打垮合成器)
        if let Err(e) = data.display.dispatch_clients(&mut data.state) {
            tracing::error!("dispatch_clients failed: {e}");
        }
        if let Err(e) = data.display.flush_clients() {
            tracing::error!("flush_clients failed: {e}");
        }

        // 2. 驱动 calloop (处理 libinput、DRM vblank、socket accept 等)
        if event_loop.dispatch(Some(Duration::from_millis(16)), &mut data).is_err() {
            break;
        }

        // 3. 共享: 刷新 space / 清 popup / arrange layer / 推进动画 (架构 F)
        data.state.pre_render_tick();

        // 4. 按需渲染 (脏标记或动画进行中才重绘)
        if data.state.should_render() {
            let render_start = std::time::Instant::now();
            render_all(&mut data.renderer, &data.shaders, &mut data.surfaces, &data.state, &data.cursor);
            let render_ms = render_start.elapsed().as_millis() as u64;
            data.state.needs_render = false;

            frame_count += 1;
            // 调试: 渲染 > 5ms 就 warn, 找出卡顿
            if render_ms > 5 {
                tracing::warn!("[perf] render {}ms (windows={}, has_anim={})",
                    render_ms, data.state.space.elements().count(), data.state.animations.has_active_animations());
            }
            if frame_count <= 10 || frame_count % 300 == 0 {
                let win_count = data.state.space.elements().count();
                tracing::info!("[loop] frame={} windows={} workspace={} render_ms={}",
                    frame_count, win_count, data.state.active_workspace + 1, render_ms);
            }
        }
    }

    info!("灵犀 compositor 已退出 (DRM)");
}

/// 渲染所有输出
fn render_all(
    renderer: &mut GlesRenderer,
    shaders: &Option<LingxiShaders>,
    surfaces: &mut Vec<OutputSurface>,
    state: &LingxiState,
    cursor: &CursorData,
) {
    for surface in surfaces.iter_mut() {
        let start = std::time::Instant::now();
        render_surface(renderer, shaders, surface, state, cursor);
        let ms = start.elapsed().as_millis() as u64;
        if ms > 5 {
            tracing::warn!("[perf] render_surface {}ms (windows={})", ms, state.space.elements().count());
        }
        tracing::debug!("[render] surface done in {}ms", ms);
    }
}

/// 渲染单个输出到 GBM surface 并提交 page flip
fn render_surface(
    renderer: &mut GlesRenderer,
    shaders: &Option<LingxiShaders>,
    surface: &mut OutputSurface,
    state: &LingxiState,
    cursor: &CursorData,
) {
    // 1. 获取下一个缓冲区 (含真实 age 用于 damage 跟踪)
    let (mut dmabuf, age) = match surface.surface.next_buffer() {
        Ok(b) => b,
        Err(_e) => {
            return;
        }
    };

    // 2. 将渲染器绑定到 dmabuf
    let mut framebuffer = match renderer.bind(&mut dmabuf) {
        Ok(fb) => fb,
        Err(e) => {
            tracing::warn!("renderer bind 失败: {}", e);
            return;
        }
    };

    // 3. 构建渲染场景 (共享逻辑: 光标 + layer + 边框 + 圆角窗口 + 投影)
    let elements = scene::build_scene(
        renderer,
        shaders,
        state,
        &surface.output,
        Some(cursor),
    );

    // 4. 用真实 age 渲染 — damage tracker 只重绘变化区域
    // 锁屏渲染已撤 (smithay 0.7 锁屏支持残缺, 留待 lingxi 0.2+ 重做)
    // state.locked 字段保留, 未来启用
    let _ = state.locked;
    let render_result = surface.damage_tracker.render_output(
        renderer,
        &mut framebuffer,
        age as usize,
        &elements,
        [0.15, 0.15, 0.15, 1.0],
    );

    // 释放 framebuffer
    drop(framebuffer);

    match render_result {
        Ok(result) => {
            // 无 damage → 画面未变化, 跳过 page flip
            match result.damage {
                Some(damage) => {
                    if state.locked {
                        tracing::info!("🔒 queue_buffer: damage_rects={}", damage.len());
                    }
                    let damage = damage.to_vec();
                    let sync = result.sync.clone();
                    match surface.surface.queue_buffer(Some(sync), Some(damage), ()) {
                        Ok(_) => {
                            if state.locked {
                                tracing::info!("🔒 queue_buffer OK (page flip 提交)");
                            }
                        }
                        Err(e) => {
                            tracing::warn!("🔒 queue_buffer 失败: {}", e);
                        }
                    }
                }
                None => {
                    tracing::trace!("无 damage, 跳过渲染提交");
                }
            }
        }
        Err(e) => {
            tracing::warn!("render_output 失败: {}", e);
        }
    }

    // 5. 发送 frame callback 给所有窗口和 layer surfaces
    scene::send_frames(state, &surface.output);
}

/// 启动 autostart 脚本 (壁纸 / 状态栏 / 通知等后台服务)
/// 异步执行, 不阻塞主循环。脚本位于 ~/.config/lingxi/autostart.sh
pub fn spawn_autostart_pub() {
    spawn_autostart();
}

fn spawn_autostart() {
    use std::process::{Command, Stdio};

    let script = format!(
        "{}/.config/lingxi/autostart.sh",
        std::env::var("HOME").unwrap_or_else(|_| "/root".into())
    );

    if !std::path::Path::new(&script).exists() {
        tracing::info!(
            "autostart 脚本不存在: {} (跳过, 可手动创建启用壁纸/状态栏)",
            script
        );
        return;
    }

    tracing::info!("🚀 启动 autostart: {}", script);
    match Command::new(&script)
        // 把 WAYLAND_DISPLAY 透传给子脚本 (脚本已经能看到)
        // 加 LINGXI_AUTOSTART=1 标记, 防止脚本在非 lingxi 环境被误触发
        .env("LINGXI_AUTOSTART", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => tracing::info!("autostart 已启动 (pid={})", child.id()),
        Err(e) => tracing::error!("启动 autostart 失败: {}", e),
    }
}
