//! 灵犀合成器核心状态 — Smithay 集成

pub mod handlers;
pub mod window;

use smithay::{
    desktop::{PopupManager, Space, Window},
    input::{keyboard::XkbConfig, Seat, SeatState},
    reexports::{
        calloop::LoopSignal,
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            Display, DisplayHandle,
        },
    },
    utils::{Clock, Logical, Monotonic, Point, Rectangle},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        cursor_shape::CursorShapeManagerState,
        dmabuf::{DmabufGlobal, DmabufState},
        fractional_scale::FractionalScaleManagerState,
        input_method::{InputMethodManagerState, PopupSurface},
        selection::{
            data_device::DataDeviceState,
            primary_selection::PrimarySelectionState,
        },
        output::OutputManagerState,
        session_lock::{SessionLockManagerState},
        shell::xdg::XdgShellState,
        shell::xdg::decoration::XdgDecorationState,
        shell::wlr_layer::WlrLayerShellState,
        shm::ShmState,
        text_input::TextInputManagerState,
        viewporter::ViewporterState,
        virtual_keyboard::VirtualKeyboardManagerState,
    },
};

use crate::config::LingxiConfig;
use crate::layout::{DwindleLayout, LayoutTree, WindowGeometry};
use window::{AnimatedRect, AnimationManager};

/// Number of workspaces
const NUM_WORKSPACES: usize = 5;

/// 全屏过渡阶段 — 等待 wayland client ack configure 后才视为真全屏
#[derive(Debug, Clone, Default)]
pub enum FullscreenPhase {
    /// 没有全屏
    #[default]
    Off,
    /// 已发 configure, 等待 client commit 时 ack
    Pending { since: std::time::Instant, fallback_geo: WindowGeometry },
    /// client 已 ack, 真正全屏
    Active,
}


/// 单个浮动窗口的完整状态 (窗口 + 几何 + z 序) — 合并旧 floating/floating_geo/floating_z_order 三 Vec
pub struct FloatingWindow {
    pub window: Window,
    pub geo: WindowGeometry,
    pub z: usize,
}

/// 浮动窗口统一管理 (单一事实源).
/// 旧的 floating / floating_geo / floating_z_order 三个平行 Vec 已合并于此,
/// 消除三处同步; z 用单调递增计数器, z 大 = 上层.
pub struct FloatingManager {
    entries: Vec<FloatingWindow>,
    next_z: usize,
}

impl FloatingManager {
    pub fn new() -> Self {
        Self { entries: Vec::new(), next_z: 0 }
    }

    pub fn contains(&self, w: &Window) -> bool {
        self.entries.iter().any(|f| &f.window == w)
    }

    /// 平铺 → 浮动 (置于最上层)
    pub fn add(&mut self, window: Window, geo: WindowGeometry) {
        let z = self.next_z;
        self.next_z += 1;
        self.entries.push(FloatingWindow { window, geo, z });
    }

    /// 浮动 → 平铺 / 销毁. 返回是否曾浮动.
    pub fn remove(&mut self, w: &Window) -> bool {
        let before = self.entries.len();
        self.entries.retain(|f| &f.window != w);
        self.entries.len() < before
    }

    pub fn geo(&self, w: &Window) -> Option<WindowGeometry> {
        self.entries.iter().find(|f| &f.window == w).map(|f| f.geo)
    }

    /// 可变几何 (移动/缩放用)
    pub fn geo_mut(&mut self, w: &Window) -> Option<&mut WindowGeometry> {
        self.entries.iter_mut().find(|f| &f.window == w).map(|f| &mut f.geo)
    }

    /// 更新几何; 不存在则 add (容错, 同旧 set_floating_geo 行为)
    pub fn set_geo(&mut self, w: &Window, geo: WindowGeometry) {
        if let Some(f) = self.entries.iter_mut().find(|f| &f.window == w) {
            f.geo = geo;
        } else {
            let z = self.next_z;
            self.next_z += 1;
            self.entries.push(FloatingWindow { window: w.clone(), geo, z });
        }
    }

    /// 提升到最上层 (bump z)
    pub fn raise(&mut self, w: &Window) {
        if let Some(f) = self.entries.iter_mut().find(|f| &f.window == w) {
            f.z = self.next_z;
            self.next_z += 1;
        }
    }

