//! Smithay 协议 handler 实现

use smithay::{
    delegate_compositor, delegate_cursor_shape, delegate_data_device, delegate_input_method_manager,
    delegate_layer_shell, delegate_output, delegate_primary_selection, delegate_seat, delegate_session_lock, delegate_shm,
    delegate_text_input_manager, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{LayerSurface as DesktopLayerSurface, Window, layer_map_for_output},
    input::{Seat, SeatHandler, SeatState},
    reexports::wayland_server::{
        protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
        Client,
    },
    utils::{Logical, Rectangle, Serial},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        output::OutputHandler,
        selection::{
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
            SelectionHandler,
        },
        shell::xdg::{
            PopupSurface as XdgPopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            decoration::XdgDecorationHandler,
        },
        shell::wlr_layer::{
            Layer, LayerSurface, WlrLayerShellHandler, WlrLayerShellState,
        },
        shm::{ShmHandler, ShmState},
        tablet_manager::TabletSeatHandler,
    },
};

use super::{ClientState, LingxiState};
use crate::layout::LayoutEngine;
use smithay::wayland::text_input::TextInputSeat;

// ========== Buffer ==========

impl BufferHandler for LingxiState {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

// ========== Compositor ==========

impl CompositorHandler for LingxiState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        use smithay::backend::renderer::utils::on_commit_buffer_handler;
        on_commit_buffer_handler::<Self>(surface);
        // popup 第一次 commit 时从 unmapped 移到 mapped tree — 必须让 PopupManager 知道
        self.popup_manager.commit(surface);

        // Update window bounding box so element_under() works correctly
        let committed_window = self.by_surface.get(surface).cloned();

        if let Some(window) = &committed_window {
            window.on_commit();
        }

        // 全屏过渡: 等待 client ack (commit = 配对的 ack 表示 client 已接受 configure)
        if let Some(win) = &committed_window {
            if self.fullscreen_window.as_ref() == Some(win) {
                if matches!(self.fs_phase, crate::compositor::FullscreenPhase::Pending { .. }) {
                    tracing::info!("全屏 client ack, 转为 Active");
                    self.fs_phase = crate::compositor::FullscreenPhase::Active;
                    self.needs_render = true;
                }
            }
        }

        // Layer surface 键盘焦点: 仅当 client 请求 Exclusive interactivity 时给焦点.
        // swaybg/waybar 默认 None 不再抢终端焦点; fuzzel 等启动器设 Exclusive 后获得焦点.
        // (new_layer_surface 时不判断, 因创建时 interactivity 恒为 None.)
        {
            use smithay::wayland::compositor::with_states;
            use smithay::wayland::shell::wlr_layer::{KeyboardInteractivity, LayerSurfaceCachedState};
            // 读 client 提交的 cached state (LayerSurfaceCachedState.keyboard_interactivity),
            // 而非 LayerSurfaceAttributes.current (那是 server 端 state, 只有 size).
            let interactivity = with_states(surface, |states| {
                states
                    .cached_state
                    .get::<LayerSurfaceCachedState>()
                    .current()
                    .keyboard_interactivity
            });
            // 仅 Exclusive 自动给焦点 (启动器); OnDemand 是按需(点击), 不在此抢焦点.
            if interactivity == KeyboardInteractivity::Exclusive {
                let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                self.set_keyboard_focus_with_selection(Some(surface.clone()), serial);
            }
        }

        // surface 有新内容提交 → 标记需要重绘
        self.needs_render = true;
    }
}

delegate_compositor!(LingxiState);

// ========== XDG Shell ==========

