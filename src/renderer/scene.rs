//! 共享渲染场景构建 — DRM 与 winit 后端共用
//!
//! 统一负责: 光标、layer shell、焦点发光边框、窗口圆角、投影。
//! 两个后端都调用 build_scene() 得到 Vec<LingxiRenderElement>，再交给
//! OutputDamageTracker::render_output。

use std::time::Duration;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                solid::SolidColorRenderElement,
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                AsRenderElements, Id, Kind,
            },
            gles::{
                element::PixelShaderElement, GlesRenderer, Uniform,
            },
        },
    },
    desktop::layer_map_for_output,
    output::Output,
    utils::{Buffer, Logical, Physical, Point as SmithayPoint, Rectangle, Scale, Size, Transform},
    wayland::shell::wlr_layer::Layer,
};

use crate::compositor::LingxiState;
use crate::renderer::elements::{LingxiRenderElement, RoundedWindowElement};
use crate::renderer::shaders::LingxiShaders;

/// 光标数据 (含 hotspot 偏移)
pub struct CursorData {
    pub buffer: MemoryRenderBuffer,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
}

/// 加载光标 (优先 xcursor 主题，失败回退到手绘箭头)
pub fn create_cursor_data(cursor_size: u32) -> CursorData {
    if let Some(data) = load_xcursor_theme(cursor_size) {
        return data;
    }
    create_fallback_cursor()
}

/// 从系统 xcursor 主题加载默认指针
fn load_xcursor_theme(cursor_size: u32) -> Option<CursorData> {
    use xcursor::{parser, CursorTheme};

    let theme_name = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "Adwaita".into());
    let theme = CursorTheme::load(&theme_name);
    let path = theme
        .load_icon("left_ptr")
        .or_else(|| theme.load_icon("default"))?;
    let content = std::fs::read(&path).ok()?;
    let images = parser::parse_xcursor(&content)?;

    // 选最接近请求尺寸的图 (优先 >= 请求值)
    let target = cursor_size as i32;
    let image = images.iter().min_by_key(|img| {
        let diff = img.size as i32 - target;
        if diff >= 0 { diff } else { -diff + 1000 }
    })?;

    let w = image.width as i32;
    let h = image.height as i32;

    let mut buffer = MemoryRenderBuffer::new(Fourcc::Argb8888, (w, h), 1, Transform::Normal, None);
    {
        let mut render_ctx = buffer.render();
        render_ctx
            .draw(|data| {
                // Fourcc::Argb8888 → GL BGRA_EXT，内存字节序需 [B,G,R,A]。
                // xcursor pixels_rgba 是 [R,G,B,A]，交换 R/B。
                let src = &image.pixels_rgba;
                let copy_len = data.len().min(src.len());
                for i in (0..copy_len).step_by(4) {
                    data[i] = src[i + 2];
                    data[i + 1] = src[i + 1];
                    data[i + 2] = src[i];
                    data[i + 3] = src[i + 3];
                }
                Ok::<Vec<Rectangle<i32, Buffer>>, ()>(vec![Rectangle::from_size((w, h).into())])
            })
            .unwrap();
    }

    Some(CursorData {
        buffer,
        hotspot_x: image.xhot as i32,
        hotspot_y: image.yhot as i32,
    })
}

