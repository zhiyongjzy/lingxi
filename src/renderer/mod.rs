//! 灵犀渲染器 — 特效管线
//!
//! 在 Smithay 的 GlesRenderer 基础上添加:
//! - 模糊 (Kawase blur)
//! - 圆角
//! - 阴影
//! - 动画变换

pub mod elements;
pub mod scene;
pub mod shaders;

/// 渲染特效参数
pub struct RenderEffects {
    /// 圆角半径 (px)
    pub corner_radius: f32,
    /// 模糊强度
    pub blur_strength: f32,
    /// 阴影偏移和大小
    pub shadow: Option<ShadowParams>,
    /// 透明度
    pub opacity: f32,
}

pub struct ShadowParams {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub color: [f32; 4],
}

impl Default for RenderEffects {
    fn default() -> Self {
        Self {
            corner_radius: 10.0,
            blur_strength: 0.0,
            shadow: None,
            opacity: 1.0,
        }
    }
}

// TODO: 实现自定义 shader 渲染管线
// - Kawase blur (多 pass 降采样/升采样)
// - SDF 圆角裁剪
// - 高斯阴影
