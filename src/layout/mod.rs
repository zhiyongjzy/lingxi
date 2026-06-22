//! 灵犀布局引擎 — 动态平铺

/// 窗口在布局中的几何信息
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowGeometry {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// 布局算法 trait
pub trait LayoutEngine {
    /// 给定一组窗口和可用区域，计算每个窗口的位置
    fn arrange(&self, window_count: usize, area: WindowGeometry) -> Vec<WindowGeometry>;
}

/// Dwindle 布局 (Hyprland 默认)
/// 每次对半分割，交替水平/垂直
pub struct DwindleLayout {
    pub split_ratio: f64,
    pub inner_gap: f64,
}

impl Default for DwindleLayout {
    fn default() -> Self {
        Self {
            split_ratio: 0.5,
            inner_gap: 5.0,
        }
    }
}

impl LayoutEngine for DwindleLayout {
    fn arrange(&self, window_count: usize, area: WindowGeometry) -> Vec<WindowGeometry> {
        if window_count == 0 {
            return vec![];
        }
        if window_count == 1 {
            return vec![area];
        }

        let gap = self.inner_gap;
        let mut result = Vec::with_capacity(window_count);
        let mut remaining = area;
        let mut horizontal = true;

        for i in 0..window_count {
            if i == window_count - 1 {
                // Last window: pixel-align to fill remaining space exactly
                result.push(WindowGeometry {
                    x: remaining.x.round(),
                    y: remaining.y.round(),
                    width: remaining.width.round(),
                    height: remaining.height.round(),
                });
                break;
            }

            let (current, next) = if horizontal {
                let w = (remaining.width * self.split_ratio - gap / 2.0).round();
                (
                    WindowGeometry { x: remaining.x.round(), y: remaining.y.round(), width: w, height: remaining.height.round() },
                    WindowGeometry {
                        x: remaining.x + w + gap,
                        y: remaining.y,
                        width: remaining.width - w - gap,
                        height: remaining.height,
                    },
                )
            } else {
                let h = (remaining.height * self.split_ratio - gap / 2.0).round();
                (
                    WindowGeometry { x: remaining.x.round(), y: remaining.y.round(), width: remaining.width.round(), height: h },
                    WindowGeometry {
                        x: remaining.x,
                        y: remaining.y + h + gap,
                        width: remaining.width,
                        height: remaining.height - h - gap,
                    },
                )
            };

            result.push(current);
            remaining = next;
            horizontal = !horizontal;
        }

        result
    }
}

/// Master-Stack 布局
/// 左边一个大窗口，右边竖排堆叠
pub struct MasterStackLayout {
    pub master_ratio: f64,
}

impl Default for MasterStackLayout {
    fn default() -> Self {
        Self { master_ratio: 0.55 }
    }
}

impl LayoutEngine for MasterStackLayout {
    fn arrange(&self, window_count: usize, area: WindowGeometry) -> Vec<WindowGeometry> {
        if window_count == 0 {
            return vec![];
        }
        if window_count == 1 {
            return vec![area];
        }

        let mut result = Vec::with_capacity(window_count);

        // Master window
        let master_width = area.width * self.master_ratio;
        result.push(WindowGeometry {
            width: master_width,
            ..area
        });

        // Stack
        let stack_width = area.width - master_width;
        let stack_height = area.height / (window_count - 1) as f64;

        for i in 0..(window_count - 1) {
            result.push(WindowGeometry {
                x: area.x + master_width,
                y: area.y + stack_height * i as f64,
                width: stack_width,
                height: stack_height,
            });
        }

        result
    }
}