impl XdgShellHandler for LingxiState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface.clone());

        // 获取输出中心作为入场动画起点
        let output_center = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
            .map(|geo| (geo.size.w as f64 / 2.0, geo.size.h as f64 / 2.0))
            .unwrap_or((640.0, 400.0));

        // Add window to current workspace
        self.workspaces[self.active_workspace].push(window.clone());
        // 维护 surface→Window 索引 (O(1) 查找用)
        self.by_surface.insert(surface.wl_surface().clone(), window.clone());

        // 先把窗口 map 到 space (初始位置放中心)
        self.space.map_element(window.clone(), (output_center.0 as i32, output_center.1 as i32), true);

        // 计算新的平铺布局 (含刚加入的窗口)
        let windows: Vec<_> = self.space.elements().cloned().collect();
        let count = windows.len();
        let area = self.usable_area();
        let geometries = self.layout.arrange(count, area);

        // 找到新窗口的目标位置
        let new_idx = windows.iter().position(|w| w == &window).unwrap_or(count - 1);
        let target = super::window::AnimatedRect {
            x: geometries[new_idx].x,
            y: geometries[new_idx].y,
            width: geometries[new_idx].width,
            height: geometries[new_idx].height,
        };

        // 注册入场动画 (从中心缩放飞入)
        self.animations.add_window(window, target, output_center);

        // 其他已有窗口也要动画到新位置 (重新平铺)
        let targets: Vec<_> = windows
            .iter()
            .zip(geometries.iter())
            .map(|(w, geo)| {
                (
                    w.clone(),
                    super::window::AnimatedRect {
                        x: geo.x,
                        y: geo.y,
                        width: geo.width,
                        height: geo.height,
                    },
                )
            })
            .collect();
        self.animations.retarget(&targets);

        // 告诉所有窗口新的 configure size (让客户端缩放)
        for (i, w) in windows.iter().enumerate() {
            if let Some(toplevel) = w.toplevel() {
                toplevel.with_pending_state(|pending| {
                    pending.size = Some(
                        (geometries[i].width as i32, geometries[i].height as i32).into(),
                    );
                });
                toplevel.send_configure();
            }
        }

        // 自动聚焦新窗口 (同时通知 data_device / primary_selection)
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        self.set_keyboard_focus_with_selection(
            Some(surface.wl_surface().clone()),
            serial,
        );

        tracing::info!("新窗口已创建 (dwindle 平铺, {}个窗口, workspace {}, 目标尺寸: {}x{})",
            count, self.active_workspace + 1, geometries[new_idx].width as i32, geometries[new_idx].height as i32);

        self.needs_render = true;
    }

    fn new_popup(&mut self, surface: XdgPopupSurface, _positioner: PositionerState) {
        // 让 PopupManager 接管这个 popup — 之后 Window::render_elements/surface_under
        // 会自动把它渲染/参与命中检测。必须调,否则 popup 永远进不了 popup tree
        if let Err(e) = self.popup_manager.track_popup(smithay::desktop::PopupKind::Xdg(surface.clone())) {
            tracing::warn!("track_popup 失败: {:?}", e);
            return;
        }
        // Wayland 协议: popup 创建后必须先收到 configure 才会显示 (commit 屏幕)
        let _ = surface.send_configure();
        tracing::debug!("XDG popup 创建 (已 track + send_configure)");
        self.needs_render = true;
    }

    fn reposition_request(&mut self, surface: XdgPopupSurface, _positioner: PositionerState, token: u32) {
        // 客户端要求重定位 (带 token) — 调 send_repositioned 让客户端知道新位置
        // 这里直接用原 positioner 重发,简单实现;Hyprland 会在此处做约束/翻转
        surface.send_repositioned(token);
        let _ = surface.send_configure();
        self.needs_render = true;
    }

    fn grab(&mut self, surface: XdgPopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // popup 请求指针 grab — 当指针离开 popup 区域,客户端会收到 popup_done
        // Smithay 的 PopupManager.grab_popup 会处理 keyboard focus + 串行号管理
        // 找到 popup 的根 surface (toplevel)
        if let Ok(wl_s) = smithay::desktop::find_popup_root_surface(
            &smithay::desktop::PopupKind::Xdg(surface.clone()),
        ) {
            if let Some(root) = self.by_surface.get(&wl_s).cloned() {
                if let Some(toplevel) = root.toplevel() {
                    let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                    let _ = self.popup_manager.grab_popup::<Self>(
                        toplevel.wl_surface().clone(),
                        smithay::desktop::PopupKind::Xdg(surface),
                        &self.seat,
                        serial,
                    );
                }
            }
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let window = self.by_surface.get(surface.wl_surface()).cloned();
        if let Some(window) = window {
            // 移除 surface→Window 索引
            self.by_surface.remove(surface.wl_surface());
            // Remove from workspace tracking
            for ws in &mut self.workspaces {
                ws.retain(|w| w != &window);
            }

            // Clear fullscreen state if this window was fullscreen
            if self.fullscreen_window.as_ref() == Some(&window) {
                self.fullscreen_window = None;
            }

            // 清理浮动跟踪 (FloatingManager.remove 同时清 window/geo/z, 修复旧 z_order 泄漏)
            self.floating.remove(&window);

            self.animations.remove_window(&window);
            self.space.unmap_elem(&window);
            // 重新平铺剩余窗口 (带动画)
            self.relayout();
            tracing::info!("窗口已关闭 (重新平铺)");
        }
        // 清理 popup manager 里的死资源
        self.popup_manager.cleanup();
    }
}

delegate_xdg_shell!(LingxiState);

// ========== Layer Shell ==========

impl WlrLayerShellHandler for LingxiState {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        tracing::info!("Layer surface created: namespace={}", namespace);