/// 回退光标 (主题不可用时的手绘白箭头)
fn create_fallback_cursor() -> CursorData {
    const W: i32 = 24;
    const H: i32 = 24;

    let mut buffer = MemoryRenderBuffer::new(Fourcc::Argb8888, (W, H), 1, Transform::Normal, None);
    {
        let mut render_ctx = buffer.render();
        render_ctx
            .draw(|data| {
                for pixel in data.chunks_exact_mut(4) {
                    pixel[0] = 0;
                    pixel[1] = 0;
                    pixel[2] = 0;
                    pixel[3] = 0;
                }
                let arrow: &[(i32, &[(i32, i32)])] = &[
                    (0, &[(0, 0)]), (1, &[(0, 1)]), (2, &[(0, 2)]), (3, &[(0, 3)]),
                    (4, &[(0, 4)]), (5, &[(0, 5)]), (6, &[(0, 6)]), (7, &[(0, 7)]),
                    (8, &[(0, 8)]), (9, &[(0, 9)]), (10, &[(0, 10)]), (11, &[(0, 11)]),
                    (12, &[(0, 5)]), (13, &[(0, 4), (5, 6)]), (14, &[(0, 3), (6, 7)]),
                    (15, &[(0, 2), (7, 8)]), (16, &[(0, 1), (8, 9)]), (17, &[(0, 0), (9, 10)]),
                ];
                for &(y, spans) in arrow {
                    for &(xs, xe) in spans {
                        for x in xs..=xe {
                            let idx = ((y * W + x) * 4) as usize;
                            if idx + 3 < data.len() {
                                data[idx] = 255;
                                data[idx + 1] = 255;
                                data[idx + 2] = 255;
                                data[idx + 3] = 255;
                            }
                        }
                    }
                }
                Ok::<Vec<Rectangle<i32, Buffer>>, ()>(vec![Rectangle::from_size((W, H).into())])
            })
            .unwrap();
    }

    CursorData { buffer, hotspot_x: 0, hotspot_y: 0 }
}

