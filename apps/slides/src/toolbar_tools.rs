//! Responsive insert toolbar: state, layout, painting, and pointer policy.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::icons::Icon;
use ui::theme::Theme;

use crate::app::{ButtonKind, Hit, UiButton};
use crate::chrome;
use crate::render;

const GAP: f64 = 8.0;
const OVERFLOW_WIDTH: f64 = 40.0;

struct Tool {
    bar: &'static str,
    menu: &'static str,
    icon: Option<Icon>,
    hit: Hit,
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

    #[cfg(test)]
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub(super) fn dismiss(&mut self) {
        self.open = false;
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "toolbar bounds and the one-dimensional insertion slot are independent geometry"
    )]
    pub(super) fn layout(
        &self,
        buttons: &mut Vec<UiButton>,
        tools_x: f64,
        tools_right: f64,
        by: f64,
        bh: f64,
        toolbar: Rect,
    ) -> ToolbarToolsLayout {
        let tools = tools();
        let widths: Vec<f64> = tools.iter().map(tool_width).collect();
        let total = widths.iter().sum::<f64>() + GAP * (tools.len() as f64 - 1.0);
        let fits_all = tools_x + total <= tools_right + 0.5;
        let limit = if fits_all {
            tools_right
        } else {
            tools_right - OVERFLOW_WIDTH - GAP
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

        let overflow_x = bx.min(tools_right - OVERFLOW_WIDTH);
        let overflow = Rect::new(overflow_x, by, overflow_x + OVERFLOW_WIDTH, by + bh);
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

        menu_layout(&tools, &hidden, overflow, toolbar)
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
        let colors = &theme.tokens.colors;
        let shadow = bg + vello::kurbo::Vec2::new(0.0, 4.0);
        chrome::fill_rrect(
            scene,
            transform,
            shadow.inflate(2.0, 2.0),
            12.0,
            render::with_alpha(Color::BLACK, 90),
        );
        chrome::fill_rrect(scene, transform, bg, 10.0, colors.surfaces.raised);
        chrome::stroke_rrect(
            scene,
            transform,
            bg,
            10.0,
            render::with_alpha(colors.text_primary, 48),
            1.0,
        );
        for button in &layout.menu_buttons {
            draw_button(scene, button);
        }
    }
}

fn tools() -> [Tool; 6] {
    [
        Tool {
            bar: "Slide",
            menu: "New slide",
            icon: Some(Icon::Plus),
            hit: Hit::NewSlide,
        },
        Tool {
            bar: "Text",
            menu: "Text box",
            icon: Some(Icon::FileText),
            hit: Hit::Insert(crate::model::ElementKind::Text),
        },
        Tool {
            bar: "Rect",
            menu: "Rectangle",
            icon: None,
            hit: Hit::Insert(crate::model::ElementKind::Rect),
        },
        Tool {
            bar: "Ellipse",
            menu: "Ellipse",
            icon: None,
            hit: Hit::Insert(crate::model::ElementKind::Ellipse),
        },
        Tool {
            bar: "Line",
            menu: "Line",
            icon: None,
            hit: Hit::Insert(crate::model::ElementKind::Line),
        },
        Tool {
            bar: "Image",
            menu: "Image",
            icon: None,
            hit: Hit::Insert(crate::model::ElementKind::Image),
        },
    ]
}

fn tool_width(tool: &Tool) -> f64 {
    match tool.icon {
        Some(_) => 18.0 + 6.0 + chrome::est_width(tool.bar, 13.0) + 22.0,
        None => chrome::est_width(tool.bar, 13.0) + 26.0,
    }
}

fn bar_button(tool: &Tool, rect: Rect) -> UiButton {
    let kind = match tool.icon {
        Some(icon) => ButtonKind::IconText {
            icon,
            label: tool.bar.into(),
            accent: false,
        },
        None => ButtonKind::Text {
            label: tool.bar.into(),
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
) -> ToolbarToolsLayout {
    let row_h = 34.0;
    let pad = 6.0;
    let menu_w = hidden
        .iter()
        .filter_map(|&index| tools.get(index))
        .map(|tool| chrome::est_width(tool.menu, 13.0) + 52.0)
        .fold(150.0_f64, f64::max)
        .min(toolbar.width() - 16.0);
    let mx1 = overflow
        .x1
        .clamp(toolbar.x0 + 8.0 + menu_w, toolbar.x1 - 8.0);
    let mx0 = mx1 - menu_w;
    let my0 = overflow.y1 + 6.0;
    let bg = Rect::new(mx0, my0, mx1, my0 + pad * 2.0 + row_h * hidden.len() as f64);
    let menu_buttons = hidden
        .iter()
        .enumerate()
        .filter_map(|(row, &index)| tools.get(index).map(|tool| (row, tool)))
        .map(|(row, tool)| {
            let y = my0 + pad + row as f64 * row_h;
            UiButton {
                rect: Rect::new(mx0 + pad, y, mx1 - pad, y + row_h - 4.0),
                kind: ButtonKind::Text {
                    label: tool.menu.into(),
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

    fn narrow_layout(component: &ToolbarTools) -> (Vec<UiButton>, ToolbarToolsLayout) {
        let mut buttons = Vec::new();
        let layout = component.layout(
            &mut buttons,
            180.0,
            430.0,
            11.0,
            34.0,
            Rect::new(0.0, 0.0, 560.0, 56.0),
        );
        (buttons, layout)
    }

    #[test]
    fn narrow_layout_keeps_hidden_tools_reachable() {
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

    #[test]
    fn pointer_policy_owns_row_selection_padding_and_click_away() {
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