    /// z 序列表 (z 升序 = 底→顶). 返回所有浮动窗口, 供渲染.
    pub fn z_ordered(&self) -> Vec<&Window> {
        let mut v: Vec<&FloatingWindow> = self.entries.iter().collect();
        v.sort_by_key(|f| f.z);
        v.into_iter().map(|f| &f.window).collect()
    }
}

/// 合成器全局状态
pub struct LingxiState {
    // === Smithay 协议状态 ===
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub layer_shell_state: WlrLayerShellState,
    pub viewporter_state: ViewporterState,
    pub fractional_scale_state: FractionalScaleManagerState,
    pub xdg_decoration_state: XdgDecorationState,
    pub cursor_shape_state: CursorShapeManagerState,
    pub input_method_state: InputMethodManagerState,
    pub text_input_state: TextInputManagerState,
    pub virtual_keyboard_state: VirtualKeyboardManagerState,
    pub session_lock_state: SessionLockManagerState,
    /// Output 管理 (含 zxdg_output_manager_v1, waybar 等需要 xdg-output 协议)
    pub output_manager_state: OutputManagerState,

    // === IME popup (fcitx5 候选词窗口) ===
    pub ime_popup: Option<PopupSurface>,

    // === Popup 管理 (xdg-popup, 如 Firefox 下载弹窗、右键菜单、下拉选择) ===
    pub popup_manager: PopupManager,

    // === 锁屏 (lingxi 0.1 自实现, 用 ext-session-lock-v1 协议做协议合规) ===
    /// 当前是否锁屏
    pub locked: bool,
    /// 锁屏 surface (lingxi 自绘时不存,保留防扩展)
    pub lock_surfaces: Vec<smithay::wayland::session_lock::LockSurface>,
    /// 锁屏时用户输入的密码 (按回车调 PAM 验证)
    pub password_input: String,
    /// 锁屏时密码错误信息 (Some = 验证失败, 显示几秒后清空)
    pub password_error: Option<String>,

    // === 核心状态 ===
    pub space: Space<Window>,
    pub seat: Seat<Self>,
    pub clock: Clock<Monotonic>,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,

    // === 输入状态 ===
    pub pointer_location: Point<f64, Logical>,

    /// 鼠标拖拽中的浮动窗口 (None = 未拖拽). review #15.
    pub drag_window: Option<Window>,
    /// 拖拽偏移: 指针位置 - 窗口左上 (拖拽期间保持, 窗口跟手)
    pub drag_offset: (f64, f64),

    // === 动画 & 布局 ===
    pub animations: AnimationManager,
    pub layout: DwindleLayout,
    /// 每工作区一棵持久化 dwindle 布局树 (架构 A). 跟踪 tiled 窗口分割结构.
    /// TODO(架构I): 合并入 Workspace struct.
    pub layout_trees: Vec<LayoutTree>,

    // === 工作区 ===
    pub workspaces: Vec<Vec<Window>>,
    pub active_workspace: usize,

    /// 窗口索引: toplevel wl_surface → Window (O(1) 查找, 替代 space.elements().find 线性扫描).
    /// 在 new_toplevel 插入, toplevel_destroyed 移除; switch/move 不新建不销毁窗口, 不触碰.
    pub by_surface: std::collections::HashMap<
        smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        Window,
    >,

    /// 当前全屏窗口 (None = 无全屏)
    pub fullscreen_window: Option<Window>,

    /// 浮动窗口统一管理 (窗口 + 几何 + z 序, 单一事实源)
    pub floating: FloatingManager,
    /// 全屏过渡阶段 (Wayland client ack 同步)
    pub fs_phase: FullscreenPhase,

    // === 灵犀特有 ===
    pub config: LingxiConfig,
    pub start_time: std::time::Instant,

    /// 脏标记: 为 true 时下一轮需要重绘 (commit/输入/布局变化时置位)
    pub needs_render: bool,
}