/// 构建完整渲染场景 (绘制顺序: 越靠前越在上层)
///
/// 顺序: 锁屏 surface (最顶) → 光标 → 上层 layer → 焦点边框 → 窗口(圆角) → 投影 → 下层 layer
pub fn build_scene(
    renderer: &mut GlesRenderer,
    shaders: &Option<LingxiShaders>,
    state: &LingxiState,
    output: &Output,
    cursor: Option<&CursorData>,
) -> Vec<LingxiRenderElement> {
    let mut elements: Vec<LingxiRenderElement> = Vec::new();

    // --- 锁屏 UI (lingxi 自绘: 深色背景 + 密码圆点框) ---
    if state.locked {
        let output_geo = state
            .space
            .outputs()
            .next()
            .and_then(|o| state.space.output_geometry(o))
            .map(|geo| (geo.size.w, geo.size.h))
            .unwrap_or((3840, 2160));
        let (ow, oh) = output_geo;

        // 渲染顺序: elements[0] = 最顶层, elements[last] = 最底层
        // 所以先 push UI (顶层), 最后 push 背景 (底层)

        // 1. 密码输入框背景 (居中, 500x80, 亮灰 #585b70 — 明显可见)
        let box_w = 500;
        let box_h = 80;
        let box_x = (ow - box_w) / 2;
        let box_y = (oh - box_h) / 2;
        elements.push(LingxiRenderElement::Solid(SolidColorRenderElement::new(
            Id::new(),
            Rectangle::new(SmithayPoint::from((box_x, box_y)), Size::from((box_w, box_h))),
            0usize,
            [0.345, 0.357, 0.439, 1.0],  // #585b70 Catppuccin overlay0
            Kind::Unspecified,
        )));

        // 3. 固定提示条 (密码框上方 — 让用户知道是锁屏)
        // 蓝色横条 200x6, 居中, 在密码框上方 20px
        let hint_w = 200;
        let hint_h = 6;
        let hint_x = (ow - hint_w) / 2;
        let hint_y = box_y - 30;
        elements.push(LingxiRenderElement::Solid(SolidColorRenderElement::new(
            Id::new(),
            Rectangle::new(SmithayPoint::from((hint_x, hint_y)), Size::from((hint_w, hint_h))),
            0usize,
            [0.537, 0.706, 0.980, 1.0],  // #89b4fa Catppuccin blue
            Kind::Unspecified,
        )));

        // 4. 密码圆点 (每输入 1 个字符画 1 个白色圆点)
        let dot_size = 14;
        let dot_gap = 10;
        let pw_len = state.password_input.len().min(20);
        let dots_total_w = pw_len as i32 * (dot_size + dot_gap);
        let dots_start_x = (ow - dots_total_w) / 2;
        let dots_y = box_y + (box_h - dot_size) / 2;
        for i in 0..pw_len {
            let dx = dots_start_x + i as i32 * (dot_size + dot_gap);
            elements.push(LingxiRenderElement::Solid(SolidColorRenderElement::new(
                Id::new(),
                Rectangle::new(SmithayPoint::from((dx, dots_y)), Size::from((dot_size, dot_size))),
                0usize,
                [1.0, 1.0, 1.0, 1.0],  // 纯白
                Kind::Unspecified,
            )));
        }

        // 5. 密码错误提示 (红色粗条)
        if state.password_error.is_some() {
            let err_w = 400;
            let err_h = 6;
            let err_x = (ow - err_w) / 2;
            let err_y = box_y + box_h + 15;
            elements.push(LingxiRenderElement::Solid(SolidColorRenderElement::new(
                Id::new(),
                Rectangle::new(SmithayPoint::from((err_x, err_y)), Size::from((err_w, err_h))),
                0usize,
                [0.953, 0.545, 0.659, 1.0],  // #f38ba8 red
                Kind::Unspecified,
            )));
        }

        // 6. 全屏背景 (最底层 — 最后 push)
        elements.push(LingxiRenderElement::Solid(SolidColorRenderElement::new(
            Id::new(),
            Rectangle::new(SmithayPoint::from((0, 0)), Size::from((ow, oh))),
            0usize,
            [0.118, 0.118, 0.180, 1.0],  // #1e1e2e Catppuccin base
            Kind::Unspecified,
        )));

        // 锁屏时不画其他 (光标/窗口/layer)
        return elements;
    }

    // --- 光标 (最上层) ---
    if let Some(cursor) = cursor {
        let cursor_pos = state.pointer_location;
        let cursor_render_pos = (
            cursor_pos.x - cursor.hotspot_x as f64,
            cursor_pos.y - cursor.hotspot_y as f64,
        );
        match MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            cursor_render_pos,
            &cursor.buffer,
            None,
            None,
            None,
            Kind::Cursor,
        ) {
            Ok(cursor_elem) => elements.push(LingxiRenderElement::Memory(cursor_elem)),
            Err(e) => tracing::warn!("Cursor render element 创建失败: {:?}", e),
        }
    }

    // --- IME popup (fcitx5 候选词窗口, 位于光标下方) ---
    // popup.location() 已经包含 parent surface 的屏幕位置 + 光标矩形偏移，
    // 不需要再手动加 win_loc（否则会 double-count）。
    if let Some(ref popup) = state.ime_popup {
        if popup.alive() {
            let pos = popup.location();
            let cursor_height = popup.text_input_rectangle().size.h;
            let render_loc: SmithayPoint<i32, Physical> = (
                pos.x,
                pos.y + cursor_height + 4,  // 候选词框放在光标下方
            ).into();
            let popup_elements = render_elements_from_surface_tree(
                renderer,
                popup.wl_surface(),
                render_loc,
                Scale::from(1.0f64),
                1.0,
                Kind::Unspecified,
            );
            elements.extend(popup_elements.into_iter().map(LingxiRenderElement::Surface));
        }
    }

    // --- 上层 layer (Overlay/Top) ---
    push_layer_surfaces(renderer, output, &mut elements, true);

    // --- 焦点发光边框 ---
    push_focus_border(state, &mut elements);

    // --- 窗口 (圆角) + 投影 ---
    let rounding = state.config.decoration.rounding as f32;
    let shadow_enabled = state.config.decoration.shadow_enabled;
    let shadow_range = state.config.decoration.shadow_range as i32;
    let shadow_color = state.config.decoration.shadow_color;
    let shadow_off = (
        state.config.decoration.shadow_offset_x,
        state.config.decoration.shadow_offset_y,
    );

    // 渲染顺序 (Painter's algorithm, 后画的盖先画的):
    //   1. 平铺 (底层, 最早画)
    //   2. 浮动 (中层, 按 z_order 正序)
    //   3. 全屏 (最上层, 最后画) — 覆盖所有 layer / 装饰
    let mut ordered: Vec<&smithay::desktop::Window> = Vec::new();

    for w in state.space.elements() {
        if !state.is_floating(w) && state.fullscreen_window.as_ref() != Some(w) {
            ordered.push(w);
        }
    }

    for w in state.floating_z_ordered() {
        if state.fullscreen_window.as_ref() != Some(w) {
            ordered.push(w);
        }
    }

    // 全屏最后画 — 覆盖 waybar / 装饰
    if let Some(fs) = state.fullscreen_window.as_ref() {
        ordered.push(fs);
    }

    for (draw_idx, window) in ordered.into_iter().enumerate() {
        let loc = state.space.element_location(window).unwrap_or_default();
        let geo = window.geometry();
        // loc 是可见内容位置; buffer 需画在 render_loc = loc - geo.loc
        // 才能让可见内容落在 loc, 与 Smithay 的命中检测 (element_under/bbox) 一致
        let render_loc: SmithayPoint<i32, Logical> = (loc.x - geo.loc.x, loc.y - geo.loc.y).into();
        let win_x = loc.x;
        let win_y = loc.y;
        // 全屏窗口: 优先用 compositor 自己的 target size (animations.target),
        // 避免渲染时还用 client 旧 geometry() 导致看起来没全屏
        let is_fs = state.fullscreen_window.as_ref() == Some(window);
        let (win_w, win_h) = if is_fs {
            if let Some(target) = state.animations.get_target(window) {
                (target.width as i32, target.height as i32)
            } else {
                (geo.size.w, geo.size.h)
            }
        } else {
            (geo.size.w, geo.size.h)
        };
        tracing::trace!(
            "[scene] draw_idx={} pos=({},{}) size={}x{} fullscreen={}",
            draw_idx, win_x, win_y, win_w, win_h, is_fs
        );

        let window_elements = window.render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
            renderer,
            render_loc.to_physical_precise_round(Scale::from(1.0f64)),
            Scale::from(1.0f64),
            1.0,
        );

        // 注意: `window.render_elements()` 返回的元素列表里,popup 元素在前面、toplevel 在最后
        //   (见 smithay desktop/space/wayland/window.rs:106-130)
        // 投影应画在 toplevel 元素之后、popup 之前(让 popup 显示在投影之上,投影属于父窗口)。
        // 圆角裁剪只对 toplevel 应用,popup 通常是直角矩形(菜单/下拉)不应被裁掉。
        let popup_count = window_elements.len().saturating_sub(1);
        let mut window_elements = window_elements.into_iter();

        // 1) popup 元素 → 不应用圆角
        for _ in 0..popup_count {
            if let Some(e) = window_elements.next() {
                elements.push(LingxiRenderElement::Surface(e));
            }
        }

        // 2) toplevel 元素 → 应用圆角裁剪
        if let Some(toplevel_elem) = window_elements.next() {
            match shaders.as_ref() {
                Some(sh) if rounding > 0.0 => {
                    elements.push(LingxiRenderElement::Rounded(RoundedWindowElement::new(
                        toplevel_elem,
                        sh.rounded.clone(),
                        rounding,
                    )));
                }
                _ => {
                    elements.push(LingxiRenderElement::Surface(toplevel_elem));
                }
            }
        }

        // 投影紧跟在该窗口之后 → 正好位于这个窗口下方
        if shadow_enabled && win_w > 0 && win_h > 0 {
            if let Some(sh) = shaders.as_ref() {
                let area = Rectangle::new(
                    SmithayPoint::<i32, Logical>::from((win_x - shadow_range, win_y - shadow_range)),
                    Size::from((win_w + 2 * shadow_range, win_h + 2 * shadow_range)),
                );
                let shadow = PixelShaderElement::new(
                    sh.shadow.clone(),
                    area,
                    None,
                    1.0,
                    vec![
                        Uniform::new("shadow_size", (win_w as f32, win_h as f32)),
                        Uniform::new(
                            "shadow_offset",
                            (shadow_range as f32 + shadow_off.0, shadow_range as f32 + shadow_off.1),
                        ),
                        Uniform::new("radius", rounding),
                        Uniform::new("blur", shadow_range as f32),
                        Uniform::new("shadow_color", shadow_color),
                    ],
                    Kind::Unspecified,
                );
                elements.push(LingxiRenderElement::Shadow(shadow));
            }
        }
    }

    // --- 下层 layer (Bottom/Background) ---
    push_layer_surfaces(renderer, output, &mut elements, false);

    elements
}

