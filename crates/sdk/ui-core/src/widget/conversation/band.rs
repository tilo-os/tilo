//! The divider a conversation divides its lines with: a caption centred in a band, ruled out to both
//! sides.

use vello::kurbo::Rect;
use vello::peniko::Color;

use crate::chrome::nav_row_height;
use crate::component::{Component, PropType, Type, Value};
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;

#[must_use]
pub fn height(theme: &Theme) -> f64 {
    nav_row_height(theme)
}

/// What a divider marks, and so the ink it is drawn in: a day of the conversation, or the place the
/// reader's unread lines start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DividerTone {
    #[default]
    Quiet,
    Accent,
}

impl DividerTone {
    #[must_use]
    pub fn ink_on(self, theme: &Theme, ground: Color) -> DividerInk {
        match self {
            Self::Quiet => DividerInk {
                label: theme.text_dim,
                rule: crate::surface::edge_on(ground),
            },
            Self::Accent => {
                let accent = theme.tokens.colors.intent.accent_ink;
                DividerInk {
                    label: accent,
                    rule: crate::surface::rule_of(accent, ground),
                }
            }
        }
    }

    #[must_use]
    pub fn ink(self, theme: &Theme) -> DividerInk {
        self.ink_on(theme, theme.tokens.colors.surfaces.content)
    }
}

impl DividerTone {
    fn said_by_default(self) -> String {
        match self {
            Self::Quiet => String::new(),
            Self::Accent => unread_label(),
        }
    }
}

