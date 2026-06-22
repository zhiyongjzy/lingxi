//! 灵犀 (lingxi) — Rust Wayland 合成器
//!
//! 目标: Hyprland 级别的动画体验 + Rust 的安全性和性能
//!
//! 架构:
//!   Smithay (协议/DRM/输入) → lingxi 核心 (布局/动画/特效) → 屏幕
//!
//! 用法:
//!   lingxi              — DRM 模式 (直接 TTY)
//!   lingxi --winit      — Winit 模式 (嵌套开发)

pub mod animation;
pub mod auth;
pub mod backend;
pub mod compositor;
pub mod config;
pub mod input;
pub mod layout;
pub mod renderer;

use tracing::info;

fn main() {
    eprintln!("[lingxi] 进程启动 pid={}", std::process::id());

    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lingxi=info,smithay=warn".parse().unwrap()),
        )
        .init();

    info!("灵犀 compositor v{}", env!("CARGO_PKG_VERSION"));
    info!("心有灵犀一点通 ✨");

    // 加载配置
    let config = config::LingxiConfig::load();
    info!(
        "配置加载完成: 动画={}, 模糊={}",
        config.animations.enabled, config.decoration.blur_enabled
    );

    // 判断后端模式
    let args: Vec<String> = std::env::args().collect();
    let use_winit = args.iter().any(|a| a == "--winit");
    let force_drm = args.iter().any(|a| a == "--drm");

    if force_drm {
        info!("🖥️ 强制 DRM 后端 (--drm)");
        backend::drm_backend::run(config);
    } else if use_winit || std::env::var("WAYLAND_DISPLAY").is_ok() || std::env::var("DISPLAY").is_ok() {
        info!("📺 选择 Winit 后端 (嵌套模式)");
        backend::winit_backend::run(config);
    } else {
        info!("🖥️ 选择 DRM 后端 (直接 TTY 模式)");
        backend::drm_backend::run(config);
    }
}