/// 焦点窗口的青色发光边框 (3 层不同 alpha)
fn push_focus_border(state: &LingxiState, elements: &mut Vec<LingxiRenderElement>) {
    let focused = match state.focused_window() {
        Some(w) => w,
        None => return,
    };
    let loc = match state.space.element_location(&focused) {
        Some(l) => l,
        None => return,
    };
    let geo = focused.geometry();
    let win_x = loc.x;
    let win_y = loc.y;
    let win_w = geo.size.w;
    let win_h = geo.size.h;

    let active_color = state.config.general.active_border_color;
    let glow_layers: [(i32, f32); 3] = [(2, 0.2), (1, 0.5), (0, 1.0)];

    for (offset, alpha) in &glow_layers {
        let outer_offset = *offset + 1;
        let color = [
            active_color[0],
            active_color[1],
            active_color[2],
            active_color[3] * alpha,
        ];
        let x = win_x - outer_offset;
        let y = win_y - outer_offset;
        let w = win_w + 2 * outer_offset;
        let h = win_h + 2 * outer_offset;
        let thickness = 1;

        elements.push(LingxiRenderElement::Solid(SolidColorRenderElement::new(
            Id::new(),
            Rectangle::new(SmithayPoint::from((x, y)), Size::from((w, thickness))),
            0usize,
            color,
            Kind::Unspecified,
        )));
        elements.push(LingxiRenderElement::Solid(SolidColorRenderElement::new(
            Id::new(),
            Rectangle::new(SmithayPoint::from((x, y + h - thickness)), Size::from((w, thickness))),
            0usize,
            color,
            Kind::Unspecified,
        )));
        elements.push(LingxiRenderElement::Solid(SolidColorRenderElement::new(
            Id::new(),
            Rectangle::new(SmithayPoint::from((x, y + thickness)), Size::from((thickness, h - 2 * thickness))),
            0usize,
            color,
            Kind::Unspecified,
        )));
        elements.push(LingxiRenderElement::Solid(SolidColorRenderElement::new(
            Id::new(),
            Rectangle::new(SmithayPoint::from((x + w - thickness, y + thickness)), Size::from((thickness, h - 2 * thickness))),
            0usize,
            color,
            Kind::Unspecified,
        )));
    }
}