        // 关键修复: 之前只 send_configure() 不带 size,client (swaybg) 收到 0x0
        // → 在屏幕中央渲染一小块。现在先建议 size = 输出分辨率,让 client
        // 拿到正确的几何 (swaybg 才会用 anchor + size 算全屏绘制)
        // 注意: anchor/exclusive_zone/keyboard_interactivity 是 client 控制,
        // server 不能改 (会触发协议错误)。只能建议 size。
        let output_size = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
            .map(|geo| (geo.size.w.max(1), geo.size.h.max(1)))
            .unwrap_or((1920, 1080));

        surface.with_pending_state(|state| {
            state.size = Some((output_size.0, output_size.1).into());
        });

        // Send initial configure — let arrange() determine the size based on anchors
        surface.send_configure();

        // Wrap in desktop LayerSurface
        let desktop_surface = DesktopLayerSurface::new(surface, namespace);

        // Map to the first (primary) output
        if let Some(output) = self.space.outputs().next().cloned() {
            let mut map = layer_map_for_output(&output);
            let _ = map.map_layer(&desktop_surface);
        }

        // 注意: 不在此处给键盘焦点. 创建时 keyboard_interactivity 恒为 None (协议默认),
        // 无条件 focus 会让 swaybg/waybar 抢走终端焦点. 焦点改在 commit() 里按 client
        // 实际请求的 interactivity 决定 (见 commit handler).

        self.needs_render = true;
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        tracing::info!("Layer surface destroyed");

        // We need to find and remove the desktop layer surface that wraps this wlr surface
        // Iterate all outputs and check their layer maps
        for output in self.space.outputs().cloned().collect::<Vec<_>>() {
            let mut map = layer_map_for_output(&output);
            // Find the layer that matches this surface by comparing wl_surface
            let to_remove: Option<DesktopLayerSurface> = map.layers().find(|ls| {
                ls.wl_surface() == surface.wl_surface()
            }).cloned();
            if let Some(desktop_ls) = to_remove {
                map.unmap_layer(&desktop_ls);
            }
        }

        // 若被销毁的 layer surface 持有键盘焦点 (如 fuzzel 关闭), 恢复到当前工作区首个 toplevel
        let was_focused = self
            .seat
            .get_keyboard()
            .and_then(|kb| kb.current_focus())
            .map(|f| &f == surface.wl_surface())
            .unwrap_or(false);
        if was_focused {
            let active = self.active_workspace;
            let focus_target = self.workspaces[active]
                .first()
                .and_then(|w| w.toplevel())
                .map(|t| t.wl_surface().clone());
            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            self.set_keyboard_focus_with_selection(focus_target, serial);
        }

        self.needs_render = true;
    }
}

delegate_layer_shell!(LingxiState);

// ========== SHM ==========

impl ShmHandler for LingxiState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

delegate_shm!(LingxiState);

// ========== Seat ==========

impl SeatHandler for LingxiState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        // 同步 text-input focus: 键盘焦点变化时更新 text_input 的 enter/leave
        let text_input = seat.text_input();
        if let Some(surface) = focused {
            text_input.set_focus(Some(surface.clone()));
            text_input.enter();
        } else {
            text_input.leave();
            text_input.set_focus(None);
        }
    }
}

delegate_seat!(LingxiState);

// ========== Data Device (Clipboard/DnD) ==========

impl SelectionHandler for LingxiState {
    type SelectionUserData = ();
}

impl ClientDndGrabHandler for LingxiState {}
impl ServerDndGrabHandler for LingxiState {}

impl DataDeviceHandler for LingxiState {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

delegate_data_device!(LingxiState);

use smithay::wayland::selection::primary_selection::PrimarySelectionHandler;
impl PrimarySelectionHandler for LingxiState {
    fn primary_selection_state(&self) -> &smithay::wayland::selection::primary_selection::PrimarySelectionState {
        &self.primary_selection_state
    }
}
delegate_primary_selection!(LingxiState);

// ========== Output ==========

impl OutputHandler for LingxiState {}

delegate_output!(LingxiState);

// ========== DMA-BUF ==========

use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::delegate_dmabuf;

impl DmabufHandler for LingxiState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, _dmabuf: Dmabuf, notifier: ImportNotifier) {
        // 直接接受 — 实际 import 在渲染时由 GlesRenderer 处理
        let _ = notifier.successful::<Self>();
    }
}

delegate_dmabuf!(LingxiState);

// ========== Viewporter ==========

smithay::delegate_viewporter!(LingxiState);

// ========== Fractional Scale ==========

// ========== Fractional Scale ==========

impl smithay::wayland::fractional_scale::FractionalScaleHandler for LingxiState {}

