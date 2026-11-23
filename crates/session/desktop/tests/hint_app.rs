//! A small app for windows that control hints are placed over: it lays out a few lines of text
//! and a field the way an everyday app does (a list down its side, an empty state in its middle,
//! a field at its foot), paints them, and tells assistive technology where each is, which is
//! where a hint's card must not stand.

use std::collections::HashMap;

use ui::a11y::{AccessNode, Role};
use ui::kurbo::{Affine, Point, Rect};
use ui::taffy::{NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::widget::{AppKind, AppWidget, IconDescriptor, TaffyTreeExt as _, Widget};
use ui::{TextRenderer, TextRole};

/// One line the app shows: its words, its type, and where it stands as a share of the window
/// across and down; `centred` stands it centred on that point.
#[derive(Clone, Copy)]
pub struct Line {
    pub words: &'static str,
    pub role: TextRole,
    pub at: (f64, f64),
    pub centred: bool,
}

/// An app showing `lines` and, at its foot, a field reading `field`.
pub struct SketchApp {
    title: &'static str,
    lines: Vec<Line>,
    field: Option<&'static str>,
    rect: Rect,
    /// Where each line and the field were painted, as last drawn.
    drawn: Vec<(Rect, &'static str, Role)>,
}

impl SketchApp {
    /// A chat with no messages yet.
    #[must_use]
    pub fn chat() -> Self {
        Self {
            title: "Chat",
            lines: vec![
                line("# general", TextRole::Label, (0.04, 0.03), false),
                line("# feedback", TextRole::Label, (0.04, 0.07), false),
                line("voice", TextRole::Label, (0.04, 0.11), false),
                line("No messages yet", TextRole::Heading, (0.68, 0.47), true),
                line(
                    "Say hello. Everyone in #general sees it.",
                    TextRole::Body,
                    (0.68, 0.52),
                    true,
                ),
            ],
            field: Some("Message #general"),
            rect: Rect::ZERO,
            drawn: Vec::new(),
        }
    }

    /// A note open beside the list of notes.
    #[must_use]
    pub fn notes() -> Self {
        Self {
            title: "Notes",
            lines: vec![
                line("Notes", TextRole::Heading, (0.04, 0.03), false),
                line(
                    "Launch plan for Friday",
                    TextRole::Label,
                    (0.04, 0.10),
                    false,
                ),
                line("New note", TextRole::Label, (0.04, 0.14), false),
                line(
                    "Launch plan for Friday",
                    TextRole::Title,
                    (0.44, 0.05),
                    false,
                ),
                line(
                    "Ship the beta to the first ten teams.",
                    TextRole::Body,
                    (0.44, 0.15),
                    false,
                ),
            ],
            field: None,
            rect: Rect::ZERO,
            drawn: Vec::new(),
        }
    }
}

const fn line(words: &'static str, role: TextRole, at: (f64, f64), centred: bool) -> Line {
    Line {
        words,
        role,
        at,
        centred,
    }
}

impl Widget for SketchApp {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        _map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(Style {
            size: Size::from_percent(1.0, 1.0),
            ..Style::default()
        })
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn draw(&mut self, ctx: &mut tilo_ui::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let rect = self.rect;
        let ink = theme.tokens.colors.text_primary;
        let quiet = theme.tokens.colors.text_tertiary;
        let mut paint = ui::paint::PaintCtx::new(scene, transform, text, theme);
        self.drawn.clear();
        for line in &self.lines {
            let size = paint.measure_role(line.words, line.role, None);
            let at = Point::new(
                rect.x0 + rect.width() * line.at.0,
                rect.y0 + rect.height() * line.at.1,
            );
            let origin = if line.centred {
                Point::new(at.x - size.width / 2.0, at.y - size.height / 2.0)
            } else {
                at
            };
            let colour = if line.role == TextRole::Body {
                quiet
            } else {
                ink
            };
            paint.text_role(line.words, line.role, colour, origin, None);
            self.drawn.push((
                Rect::from_origin_size(origin, size),
                line.words,
                Role::StaticText,
            ));
        }
        if let Some(field) = self.field {
            let box_ = Rect::new(
                rect.x0 + rect.width() * 0.38,
                rect.y1 - 64.0,
                rect.x1 - 20.0,
                rect.y1 - 20.0,
            );
            paint.stroke_round(box_, 10.0, quiet, 1.0);
            let size = paint.measure_role(field, TextRole::Body, None);
            paint.text_role(
                field,
                TextRole::Body,
                quiet,
                Point::new(box_.x0 + 16.0, box_.center().y - size.height / 2.0),
                None,
            );
            self.drawn.push((box_, field, Role::TextInput));
        }
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(
            AccessNode::new(Role::Group)
                .with_name(self.title)
                .with_bounds(self.rect)
                .with_children(
                    self.drawn
                        .iter()
                        .map(|(rect, words, role)| {
                            AccessNode::new(*role).with_name(*words).with_bounds(*rect)
                        })
                        .collect(),
                ),
        )
    }

    fn name(&self) -> &'static str {
        "SketchApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl AppWidget for SketchApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }

    fn app_title(&self) -> &str {
        self.title
    }

    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph(if self.title == "Chat" { "C" } else { "N" })
    }
}

/// Windows titled "Chat" show [`SketchApp::chat`], every other window [`SketchApp::notes`].
pub struct SketchTiles;

impl tilo_desktop_surface::TileSource for SketchTiles {
    fn tile(
        &mut self,
        _: &tilo_desktop_model::Workspace,
        tab: &tilo_desktop_model::Tab,
    ) -> ui::widget::TileHost {
        let app = if tab.title == "Chat" {
            SketchApp::chat()
        } else {
            SketchApp::notes()
        };
        ui::widget::TileHost::new(ui::widget::TileContent::App(Box::new(app)))
    }
}
