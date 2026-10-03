//! Responsive insert toolbar: state, layout, painting, and pointer policy.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::icons::Icon;
use ui::theme::Theme;

use crate::app::{ButtonKind, Hit, UiButton};
use crate::chrome;
use crate::strings::tr;

/// Gap between insert pills and the overflow menu's panel padding (the `s2`
/// token; the toolbar is renderer-free here so the value is spelled out).
const GAP: f64 = 8.0;
/// The "⋯" overflow button is a square of the pill height, never narrower.
const OVERFLOW_MIN_WIDTH: f64 = 40.0;
/// Classic minimum width of the overflow menu panel.
const MENU_MIN_WIDTH: f64 = 150.0;
/// Deliberately off-scale: menu rows leave a 4 px seam so adjacent hover
/// fills never merge into one block.
const MENU_ROW_SEAM: f64 = 4.0;

/// One insert tool. `bar` / `menu` are the fluent keys of the label it
/// paints on the toolbar and in the overflow menu.
struct Tool {
    bar: &'static str,
    menu: &'static str,
    icon: Option<Icon>,
    hit: Hit,
}

/// The measured sizes the tool layout reads, supplied by the app so this
/// module stays renderer-free.
pub(super) trait ToolMetrics {
    /// Width of a toolbar pill for `label`, with room for an icon when `icon`.
    fn pill_w(&self, label: &str, icon: bool) -> f64;
    /// Height of one overflow-menu row.
    fn menu_row_h(&self) -> f64;
}

#[derive(Default)]
pub(super) struct ToolbarToolsLayout {
    pub(super) menu_bg: Option<Rect>,
    pub(super) menu_buttons: Vec<UiButton>,
}

pub(super) enum MenuPress {
    PassThrough,
    Consumed,
    Action(Hit),
}

#[derive(Default)]
pub(super) struct ToolbarTools {
    open: bool,
}

impl ToolbarTools {
    pub(super) fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub(super) fn dismiss(&mut self) {
        self.open = false;
    }

    /// Lay out the insert tools as pills filling `slot`'s height, from its
    /// left edge up to its right edge.
    ///
    /// `metrics` sizes a pill for a label (without / with its icon) from the
    /// app's measured chrome metrics, so the slot a tool gets is exactly as
    /// wide as the text it paints, and gives the measured control height the
    /// overflow menu's rows take.
    pub(super) fn layout(
        &self,
        buttons: &mut Vec<UiButton>,
        slot: Rect,
        toolbar: Rect,
        metrics: &dyn ToolMetrics,
    ) -> ToolbarToolsLayout {
        let (tools_x, tools_right, by, bh) = (slot.x0, slot.x1, slot.y0, slot.height());
        let tools = tools();
        let widths: Vec<f64> = tools
            .iter()
            .map(|tool| metrics.pill_w(&tr(tool.bar), tool.icon.is_some()))
            .collect();
        let total = widths.iter().sum::<f64>() + GAP * (tools.len() as f64 - 1.0);
        let fits_all = tools_x + total <= tools_right + 0.5;
        // The overflow button is a square of the pill height.
        let overflow_w = bh.max(OVERFLOW_MIN_WIDTH);
        let limit = if fits_all {
            tools_right
        } else {
            tools_right - overflow_w - GAP
        };

        let mut bx = tools_x;
        let mut hidden = Vec::new();
        for (index, (tool, width)) in tools.iter().zip(widths).enumerate() {
            if hidden.is_empty() && bx + width <= limit + 0.5 {
                buttons.push(bar_button(tool, Rect::new(bx, by, bx + width, by + bh)));
                bx += width + GAP;
            } else {
                hidden.push(index);
            }
        }
        if hidden.is_empty() {
            return ToolbarToolsLayout::default();
        }

        let overflow_x = bx.min(tools_right - overflow_w);
        let overflow = Rect::new(overflow_x, by, overflow_x + overflow_w, by + bh);
        buttons.push(UiButton {
            rect: overflow,
            kind: ButtonKind::IconOnly {
                icon: Icon::MoreHorizontal,
                active: self.open,
            },
            hit: Hit::ToggleToolbarMenu,
        });
        if !self.open {
            return ToolbarToolsLayout::default();
        }

        menu_layout(&tools, &hidden, overflow, toolbar, metrics)
    }

