//! 圆角窗口元素 — 包裹 WaylandSurfaceRenderElement，在 draw 时启用圆角 texture shader
//!
//! 通过 GlesFrame::override_default_tex_program 在绘制本元素期间临时替换默认
//! 纹理着色器，绘制完成后立即清除，从而只对该窗口生效，且不破坏 render_output
//! 的 damage 跟踪管线。

use smithay::{
    backend::renderer::{
        element::{
            surface::WaylandSurfaceRenderElement, Element, Id, Kind, RenderElement,
            UnderlyingStorage,
        },
        gles::{element::PixelShaderElement, GlesRenderer, GlesTexProgram, Uniform},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Transform},
};

/// 圆角窗口元素：包裹一个表面元素 + 圆角半径 (逻辑像素)
pub struct RoundedWindowElement {
    inner: WaylandSurfaceRenderElement<GlesRenderer>,
    program: GlesTexProgram,
    radius: f32,
}

impl RoundedWindowElement {
    pub fn new(
        inner: WaylandSurfaceRenderElement<GlesRenderer>,
        program: GlesTexProgram,
        radius: f32,
    ) -> Self {
        Self {
            inner,
            program,
            radius,
        }
    }
}

impl Element for RoundedWindowElement {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        self.inner.src()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // 圆角后四角变透明，不能声明为不透明，否则角落会被当作不透明导致黑边
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for RoundedWindowElement {
    fn draw(
        &self,
        frame: &mut <GlesRenderer as smithay::backend::renderer::RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <GlesRenderer as smithay::backend::renderer::RendererSuper>::Error> {
        // 以元素的物理尺寸作为 shader 的窗口尺寸 (v_coords 在纹理上归一化 0..1)
        let win_size = (dst.size.w as f32, dst.size.h as f32);
        frame.override_default_tex_program(
            self.program.clone(),
            vec![
                Uniform::new("win_size", win_size),
                Uniform::new("radius", self.radius),
            ],
        );
        let res = self.inner.draw(frame, src, dst, damage, opaque_regions);
        frame.clear_tex_program_override();
        res
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        // 圆角需要 shader，不能走直接 scanout plane
        None
    }
}

// 统一渲染元素枚举 (固定到 GlesRenderer，DRM 与 winit 共用)
smithay::backend::renderer::element::render_elements! {
    pub LingxiRenderElement<=GlesRenderer>;
    Surface = WaylandSurfaceRenderElement<GlesRenderer>,
    Rounded = RoundedWindowElement,
    Solid = smithay::backend::renderer::element::solid::SolidColorRenderElement,
    Memory = smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<GlesRenderer>,
    Shadow = PixelShaderElement,
}