impl PropType for DividerTone {
    fn ty() -> Type {
        Type::Choice {
            name: "DividerTone".into(),
            cases: ["quiet", "accent"].map(String::from).to_vec(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let (Value::Choice(case) | Value::Text(case)) = value else {
            return Err(format!("takes DividerTone, not {}", value.kind()));
        };
        Ok(match case.as_str() {
            "quiet" => Self::Quiet,
            "accent" => Self::Accent,
            other => return Err(format!("has no case `{other}`")),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DividerInk {
    pub label: Color,
    pub rule: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DividerParts {
    pub lead: Option<Rect>,
    pub trail: Option<Rect>,
    pub label: Rect,
}

/// Paint `label` centred in `band`, its rules running from `leading` to the words and from them to
/// `trailing` (the message column's edges, not the window's), each stopping an `s3` gap short of
/// the label, in `tone`'s ink.
pub fn paint(
    paint: &mut PaintCtx<'_>,
    band: Rect,
    leading: f64,
    trailing: f64,
    label: &str,
    tone: DividerTone,
) -> DividerParts {
    let theme = paint.theme;
    let ink = tone.ink_on(theme, theme.tokens.colors.surfaces.content);
    let style = theme.text(TextRole::Caption);
    let gap = f64::from(theme.tokens.spacing.s3);
    let room = crate::num::f32_of_f64((trailing - leading).max(0.0));
    // A label wider than the band (a long date at 1.5× in a narrow window) is elided rather than
    // drawn over the column's edges.
    let shown = paint.text.elide_to_width_style(label, room, &style);
    let width = f64::from(paint.text.measure_style(&shown, None, &style).0);
    let centre = band.center().x;
    let left_edge = centre - width / 2.0 - gap;
    let right_edge = centre + width / 2.0 + gap;
    let rule_y = band.center().y;
    let lead = crate::surface::separator_rect(leading, left_edge, rule_y, 0.0);
    let trail = crate::surface::separator_rect(right_edge, trailing, rule_y, 0.0);
    // The rule is the ink the tone derived against the ground it lands on, painted as it comes:
    // deriving it again here would derive it of its own wash, and a wash of a wash moves
    // nothing.
    for rule in [lead, trail].into_iter().flatten() {
        paint.scene.fill(
            vello::peniko::Fill::NonZero,
            paint.transform,
            ink.rule,
            None,
            &rule,
        );
    }
    let words = Rect::new(centre - width / 2.0, band.y0, centre + width / 2.0, band.y1);
    if !label.is_empty() {
        paint.text_role_in_band(
            &shown,
            TextRole::Caption,
            ink.label,
            centre - width / 2.0,
            band,
            None,
        );
    }
    DividerParts {
        lead,
        trail,
        label: words,
    }
}

#[must_use]
pub fn unread_label() -> String {
    super::t("conversation-new-messages")
}

#[must_use]
pub fn day_with_unread(day: &str) -> String {
    crate::i18n::widget_format("conversation-day-new", &[("day", day.into())])
}

#[must_use]
pub fn says_where_reading_starts(words: &str) -> bool {
    words.ends_with(&unread_label()) || words.ends_with(&day_with_unread(""))
}

pub struct SectionDividerFace<'a> {
    label: &'a str,
    tone: DividerTone,
}

impl<'a> SectionDividerFace<'a> {
    #[must_use]
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            tone: DividerTone::Quiet,
        }
    }

    /// The divider where the reader's unread lines start: the accent, over a label saying so.
    #[must_use]
    pub fn accent(label: &'a str) -> Self {
        Self {
            label,
            tone: DividerTone::Accent,
        }
    }

    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        height(theme)
    }

    pub fn paint(
        &self,
        paint: &mut PaintCtx<'_>,
        band: Rect,
        leading: f64,
        trailing: f64,
    ) -> DividerParts {
        super::band::paint(paint, band, leading, trailing, self.label, self.tone)
    }
}

pub struct SectionDivider {
    label: String,
    tone: DividerTone,
    rect: Rect,
    /// It was drawn, and shows the same since: a divider never changes, so it repaints only where it
    /// moved.
    drawn: bool,
}

impl SectionDivider {
    /// A day's divider, saying `label` ("Today", "Yesterday", "Sunday", "Jun 21").
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn accent(label: impl Into<String>) -> Self {
        Self {
            tone: DividerTone::Accent,
            ..Self::new(label)
        }
    }

    #[must_use]
    pub fn unread() -> Self {
        Self::accent(unread_label())
    }

    /// The one band where a day turns over at the first line the reader has not seen, saying both in
    /// the accent ([`day_with_unread`]): one divider, never a day's over the unread one.
    #[must_use]
    pub fn unread_day(day: &str) -> Self {
        Self::accent(day_with_unread(day))
    }

    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        height(theme)
    }

    #[must_use]
    pub fn tone(&self) -> DividerTone {
        self.tone
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Whether it marks a day of the conversation: a day's own band, or the one band that opens a day
    /// where reading starts again and so says both ([`Self::unread_day`]).
    #[must_use]
    pub fn names_a_day(&self) -> bool {
        match self.tone {
            DividerTone::Quiet => true,
            DividerTone::Accent => self
                .label
                .strip_suffix(&day_with_unread(""))
                .is_some_and(|day| !day.trim().is_empty()),
        }
    }
}

impl Default for SectionDivider {
    fn default() -> Self {
        Self {
            label: String::new(),
            tone: DividerTone::Quiet,
            rect: Rect::ZERO,
            drawn: false,
        }
    }
}

/// A divider in a conversation: a caption centred in a band, ruled out to both sides, over the row
/// where the part it opens begins — a day of the lines, or the place the reader's unread lines start.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "SectionDivider", role = "Group", label = "label")]
pub struct SectionDividerProps {
    /// What the divider says: the day it opens ("Today", "Sunday", "Jun 21").
    #[component(positional)]
    pub label: Option<String>,
    /// What it marks, which sets its ink: a day of the conversation unless said, or the accent where the
    /// reader's unread lines start.
    pub tone: DividerTone,
}

impl crate::component::Described for SectionDivider {
    type Props = SectionDividerProps;

    fn from_props(_key: &str, props: SectionDividerProps) -> Self {
        Self {
            label: props.label.unwrap_or_else(|| props.tone.said_by_default()),
            tone: props.tone,
            ..Self::default()
        }
    }

    fn set_props(&mut self, props: SectionDividerProps) -> bool {
        let label = props.label.unwrap_or_else(|| props.tone.said_by_default());
        if self.label == label && self.tone == props.tone {
            return true;
        }
        self.label = label;
        self.tone = props.tone;
        self.drawn = false;
        true
    }
}

impl crate::widget::Widget for SectionDivider {
    crate::described!();

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> taffy::NodeId {
        use crate::widget::TaffyTreeExt as _;
        let style = taffy::Style {
            flex_shrink: 0.0,
            size: taffy::Size {
                width: taffy::style_helpers::percent(1.0_f32),
                height: taffy::style_helpers::length(crate::num::f32_of_f64(Self::height(theme))),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        if presence.should_cull(self.rect) {
            return;
        }
        SectionDividerFace {
            label: &self.label,
            tone: self.tone,
        }
        .paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
            self.rect.x0,
            self.rect.x1,
        );
    }

    fn name(&self) -> &'static str {
        "SectionDivider"
    }

    fn needs_redraw(&self) -> bool {
        !self.drawn
    }

    fn mark_drawn(&mut self) {
        self.drawn = true;
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(AccessNode::new(Role::Heading { level: 3 }).with_name(self.label.clone()))
    }
}