    pub(super) fn on_press(
        &mut self,
        layout: &ToolbarToolsLayout,
        toolbar_buttons: &[UiButton],
        point: Point,
    ) -> MenuPress {
        let Some(bg) = layout.menu_bg else {
            return MenuPress::PassThrough;
        };
        if let Some(button) = layout.menu_buttons.iter().find(|b| b.rect.contains(point)) {
            self.dismiss();
            return MenuPress::Action(button.hit.clone());
        }
        if bg.contains(point) {
            return MenuPress::Consumed;
        }
        let on_toggle = toolbar_buttons
            .iter()
            .any(|b| matches!(b.hit, Hit::ToggleToolbarMenu) && b.rect.contains(point));
        if on_toggle {
            MenuPress::PassThrough
        } else {
            self.dismiss();
            MenuPress::Consumed
        }
    }

    pub(super) fn draw(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        layout: &ToolbarToolsLayout,
        mut draw_button: impl FnMut(&mut Scene, &UiButton),
    ) {
        let Some(bg) = layout.menu_bg else { return };
        // The overflow menu is a floating panel (HIG §9.1): panel ground,
        // panel border, elevation.
        chrome::panel(
            scene,
            transform,
            theme,
            bg,
            f64::from(theme.tokens.radii.md),
        );
        for button in &layout.menu_buttons {
            draw_button(scene, button);
        }
    }
}

fn tools() -> [Tool; 6] {
    [
        Tool {
            bar: "slides-tool-slide",
            menu: "slides-tool-new-slide",
            icon: Some(Icon::Plus),
            hit: Hit::NewSlide,
        },
        Tool {
            bar: "slides-tool-text",
            menu: "slides-tool-text-box",
            icon: Some(Icon::FileText),
            hit: Hit::Insert(crate::model::ElementKind::Text),
        },
        Tool {
            bar: "slides-tool-rect",
            menu: "slides-tool-rectangle",
            icon: None,
            hit: Hit::Insert(crate::model::ElementKind::Rect),
        },
        Tool {
            bar: "slides-tool-ellipse",
            menu: "slides-tool-ellipse",
            icon: None,
            hit: Hit::Insert(crate::model::ElementKind::Ellipse),
        },
        Tool {
            bar: "slides-tool-line",
            menu: "slides-tool-line",
            icon: None,
            hit: Hit::Insert(crate::model::ElementKind::Line),
        },
        Tool {
            bar: "slides-tool-image",
            menu: "slides-tool-image",
            icon: None,
            hit: Hit::Insert(crate::model::ElementKind::Image),
        },
    ]
}

/// The labels the insert tools paint on the bar, so the app can pre-measure
/// them with the style they are painted in.
pub(super) fn bar_labels() -> [String; 6] {
    tools().map(|tool| tr(tool.bar))
}

/// The labels the overflow menu paints, likewise pre-measured by the app.
pub(super) fn menu_labels() -> [String; 6] {
    tools().map(|tool| tr(tool.menu))
}

fn bar_button(tool: &Tool, rect: Rect) -> UiButton {
    let label = tr(tool.bar);
    let kind = match tool.icon {
        Some(icon) => ButtonKind::IconText {
            icon,
            label: label.clone(),
            accent: false,
        },
        None => ButtonKind::Text {
            label,
            active: false,
            danger: false,
        },
    };
    UiButton {
        rect,
        kind,
        hit: tool.hit.clone(),
    }
}

