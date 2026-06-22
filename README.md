# 灵犀 (lingxi)

> 心有灵犀一点通 ✨

A Rust Wayland compositor with silky animations and dynamic tiling.
Inspired by Hyprland's visual experience, built on Smithay for safety and performance.

## 目标

- 🎬 Hyprland 级丝滑动画 (贝塞尔曲线 + 弹簧物理)
- 🪟 动态平铺 (dwindle + master-stack)
- 🎨 视觉特效 (模糊/圆角/阴影)
- 🦀 纯 Rust，内存安全
- ⚡ 基于 Smithay，协议完备

## 架构

```
src/
├── main.rs           # 入口，事件循环
├── compositor/       # 核心状态机，窗口管理
├── layout/           # 平铺布局算法 (dwindle, master-stack)
├── animation/        # 动画引擎 (贝塞尔, 弹簧)
├── renderer/         # 渲染特效 (模糊, 圆角, 阴影)
├── input/            # 输入处理，快捷键
└── config/           # TOML 配置
```

## 开发路线

- [ ] M1: Smithay winit 后端 + 能显示窗口
- [ ] M2: 基本平铺布局
- [ ] M3: 动画引擎接入渲染
- [ ] M4: 模糊/圆角/阴影特效
- [ ] M5: DRM 后端 (真实 TTY 运行)
- [ ] M6: 配置热重载 + IPC
- [ ] M7: XWayland 支持
- [ ] M8: 多显示器

## 开发

```bash
# 在现有 Wayland 会话中以窗口模式运行 (开发用)
RUST_LOG=lingxi=debug cargo run

# 在远程 Arch 机器测试 (TTY 模式)
cargo build --release
scp target/release/lingxi jzy@192.168.66.66:~/
# 在 TTY 中: ./lingxi
```

## 配置

`~/.config/lingxi/lingxi.toml`:

```toml
[general]
border_size = 2
gaps_inner = 5
gaps_outer = 10

[animations]
enabled = true
window_open_ms = 200
workspace_switch_ms = 300

[layout]
default_layout = "dwindle"
split_ratio = 0.5

[decoration]
rounding = 10
blur_enabled = true
blur_size = 8
shadow_enabled = true
```

## License

MIT
