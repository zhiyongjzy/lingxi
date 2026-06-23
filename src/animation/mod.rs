//! 灵犀动画引擎 — 贝塞尔曲线 + 弹簧物理
//!
//! 这是 lingxi 的核心差异化模块。

use std::time::{Duration, Instant};

/// 动画状态
#[derive(Debug, Clone)]
pub struct Animation {
    pub start: f64,
    pub end: f64,
    pub started_at: Instant,
    pub duration: Duration,
    pub curve: AnimationCurve,
}

/// 动画曲线类型
#[derive(Debug, Clone)]
pub enum AnimationCurve {
    /// 三阶贝塞尔 (Hyprland 风格)
    CubicBezier { x1: f64, y1: f64, x2: f64, y2: f64 },
    /// 弹簧物理 (更自然的回弹)
    Spring { stiffness: f64, damping: f64, mass: f64 },
    /// 线性
    Linear,
}

impl Animation {
    pub fn new(start: f64, end: f64, duration: Duration, curve: AnimationCurve) -> Self {
        Self {
            start,
            end,
            started_at: Instant::now(),
            duration,
            curve,
        }
    }

    /// 获取当前动画值 (0.0 ~ 1.0 进度)
    pub fn value(&self) -> f64 {
        let elapsed = self.started_at.elapsed();
        if elapsed >= self.duration {
            return self.end;
        }

        let t = elapsed.as_secs_f64() / self.duration.as_secs_f64();
        let progress = match &self.curve {
            AnimationCurve::Linear => t,
            AnimationCurve::CubicBezier { x1, y1, x2, y2 } => {
                cubic_bezier_sample(t, *x1, *y1, *x2, *y2)
            }
            AnimationCurve::Spring { stiffness, damping, mass } => {
                spring_sample(t, *stiffness, *damping, *mass)
            }
        };

        self.start + (self.end - self.start) * progress
    }

    /// 是否结束 — 用 duration 作硬上限。
    /// Spring 会渐近到 end 但永不"完全到",所以这里靠 duration 强制结束。
    /// duration 越长 → 视觉上 spring 回弹越久 (类似 Hyprland 的 "bouncy" 风格)
    pub fn is_finished(&self) -> bool {
        self.started_at.elapsed() >= self.duration
    }
}

/// 三阶贝塞尔曲线采样 (CSS cubic-bezier 兼容)
fn cubic_bezier_sample(t: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    // 用牛顿法求解 x(t) = target_x 对应的 t 参数，再求 y(t)
    let mut guess = t;
    for _ in 0..8 {
        let x = bezier_component(guess, x1, x2) - t;
        let dx = bezier_derivative(guess, x1, x2);
        if dx.abs() < 1e-7 {
            break;
        }
        guess -= x / dx;
    }
    bezier_component(guess, y1, y2)
}

fn bezier_component(t: f64, p1: f64, p2: f64) -> f64 {
    let t2 = t * t;
    let t3 = t2 * t;
    3.0 * (1.0 - t) * (1.0 - t) * t * p1 + 3.0 * (1.0 - t) * t2 * p2 + t3
}

fn bezier_derivative(t: f64, p1: f64, p2: f64) -> f64 {
    let t2 = t * t;
    3.0 * (1.0 - t) * (1.0 - t) * p1 + 6.0 * (1.0 - t) * t * (p2 - p1) + 3.0 * t2 * (1.0 - p2)
}

/// 弹簧物理采样 (阻尼谐振)
fn spring_sample(t: f64, stiffness: f64, damping: f64, mass: f64) -> f64 {
    let omega = (stiffness / mass).sqrt();
    let zeta = damping / (2.0 * (stiffness * mass).sqrt());

    if zeta < 1.0 {
        // 欠阻尼 (有回弹)
        let omega_d = omega * (1.0 - zeta * zeta).sqrt();
        1.0 - (-zeta * omega * t).exp() * ((zeta * omega * t / omega_d).sin() + (omega_d * t).cos())
    } else if zeta == 1.0 {
        // 临界阻尼 (重根) — 专用公式, 避免过阻尼分支除以 (r1-r2)=0 产生 NaN
        1.0 - (1.0 + omega * t) * (-omega * t).exp()
    } else {
        // 过阻尼 (无回弹，平滑到达)
        let r1 = -omega * (zeta + (zeta * zeta - 1.0).sqrt());
        let r2 = -omega * (zeta - (zeta * zeta - 1.0).sqrt());
        1.0 - (r1 * (r2 * t).exp() - r2 * (r1 * t).exp()) / (r1 - r2)
    }
}

/// 预定义曲线 (模仿 Hyprland 风格)
pub mod presets {
    use super::AnimationCurve;

    /// 默认窗口打开/关闭动画 — spring 物理,丝滑回弹
    /// 相当于 ~200ms 到达 95% 然后轻微回弹 (Hyprland 风格)
    pub fn default_window() -> AnimationCurve {
        AnimationCurve::Spring {
            stiffness: 400.0,  // 刚度 — 越大越快到位
            damping: 30.0,     // 阻尼 — 越大越不振荡
            mass: 1.0,
        }
    }

    /// 工作区切换 — spring 略快一点,避免切换拖沓
    pub fn workspace_switch() -> AnimationCurve {
        AnimationCurve::Spring {
            stiffness: 250.0,
            damping: 25.0,
            mass: 1.0,
        }
    }

    /// 弹性回弹 (夸张) — 用于拖动/缩放
    pub fn bouncy() -> AnimationCurve {
        AnimationCurve::Spring {
            stiffness: 300.0,
            damping: 15.0,  // 阻尼小 → 明显回弹
            mass: 1.0,
        }
    }

    /// 平滑无回弹 — 用于 resize 等不希望过冲的场景
    pub fn smooth() -> AnimationCurve {
        AnimationCurve::Spring {
            stiffness: 500.0,
            damping: 40.0,  // 高阻尼 → 临界阻尼,无回弹
            mass: 1.0,
        }
    }
}
