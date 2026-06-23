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

// ============================================================================
// 持久化 dwindle 布局树 (架构 A)
// ============================================================================

enum LayoutNode {
    Leaf,
    Split { horizontal: bool, left: Box<LayoutNode>, right: Box<LayoutNode> },
}

pub struct LayoutTree {
    root: Option<Box<LayoutNode>>,
    split_ratio: f64,
    inner_gap: f64,
    len: usize,
}

impl LayoutTree {
    pub fn new(split_ratio: f64, inner_gap: f64) -> Self {
        Self { root: None, split_ratio, inner_gap, len: 0 }
    }
    pub fn len(&self) -> usize { self.len }
    pub fn insert(&mut self) {
        let horizontal = self.len % 2 == 1;
        self.len += 1;
        self.root = Some(match self.root.take() {
            None => Box::new(LayoutNode::Leaf),
            Some(root) => split_deepest_right(root, horizontal),
        });
    }
    pub fn remove(&mut self, idx: usize) {
        if let Some(root) = self.root.take() {
            let mut counter = 0usize;
            let mut done = false;
            self.root = remove_node(root, idx, &mut counter, &mut done);
            if done { self.len -= 1; }
        }
    }
    pub fn arrange(&self, area: crate::layout::WindowGeometry) -> Vec<crate::layout::WindowGeometry> {
        let mut out = Vec::with_capacity(self.len);
        if let Some(root) = &self.root {
            arrange_node(root, area, self.inner_gap, self.split_ratio, &mut out);
        }
        out
    }
}

fn split_deepest_right(node: Box<LayoutNode>, horizontal: bool) -> Box<LayoutNode> {
    match *node {
        LayoutNode::Leaf => Box::new(LayoutNode::Split {
            horizontal,
            left: Box::new(LayoutNode::Leaf),
            right: Box::new(LayoutNode::Leaf),
        }),
        LayoutNode::Split { horizontal: h, left, right } => Box::new(LayoutNode::Split {
            horizontal: h, left, right: split_deepest_right(right, horizontal),
        }),
    }
}

fn remove_node(node: Box<LayoutNode>, idx: usize, counter: &mut usize, done: &mut bool) -> Option<Box<LayoutNode>> {
    if *done { return Some(node); }
    match *node {
        LayoutNode::Leaf => {
            if *counter == idx { *done = true; return None; }
            *counter += 1;
            Some(node)
        }
        LayoutNode::Split { horizontal, left, right } => {
            let new_left = remove_node(left, idx, counter, done);
            if new_left.is_none() { return Some(right); }
            let new_right = remove_node(right, idx, counter, done);
            if new_right.is_none() { return new_left; }
            Some(Box::new(LayoutNode::Split { horizontal, left: new_left.unwrap(), right: new_right.unwrap() }))
        }
    }
}

fn arrange_node(node: &LayoutNode, area: crate::layout::WindowGeometry, gap: f64, split_ratio: f64, out: &mut Vec<crate::layout::WindowGeometry>) {
    match node {
        LayoutNode::Leaf => {
            out.push(crate::layout::WindowGeometry {
                x: area.x.round(), y: area.y.round(),
                width: area.width.round(), height: area.height.round(),
            });
        }
        LayoutNode::Split { horizontal, left, right } => {
            if *horizontal {
                let w = (area.width * split_ratio - gap / 2.0).round();
                let left_area = crate::layout::WindowGeometry { x: area.x.round(), y: area.y.round(), width: w, height: area.height.round() };
                let right_area = crate::layout::WindowGeometry { x: area.x + w + gap, y: area.y, width: area.width - w - gap, height: area.height };
                arrange_node(left, left_area, gap, split_ratio, out);
                arrange_node(right, right_area, gap, split_ratio, out);
            } else {
                let h = (area.height * split_ratio - gap / 2.0).round();
                let left_area = crate::layout::WindowGeometry { x: area.x.round(), y: area.y.round(), width: area.width.round(), height: h };
                let right_area = crate::layout::WindowGeometry { x: area.x, y: area.y + h + gap, width: area.width, height: area.height - h - gap };
                arrange_node(left, left_area, gap, split_ratio, out);
                arrange_node(right, right_area, gap, split_ratio, out);
            }
        }
    }
}