/// 客户端私有数据
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl LingxiState {
    pub fn new(
        display: &Display<Self>,
        loop_signal: LoopSignal,
        config: LingxiConfig,
    ) -> Self {
        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        let fractional_scale_state = FractionalScaleManagerState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let cursor_shape_state = CursorShapeManagerState::new::<Self>(&dh);

        // 注册输入法协议 (中文输入 fcitx5 需要)
        let input_method_state =
            InputMethodManagerState::new::<Self, _>(&dh, |_client| true);
        let text_input_state = TextInputManagerState::new::<Self>(&dh);
        let virtual_keyboard_state =
            VirtualKeyboardManagerState::new::<Self, _>(&dh, |_client| true);

        // 锁屏协议 (swaylock / hyprlock 用 ext-session-lock-v1)
        let session_lock_state = SessionLockManagerState::new::<Self, _>(&dh, |_| true);
        // Output 管理 (zxdg_output_manager_v1, 让 waybar 等客户端能拿准确的 monitor 信息)
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);

        let mut seat = seat_state.new_wl_seat(&dh, "lingxi");

        // 初始化键盘 (默认 xkb 配置)
        seat.add_keyboard(XkbConfig::default(), 200, 25)
            .expect("Failed to add keyboard");

        // 初始化鼠标
        seat.add_pointer();

        // Initialize workspaces
        let workspaces: Vec<Vec<Window>> = (0..NUM_WORKSPACES).map(|_| Vec::new()).collect();

        Self {
            compositor_state,
            xdg_shell_state,
            shm_state,
            seat_state,
            data_device_state,
            primary_selection_state,
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            layer_shell_state,
            viewporter_state,
            fractional_scale_state,
            xdg_decoration_state,
            cursor_shape_state,
            input_method_state,
            text_input_state,
            virtual_keyboard_state,
            session_lock_state,
            output_manager_state,
            ime_popup: None,
            popup_manager: PopupManager::default(),
            locked: false,
            lock_surfaces: Vec::new(),
            password_input: String::new(),
            password_error: None,
            space: Space::default(),
            seat,
            clock: Clock::new(),
            display_handle: dh,
            loop_signal,
            pointer_location: Point::from((0.0, 0.0)),
            drag_window: None,
            drag_offset: (0.0, 0.0),
            animations: AnimationManager::new(),
            layout: DwindleLayout {
                split_ratio: config.layout.split_ratio,
                inner_gap: config.general.gaps_inner as f64,
            },
            layout_trees: (0..NUM_WORKSPACES)
                .map(|_| LayoutTree::new(config.layout.split_ratio, config.general.gaps_inner as f64))
                .collect(),
            workspaces,
            active_workspace: 0,
            by_surface: std::collections::HashMap::new(),
            fullscreen_window: None,
            floating: FloatingManager::new(),
            fs_phase: FullscreenPhase::Off,
            config,
            start_time: std::time::Instant::now(),
            needs_render: true,
        }
    }

    /// 获取输出可用区域 (留 outer gap + 避开 Top/Bottom exclusive layer)
    ///
    /// Top layer (waybar) 自动从 y=0 占一些高度, Bottom layer (dock 等) 从底部占.
    /// 我们读 layer map 的 layer_geometry 算被占的边界,让窗口不重叠.
    fn usable_area(&self) -> WindowGeometry {
        let gap = self.config.general.gaps_outer as f64;
        let output_geo = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
            .map(|geo| (geo.size.w as f64, geo.size.h as f64))
            .unwrap_or((2560.0, 1600.0));

        // 计算 Top/Bottom layer 占据的区域 (waybar 等)
        //
        // waybar 不会主动设 exclusive_zone, 所以 smithay 不会让位。
        // 我们手动读 layer_map: 找顶部 anchor TOP-only 的 layer (waybar 风格),
        // 用它的 buffer height 作为 waybar 高度 (默认 28px)
        use smithay::wayland::shell::wlr_layer::Layer;
        let output = self.space.outputs().next();
        let (mut top, mut bottom, mut left, mut right): (f64, f64, f64, f64) =
            (0.0, 0.0, 0.0, 0.0);
        if let Some(output) = output {
            use smithay::desktop::layer_map_for_output;
            let map = layer_map_for_output(output);
            // 顶部状态栏 (Top layer, anchor TOP)
            for layer_surface in map.layers_on(Layer::Top) {
                let Some(geo) = map.layer_geometry(layer_surface) else { continue };
                let buffer_h = layer_surface.bbox().size.h as f64;
                if buffer_h > 0.0 && buffer_h < 200.0 && geo.loc.y <= 0 {
                    top = top.max(buffer_h);
                }
            }
            // 底部 dock (Bottom layer, anchor BOTTOM)
            for layer_surface in map.layers_on(Layer::Bottom) {
                let Some(geo) = map.layer_geometry(layer_surface) else { continue };
                let buffer_h = layer_surface.bbox().size.h as f64;
                if buffer_h > 0.0 && buffer_h < 200.0
                    && (geo.loc.y as f64 + geo.size.h as f64) >= (output_geo.1 - 1.0)
                {
                    bottom = bottom.max(buffer_h);
                }
            }
            // 左侧 layer (Top, anchor LEFT)
            for layer_surface in map.layers_on(Layer::Top) {
                let Some(geo) = map.layer_geometry(layer_surface) else { continue };
                let buffer_w = layer_surface.bbox().size.w as f64;
                if buffer_w > 0.0 && buffer_w < 200.0 && geo.loc.x <= 0 {
                    left = left.max(buffer_w);
                }
            }
            // 右侧 layer (Top, anchor RIGHT)
            for layer_surface in map.layers_on(Layer::Top) {
                let Some(geo) = map.layer_geometry(layer_surface) else { continue };
                let buffer_w = layer_surface.bbox().size.w as f64;
                if buffer_w > 0.0 && buffer_w < 200.0
                    && (geo.loc.x as f64 + geo.size.w as f64) >= (output_geo.0 - 1.0)
                {
                    right = right.max(buffer_w);
                }
            }
        }

        WindowGeometry {
            x: gap + left,
            y: gap + top,
            width: output_geo.0 - 2.0 * gap - left - right,
            height: output_geo.1 - 2.0 * gap - top - bottom,
        }
    }

    /// 重新计算布局并动画过渡所有窗口到新位置
    /// 取窗口在当前工作区 tiled 列表中的槽位 (排除浮动). 用于布局树 insert/remove 定位.
    fn tiled_slot_of(&self, window: &Window) -> Option<usize> {
        self.workspaces[self.active_workspace]
            .iter()
            .filter(|w| !self.floating.contains(w))
            .position(|w| w == window)
    }

    pub fn relayout(&mut self) {
        let active = self.active_workspace;
        let ws: Vec<Window> = self.workspaces[active].clone();
        if ws.is_empty() {
            return;
        }
        self.needs_render = true;

        let area = self.usable_area();
        let full = self.output_full_area();
        let fs = self.fullscreen_window.clone();

        // 几何由持久化布局树算 (树自带 tiled 叶子数, 架构 A). 全屏窗口仍占树槽位但渲染时覆盖.
        let geometries = self.layout_trees[active].arrange(area);

        // 计算每个窗口的目标矩形 (按 workspaces 顺序, 与树槽位对齐)
        let mut targets: Vec<(Window, AnimatedRect)> = Vec::with_capacity(ws.len());
        let mut tiled_idx = 0usize;
        for w in &ws {
            let rect = if Some(w) == fs.as_ref() {
                WindowGeometry { x: full.x, y: full.y, width: full.width, height: full.height }
            } else if self.floating.contains(w) {
                self.floating
                    .geo(w)
                    .unwrap_or(WindowGeometry { x: area.x + 100.0, y: area.y + 100.0, width: 800.0, height: 600.0 })
            } else {
                let g = geometries.get(tiled_idx).copied().unwrap_or(area);
                tiled_idx += 1;
                g
            };
            targets.push((
                w.clone(),
                AnimatedRect { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
            ));
        }

        self.animations.retarget(&targets);

        // 发送 configure (尺寸)
        for (w, rect) in &targets {
            if let Some(toplevel) = w.toplevel() {
                toplevel.with_pending_state(|pending| {
                    pending.size = Some((rect.width as i32, rect.height as i32).into());
                });
                toplevel.send_configure();
            }
        }
    }

    /// 输出完整区域 (不留 gap, 用于全屏)
    fn output_full_area(&self) -> WindowGeometry {
        let (w, h) = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
            .map(|geo| (geo.size.w as f64, geo.size.h as f64))
            .unwrap_or((2560.0, 1600.0));
        WindowGeometry { x: 0.0, y: 0.0, width: w, height: h }
    }

    /// 切换当前焦点窗口的全屏状态
    pub fn toggle_fullscreen(&mut self) {
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgState;

        let focused = match self.focused_window() {
            Some(w) => w,
            None => return,
        };

        let is_fullscreen = self.fullscreen_window.as_ref() == Some(&focused);

        if is_fullscreen {
            // 退出全屏
            self.fullscreen_window = None;
            self.fs_phase = FullscreenPhase::Off;
            if let Some(toplevel) = focused.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.states.unset(XdgState::Fullscreen);
                });
                toplevel.send_configure();
            }
            tracing::info!("退出全屏");
        } else {
            // 进入全屏: 先发 configure, 等 client ack 后才真正全屏
            let fallback_geo = self.window_geometry(&focused);
            self.fullscreen_window = Some(focused.clone());
            self.fs_phase = FullscreenPhase::Pending {
                since: std::time::Instant::now(),
                fallback_geo,
            };
            if let Some(toplevel) = focused.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.states.set(XdgState::Fullscreen);
                });
                toplevel.send_configure();
            }
            tracing::info!("进入全屏 (等待 client ack)");
        }

        self.relayout();
    }

    /// 切换焦点窗口的浮动/平铺状态
    pub fn toggle_floating(&mut self) {
        let focused = match self.focused_window() {
            Some(w) => w,
            None => return,
        };

        if self.floating.contains(&focused) {
            // 浮动 → 平铺: 先把窗口移到 workspaces 末尾, 再树加最右叶子,
            // 保持 workspaces tiled 顺序 == 树 in-order 顺序 (架构 A 顺序不变量, 复审 5.4)
            self.floating.remove(&focused);
            {
                let ws = &mut self.workspaces[self.active_workspace];
                ws.retain(|w| w != &focused);
                ws.push(focused.clone());
            }
            self.layout_trees[self.active_workspace].insert();
            tracing::info!("窗口切回平铺");
        } else {
            // 平铺 → 浮动: 先算 tiled 槽位, 再加浮动, 再删树叶
            let slot = self.tiled_slot_of(&focused);
            let area = self.usable_area();
            let width = area.width * 0.6;
            let height = area.height * 0.6;
            let cur = WindowGeometry {
                x: area.x + (area.width - width) / 2.0,
                y: area.y + (area.height - height) / 2.0,
                width,
                height,
            };
            self.floating.add(focused.clone(), cur);
            if let Some(s) = slot {
                self.layout_trees[self.active_workspace].remove(s);
            }
            tracing::info!("窗口切为浮动 (居中 {}x{})", width as i32, height as i32);
        }
        self.relayout();
    }

    /// 提升窗口到 z_order 最上 (focus / click 时调用)
    pub fn raise_window(&mut self, w: &Window) {
        self.floating.raise(w);
    }

    /// 获取 z 序的浮动窗口列表 (底 → 顶), 供渲染. 返回所有浮动窗口.
    pub fn floating_z_ordered(&self) -> Vec<&Window> {
        self.floating.z_ordered()
    }

    /// 窗口是否浮动
    pub fn is_floating(&self, window: &Window) -> bool {
        self.floating.contains(window)
    }

    /// 获取某窗口的屏幕几何 (用于定位 IME popup)
    pub fn window_screen_geometry(&self, window: &Window) -> Rectangle<i32, Logical> {
        let loc = self.space.element_location(window).unwrap_or_default();
        let geo = window.geometry();
        Rectangle::new(loc, geo.size)
    }

    /// 取窗口当前几何 (优先浮动几何, 否则用 space 中的位置+尺寸)
    fn window_geometry(&self, window: &Window) -> WindowGeometry {
        if let Some(g) = self.floating.geo(window) {
            return g;
        }
        // 优先使用 compositor 自己的 layout target (AnimationManager.target),
        // 避免读到 wayland client 还没 ack 的 stale window.geometry()
        if let Some(target) = self.animations.get_target(window) {
            let loc = self.space.element_location(window).unwrap_or_default();
            return WindowGeometry {
                x: loc.x as f64,
                y: loc.y as f64,
                width: target.width,
                height: target.height,
            };
        }
        // Fallback: 窗口刚创建, 还未注册到 animations
        let loc = self.space.element_location(window).unwrap_or_default();
        let geo = window.geometry();
        WindowGeometry {
            x: loc.x as f64,
            y: loc.y as f64,
            width: geo.size.w as f64,
            height: geo.size.h as f64,
        }
    }


    /// 拖动过程中更新 grab
    ///
    /// Hyprland 参考: 拖动中只做位置更新，不触发布局重算。
    /// 有拖动阈值 (~4px) 防止误触。
    /// 立即更新浮动窗口几何, 不走动画。reconfigure=true 时重发尺寸 configure。
    fn set_floating_geo(&mut self, window: &Window, geo: WindowGeometry, reconfigure: bool) {
        self.floating.set_geo(window, geo);
        // 直接落位 (移动/缩放需即时跟手)
        self.animations.remove_window(window);
        self.space.map_element(window.clone(), (geo.x as i32, geo.y as i32), false);
        if reconfigure {
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|pending| {
                    pending.size = Some((geo.width as i32, geo.height as i32).into());
                });
                toplevel.send_configure();
            }
        }
    }

    /// 开始拖拽浮动窗口: 记录指针相对窗口左上的偏移 (review #15)
    pub fn start_drag(&mut self, window: &Window) {
        let win_loc = self.space.element_location(window).unwrap_or_default();
        self.drag_offset = (
            self.pointer_location.x - win_loc.x as f64,
            self.pointer_location.y - win_loc.y as f64,
        );
        self.drag_window = Some(window.clone());
    }

    /// 拖拽中: 按指针位置 - 偏移 移动浮动窗口 (即时跟手, 不走动画/不发 configure)
    pub fn update_drag(&mut self) {
        let window = match self.drag_window.clone() {
            Some(w) => w,
            None => return,
        };
        let geo = match self.floating.geo(&window) {
            Some(g) => g,
            None => return,
        };
        let new_geo = WindowGeometry {
            x: self.pointer_location.x - self.drag_offset.0,
            y: self.pointer_location.y - self.drag_offset.1,
            width: geo.width,
            height: geo.height,
        };
        self.set_floating_geo(&window, new_geo, false);
        self.needs_render = true;
    }

    /// 结束拖拽
    pub fn end_drag(&mut self) {
        self.drag_window = None;
    }

    /// 标记需要重绘 (commit/输入/布局/焦点变化时调用)
    pub fn mark_dirty(&mut self) {
        self.needs_render = true;
    }

    /// 每帧推进动画，更新窗口位置
    pub fn tick_animations(&mut self) {
        if !self.animations.has_active_animations() {
            return;
        }

        let (updates, any_finished) = self.animations.tick();
        for (window, pos) in updates {
            // location 即可见内容目标位置; 渲染器会自行减去 geometry().loc 得到 buffer 原点
            self.space.map_element(window, pos, false);
        }
        // 动画刚结束的那帧置 needs_render, 保证最后一帧(落位)被绘制
        if any_finished {
            self.needs_render = true;
        }
    }

    /// 主循环共享子序列: 刷新 space / 清 popup / arrange layer / 推进动画.
    /// winit 与 drm 两个后端主循环共用, 避免重复 (架构 F).
    /// 注: dispatch_clients/flush 与 event_loop.dispatch 的顺序两后端刻意不同, 不在此统一.
    pub fn pre_render_tick(&mut self) {
        self.space.refresh();
        self.popup_manager.cleanup();
        for output in self.space.outputs().cloned().collect::<Vec<_>>() {
            let _ = smithay::desktop::layer_map_for_output(&output).arrange();
        }
        self.tick_animations();
    }

    /// 本帧是否需要渲染 (脏标记或动画进行中)
    pub fn should_render(&self) -> bool {
        self.needs_render || self.animations.has_active_animations()
    }

    /// Get the currently focused window (matching keyboard focus surface)
    pub fn focused_window(&self) -> Option<Window> {
        let keyboard = self.seat.get_keyboard()?;
        let focus_surface = keyboard.current_focus()?;
        self.by_surface.get(&focus_surface).cloned()
    }

    /// Switch to workspace N (0-based index)
    pub fn switch_workspace(&mut self, target: usize) {
        if target >= self.workspaces.len() || target == self.active_workspace {
            return;
        }
        self.needs_render = true;

        tracing::info!("Switching workspace {} -> {}", self.active_workspace + 1, target + 1);

        // Unmap all current windows from space
        let current_windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in &current_windows {
            self.animations.remove_window(window);
            self.space.unmap_elem(window);
        }

        // Update active workspace
        self.active_workspace = target;

        // Map windows from target workspace
        let ws_windows = self.workspaces[target].clone();
        for window in &ws_windows {
            self.space.map_element(window.clone(), (0, 0), false);
        }

        // Relayout (用目标工作区的布局树; 只给 tiled 窗口入场动画)
        if !ws_windows.is_empty() {
            let area = self.usable_area();
            let geometries = self.layout_trees[target].arrange(area);
            let tiled: Vec<Window> = ws_windows
                .iter()
                .filter(|w| !self.floating.contains(w))
                .cloned()
                .collect();

            let output_center = self
                .space
                .outputs()
                .next()
                .and_then(|o| self.space.output_geometry(o))
                .map(|geo| (geo.size.w as f64 / 2.0, geo.size.h as f64 / 2.0))
                .unwrap_or((640.0, 400.0));

            for (i, window) in tiled.iter().enumerate() {
                let target_rect = AnimatedRect {
                    x: geometries[i].x,
                    y: geometries[i].y,
                    width: geometries[i].width,
                    height: geometries[i].height,
                };
                self.animations.add_window(window.clone(), target_rect, output_center);
            }

            // Set focus to first window in new workspace
            if let Some(first_window) = ws_windows.first() {
                if let Some(toplevel) = first_window.toplevel() {
                    let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                    self.set_keyboard_focus_with_selection(
                        Some(toplevel.wl_surface().clone()),
                        serial,
                    );
                }
            }
        }
    }

    /// Move focused window to workspace N (0-based index)
    /// i3 风格: 移动浮动窗口 (Mod+Shift+方向键)
    pub fn move_floating_focused(&mut self, dx: f64, dy: f64) {
        let focused = match self.focused_window() {
            Some(w) => w,
            None => return,
        };
        if !self.floating.contains(&focused) {
            tracing::info!("move_floating: 窗口非浮动, 忽略");
            return;
        }
        if let Some(g) = self.floating.geo_mut(&focused) {
            g.x += dx;
            g.y += dy;
            self.space.map_element(focused.clone(), (g.x as i32, g.y as i32), false);
            self.needs_render = true;
            tracing::info!("浮动窗口移动 +({},{})", dx, dy);
        }
    }

    /// i3 风格: 缩放浮动窗口 (Mod+R 模式 + 方向键)
    pub fn resize_floating_focused(&mut self, dw: f64, dh: f64) {
        let focused = match self.focused_window() {
            Some(w) => w,
            None => return,
        };
        if !self.floating.contains(&focused) {
            return;
        }
        if let Some(g) = self.floating.geo_mut(&focused) {
            g.width = (g.width + dw).max(200.0);
            g.height = (g.height + dh).max(150.0);
            self.space.map_element(focused.clone(), (g.x as i32, g.y as i32), false);
            if let Some(toplevel) = focused.toplevel() {
                toplevel.with_pending_state(|pending| {
                    pending.size = Some((g.width as i32, g.height as i32).into());
                });
                toplevel.send_configure();
            }
            self.needs_render = true;
            tracing::info!("浮动窗口缩放 +{},{}", dw, dh);
        }
    }

    pub fn move_to_workspace(&mut self, target: usize) {
        if target >= self.workspaces.len() {
            return;
        }

        let focused = match self.focused_window() {
            Some(w) => w,
            None => return,
        };

        // Remove from current workspace
        let current = self.active_workspace;
        let was_floating = self.floating.contains(&focused);
        // 布局树: tiled 窗口从源工作区删叶子 (浮动不在树中, 不动)
        if !was_floating {
            if let Some(slot) = self.tiled_slot_of(&focused) {
                self.layout_trees[current].remove(slot);
            }
        }
        self.workspaces[current].retain(|w| w != &focused);

        // Add to target workspace
        self.workspaces[target].push(focused.clone());
        if !was_floating {
            self.layout_trees[target].insert();
        }

        if target != current {
            // Unmap from space (since it's now in a different workspace)
            self.animations.remove_window(&focused);
            self.space.unmap_elem(&focused);
            self.relayout();
            tracing::info!("Moved window to workspace {}", target + 1);
        }
    }
}
