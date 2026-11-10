//! The divider a conversation divides its lines with: a caption centred in a band, ruled out to
//! both sides.
//!
//! A conversation has one divider, however it divides its lines. The day its lines turn over
//! ([`super::day`]) and the place the reader's unread lines start ([`unread_label`]) are the same
//! mark: one band, a list row tall, the label centred with an `s3` gap between it and each rule,
//! so the two read as the same thing where a conversation changes from one day to the next and
//! where reading starts again. What tells them apart is the ink alone ([`DividerTone`]): a day in
//! dimmed text over a hairline, the place reading starts again in the accent. Where a day turns
//! over at the very line the reader has not seen — the two marks over one row — they are one
//! band saying both ([`day_with_unread`]), never a day's band stacked over the unread one.

use vello::kurbo::Rect;
use vello::peniko::Color;

use crate::chrome::nav_row_height;
use crate::component::{Component, PropType, Type, Value};
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;

/// The height a divider takes: one list row, at every text scale.
#[must_use]
pub fn height(theme: &Theme) -> f64 {
    nav_row_height(theme)
}

/// What a divider marks, and so the ink it is drawn in: a day of the conversation, or the place
/// the reader's unread lines start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DividerTone {
    /// A day of the conversation: dimmed text over a hairline.
    #[default]
    Quiet,
    /// The place reading starts again: the accent, as an unread count is, over its own colour.
    Accent,
}

impl DividerTone {
    /// The ink a divider of this tone is drawn in over `ground`, the surface it lands on: a day
    /// is dimmed text over the ground's own hairline, the place reading starts again is the
    /// accent over a rule in that accent, at a weight a person reads at a glance — a day's rule
    /// divides two of one kind and says nothing by itself, this one says where reading starts.
    ///
    /// The rule is derived against `ground` and not against the ink: a rule in the accent
    /// derived as an edge of the accent is black or white at a wash, and painted on the
    /// conversation's ground it moves nothing, so the divider reads as bare text.
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

    /// The ink a divider of this tone is drawn in over a conversation's ground: the content the
    /// lines of the conversation stand on.
    #[must_use]
    pub fn ink(self, theme: &Theme) -> DividerInk {
        self.ink_on(theme, theme.tokens.colors.surfaces.content)
    }
}

impl DividerTone {
    /// The words a divider of this tone says when nobody gives it any: the toolkit's own word
    /// for the place the reader's unread lines start, and none for a day, which is named by the
    /// day it marks.
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

/// The ink of a divider: the label's words and the rules beside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DividerInk {
    /// The label's words.
    pub label: Color,
    /// The rules running out to the column's edges.
    pub rule: Color,
}

/// What a divider painted: the rule running from the column's leading edge to the words, the one
/// running from them to its trailing edge, and where the words stand between the two. A rule is
/// `None` where the band leaves it no room — a label wider than the column takes the whole band.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DividerParts {
    /// The rule from `leading` to the words.
    pub lead: Option<Rect>,
    /// The rule from the words to `trailing`.
    pub trail: Option<Rect>,
    /// Where the label stands, centred in the band.
    pub label: Rect,
}

/// Paint `label` centred in `band`, its rules running from `leading` to the words and from them
/// to `trailing` (the message column's edges, not the window's), each stopping an `s3` gap short
/// of the label, in `tone`'s ink.
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

/// The words a conversation's unread divider says, in the language of the person reading it.
#[must_use]
pub fn unread_label() -> String {
    super::t("conversation-new-messages")
}

/// The words of the one band a conversation draws where the day turns over at the first line the
/// reader has not seen: the day and that reading starts there ("Yesterday · New").
///
/// A day's band stacked over the unread one reads as two dividers saying two things about one
/// place; a conversation that meets them draws one band, in the accent, saying both.
#[must_use]
pub fn day_with_unread(day: &str) -> String {
    crate::i18n::widget_format("conversation-day-new", &[("day", day.into())])
}

/// Whether what a divider says is where reading starts again — the band drawn in the accent —
/// told by its words in the language the person reads: the label alone, or the day it opens
/// saying both ([`day_with_unread`]).
#[must_use]
pub fn says_where_reading_starts(words: &str) -> bool {
    words.ends_with(&unread_label()) || words.ends_with(&day_with_unread(""))
}

/// A divider drawn where a canvas painter places it: the day a run of lines turns over, or the
/// place the reader's unread lines start.
///
/// The band is a list row tall, so a divider sits in the same rhythm as the lines it separates,
/// and the rules stop short of the text at an `s3` gap so the label never touches one.
pub struct SectionDividerFace<'a> {
    label: &'a str,
    tone: DividerTone,
}

impl<'a> SectionDividerFace<'a> {
    /// A day's divider: dimmed text over a hairline.
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

    /// The height a divider takes: one list row, at every text scale.
    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        height(theme)
    }

    /// Paint the divider across `band`, its rules running from `leading` to `trailing` (the
    /// message column's edges, not the window's).
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

/// The divider as a widget, for a list built of widgets: a band across its column with the label
/// at its centre, over the row of the item it marks.
///
/// A described component (`ui.SectionDivider`), so a host that validates what an app sends builds
/// it, and a view that draws a conversation's own rows marks them with the very divider a
/// transcript draws, in the same band, at the same rhythm.
pub struct SectionDivider {
    label: String,
    tone: DividerTone,
    rect: Rect,
    /// It was drawn, and shows the same since: a divider never changes, so it repaints only
    /// where it moved.
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

    /// A divider where the reader's unread lines start: the accent over `label`.
    #[must_use]
    pub fn accent(label: impl Into<String>) -> Self {
        Self {
            tone: DividerTone::Accent,
            ..Self::new(label)
        }
    }

    /// The divider where a reader's unread lines start, in the toolkit's words: the place to
    /// start reading again.
    #[must_use]
    pub fn unread() -> Self {
        Self::accent(unread_label())
    }

    /// The one band where a day turns over at the first line the reader has not seen, saying
    /// both in the accent ([`day_with_unread`]): one divider, never a day's over the unread
    /// one.
    #[must_use]
    pub fn unread_day(day: &str) -> Self {
        Self::accent(day_with_unread(day))
    }

    /// The height a divider takes: one list row, at every text scale.
    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        height(theme)
    }

    /// What this divider marks: a day of the conversation, or the place the reader's unread
    /// lines start.
    #[must_use]
    pub fn tone(&self) -> DividerTone {
        self.tone
    }

    /// What it says at its centre.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
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

/// A divider in a conversation: a caption centred in a band, ruled out to both sides, over the
/// row where the part it opens begins — a day of the lines, or the place the reader's unread
/// lines start. Both are this one component, told apart by the ink they are drawn in.
///
/// A day's label is the day as the person reads it ("Today", "Yesterday", "Sunday", "Jun 21"); a
/// view that draws its own rows reads it from the clock it already has, as a transcript does
/// (`ConversationClock::day_label`). The place reading starts again says so in the person's
/// language, which is the toolkit's word to say (`unread_label`): a divider in the accent that
/// is given no words of its own says it.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "SectionDivider", role = "Group", label = "label")]
pub struct SectionDividerProps {
    /// What the divider says: the day it opens ("Today", "Sunday", "Jun 21"). Unsaid, it says
    /// what its tone marks: the toolkit's own word where the reader's unread lines start.
    #[component(positional)]
    pub label: Option<String>,
    /// What it marks, which sets its ink: a day of the conversation unless said, or the accent
    /// where the reader's unread lines start.
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

    /// Another day or another tone: the divider shows the new one where it stands.
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