/// 收集 layer shell surface (top_layers=true → Overlay/Top；否则 Bottom/Background)
fn push_layer_surfaces(
    renderer: &mut GlesRenderer,
    output: &Output,
    elements: &mut Vec<LingxiRenderElement>,
    top_layers: bool,
) {
    let layer_map = layer_map_for_output(output);
    let layers_to_render: Vec<Layer> = if top_layers {
        vec![Layer::Overlay, Layer::Top]
    } else {
        vec![Layer::Bottom, Layer::Background]
    };

    for layer in layers_to_render {
        for layer_surface in layer_map.layers_on(layer) {
            let loc: SmithayPoint<i32, Physical> = match layer_map.layer_geometry(layer_surface) {
                Some(geo) => geo.loc.to_physical_precise_round(Scale::from(1.0f64)),
                None => (0, 0).into(),
            };
            let surface_elements = layer_surface
                .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                    renderer,
                    loc,
                    Scale::from(1.0f64),
                    1.0,
                );
            elements.extend(surface_elements.into_iter().map(LingxiRenderElement::Surface));
        }
    }
}

/// 发送 frame callback 给所有窗口和 layer surface
pub fn send_frames(state: &LingxiState, output: &Output) {
    let time = state.start_time.elapsed();
    for window in state.space.elements() {
        window.send_frame(output, time, Some(Duration::from_millis(16)), |_, _| None);
    }
    let layer_map = layer_map_for_output(output);
    for layer_surface in layer_map.layers() {
        layer_surface.send_frame(output, time, Some(Duration::from_millis(16)), |_, _| None);
    }
}