smithay::delegate_fractional_scale!(LingxiState);

// ========== XDG Decoration ==========

use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;

impl XdgDecorationHandler for LingxiState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_configure();
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        // Always force server-side decorations
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_configure();
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_configure();
    }
}

delegate_xdg_decoration!(LingxiState);

// ========== Cursor Shape ==========

impl TabletSeatHandler for LingxiState {}

delegate_cursor_shape!(LingxiState);

// ========== Input Method (IME / fcitx5) ==========

use smithay::wayland::input_method::{InputMethodHandler, PopupSurface as ImePopupSurface};

impl InputMethodHandler for LingxiState {
    fn new_popup(&mut self, surface: ImePopupSurface) {
        tracing::debug!("IME popup 显示");
        self.ime_popup = Some(surface);
        self.needs_render = true;
    }

    fn dismiss_popup(&mut self, _surface: ImePopupSurface) {
        tracing::debug!("IME popup 关闭");
        self.ime_popup = None;
        self.needs_render = true;
    }

    fn popup_repositioned(&mut self, _surface: ImePopupSurface) {
        tracing::debug!("IME popup 位置更新");
        self.needs_render = true;
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        // 找到 parent surface 对应的窗口，返回其屏幕位置
        self.by_surface
            .get(parent)
            .map(|w| self.window_screen_geometry(w))
            .unwrap_or_default()
    }
}

delegate_input_method_manager!(LingxiState);
delegate_text_input_manager!(LingxiState);

// ========== Virtual Keyboard ==========

use smithay::delegate_virtual_keyboard_manager;

delegate_virtual_keyboard_manager!(LingxiState);

// ========== Session Lock (ext-session-lock-v1) ==========
//
// swaylock / hyprlock 用这个协议锁屏:
// 1. client 发 lock → server 调 lock() 回调
// 2. server 把所有正常 surface 隐藏,只显示 client 创建的 LockSurface
// 3. 输入全部给 lock surface,其他 client 看不见
// 4. unlock 时清理

use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
// 锁屏 SessionLockHandler 实现
// lingxi 0.1 自实现锁屏 UI (取代 waylock — 外观朴素)
// 流程: lock() 隐藏所有窗口 + 清空密码缓冲 + 立即 confirmation.lock()
//  client (暂无) 不实际触发, 我们通过内部 Action::Lock 路径走
use smithay::wayland::session_lock::{LockSurface, SessionLockHandler, SessionLocker};

impl SessionLockHandler for LingxiState {
    fn lock_state(&mut self) -> &mut smithay::wayland::session_lock::SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        tracing::info!("🔒 会话锁定请求 (lingxi 自绘 UI)");
        // 1. 隐藏所有 workspace 窗口 (锁屏后看不到原内容)
        for w in self.space.elements().cloned().collect::<Vec<_>>() {
            self.space.unmap_elem(&w);
        }
        // 2. 隐藏 ime_popup
        self.ime_popup = None;
        // 3. 标记锁屏态 + 清空密码输入
        self.locked = true;
        self.password_input.clear();
        self.password_error = None;
        // 4. 调 confirmation.lock() 发 "locked" 事件
        //    (协议规定必须先渲染 cleared frame 再调,但我们无 client, 立即调即可)
        confirmation.lock();
        self.needs_render = true;
        tracing::info!("✅ 进入锁屏态,等待用户输入密码");
    }

    fn unlock(&mut self) {
        tracing::info!("🔓 解锁请求");
        // 1. 清理
        self.lock_surfaces.clear();
        self.locked = false;
        self.password_input.clear();
        self.password_error = None;
        // 2. 重新 map 当前 workspace 的所有窗口
        let active = self.active_workspace;
        let ws_windows = self.workspaces[active].clone();
        for w in &ws_windows {
            self.space.map_element(w.clone(), (0, 0), true);
        }
        // 3. 重新布局
        if !ws_windows.is_empty() {
            self.relayout();
        }
        // 4. 恢复键盘焦点到当前活动窗口
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        let focus_target = ws_windows
            .first()
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        if let Some(surface) = focus_target {
            self.set_keyboard_focus_with_selection(Some(surface), serial);
        } else {
            self.set_keyboard_focus_with_selection(None, serial);
        }
        self.needs_render = true;
        tracing::info!("✅ 屏幕已解锁, 恢复 workspace {}", active + 1);
    }

    fn new_surface(&mut self, _surface: LockSurface, _output: WlOutput) {
        // 暂无 client 调 lock() (lingxi 0.1 自实现 UI), 这里空实现
        // 保留 trait impl 因为协议已 delegate
    }
}

delegate_session_lock!(LingxiState);