fn menu_layout(
    tools: &[Tool],
    hidden: &[usize],
    overflow: Rect,
    toolbar: Rect,
    metrics: &dyn ToolMetrics,
) -> ToolbarToolsLayout {
    let menu_row_h = metrics.menu_row_h();
    // Rows are measured controls with a `MENU_ROW_SEAM` between them; the
    // panel is as wide as its widest row (plus the row inset and panel
    // padding), never narrower than its classic width.
    let row_h = menu_row_h + MENU_ROW_SEAM;
    let pad = GAP;
    let menu_w = hidden
        .iter()
        .filter_map(|&index| tools.get(index))
        .map(|tool| metrics.pill_w(&tr(tool.menu), false) + 2.0 * pad)
        .fold(MENU_MIN_WIDTH, f64::max)
        .min(toolbar.width() - 2.0 * pad);
    let mx1 = overflow
        .x1
        .clamp(toolbar.x0 + pad + menu_w, toolbar.x1 - pad);
    let mx0 = mx1 - menu_w;
    let my0 = overflow.y1 + GAP;
    let bg = Rect::new(mx0, my0, mx1, my0 + pad * 2.0 + row_h * hidden.len() as f64);
    let menu_buttons = hidden
        .iter()
        .enumerate()
        .filter_map(|(row, &index)| tools.get(index).map(|tool| (row, tool)))
        .map(|(row, tool)| {
            let y = my0 + pad + row as f64 * row_h;
            UiButton {
                rect: Rect::new(mx0 + pad, y, mx1 - pad, y + menu_row_h),
                kind: ButtonKind::Text {
                    label: tr(tool.menu),
                    active: false,
                    danger: false,
                },
                hit: tool.hit.clone(),
            }
        })
        .collect();
    ToolbarToolsLayout {
        menu_bg: Some(bg),
        menu_buttons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ElementKind;

    /// The 1× estimate the app falls back to before its first draw has
    /// measured the labels: a text pill is its label plus 12 px each side, an
    /// icon pill adds the 18 px icon and an 8 px gap.
    struct Estimate;

    impl ToolMetrics for Estimate {
        fn pill_w(&self, label: &str, icon: bool) -> f64 {
            let icon_w = if icon { 26.0 } else { 0.0 };
            icon_w + chrome::est_width(label, 13.0) + 24.0
        }

        fn menu_row_h(&self) -> f64 {
            30.0
        }
    }

    fn narrow_layout(component: &ToolbarTools) -> (Vec<UiButton>, ToolbarToolsLayout) {
        let mut buttons = Vec::new();
        let layout = component.layout(
            &mut buttons,
            Rect::new(180.0, 11.0, 430.0, 45.0),
            Rect::new(0.0, 0.0, 560.0, 56.0),
            &Estimate,
        );
        (buttons, layout)
    }

    /// Narrow layouts keep hidden tools reachable and the pointer policy owns selection/click-away.
    #[test]
    fn toolbar_tools_stay_reachable_and_own_pointer_policy() {
        // Case `narrow_layout_keeps_hidden_tools_reachable`.
        {
            let mut component = ToolbarTools::default();
            component.toggle();
            let (buttons, layout) = narrow_layout(&component);
            assert!(
                buttons
                    .iter()
                    .any(|b| matches!(b.hit, Hit::ToggleToolbarMenu))
            );
            assert!(layout.menu_bg.is_some());
            for kind in [
                ElementKind::Rect,
                ElementKind::Ellipse,
                ElementKind::Line,
                ElementKind::Image,
            ] {
                assert!(
                    buttons
                        .iter()
                        .chain(&layout.menu_buttons)
                        .any(|b| matches!(&b.hit, Hit::Insert(candidate) if *candidate == kind))
                );
            }
        }

        // Case `pointer_policy_owns_row_selection_padding_and_click_away`.
        {
            let mut component = ToolbarTools::default();
            component.toggle();
            let (buttons, layout) = narrow_layout(&component);
            let row = layout.menu_buttons.last().expect("narrow menu has rows");
            assert!(matches!(
                component.on_press(&layout, &buttons, row.rect.center()),
                MenuPress::Action(_)
            ));
            assert!(!component.is_open());

            component.toggle();
            let (buttons, layout) = narrow_layout(&component);
            let padding = layout.menu_bg.expect("open panel").origin() + (2.0, 2.0);
            assert!(matches!(
                component.on_press(&layout, &buttons, padding),
                MenuPress::Consumed
            ));
            assert!(component.is_open());
            assert!(matches!(
                component.on_press(&layout, &buttons, Point::new(10.0, 200.0)),
                MenuPress::Consumed
            ));
            assert!(!component.is_open());
        }
    }
}
