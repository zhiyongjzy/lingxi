//! GLSL shader 源 + 编译后的 program 持有者
//!
//! - rounded: 自定义 texture shader，对窗口表面做圆角裁剪 (SDF mask)
//! - shadow:  自定义 pixel shader，在窗口后面画 SDF 圆角投影
//! - blur_down / blur_up: Kawase 模糊的降/升采样 pixel shader (需半透明/壁纸才可见)

use smithay::backend::renderer::gles::{
    GlesPixelProgram, GlesRenderer, GlesTexProgram, UniformName, UniformType,
};

/// 圆角 texture shader。必须包含 `//_DEFINES_` 行。
/// 额外 uniform: win_size (vec2, 像素), radius (float, 像素)
pub const ROUNDED_SRC: &str = r#"#version 100
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

uniform vec2 win_size;
uniform float radius;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    // 圆角裁剪: 计算到圆角矩形边界的有符号距离, 在边缘做 1px 抗锯齿
    vec2 px = v_coords * win_size;
    vec2 center = win_size * 0.5;
    vec2 q = abs(px - center) - (center - vec2(radius));
    float dist = min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - radius;
    float aa = 1.0 - smoothstep(-1.0, 1.0, dist);
    color *= aa;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
"#;

/// 投影 pixel shader。不需要 `//_DEFINES_` (pixel shader 自动加 #version 和 DEBUG_FLAGS)
/// 内置 uniform: size (vec2 区域像素), alpha (float)
/// 额外 uniform: shadow_size (vec2), shadow_offset (vec2), radius (float),
///               blur (float), shadow_color (vec4, 直色非预乘)
pub const SHADOW_SRC: &str = r#"
precision mediump float;

uniform vec2 size;
uniform float alpha;
varying vec2 v_coords;

uniform vec2 shadow_size;
uniform vec2 shadow_offset;
uniform float radius;
uniform float blur;
uniform vec4 shadow_color;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

float rounded_box_sdf(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

void main() {
    vec2 px = v_coords * size;
    vec2 center = shadow_offset + shadow_size * 0.5;
    vec2 half_b = shadow_size * 0.5;
    float d = rounded_box_sdf(px - center, half_b, radius);

    // 距离 box 越远越透明, 在 blur 像素范围内衰减
    float a = 1.0 - smoothstep(0.0, max(blur, 1.0), d);
    float final_a = shadow_color.a * a * alpha;

    // 预乘 alpha (渲染器使用 ONE / ONE_MINUS_SRC_ALPHA 混合)
    gl_FragColor = vec4(shadow_color.rgb * final_a, final_a);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        gl_FragColor = vec4(0.0, 0.2, 0.0, 0.2) + gl_FragColor * 0.8;
#endif
}
"#;

/// Kawase 降采样 texture shader。额外 uniform: halfpixel (vec2), offset (float)
pub const BLUR_DOWN_SRC: &str = r#"#version 100
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

uniform vec2 halfpixel;
uniform float offset;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    vec2 uv = v_coords;
    vec4 sum = texture2D(tex, uv) * 4.0;
    sum += texture2D(tex, uv - halfpixel.xy * offset);
    sum += texture2D(tex, uv + halfpixel.xy * offset);
    sum += texture2D(tex, uv + vec2(halfpixel.x, -halfpixel.y) * offset);
    sum += texture2D(tex, uv - vec2(halfpixel.x, -halfpixel.y) * offset);
    gl_FragColor = sum / 8.0;
}
"#;

/// Kawase 升采样 texture shader。额外 uniform: halfpixel (vec2), offset (float)
pub const BLUR_UP_SRC: &str = r#"#version 100
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

uniform vec2 halfpixel;
uniform float offset;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    vec2 uv = v_coords;
    vec4 sum = texture2D(tex, uv + vec2(-halfpixel.x * 2.0, 0.0) * offset);
    sum += texture2D(tex, uv + vec2(-halfpixel.x, halfpixel.y) * offset) * 2.0;
    sum += texture2D(tex, uv + vec2(0.0, halfpixel.y * 2.0) * offset);
    sum += texture2D(tex, uv + vec2(halfpixel.x, halfpixel.y) * offset) * 2.0;
    sum += texture2D(tex, uv + vec2(halfpixel.x * 2.0, 0.0) * offset);
    sum += texture2D(tex, uv + vec2(halfpixel.x, -halfpixel.y) * offset) * 2.0;
    sum += texture2D(tex, uv + vec2(0.0, -halfpixel.y * 2.0) * offset);
    sum += texture2D(tex, uv + vec2(-halfpixel.x, -halfpixel.y) * offset) * 2.0;
    gl_FragColor = sum / 12.0;
}
"#;

/// 编译后的全部 program (在 renderer 创建后调用 compile 一次)
#[derive(Clone)]
pub struct LingxiShaders {
    pub rounded: GlesTexProgram,
    pub shadow: GlesPixelProgram,
    pub blur_down: GlesTexProgram,
    pub blur_up: GlesTexProgram,
}

impl LingxiShaders {
    /// 编译所有自定义 shader。失败返回 None (上层降级为无特效)。
    pub fn compile(renderer: &mut GlesRenderer) -> Option<Self> {
        let rounded = renderer
            .compile_custom_texture_shader(
                ROUNDED_SRC,
                &[
                    UniformName::new("win_size", UniformType::_2f),
                    UniformName::new("radius", UniformType::_1f),
                ],
            )
            .map_err(|e| tracing::error!("圆角 shader 编译失败: {:?}", e))
            .ok()?;

        let shadow = renderer
            .compile_custom_pixel_shader(
                SHADOW_SRC,
                &[
                    UniformName::new("shadow_size", UniformType::_2f),
                    UniformName::new("shadow_offset", UniformType::_2f),
                    UniformName::new("radius", UniformType::_1f),
                    UniformName::new("blur", UniformType::_1f),
                    UniformName::new("shadow_color", UniformType::_4f),
                ],
            )
            .map_err(|e| tracing::error!("阴影 shader 编译失败: {:?}", e))
            .ok()?;

        let blur_down = renderer
            .compile_custom_texture_shader(
                BLUR_DOWN_SRC,
                &[
                    UniformName::new("halfpixel", UniformType::_2f),
                    UniformName::new("offset", UniformType::_1f),
                ],
            )
            .map_err(|e| tracing::error!("blur_down shader 编译失败: {:?}", e))
            .ok()?;

        let blur_up = renderer
            .compile_custom_texture_shader(
                BLUR_UP_SRC,
                &[
                    UniformName::new("halfpixel", UniformType::_2f),
                    UniformName::new("offset", UniformType::_1f),
                ],
            )
            .map_err(|e| tracing::error!("blur_up shader 编译失败: {:?}", e))
            .ok()?;

        tracing::info!("✨ 自定义 shader 编译完成 (圆角/阴影/模糊)");
        Some(Self {
            rounded,
            shadow,
            blur_down,
            blur_up,
        })
    }
}
