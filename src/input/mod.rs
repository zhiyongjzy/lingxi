//! 灵犀输入处理 — 键盘/鼠标事件路由 + 快捷键系统

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
        PointerMotionEvent,
    },
    desktop::{WindowSurfaceType, layer_map_for_output},
    input::{
        keyboard::{FilterResult, KeysymHandle, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::{
        protocol::wl_surface::WlSurface, Resource,
    },
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::{
        selection::{
            data_device::DataDeviceHandler,
            primary_selection::PrimarySelectionHandler,
        },
        shell::wlr_layer::Layer,
    },
};

use crate::compositor::LingxiState;
use crate::config::ParsedKeyBind;

/// 合成器动作 (keybind 触发)
#[derive(Debug, Clone)]
enum Action {
    Exec(String),
    CloseWindow,
    Quit,
    FocusNext,
    FocusPrev,
    SwapNext,
    SwapPrev,
    ToggleFullscreen,
    ToggleFloating,
    /// i3 风格: 移动浮动窗口 (dx, dy 像素)
    MoveFloating(f64, f64),
    ResizeRatio(f64),
    Workspace(usize),
    MoveToWorkspace(usize),
    /// 锁屏: lingxi 自绘 UI (壁纸 + 密码框 + PAM 认证)
    Lock,
    /// 标记已处理 (用于音量/亮度键等 — 在 filter 阶段弹 OSD,不需要 execute)
    OsdHandled,
}

impl LingxiState {
    /// 焦点切换 helper — 同时更新 keyboard focus, data device focus, primary selection focus
    /// 否则剪贴板复制粘贴不工作 (smithay 默认不自动调 set_data_device_focus)
    pub fn set_keyboard_focus_with_selection(
        &mut self,
        surface: Option<WlSurface>,
        serial: smithay::utils::Serial,
    ) {
        // 拆借为借用: 顺序执行 keyboard.set_focus, set_data_device_focus, set_primary_focus
        // 用 DisplayHandle clone (cheap) 避免 self 同时被多借
        let dh = self.display_handle.clone();
        if let Some(keyboard) = self.seat.get_keyboard() {
            // 1. 通知 client 它的键盘 focus 变了
            keyboard.set_focus(self, surface.clone(), serial);
        }
        // 2. 通知 data_device (CLIPBOARD) focus — 跨应用复制粘贴必需
        //    Resource::client() 从 WlSurface 反查它所属的 Client
        let client = surface.as_ref().and_then(|s| s.client());
        smithay::wayland::selection::data_device::set_data_device_focus::<Self>(&dh, &self.seat, client.clone());
        // 3. 通知 primary_selection (鼠标中键粘贴)
        smithay::wayland::selection::primary_selection::set_primary_focus::<Self>(&dh, &self.seat, client);
    }

    /// 处理来自后端的输入事件
    pub fn handle_input<B: InputBackend>(&mut self, event: InputEvent<B>) {
        // 任何输入都可能改变画面 (光标移动/焦点/动作) → 标记重绘
        self.needs_render = true;
        match event {
            InputEvent::Keyboard { event } => self.handle_keyboard::<B>(event),
            InputEvent::PointerMotion { event } => {
                self.handle_pointer_motion_relative::<B>(event)
            }
            InputEvent::PointerMotionAbsolute { event } => {
                self.handle_pointer_motion_absolute::<B>(event)
            }
            InputEvent::PointerButton { event } => self.handle_pointer_button::<B>(event),
            InputEvent::PointerAxis { event } => self.handle_pointer_axis::<B>(event),
            _ => {}
        }
    }

    /// 处理键盘事件 — 优先拦截合成器快捷键
    fn handle_keyboard<B: InputBackend>(&mut self, event: B::KeyboardKeyEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let time = Event::time_msec(&event);
        let keycode = event.key_code();
        let press_state = event.state();

        // 锁屏时: 拦截所有键盘事件,不 forward 给 client
        // 密码积累 + 回车验证 + Backspace 删除
        if self.locked {
            if press_state != smithay::backend::input::KeyState::Pressed {
                return;
            }
            // 用 input_intercept 拿 keysym (不 forward 给 client)
            if let Some(keyboard) = self.seat.get_keyboard() {
                let (raw_sym, _) = keyboard.input_intercept::<u32, _>(
                    self, keycode, press_state,
                    |_state, _mods, keysym| {
                        keysym.modified_sym().raw()
                    },
                );
                {
                    let raw_sym = raw_sym;
                    match raw_sym {
                        0xff0d => {
                            // Enter: 验证密码
                            let pw = self.password_input.clone();
                            match crate::auth::verify_password(&pw) {
                                Ok(()) => {
                                    tracing::info!("🔓 密码正确, 解锁");
                                    self.locked = false;
                                    self.password_input.clear();
                                    self.password_error = None;
                                    // 恢复窗口
                                    let active = self.active_workspace;
                                    let ws = self.workspaces[active].clone();
                                    for w in &ws {
                                        self.space.map_element(w.clone(), (0, 0), true);
                                    }
                                    if !ws.is_empty() {
                                        self.relayout();
                                    }
                                    // 恢复键盘焦点 (与协议解锁路径一致, 否则解锁后焦点为 None)
                                    let focus_target = ws
                                        .first()
                                        .and_then(|w| w.toplevel())
                                        .map(|t| t.wl_surface().clone());
                                    let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                                    self.set_keyboard_focus_with_selection(focus_target, serial);
                                }
                                Err(e) => {
                                    tracing::warn!("🔒 密码错误: {}", e);
                                    self.password_input.clear();
                                    self.password_error = Some("Password incorrect".into());
                                }
                            }
                        }
                        0xff08 => {
                            // Backspace: 删除最后一个字符
                            self.password_input.pop();
                        }
                        0xff1b => {
                            // Escape: 清空密码
                            self.password_input.clear();
                            self.password_error = None;
                        }
                        sym if sym >= 0x20 && sym <= 0x7e => {
                            // 可打印 ASCII 字符
                            let ch = sym as u8 as char;
                            self.password_input.push(ch);
                            self.password_error = None;
                            tracing::debug!("🔒 pw char: '{}' (sym=0x{:x}), total_len={}", ch, sym, self.password_input.len());
                        }
                        _ => {}
                    }
                }
            }
            self.needs_render = true;
            return;
        }

        let keyboard = match self.seat.get_keyboard() {
            Some(kb) => kb,
            None => return,
        };

        // Pre-parse keybinds from config
        let parsed_binds = self.config.parsed_binds();

        // 用 filter 闭包检测快捷键
        let action = keyboard.input(
            self,
            keycode,
            press_state,
            serial,
            time,
            |_state, modifiers, keysym| {
                if press_state != KeyState::Pressed {
                    return FilterResult::Forward;
                }

                let sym = keysym.modified_sym();
                tracing::debug!(
                    "按键: keycode={:?}, sym=0x{:x}, logo={}, shift={}, ctrl={}, alt={}",
                    keycode, sym.raw(), modifiers.logo, modifiers.shift, modifiers.ctrl, modifiers.alt
                );

                // 音量/亮度键: 拦截并调 swayosd-client 弹 OSD
                // XF86AudioLowerVolume = 0x1008ff11, AudioMute = 0x1008ff12, AudioRaise = 0x1008ff13
                // XF86MonBrightnessDown = 0x1008ff03, MonBrightnessUp = 0x1008ff02
                let raw = sym.raw();
                let osd_arg = match raw {
                    0x1008ff11 => Some("output-volume lower"),
                    0x1008ff13 => Some("output-volume raise"),
                    0x1008ff12 => Some("output-volume mute-toggle"),
                    0x1008ff03 => Some("brightness lower"),
                    0x1008ff02 => Some("brightness raise"),
                    _ => None,
                };
                if let Some(arg) = osd_arg {
                    // 调 swayosd-client 弹 OSD (不 forward 给 client,避免 Firefox 也响应)
                    std::process::Command::new("swayosd-client")
                        .args(arg.split_whitespace())
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                        .ok();
                    tracing::info!("OSD 触发: {}", arg);
                    return FilterResult::Intercept(Action::OsdHandled);
                }

                // i3 风格内置快捷键 (不依赖 keybind 配置)
                if modifiers.logo && modifiers.shift && !modifiers.ctrl && !modifiers.alt {
                    // Super+Shift+方向键: 移动浮动窗口 50px
                    let raw = sym.raw();
                    let (dx, dy) = match raw {
                        0xff51 => (-50.0,   0.0), // Left
                        0xff53 => ( 50.0,   0.0), // Right
                        0xff52 => (  0.0, -50.0), // Up
                        0xff54 => (  0.0,  50.0), // Down
                        _ => (0.0, 0.0),
                    };
                    if dx != 0.0 || dy != 0.0 {
                        return FilterResult::Intercept(Action::MoveFloating(dx, dy));
                    }
                }

                if let Some(action) = check_configurable_keybind(modifiers, &keysym, &parsed_binds) {
                    tracing::info!("匹配快捷键: {:?}", action);
                    FilterResult::Intercept(action)
                } else {
                    FilterResult::Forward
                }
            },
        );

        // 执行被拦截的动作 (此时 filter 闭包已结束，可以 mut borrow self)
        if let Some(action) = action {
            self.execute_action(action);
        }
    }

    /// 执行合成器动作
    fn execute_action(&mut self, action: Action) {
        match action {
            Action::Exec(cmd) => {
                tracing::info!("执行命令: {}", cmd);
                self.spawn_command(&cmd);
            }
            Action::CloseWindow => {
                tracing::info!("Super+Q: 关闭窗口");
                self.close_focused_window();
            }
            Action::Quit => {
                tracing::info!("Super+Shift+Q: 退出合成器");
                self.loop_signal.stop();
            }
            Action::FocusNext => self.cycle_focus(true),
            Action::FocusPrev => self.cycle_focus(false),
            Action::SwapNext => self.swap_focused(true),
            Action::SwapPrev => self.swap_focused(false),
            Action::ToggleFullscreen => {
                tracing::info!("Super+F: 全屏切换");
                self.toggle_fullscreen();
            }
            Action::ToggleFloating => {
                tracing::info!("Super+V: 浮动切换");
                self.toggle_floating();
            }
            Action::MoveFloating(dx, dy) => {
                self.move_floating_focused(dx, dy);
            }
            Action::ResizeRatio(delta) => {
                self.layout.split_ratio = (self.layout.split_ratio + delta).clamp(0.1, 0.9);
                self.relayout();
            }
            Action::Workspace(n) => {
                self.switch_workspace(n);
            }
            Action::MoveToWorkspace(n) => {
                self.move_to_workspace(n);
            }
            Action::Lock => {
                tracing::info!("🔒 Super+Esc: 进入锁屏");
                self.locked = true;
                self.password_input.clear();
                self.password_error = None;
                // 隐藏所有窗口
                for w in self.space.elements().cloned().collect::<Vec<_>>() {
                    self.space.unmap_elem(&w);
                }
                self.needs_render = true;
            }
            // OsdHandled: 音量/亮度键已在 filter 中处理 (调了 swayosd-client)
            // 这里不需要做任何事
            Action::OsdHandled => {}
        }
    }

    /// 启动命令
    fn spawn_command(&self, cmd: &str) {
        use std::process::{Command, Stdio};
        let xdg = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
        let wayland_display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
        tracing::info!("启动命令: {} (WAYLAND_DISPLAY={}, XDG_RUNTIME_DIR={})", cmd, wayland_display, xdg);

        // Split command into program and args
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.is_empty() {
            return;
        }

        let mut command = Command::new(parts[0]);
        if parts.len() > 1 {
            command.args(&parts[1..]);
        }

        match command
            .env("XDG_RUNTIME_DIR", &xdg)
            .env("WAYLAND_DISPLAY", &wayland_display)
            .env("GDK_BACKEND", "wayland")
            .env("QT_QPA_PLATFORM", "wayland")
            .env_remove("DISPLAY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => tracing::info!("已启动 {} (pid={})", parts[0], child.id()),
            Err(e) => tracing::error!("启动命令失败: {} - {}", cmd, e),
        }
    }

    /// 关闭当前焦点窗口
    fn close_focused_window(&self) {
        let keyboard = match self.seat.get_keyboard() {
            Some(kb) => kb,
            None => return,
        };

        if let Some(focus_surface) = keyboard.current_focus() {
            // 找到对应的 Window
            let window = self.space.elements().find(|w| {
                w.toplevel()
                    .map(|t| *t.wl_surface() == focus_surface)
                    .unwrap_or(false)
            });

            if let Some(window) = window {
                if let Some(toplevel) = window.toplevel() {
                    toplevel.send_close();
                }
            }
        }
    }

    /// 循环切换焦点
    fn cycle_focus(&mut self, forward: bool) {
        let windows: Vec<_> = self.space.elements().cloned().collect();
        if windows.is_empty() {
            return;
        }

        let keyboard = match self.seat.get_keyboard() {
            Some(kb) => kb,
            None => return,
        };
        let focused = keyboard.current_focus();

        let current_idx = focused
            .as_ref()
            .and_then(|surface| {
                windows.iter().position(|w| {
                    w.toplevel()
                        .map(|t| *t.wl_surface() == *surface)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(0);

        let next_idx = if forward {
            (current_idx + 1) % windows.len()
        } else {
            if current_idx == 0 { windows.len() - 1 } else { current_idx - 1 }
        };

        let target = &windows[next_idx];
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(toplevel) = target.toplevel() {
            self.set_keyboard_focus_with_selection(
                Some(toplevel.wl_surface().clone()),
                serial,
            );
            // 注意: 平铺模式不 raise，否则会打乱 space.elements() 顺序导致循环索引错乱
            self.needs_render = true;
            tracing::debug!("焦点: {} -> {}", current_idx, next_idx);
        }
    }

    /// 交换焦点窗口与相邻窗口
    ///
    /// 关键: 交换的是 workspaces[active] 底层列表顺序, 再重 map + relayout.
    /// 旧实现只 swap space 位置后 relayout, 而 relayout 按 space.elements() 插入序
    /// 重新分配几何, 立刻覆盖交换 → 视觉无效果.
    fn swap_focused(&mut self, forward: bool) {
        let active = self.active_workspace;
        if self.workspaces[active].len() < 2 {
            return;
        }

        let keyboard = match self.seat.get_keyboard() {
            Some(kb) => kb,
            None => return,
        };
        let focused = keyboard.current_focus();

        let current_idx = match focused.as_ref().and_then(|surface| {
            self.workspaces[active].iter().position(|w| {
                w.toplevel()
                    .map(|t| *t.wl_surface() == *surface)
                    .unwrap_or(false)
            })
        }) {
            Some(i) => i,
            None => return,
        };

        let len = self.workspaces[active].len();
        let swap_idx = if forward {
            (current_idx + 1) % len
        } else if current_idx == 0 {
            len - 1
        } else {
            current_idx - 1
        };

        // 交换底层窗口列表顺序 — relayout 按此顺序分配几何, 交换才生效
        self.workspaces[active].swap(current_idx, swap_idx);

        // 重 map 让 space.elements() 顺序与 workspaces 一致, 再 relayout 触发动画
        let ws = self.workspaces[active].clone();
        for w in &ws {
            self.space.unmap_elem(w);
        }
        for w in &ws {
            self.space.map_element(w.clone(), (0, 0), false);
        }
        self.relayout();
        tracing::info!("交换窗口: {} <-> {}", current_idx, swap_idx);
    }

    /// 处理鼠标相对移动 (DRM/libinput 后端)
    fn handle_pointer_motion_relative<B: InputBackend>(&mut self, event: B::PointerMotionEvent) {
        let output_size = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
            .map(|geo| (geo.size.w as f64, geo.size.h as f64))
            .unwrap_or((3840.0, 2160.0));

        let delta = event.delta();
        let new_x = (self.pointer_location.x + delta.x).clamp(0.0, output_size.0 - 1.0);
        let new_y = (self.pointer_location.y + delta.y).clamp(0.0, output_size.1 - 1.0);
        let pos: Point<f64, Logical> = (new_x, new_y).into();

        let serial = SERIAL_COUNTER.next_serial();
        let time = Event::time_msec(&event);
        let surface_under = self.surface_under(pos);

        let pointer = match self.seat.get_pointer() {
            Some(ptr) => ptr,
            None => return,
        };

        pointer.motion(
            self,
            surface_under,
            &MotionEvent {
                location: pos,
                serial,
                time,
            },
        );
        pointer.frame(self);
        self.pointer_location = pos;
    }

    /// 处理鼠标绝对移动 (winit 后端用绝对坐标)
    fn handle_pointer_motion_absolute<B: InputBackend>(&mut self, event: B::PointerMotionAbsoluteEvent) {
        let output = self.space.outputs().next().cloned();
        let output_geo = output
            .as_ref()
            .map(|o| self.space.output_geometry(o).unwrap_or_default())
            .unwrap_or_default();

        let pos: Point<f64, Logical> = (
            event.x_transformed(output_geo.size.w),
            event.y_transformed(output_geo.size.h),
        )
            .into();

        let serial = SERIAL_COUNTER.next_serial();
        let time = Event::time_msec(&event);
        let surface_under = self.surface_under(pos);

        let pointer = match self.seat.get_pointer() {
            Some(ptr) => ptr,
            None => return,
        };

        pointer.motion(
            self,
            surface_under,
            &MotionEvent {
                location: pos,
                serial,
                time,
            },
        );
        pointer.frame(self);
        self.pointer_location = pos;
    }

    /// 处理鼠标按键
    fn handle_pointer_button<B: InputBackend>(&mut self, event: B::PointerButtonEvent) {
        const BTN_LEFT: u32 = 0x110;
        const BTN_RIGHT: u32 = 0x111;

        let serial = SERIAL_COUNTER.next_serial();
        let button = event.button_code();
        let button_state = event.state();

        let mods = self
            .seat
            .get_keyboard()
            .map(|k| k.modifier_state());
        let logo = mods.map(|m| m.logo).unwrap_or(false);

        if button_state == ButtonState::Pressed {
            // B 方案: 鼠标只用于点击聚焦, 拖动用 Mod+Shift+方向键
            let under = self.space.element_under(self.pointer_location);
            if let Some((window, _)) = under {
                let window = window.clone();
                // 平铺布局: 点击只改焦点，不 raise (raise 会打乱 dwindle 顺序导致窗口跳位)
                let surface = window
                    .toplevel()
                    .expect("xdg toplevel")
                    .wl_surface()
                    .clone();
                self.set_keyboard_focus_with_selection(
                    Some(surface),
                    serial,
                );
            } else {
                self.set_keyboard_focus_with_selection(
                    None,
                    serial,
                );
            }
        }

        let pointer = match self.seat.get_pointer() {
            Some(ptr) => ptr,
            None => return,
        };

        pointer.button(
            self,
            &ButtonEvent {
                serial,
                time: Event::time_msec(&event),
                button,
                state: button_state,
            },
        );
        pointer.frame(self);
    }

    /// 处理滚轮 / 触控板滚动
    ///
    /// 两类源要分别处理:
    /// - Finger/Continuous (触控板): 走连续 amount() → wl_pointer.axis
    /// - Wheel (鼠标滚轮): amount() 多半为 0, 必须走 amount_v120() → wl_pointer.axis_value120
    ///   否则 Firefox 等客户端收不到离散滚动信号, 滚轮无反应 (拖滚动条仍可用, 走按键事件).
    fn handle_pointer_axis<B: InputBackend>(&mut self, event: B::PointerAxisEvent) {
        let pointer = match self.seat.get_pointer() {
            Some(ptr) => ptr,
            None => return,
        };

        let source = event.source();
        let mut frame = AxisFrame::new(Event::time_msec(&event)).source(source);

        // 连续值 (触控板)
        if let Some(amount) = event.amount(Axis::Horizontal) {
            if amount.abs() > f64::EPSILON {
                frame = frame.value(Axis::Horizontal, amount);
            }
        }
        if let Some(amount) = event.amount(Axis::Vertical) {
            if amount.abs() > f64::EPSILON {
                frame = frame.value(Axis::Vertical, amount);
            }
        }

        // 离散 v120 (鼠标滚轮) — Firefox 依赖此事件滚动
        if let Some(v120) = event.amount_v120(Axis::Horizontal) {
            if v120 != 0.0 {
                frame = frame.v120(Axis::Horizontal, v120 as i32);
            }
        }
        if let Some(v120) = event.amount_v120(Axis::Vertical) {
            if v120 != 0.0 {
                frame = frame.v120(Axis::Vertical, v120 as i32);
            }
        }

        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// 找到坐标下的 WlSurface (checks layer surfaces first, then windows)
    fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        // Check top/overlay layer surfaces first (they render above windows)
        if let Some(output) = self.space.outputs().next().cloned() {
            let layer_map = layer_map_for_output(&output);
            for layer in [Layer::Overlay, Layer::Top] {
                for layer_surface in layer_map.layers_on(layer) {
                    if let Some(geo) = layer_map.layer_geometry(layer_surface) {
                        let local = pos - geo.loc.to_f64();
                        if let Some((surface, surface_loc)) =
                            layer_surface.surface_under(local, WindowSurfaceType::ALL)
                        {
                            return Some((surface, (geo.loc + surface_loc).to_f64()));
                        }
                    }
                }
            }
        }

        // Then check windows
        if let Some(result) = self.space
            .element_under(pos)
            .and_then(|(window, window_loc)| {
                let local = pos - window_loc.to_f64();
                window
                    .surface_under(local, WindowSurfaceType::ALL)
                    .map(|(surface, surface_loc)| {
                        (surface, (window_loc + surface_loc).to_f64())
                    })
            })
        {
            return Some(result);
        }

        // Finally check bottom/background layer surfaces
        if let Some(output) = self.space.outputs().next().cloned() {
            let layer_map = layer_map_for_output(&output);
            for layer in [Layer::Bottom, Layer::Background] {
                for layer_surface in layer_map.layers_on(layer) {
                    if let Some(geo) = layer_map.layer_geometry(layer_surface) {
                        let local = pos - geo.loc.to_f64();
                        if let Some((surface, surface_loc)) =
                            layer_surface.surface_under(local, WindowSurfaceType::ALL)
                        {
                            return Some((surface, (geo.loc + surface_loc).to_f64()));
                        }
                    }
                }
            }
        }

        None
    }
}

// ========== Configurable keybind matching ==========

fn check_configurable_keybind(
    modifiers: &ModifiersState,
    keysym: &KeysymHandle,
    parsed_binds: &[(ParsedKeyBind, String, Option<String>)],
) -> Option<Action> {
    // 用未修饰的 raw_syms 匹配: Shift+1 的 raw sym 仍是 '1' (0x31),
    // 能正确命中数字绑定. 旧实现用 modified_sym() 导致 Shift+1 变成 '!' (0x21),
    // Super+Shift+数字 (movetoworkspace) 全部失效.
    let syms = keysym.raw_syms();

    for (bind, action, arg) in parsed_binds {
        // Check modifiers match
        if bind.logo != modifiers.logo {
            continue;
        }
        if bind.shift != modifiers.shift {
            continue;
        }
        if bind.ctrl != modifiers.ctrl {
            continue;
        }
        if bind.alt != modifiers.alt {
            continue;
        }

        // Check keysym match (raw, 未修饰 — 字母无需大小写回退, 数字也能匹配)
        let bind_sym = bind.keysym;
        if !syms.iter().any(|s| s.raw() == bind_sym) {
            continue;
        }

        // Match! Convert action string to Action enum
        return match action.as_str() {
            "exec" => {
                let cmd = arg.as_deref().unwrap_or("").to_string();
                Some(Action::Exec(cmd))
            }
            "close" => Some(Action::CloseWindow),
            "quit" => Some(Action::Quit),
            "lock" => Some(Action::Lock),
            "fullscreen" => Some(Action::ToggleFullscreen),
            "floating" => Some(Action::ToggleFloating),
            "resize" => {
                let delta: f64 = arg.as_deref().unwrap_or("0.05").parse().unwrap_or(0.05);
                Some(Action::ResizeRatio(delta))
            }
            "focus" => {
                let dir = arg.as_deref().unwrap_or("next");
                match dir {
                    "next" => Some(Action::FocusNext),
                    "prev" => Some(Action::FocusPrev),
                    _ => Some(Action::FocusNext),
                }
            }
            "swap" => {
                let dir = arg.as_deref().unwrap_or("next");
                match dir {
                    "next" => Some(Action::SwapNext),
                    "prev" => Some(Action::SwapPrev),
                    _ => Some(Action::SwapNext),
                }
            }
            "workspace" => {
                let n: usize = arg.as_deref().unwrap_or("1").parse().unwrap_or(1);
                Some(Action::Workspace(n.saturating_sub(1))) // Convert 1-based to 0-based
            }
            "movetoworkspace" => {
                let n: usize = arg.as_deref().unwrap_or("1").parse().unwrap_or(1);
                Some(Action::MoveToWorkspace(n.saturating_sub(1))) // Convert 1-based to 0-based
            }
            _ => None,
        };
    }

    None
}
