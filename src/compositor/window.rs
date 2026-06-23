//! 窗口动画状态管理

use std::collections::HashMap;
use std::time::Duration;

use smithay::{
    desktop::Window,
    utils::{Logical, Point},
};

use crate::animation::{Animation, AnimationCurve, presets};

/// 窗口动画位置 (浮点精度)
#[derive(Debug, Clone, Copy)]
pub struct AnimatedRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl AnimatedRect {
    pub fn to_point(&self) -> Point<i32, Logical> {
        (self.x as i32, self.y as i32).into()
    }
}

/// 单个窗口的动画状态
pub struct WindowAnimation {
    pub x: Option<Animation>,
    pub y: Option<Animation>,
    pub width: Option<Animation>,
    pub height: Option<Animation>,
    pub current: AnimatedRect,
    pub target: AnimatedRect,
}

impl WindowAnimation {
    /// 创建新动画，从 current 过渡到 target
    pub fn animate_to(&mut self, target: AnimatedRect, curve: AnimationCurve, duration: Duration) {
        let old = self.current;
        self.target = target;

        if (old.x - target.x).abs() > 1.0 {
            self.x = Some(Animation::new(old.x, target.x, duration, curve.clone()));
        }
        if (old.y - target.y).abs() > 1.0 {
            self.y = Some(Animation::new(old.y, target.y, duration, curve.clone()));
        }
        if (old.width - target.width).abs() > 1.0 {
            self.width = Some(Animation::new(old.width, target.width, duration, curve.clone()));
        }
        if (old.height - target.height).abs() > 1.0 {
            self.height = Some(Animation::new(old.height, target.height, duration, curve));
        }
    }

    /// 推进动画一帧，返回是否仍在动画中
    pub fn tick(&mut self) -> bool {
        let mut animating = false;

        if let Some(ref anim) = self.x {
            self.current.x = anim.value();
            if anim.is_finished() {
                self.current.x = self.target.x;
                self.x = None;
            } else {
                animating = true;
            }
        }
        if let Some(ref anim) = self.y {
            self.current.y = anim.value();
            if anim.is_finished() {
                self.current.y = self.target.y;
                self.y = None;
            } else {
                animating = true;
            }
        }
        if let Some(ref anim) = self.width {
            self.current.width = anim.value();
            if anim.is_finished() {
                self.current.width = self.target.width;
                self.width = None;
            } else {
                animating = true;
            }
        }
        if let Some(ref anim) = self.height {
            self.current.height = anim.value();
            if anim.is_finished() {
                self.current.height = self.target.height;
                self.height = None;
            } else {
                animating = true;
            }
        }

        animating
    }

    pub fn is_animating(&self) -> bool {
        self.x.is_some() || self.y.is_some() || self.width.is_some() || self.height.is_some()
    }
}

/// 管理所有窗口的动画状态
pub struct AnimationManager {
    /// Window → 动画状态 (Window 实现 Hash+Eq, 直接做 key, O(1) 查找)
    animations: HashMap<Window, WindowAnimation>,
    /// 动画持续时间
    pub duration: Duration,
    /// 动画曲线
    pub curve: AnimationCurve,
}

impl AnimationManager {
    pub fn new() -> Self {
        Self {
            animations: HashMap::new(),
            // 用 spring 物理时,持续时间作为 "硬上限" (强制吸附到 target)
            // spring 数学上渐近到 end 但永不"完全到",所以给到 350ms 既能
            // 让回弹明显,又不会动画一直跑浪费 GPU
            duration: Duration::from_millis(350),
            curve: presets::default_window(),
        }
    }

    /// 注册新窗口 (带入场动画: 从中心缩放出现)
    pub fn add_window(&mut self, window: Window, target: AnimatedRect, output_center: (f64, f64)) {
        let initial = AnimatedRect {
            x: output_center.0 - target.width / 4.0,
            y: output_center.1 - target.height / 4.0,
            width: target.width * 0.5,
            height: target.height * 0.5,
        };

        let mut anim = WindowAnimation {
            x: None,
            y: None,
            width: None,
            height: None,
            current: initial,
            target,
        };
        anim.animate_to(target, self.curve.clone(), self.duration);
        self.animations.insert(window, anim);
    }

    /// 移除窗口的动画跟踪
    pub fn remove_window(&mut self, window: &Window) {
        self.animations.remove(window);
    }

    /// 更新所有窗口的目标位置 (layout 变化时调用)
    pub fn retarget(&mut self, targets: &[(Window, AnimatedRect)]) {
        for (window, target) in targets {
            if let Some(anim) = self.animations.get_mut(window) {
                if (anim.target.x - target.x).abs() > 1.0
                    || (anim.target.y - target.y).abs() > 1.0
                    || (anim.target.width - target.width).abs() > 1.0
                    || (anim.target.height - target.height).abs() > 1.0
                {
                    anim.animate_to(*target, self.curve.clone(), self.duration);
                }
            }
        }
    }

    /// 推进所有动画一帧，返回 (有动画在播放的窗口列表, 本帧是否有动画刚结束)
    ///
    /// any_finished 用于让主循环在动画结束的那帧再渲染一次, 否则末帧可能不绘制.
    pub fn tick(&mut self) -> (Vec<(Window, Point<i32, Logical>)>, bool) {
        let mut updates = Vec::new();
        let mut any_finished = false;
        for (window, anim) in &mut self.animations {
            let was_animating = anim.is_animating();
            if !was_animating {
                continue; // 跳过未在动画的窗口, 避免每帧 clone+map 全部窗口 (review #22)
            }
            anim.tick();
            if !anim.is_animating() {
                any_finished = true;
            }
            updates.push((window.clone(), anim.current.to_point()));
        }
        (updates, any_finished)
    }
    // 注: HashMap iter 顺序不定, 但 tick 对每个窗口独立推进, 顺序不影响结果.

    /// 是否有任何动画在播放
    pub fn has_active_animations(&self) -> bool {
        self.animations.values().any(|a| a.is_animating())
    }

    /// 获取窗口当前动画位置
    pub fn get_position(&self, window: &Window) -> Option<Point<i32, Logical>> {
        self.animations.get(window).map(|a| a.current.to_point())
    }

    /// 拖动期间: 同步 current 位置, 清掉未完成的 x/y 动画 (避免拖动时还播动画)
    pub fn set_current_position(&mut self, window: &Window, x: f64, y: f64) {
        if let Some(anim) = self.animations.get_mut(window) {
            anim.current.x = x;
            anim.current.y = y;
            anim.x = None;
            anim.y = None;
        }
    }
}

impl AnimationManager {
    /// 获取窗口 compositor 目标几何 (lingxi 算的 layout target, 不依赖 wayland client ack)
    ///
    /// 用于: interactive_move_start / relayout 等需膁"compositor 意图 size" 的场景,
    /// 避免读到 wayland client 还没 ack 的 stale `window.geometry()`.
    pub fn get_target(&self, window: &Window) -> Option<AnimatedRect> {
        self.animations.get(window).map(|a| a.target)
    }

    /// 焦点切换时"上推"动画 — 让当前窗口位置不变, 但重新触发一次轻量的 reflow
    /// (保留给将来 focus visual feedback 用)
    pub fn nudge_focus(&mut self, _window: &Window) {
        // 当前实现: 不动 position, 避免破坏 dwindle 布局
        // 未来可以加 1.02x 缩放回弹
    }
}

