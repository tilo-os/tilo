//! The remark surface on screen: the pick overlay, the composer, review,
//! and the recording pill.
//!
//! Immediate-mode chrome over shared faces (`ButtonFace`, `ChoiceCardFace`,
//! `ToggleFace`, `Chip`, `surface::panel`, `selection_tier_on`) and one
//! shared `TextInput`, so every control keeps the toolkit's focus,
//! accessibility and theming. Nothing here decides anything: every target
//! names the shell action it stands for ([`Target::action`]), and the host
//! runs that action exactly as `Ctrl+K` or `tilo-control` would.

use std::time::Instant;

use serde_json::{Value, json};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Shape as _, Stroke};
use vello::peniko::{Color, Fill};

use ui::ColorCompat as _;
use ui::chrome::shell::{SelectionState, selection_tier_on};
use ui::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{
    BadgeTone, Button, ButtonFace, Chip, ChipLead, ChipStyle, ChoiceCardFace, OfferPill,
    OfferTarget, SetupChoice, SetupOffer, Toggle, ToggleFace,
};

use crate::shell::i18n::{number, t, t_args, text};

use super::pick::PickState;
use super::review::{ModelUse, Review, SetUpChoice, Verdict};
use super::{Composer, DestinationKind, DestinationOption, Flow, Opening, Sending, Stage};

/// The pill a set-up choice's tag stands for.
fn offer_pill(tag: Option<&str>) -> Option<OfferPill> {
    let (key, tone) = match tag? {
        "fit-excellent" => ("remark-review-offer-fit-excellent", BadgeTone::Success),
        "fit-good" => ("remark-review-offer-fit-good", BadgeTone::Success),
        "fit-tight" => ("remark-review-offer-fit-tight", BadgeTone::Warning),
        "speed-interactive" => ("remark-review-offer-speed-fast", BadgeTone::Success),
        "speed-slow" => ("remark-review-offer-speed-slow", BadgeTone::Warning),
        _ => return None,
    };
    Some(OfferPill::new(t(key), tone))
}

fn offer_choice(choice: &SetUpChoice) -> SetupChoice {
    SetupChoice {
        title: choice.label.clone(),
        terms: choice.terms.clone(),
        pills: [choice.fit.as_deref(), choice.speed.as_deref()]
            .into_iter()
            .filter_map(offer_pill)
            .collect(),
    }
}

/// The hub's choices as the toolkit's shared offer: the recommendation,
/// then the alternatives behind "Show more".
pub(crate) fn review_offer(choices: &[SetUpChoice]) -> Option<SetupOffer> {
    let (recommended, alternatives) = choices.split_first()?;
    Some(SetupOffer {
        heading: t("remark-review-offer-heading"),
        question: t_args(
            "remark-review-offer-question",
            &[("offer", text(recommended.label.clone()))],
        ),
        description: t("remark-review-offer-description"),
        recommended: offer_choice(recommended),
        set_up: t("remark-review-offer-set-up"),
        show_more: t_args(
            "remark-review-offer-more",
            &[(
                "count",
                number(f64::from(
                    u32::try_from(alternatives.len()).unwrap_or(u32::MAX),
                )),
            )],
        ),
        show_fewer: t("remark-review-offer-fewer"),
        dismiss: Some(t("remark-review-offer-dismiss")),
        alternatives_title: t("remark-review-offer-alternatives"),
        instead: t("remark-review-offer-instead"),
        alternatives: alternatives.iter().map(offer_choice).collect(),
    })
}

fn f32_of(value: f64) -> f32 {
    ui::num::f32_of_f64(value)
}

fn f64_of(value: f32) -> f64 {
    f64::from(value)
}

/// Everything on the remark surface that can be pressed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Close,
    PickMore,
    RemovePick(usize),
    Text,
    DestinationList,
    Destination(String),
    RecordNow,
    UseRecent,
    KeepRecent,
    Stop,
    RemoveRecording,
    Review,
    Frames(bool),
    Discard,
    Send,
    PickDone,
    PickCancel,
    ReviewBack,
    ReviewStep(u32),
    ReviewCut {
        step: u32,
        cut: bool,
    },
    ReviewWord {
        step: u32,
        node: String,
        at: usize,
    },
    ReviewHideText {
        step: u32,
        node: String,
    },
    ReviewSetUp,
    /// A press on the offer to set the review model up.
    ReviewOffer(OfferTarget),
    ReviewSend,
    PillStop,
    /// Inside a surface, on nothing: absorbs the press.
    Absorb,
}

impl Target {
    /// The shell action this target is, and its arguments. `None` for a
    /// target that changes only this view (the destination list, focus).
    pub fn action(&self) -> Option<(&'static str, Value)> {
        Some(match self {
            Self::Close | Self::Discard => ("remark.discard", json!({})),
            Self::PickMore => ("remark.pick.start", json!({})),
            Self::RemovePick(index) => ("remark.pick.remove", json!({ "index": index })),
            Self::Destination(id) => ("remark.destination.set", json!({ "id": id })),
            Self::RecordNow => ("remark.record.start", json!({})),
            Self::UseRecent => ("remark.record.recent", json!({})),
            Self::KeepRecent => ("feedback.recent-buffer.set", json!({ "enabled": true })),
            Self::Stop | Self::PillStop => ("remark.record.stop", json!({})),
            Self::RemoveRecording => ("remark.record.remove", json!({})),
            Self::Review => ("remark.review.open", json!({})),
            Self::Frames(enabled) => ("remark.frames.set", json!({ "enabled": enabled })),
            Self::Send | Self::ReviewSend => ("remark.send", json!({})),
            Self::PickDone => ("remark.pick.done", json!({})),
            Self::PickCancel => ("remark.pick.cancel", json!({})),
            Self::ReviewBack => ("remark.review.close", json!({})),
            Self::ReviewStep(step) => ("remark.review.select", json!({ "step": step })),
            Self::ReviewCut { step, cut } => {
                ("remark.review.cut", json!({ "step": step, "cut": cut }))
            }
            Self::ReviewWord { step, node, at } => (
                "remark.review.redact-word",
                json!({ "step": step, "node": node, "at": at }),
            ),
            Self::ReviewSetUp => ("remark.review.model.set-up", json!({})),
            Self::ReviewOffer(OfferTarget::SetUp) => {
                ("remark.review.model.confirm", json!({ "choice": 0 }))
            }
            Self::ReviewOffer(OfferTarget::Alternative(index)) => (
                "remark.review.model.confirm",
                json!({ "choice": index.saturating_add(1) }),
            ),
            Self::ReviewOffer(OfferTarget::ToggleMore) => ("remark.review.model.more", json!({})),
            Self::ReviewOffer(OfferTarget::Dismiss) => ("remark.review.model.dismiss", json!({})),
            Self::ReviewHideText { step, node } => {
                ("remark.review.hide", json!({ "step": step, "node": node }))
            }
            Self::Text | Self::DestinationList | Self::Absorb => return None,
        })
    }

    /// The target's name for `tilo-control click`.
    pub fn test_name(&self) -> Option<String> {
        Some(match self {
            Self::Close => "remark_close".into(),
            Self::PickMore => "remark_pick_more".into(),
            Self::RemovePick(index) => format!("remark_remove_pick[{index}]"),
            Self::Text => "remark_text".into(),
            Self::DestinationList => "remark_destination".into(),
            Self::Destination(id) => format!("remark_destination[{id}]"),
            Self::RecordNow => "remark_record_now".into(),
            Self::UseRecent => "remark_use_recent".into(),
            Self::KeepRecent => "remark_keep_recent".into(),
            Self::Stop => "remark_stop".into(),
            Self::RemoveRecording => "remark_remove_recording".into(),
            Self::Review => "remark_review".into(),
            Self::Frames(_) => "remark_frames".into(),
            Self::Discard => "remark_discard".into(),
            Self::Send => "remark_send".into(),
            Self::PickDone => "remark_pick_done".into(),
            Self::PickCancel => "remark_pick_cancel".into(),
            Self::ReviewBack => "remark_review_back".into(),
            Self::ReviewStep(step) => format!("remark_review_step[{step}]"),
            Self::ReviewCut { step, .. } => format!("remark_review_cut[{step}]"),
            Self::ReviewWord { step, node, at } => {
                format!("remark_review_word[{step}:{node}:{at}]")
            }
            Self::ReviewSetUp => "remark_review_set_up".into(),
            Self::ReviewOffer(OfferTarget::SetUp) => "remark_review_offer_set_up".into(),
            Self::ReviewOffer(OfferTarget::ToggleMore) => "remark_review_offer_more".into(),
            Self::ReviewOffer(OfferTarget::Dismiss) => "remark_review_offer_dismiss".into(),
            Self::ReviewOffer(OfferTarget::Alternative(index)) => {
                format!("remark_review_offer_instead[{index}]")
            }
            Self::ReviewHideText { step, node } => format!("remark_review_hide[{step}:{node}]"),
            Self::ReviewSend => "remark_review_send".into(),
            Self::PillStop => "remark_pill_stop".into(),
            Self::Absorb => return None,
        })
    }

    /// The accessible name of the target, for the semantic tree.
    fn name(&self, composer: &Composer) -> String {
        match self {
            Self::Close | Self::PickCancel => t("remark-close"),
            Self::PickMore => t("remark-pick-more-description"),
            Self::RemovePick(index) => t_args(
                "remark-remove-pick",
                &[(
                    "label",
                    text(
                        composer
                            .picks
                            .get(*index)
                            .map_or_else(String::new, |pick| pick.label.clone()),
                    ),
                )],
            ),
            Self::Text => t("remark-text-label"),
            Self::DestinationList => t("remark-destination"),
            Self::Destination(id) => id.clone(),
            Self::RecordNow => t("remark-record-now"),
            Self::UseRecent => t("remark-use-recent"),
            Self::KeepRecent => t("remark-recent-off"),
            Self::Stop | Self::PillStop => t("remark-pill-stop"),
            Self::RemoveRecording => t("remark-remove-recording"),
            Self::Review => t("remark-review"),
            Self::Frames(_) => t("remark-frames"),
            Self::Discard => t("remark-discard"),
            Self::Send | Self::ReviewSend => t("remark-send"),
            Self::PickDone => t("remark-pick-done"),
            Self::ReviewBack => t("remark-review-back"),
            Self::ReviewStep(step) => t_args(
                "remark-review-step",
                &[("n", number(f64::from(step.saturating_add(1))))],
            ),
            Self::ReviewCut { cut, .. } => {
                if *cut {
                    t("remark-review-cut")
                } else {
                    t("remark-review-keep")
                }
            }
            Self::ReviewWord { .. } | Self::ReviewHideText { .. } => t("remark-review-subtitle"),
            Self::ReviewSetUp => t("remark-review-model-floor"),
            Self::ReviewOffer(OfferTarget::SetUp) => t("remark-review-offer-set-up"),
            Self::ReviewOffer(OfferTarget::ToggleMore) => t("remark-review-offer-more-label"),
            Self::ReviewOffer(OfferTarget::Dismiss) => t("remark-review-offer-dismiss"),
            Self::ReviewOffer(OfferTarget::Alternative(_)) => t("remark-review-offer-instead"),
            Self::Absorb => String::new(),
        }
    }
}

/// The view's own state: geometry of what was painted, what the pointer is
/// over, and the one shared text field.
pub struct RemarkView {
    targets: Vec<(Rect, Target)>,
    pointer: Option<Point>,
    /// Keyboard focus among the targets, by index into the focus order.
    focus: Option<usize>,
    pub text: ui::widget::TextInput,
    text_rect: Rect,
    destination_open: bool,
    panel: Rect,
    opened: Option<Instant>,
    /// Where pick mode's hint bar is, so the host knows a press there is
    /// the bar's rather than a pick.
    pick_bar: Rect,
    /// Recorded frames as images, by step, so each decodes once.
    frame_images: Vec<(u32, ui::widget::Image)>,
}

impl Default for RemarkView {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            pointer: None,
            focus: None,
            text: ui::widget::TextInput::local()
                .with_id("remark-text")
                .multiline(true)
                .with_accessible_name(t("remark-text-label")),
            text_rect: Rect::ZERO,
            destination_open: false,
            panel: Rect::ZERO,
            opened: None,
            pick_bar: Rect::ZERO,
            frame_images: Vec::new(),
        }
    }
}

/// How far a panel has come in on its entrance: opacity 0→1 over `base`.
fn entrance(opened: Option<Instant>, theme: &Theme) -> f64 {
    if theme.reduce_motion {
        return 1.0;
    }
    let duration = theme
        .tokens
        .motion
        .durations
        .base()
        .as_secs_f64()
        .max(0.001);
    opened.map_or(1.0, |at| {
        (at.elapsed().as_secs_f64() / duration).clamp(0.0, 1.0)
    })
}

fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

/// A role's glyph, so a chip or a tag says what kind of thing was picked.
fn role_icon(role: &str) -> Icon {
    match role {
        "button" | "link" | "menu_item" | "toggle_button" => Icon::MousePointer2,
        "text_input" | "text" | "paragraph" | "static_text" | "label" | "heading" => Icon::FileText,
        "image" | "canvas" => Icon::Image,
        "list" | "list_item" | "tree" | "tree_item" | "table" | "row" | "cell" => Icon::List,
        "application" | "window" | "document" => Icon::LayoutGrid,
        "dialog" | "alert" | "alert_dialog" | "banner" | "status" => Icon::Info,
        _ => Icon::Crosshair,
    }
}

/// A role as a person reads it: `text_input` → `Text input`.
/// A role as a person reads it: the localized name of a known role, else
/// the app's own spelling made readable.
fn role_label(role: &str) -> String {
    match role {
        "button" => t("remark-role-button"),
        "toggle_button" => t("remark-role-toggle-button"),
        "text_input" => t("remark-role-text-input"),
        "multiline_text_input" => t("remark-role-multiline-text-input"),
        "search_box" => t("remark-role-search-box"),
        "combo_box" => t("remark-role-combo-box"),
        "link" => t("remark-role-link"),
        "checkbox" => t("remark-role-checkbox"),
        "switch" => t("remark-role-switch"),
        "radio_button" => t("remark-role-radio-button"),
        "tab" => t("remark-role-tab"),
        "menu_item" => t("remark-role-menu-item"),
        "heading" => t("remark-role-heading"),
        "image" => t("remark-role-image"),
        "list" => t("remark-role-list"),
        "list_item" => t("remark-role-list-item"),
        "alert" => t("remark-role-alert"),
        "status" => t("remark-role-status"),
        "window" => t("remark-role-window"),
        "text" => t("remark-role-text"),
        "label" => t("remark-role-label"),
        "slider" => t("remark-role-slider"),
        "toolbar" => t("remark-role-toolbar"),
        "group" => t("remark-role-group"),
        "dialog" => t("remark-role-dialog"),
        "application" => t("remark-role-application"),
        _ => readable_role(role),
    }
}

fn readable_role(role: &str) -> String {
    let spaced = role.replace('_', " ");
    let mut chars = spaced.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn destination_icon(option: &DestinationOption) -> Icon {
    match &option.kind {
        DestinationKind::Team { .. } => Icon::Lock,
        DestinationKind::Community { .. } => Icon::Globe,
        DestinationKind::Room { .. } => Icon::Users,
        DestinationKind::Person { is_agent: true, .. } => Icon::Bot,
        DestinationKind::Person { .. } => Icon::User,
    }
}

fn destination_detail(option: &DestinationOption) -> String {
    match &option.kind {
        DestinationKind::Team { .. } => t("remark-dest-team-detail"),
        // Everyone is a member by default; only a person who left is told
        // that sending joins again.
        DestinationKind::Community { left: true, .. } => t("remark-dest-community-join"),
        DestinationKind::Community { .. } => t("remark-dest-community-detail"),
        DestinationKind::Room { .. } => t("remark-dest-room-detail"),
        DestinationKind::Person { is_agent: true, .. } => t_args(
            "remark-dest-agent-detail",
            &[("name", text(option.label.clone()))],
        ),
        DestinationKind::Person { .. } => t_args(
            "remark-dest-person-detail",
            &[("name", text(option.label.clone()))],
        ),
    }
}

/// `m:ss` for a duration in milliseconds.
pub fn clock(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// How a line of text is set: its size, colour and weight.
#[derive(Clone, Copy)]
struct Ink {
    size: f32,
    color: Color,
    weight: TextWeight,
}

/// Draw one line of text with its top at `y`, returning its height.
fn line(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    value: &str,
    ink: Ink,
    at: Point,
    max_width: f64,
) -> f64 {
    let Ink {
        size,
        color,
        weight,
    } = ink;
    let fitted = tr.elide_to_width_weighted(value, size, f32_of(max_width.max(0.0)), weight);
    let (_, h) = tr.measure_weighted(&fitted, size, None, weight);
    tr.draw_text_weighted(
        scene,
        &fitted,
        size,
        color,
        transform.pre_translate(at.to_vec2()),
        None,
        weight,
    );
    f64::from(h)
}

/// Wrapped text with its top at `y`, returning its height.
fn paragraph(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    value: &str,
    size: f32,
    color: Color,
    at: Point,
    width: f64,
) -> f64 {
    let (_, h) = tr.measure(value, size, Some(f32_of(width)));
    tr.draw_text(
        scene,
        value,
        size,
        color,
        transform.pre_translate(at.to_vec2()),
        Some(f32_of(width)),
    );
    f64::from(h)
}

fn section_header(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    label: &str,
    at: Point,
    width: f64,
) -> f64 {
    let scale = &theme.tokens.typography.scale;
    line(
        scene,
        transform,
        tr,
        &label.to_uppercase(),
        Ink {
            size: scale.xs,
            color: theme.tokens.colors.text_tertiary,
            weight: TextWeight::Medium,
        },
        at,
        width,
    )
}

/// A picked node's outline on screen.
fn node_outline(rect: Rect, theme: &Theme) -> RoundedRect {
    RoundedRect::from_rect(rect.inflate(2.0, 2.0), f64::from(theme.tokens.radii.sm))
}

impl RemarkView {
    /// The composer (or review) opened this frame: start its entrance.
    pub fn note_opened(&mut self) {
        self.opened = Some(Instant::now());
        self.focus = None;
        self.destination_open = false;
    }

    pub fn panel_rect(&self) -> Rect {
        self.panel
    }

    pub fn pick_bar_rect(&self) -> Rect {
        self.pick_bar
    }

    pub fn text_rect(&self) -> Rect {
        self.text_rect
    }

    pub fn set_pointer(&mut self, point: Option<Point>) {
        self.pointer = point;
    }

    fn hovered(&self, rect: Rect) -> bool {
        self.pointer.is_some_and(|point| rect.contains(point))
    }

    fn focused(&self, target: &Target) -> bool {
        self.focus
            .and_then(|index| self.focus_order().get(index).cloned())
            .is_some_and(|focused| &focused == target)
    }

    /// The target under `point`, topmost first.
    pub fn target_at(&self, point: Point) -> Option<Target> {
        self.targets
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, target)| target.clone())
    }

    /// Named rectangles, for `tilo-control list-targets` and `click NAME`.
    pub fn test_rects(&self) -> Vec<(String, Rect)> {
        self.targets
            .iter()
            .filter_map(|(rect, target)| Some((target.test_name()?, *rect)))
            .collect()
    }

    /// Targets that take keyboard focus, in reading order.
    fn focus_order(&self) -> Vec<Target> {
        self.targets
            .iter()
            .filter(|(_, target)| {
                !matches!(
                    target,
                    Target::Absorb
                        | Target::ReviewWord { .. }
                        | Target::ReviewHideText { .. }
                        | Target::Text
                )
            })
            .map(|(_, target)| target.clone())
            .collect()
    }

    /// Tab / Shift+Tab.
    pub fn move_focus(&mut self, backwards: bool) {
        let count = self.focus_order().len();
        if count == 0 {
            self.focus = None;
            return;
        }
        self.focus = Some(match (self.focus, backwards) {
            (None, false) => 0,
            (None, true) => count.saturating_sub(1),
            (Some(at), false) => at.saturating_add(1).checked_rem(count).unwrap_or(0),
            (Some(at), true) => at.checked_sub(1).unwrap_or_else(|| count.saturating_sub(1)),
        });
    }

    pub fn focused_target(&self) -> Option<Target> {
        self.focus
            .and_then(|index| self.focus_order().get(index).cloned())
    }

    pub fn clear_focus(&mut self) {
        self.focus = None;
    }

    /// Open or close the destination list: a view-only change.
    pub fn toggle_destinations(&mut self) {
        self.destination_open = !self.destination_open;
    }

    pub fn close_destinations(&mut self) {
        self.destination_open = false;
    }

    fn push(&mut self, rect: Rect, target: Target) {
        self.targets.push((rect, target));
    }

    // ------------------------------------------------------------------
    // Pick mode
    // ------------------------------------------------------------------

    /// The pick overlay: a gentle dim with the focused node lit, every
    /// picked node outlined and numbered, the region being dragged, a tag
    /// naming what is under the pointer, and the hint bar.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered paint pass over the pick overlay's layers"
    )]
    pub fn draw_pick(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        pick: &PickState,
    ) {
        self.targets.clear();
        let colors = &theme.tokens.colors;
        let spacing = &theme.tokens.spacing;
        let accent = colors.accent[5];
        let focus = pick.focus.as_ref().and_then(|key| pick.scene.resolve(key));
        let region = pick.region();

        // The dim, with the focused node cut out of it: the node is what the
        // eye should land on, lit by the desktop's own pixels.
        // Everything picked is lit the same way, so the selection reads as
        // one set of holes in the dim.
        let mut dim = BezPath::new();
        dim.extend(viewport.path_elements(0.1));
        let mut lit: Vec<Rect> = pick
            .selection
            .keys()
            .iter()
            .filter_map(|key| pick.scene.resolve(key))
            .map(|node| node_outline(node.screen_rect(), theme).rect())
            .collect();
        if let Some(focus) = focus.as_ref().filter(|_| region.is_none()) {
            lit.push(node_outline(focus.screen_rect(), theme).rect());
        }
        // Holes must not overlap, or even-odd fills their overlap again.
        let mut holes: Vec<Rect> = Vec::new();
        for rect in lit {
            if !holes.iter().any(|hole| hole.intersect(rect).area() > 0.0) {
                holes.push(rect);
            }
        }
        for hole in holes {
            dim.extend(
                RoundedRect::from_rect(hole, f64::from(theme.tokens.radii.sm)).path_elements(0.1),
            );
        }
        scene.fill(
            Fill::EvenOdd,
            transform,
            colors.surfaces.scrim.with_alpha_factor(0.6),
            None,
            &dim,
        );

        // Where each app can be picked, so a surface with no tree yet reads
        // as "not describable", not as a dead click.
        for surface in &pick.scene.surfaces {
            scene.stroke(
                &Stroke::new(1.0).with_dashes(0.0, [4.0, 4.0]),
                transform,
                accent.with_alpha_factor(0.35),
                None,
                &RoundedRect::from_rect(
                    surface.screen.inset(-1.0),
                    f64::from(theme.tokens.radii.md),
                ),
            );
        }

        // What is picked: tinted, outlined and numbered in pick order.
        for (index, key) in pick.selection.keys().iter().enumerate() {
            let Some(node) = pick.scene.resolve(key) else {
                continue;
            };
            let rect = node.screen_rect();
            let shape = node_outline(rect, theme);
            scene.fill(
                Fill::NonZero,
                transform,
                accent.with_alpha_factor(0.18),
                None,
                &shape,
            );
            scene.stroke(&Stroke::new(2.0), transform, accent, None, &shape);
            let badge = Circle::new(Point::new(rect.x0 + 1.0, rect.y0 + 1.0), 9.0);
            scene.fill(Fill::NonZero, transform, accent, None, &badge);
            let label = index.saturating_add(1).to_string();
            let size = theme.tokens.typography.scale.xs;
            let (w, h) = tr.measure_weighted(&label, size, None, TextWeight::Semibold);
            tr.draw_text_weighted(
                scene,
                &label,
                size,
                colors.on_accent(4.5),
                transform.pre_translate(
                    (
                        badge.center.x - f64::from(w) / 2.0,
                        badge.center.y - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
                TextWeight::Semibold,
            );
        }

        // The region being swept, and what it would pick.
        if let Some(region) = region {
            for node in pick.scene.nodes_in(region) {
                scene.stroke(
                    &Stroke::new(1.5),
                    transform,
                    accent,
                    None,
                    &node_outline(node.screen_rect(), theme),
                );
            }
            let shape = RoundedRect::from_rect(region, f64::from(theme.tokens.radii.sm));
            scene.fill(
                Fill::NonZero,
                transform,
                accent.with_alpha_factor(0.10),
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.5).with_dashes(0.0, [6.0, 4.0]),
                transform,
                accent,
                None,
                &shape,
            );
        }

        // The focused node: its enclosing node dashed for context, the node
        // itself ringed, and a tag that names it.
        if let Some(focus) = focus.filter(|_| region.is_none()) {
            let parent = focus
                .surface
                .nodes
                .iter()
                .find(|node| node.children.contains(&focus.node.id));
            if let Some(parent) = parent {
                scene.stroke(
                    &Stroke::new(1.0).with_dashes(0.0, [3.0, 3.0]),
                    transform,
                    accent.with_alpha_factor(0.6),
                    None,
                    &node_outline(focus.surface.to_screen(parent.bounds), theme),
                );
            }
            let rect = focus.screen_rect();
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                colors.border_focus,
                None,
                &node_outline(rect, theme),
            );
            Self::draw_tag(scene, transform, tr, theme, viewport, rect, &focus);
        }

        // The hint bar: what the gestures are, how many are picked, done.
        let scale = &theme.tokens.typography.scale;
        let hint = if pick.scene.surfaces.is_empty() {
            t("remark-pick-nothing")
        } else {
            t("remark-pick-hint")
        };
        let count = pick.selection.keys().len();
        let count_label = t_args(
            "remark-pick-count",
            &[("count", number(ui::num::f64_of_usize(count)))],
        );
        let pad = f64_of(spacing.s3);
        let button_h = f64_of(theme.tokens.metrics.min_pointer_target);
        let done_label = t("remark-pick-done");
        let (done_w, _) = tr.measure_weighted(&done_label, scale.sm, None, TextWeight::Medium);
        let done_w = f64::from(done_w) + pad * 2.0;
        let (count_w, _) = tr.measure_weighted(&count_label, scale.sm, None, TextWeight::Semibold);
        let bar_w = (viewport.width() - f64_of(spacing.s8) * 2.0).clamp(280.0, 820.0);
        let bar_h = button_h + pad * 2.0;
        let bar = Rect::from_origin_size(
            (
                viewport.center().x - bar_w / 2.0,
                viewport.y1 - bar_h - f64_of(spacing.s6),
            ),
            (bar_w, bar_h),
        );
        self.pick_bar = bar;
        ui::surface::shadow(
            scene,
            transform,
            bar,
            f64_of(theme.tokens.radii.full),
            &theme.tokens.elevation.e4,
        );
        let pill = RoundedRect::from_rect(bar, f64_of(theme.tokens.radii.full));
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &pill,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &pill,
        );
        self.push(bar, Target::Absorb);
        let cy = bar.center().y;
        let icon_size = f64_of(theme.tokens.metrics.icon_md);
        let icon_rect = Rect::from_center_size(
            (bar.x0 + pad * 1.5 + icon_size / 2.0, cy),
            (icon_size, icon_size),
        );
        draw_icon(
            scene,
            transform,
            icon_rect,
            Icon::Crosshair,
            accent,
            LUCIDE_DEFAULT_STROKE_PX,
        );
        let close_rect =
            Rect::from_center_size((bar.x1 - pad - button_h / 2.0, cy), (button_h, button_h));
        let done_rect = Rect::new(
            close_rect.x0 - f64_of(spacing.s2) - done_w,
            cy - button_h / 2.0,
            close_rect.x0 - f64_of(spacing.s2),
            cy + button_h / 2.0,
        );
        let count_x = done_rect.x0 - f64_of(spacing.s3) - f64::from(count_w);
        let text_x = icon_rect.x1 + f64_of(spacing.s2);
        let (_, hint_h) = tr.measure(&hint, scale.sm, None);
        line(
            scene,
            transform,
            tr,
            &hint,
            Ink {
                size: scale.sm,
                color: colors.text_dim,
                weight: TextWeight::Regular,
            },
            Point::new(text_x, cy - f64::from(hint_h) / 2.0),
            count_x - text_x - f64_of(spacing.s3),
        );
        if count > 0 {
            line(
                scene,
                transform,
                tr,
                &count_label,
                Ink {
                    size: scale.sm,
                    color: colors.text_primary,
                    weight: TextWeight::Semibold,
                },
                Point::new(count_x, cy - f64::from(hint_h) / 2.0),
                f64::from(count_w) + 1.0,
            );
        }
        let done_face = ButtonFace::new(done_rect, &done_label)
            .primary()
            .radius(f64_of(theme.tokens.radii.full))
            .hovered(self.hovered(done_rect))
            .focused(self.focused(&Target::PickDone));
        Button::paint(scene, transform, tr, theme, &done_face);
        self.push(done_rect, Target::PickDone);
        let close_label = t("remark-close");
        let close_face = ButtonFace::new(close_rect, "")
            .ghost()
            .icon(Icon::X)
            .accessible_name(&close_label)
            .radius(f64_of(theme.tokens.radii.full))
            .hovered(self.hovered(close_rect))
            .focused(self.focused(&Target::PickCancel));
        Button::paint(scene, transform, tr, theme, &close_face);
        self.push(close_rect, Target::PickCancel);
    }

    /// The tag beside the focused node: its role glyph, name, role and app,
    /// like an inspector's, placed above the node when it fits.
    fn draw_tag(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        node_rect: Rect,
        focus: &super::pick::Resolved<'_>,
    ) {
        let colors = &theme.tokens.colors;
        let spacing = &theme.tokens.spacing;
        let scale = &theme.tokens.typography.scale;
        let name = focus
            .node
            .label
            .clone()
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| role_label(&focus.node.role));
        let detail = format!(
            "{} · {}",
            role_label(&focus.node.role),
            focus.surface.app_label
        );
        let name = tr.elide_to_width_weighted(&name, scale.sm, 260.0, TextWeight::Semibold);
        let (name_w, name_h) = tr.measure_weighted(&name, scale.sm, None, TextWeight::Semibold);
        let (detail_w, _) = tr.measure(&detail, scale.xs, None);
        let personal = t("remark-pick-personal");
        let (personal_w, _) = tr.measure(&personal, scale.xs, None);
        let pad = f64_of(spacing.s2);
        let icon = f64_of(theme.tokens.metrics.icon_sm);
        let gap = f64_of(theme.tokens.metrics.icon_label_gap);
        let personal_block = if focus.node.personal {
            f64::from(personal_w) + pad * 2.0 + gap
        } else {
            0.0
        };
        let width =
            pad * 2.0 + icon + gap + f64::from(name_w) + gap + f64::from(detail_w) + personal_block;
        let height = f64::from(name_h).max(icon) + pad * 1.5;
        let tag = tag_rect(
            node_rect,
            focus.surface.screen.intersect(viewport),
            (width, height),
            f64_of(spacing.s2),
        );
        ui::surface::panel(scene, transform, tag, f64_of(theme.tokens.radii.lg), theme);
        let cy = tag.center().y;
        draw_icon(
            scene,
            transform,
            Rect::from_center_size((tag.x0 + pad + icon / 2.0, cy), (icon, icon)),
            role_icon(&focus.node.role),
            colors.accent[5],
            LUCIDE_DEFAULT_STROKE_PX,
        );
        let mut cursor = tag.x0 + pad + icon + gap;
        tr.draw_text_weighted(
            scene,
            &name,
            scale.sm,
            colors.text_primary,
            transform.pre_translate((cursor, cy - f64::from(name_h) / 2.0).into()),
            None,
            TextWeight::Semibold,
        );
        cursor += f64::from(name_w) + gap;
        let (_, detail_h) = tr.measure(&detail, scale.xs, None);
        tr.draw_text(
            scene,
            &detail,
            scale.xs,
            colors.text_dim,
            transform.pre_translate((cursor, cy - f64::from(detail_h) / 2.0).into()),
            None,
        );
        cursor += f64::from(detail_w) + gap;
        if focus.node.personal {
            let chip = Rect::new(
                cursor,
                cy - 9.0,
                cursor + f64::from(personal_w) + pad * 2.0,
                cy + 9.0,
            );
            Chip::draw_at(
                scene,
                transform,
                chip,
                &personal,
                ChipLead::None,
                ChipStyle::tinted(colors.semantic.warning),
                theme,
                tr,
            );
        }
    }

    // ------------------------------------------------------------------
    // Composer
    // ------------------------------------------------------------------

    /// Where the composer sits: beside what it is about, like a comment on
    /// it, else in the lower right of the desktop.
    fn composer_origin(
        viewport: Rect,
        anchor: Option<Rect>,
        size: (f64, f64),
        margin: f64,
    ) -> Point {
        let (w, h) = size;
        let fits_y = |y: f64| {
            y.clamp(
                viewport.y0 + margin,
                (viewport.y1 - h - margin).max(viewport.y0 + margin),
            )
        };
        if let Some(anchor) = anchor.filter(|rect| rect.area() > 0.0) {
            if anchor.x1 + margin + w <= viewport.x1 - margin {
                return Point::new(anchor.x1 + margin, fits_y(anchor.y0));
            }
            if anchor.x0 - margin - w >= viewport.x0 + margin {
                return Point::new(anchor.x0 - margin - w, fits_y(anchor.y0));
            }
            if anchor.y1 + margin + h <= viewport.y1 - margin {
                let x = anchor.x0.clamp(
                    viewport.x0 + margin,
                    (viewport.x1 - w - margin).max(viewport.x0),
                );
                return Point::new(x, anchor.y1 + margin);
            }
        }
        Point::new(
            (viewport.x1 - w - margin * 2.0).max(viewport.x0 + margin),
            fits_y(viewport.y1 - h - margin * 2.0),
        )
    }

    /// The composer panel.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered layout-and-paint pass over the composer's sections"
    )]
    pub fn draw_composer(
        &mut self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        presence: &ui::presence::PresenceCtx,
        viewport: Rect,
        composer: &Composer,
        options: &[DestinationOption],
        recent_buffer: bool,
        now_recording_ms: Option<u64>,
    ) {
        let (scene, transform, tr, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        self.targets.clear();
        let colors = &theme.tokens.colors;
        let spacing = &theme.tokens.spacing;
        let scale = &theme.tokens.typography.scale;
        let metrics = &theme.tokens.metrics;
        let pad = f64_of(spacing.s4);
        let gap = f64_of(spacing.s3);
        let target_h = f64_of(metrics.min_pointer_target);
        let row_h = f64_of(metrics.panel_row_height);
        let width = (viewport.width() - pad * 2.0).clamp(280.0, 440.0);
        let inner = width - pad * 2.0;
        let chip_h = f64_of(metrics.selector_height);

        // Measure the chip rows first: the panel's height follows them.
        let mut chips: Vec<(String, ChipLead, Target)> = composer
            .picks
            .iter()
            .enumerate()
            .map(|(index, pick)| {
                (
                    pick.label.clone(),
                    ChipLead::Glyph(role_icon(&pick.role)),
                    Target::RemovePick(index),
                )
            })
            .collect();
        if composer.picks.is_empty()
            && let Some(context) = &composer.context
        {
            chips.push((
                t_args(
                    "remark-whole-app",
                    &[("app", text(context.app_label.clone()))],
                ),
                ChipLead::Glyph(Icon::LayoutGrid),
                Target::Absorb,
            ));
        }
        chips.push((
            t("remark-pick-more"),
            ChipLead::Glyph(Icon::Crosshair),
            Target::PickMore,
        ));
        let chip_gap = f64_of(spacing.s2);
        let mut chip_rows = 1_usize;
        let mut x = 0.0_f64;
        let mut chip_layout: Vec<(f64, usize, f64)> = Vec::new();
        for (label, lead, _) in &chips {
            let natural = Chip::natural_width_at(label, chip_h, *lead, theme, tr);
            let w = natural.min(inner);
            if x > 0.0 && x + w > inner {
                chip_rows = chip_rows.saturating_add(1);
                x = 0.0;
            }
            chip_layout.push((x, chip_rows.saturating_sub(1), w));
            x += w + chip_gap;
        }
        let chip_block_h = ui::num::f64_of_usize(chip_rows) * chip_h
            + ui::num::f64_of_usize(chip_rows.saturating_sub(1)) * chip_gap;
        let text_h = 92.0;
        let selected = composer
            .destination
            .as_deref()
            .and_then(|id| options.iter().find(|option| option.id == id));
        let list_h = if self.destination_open {
            ui::num::f64_of_usize(options.len()) * row_h + f64_of(spacing.s1)
        } else {
            0.0
        };
        let flow_h = match &composer.flow {
            Flow::None => 60.0 * 2.0 + f64_of(spacing.s2) + row_h + f64_of(spacing.s2),
            Flow::Recording { .. } | Flow::Capturing => row_h,
            Flow::Attached { .. } => row_h + f64_of(spacing.s2) + row_h,
        };
        let header_h = target_h;
        let section_h = 14.0 + f64_of(spacing.s2);
        let note_h = 34.0;
        let footer_h = target_h;
        let height = pad
            + header_h
            + gap
            + chip_block_h
            + gap
            + text_h
            + gap
            + section_h
            + row_h
            + list_h
            + f64_of(spacing.s2)
            + note_h
            + gap
            + section_h
            + flow_h
            + gap
            + footer_h
            + pad;
        let anchor = composer.picks.first().map(|pick| pick.screen);
        let origin = Self::composer_origin(viewport, anchor, (width, height), f64_of(spacing.s4));
        let enter = ease_out(entrance(self.opened, theme));
        let panel =
            Rect::from_origin_size((origin.x, origin.y - 10.0 * (1.0 - enter)), (width, height));
        self.panel = panel;
        let radius = f64_of(theme.tokens.radii.xl);
        let fade = f32_of(enter);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            fade,
            transform,
            &RoundedRect::from_rect(panel.inflate(40.0, 40.0), radius),
        );
        ui::surface::shadow(scene, transform, panel, radius, &theme.tokens.elevation.e4);
        ui::surface::panel(scene, transform, panel, radius, theme);
        self.push(panel, Target::Absorb);

        let left = panel.x0 + pad;
        let mut y = panel.y0 + pad;

        // Header: what this is, and closing it.
        let title = match &composer.opening {
            Opening::Crash { .. } => t("remark-title-crash"),
            Opening::Gesture => t("remark-title"),
        };
        let icon = f64_of(metrics.icon_md);
        draw_icon(
            scene,
            transform,
            Rect::from_center_size((left + icon / 2.0, y + header_h / 2.0), (icon, icon)),
            Icon::MessageSquare,
            colors.text_primary,
            LUCIDE_DEFAULT_STROKE_PX,
        );
        let (_, title_h) = tr.measure_weighted(&title, scale.base, None, TextWeight::Semibold);
        line(
            scene,
            transform,
            tr,
            &title,
            Ink {
                size: scale.base,
                color: colors.text_primary,
                weight: TextWeight::Semibold,
            },
            Point::new(
                left + icon + f64_of(metrics.icon_label_gap),
                y + (header_h - f64::from(title_h)) / 2.0,
            ),
            inner - icon - target_h - gap,
        );
        let close = Rect::from_origin_size((panel.x1 - pad - target_h, y), (target_h, target_h));
        let close_label = t("remark-close");
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(close, "")
                .ghost()
                .icon(Icon::X)
                .accessible_name(&close_label)
                .radius(f64_of(theme.tokens.radii.lg))
                .hovered(self.hovered(close))
                .focused(self.focused(&Target::Close)),
        );
        self.push(close, Target::Close);
        y += header_h + gap;

        // What it is about: one chip per pick, plus picking more.
        for ((label, lead, target), (cx, row, w)) in chips.iter().zip(&chip_layout) {
            let rect = Rect::from_origin_size(
                (
                    left + cx,
                    y + ui::num::f64_of_usize(*row) * (chip_h + chip_gap),
                ),
                (*w, chip_h),
            );
            let style = match target {
                Target::PickMore => ChipStyle::ghost(),
                Target::RemovePick(_) if self.hovered(rect) || self.focused(target) => {
                    ChipStyle::tinted(colors.semantic.danger)
                }
                _ => ChipStyle::quiet(),
            };
            Chip::draw_at(scene, transform, rect, label, *lead, style, theme, tr);
            if !matches!(target, Target::Absorb) {
                self.push(rect, target.clone());
            }
        }
        y += chip_block_h + gap;

        // What it says.
        let text_rect = Rect::from_origin_size((left, y), (inner, text_h));
        self.text_rect = text_rect;
        let placeholder = match selected.map(|option| &option.kind) {
            Some(DestinationKind::Team { .. } | DestinationKind::Community { .. }) => {
                t("remark-placeholder-feedback")
            }
            _ => t("remark-placeholder"),
        };
        self.text.set_placeholder(&placeholder);
        if !ui::widget::Widget::is_focused(&self.text) && self.text.value() != composer.text {
            self.text.set_text(&composer.text);
        }
        self.text.apply_layout_external(text_rect);
        ui::widget::Widget::draw(&mut self.text, scene, transform, tr, theme, presence);
        self.push(text_rect, Target::Text);
        y += text_h + gap;

        // Where it goes.
        y += section_header(
            scene,
            transform,
            tr,
            theme,
            &t("remark-destination"),
            Point::new(left, y),
            inner,
        ) + f64_of(spacing.s2);
        let selector = Rect::from_origin_size((left, y), (inner, row_h));
        let raised = colors.surfaces.raised;
        ui::surface::card(
            scene,
            transform,
            selector,
            f64_of(theme.tokens.radii.lg),
            raised,
        );
        if self.hovered(selector) || self.focused(&Target::DestinationList) {
            let tier = selection_tier_on(theme, SelectionState::new(false, true, false), raised);
            scene.fill(
                Fill::NonZero,
                transform,
                tier.fill,
                None,
                &RoundedRect::from_rect(selector, f64_of(theme.tokens.radii.lg)),
            );
        }
        if let Some(option) = selected {
            Self::draw_destination_row(scene, transform, tr, theme, selector, option, true);
        }
        draw_icon(
            scene,
            transform,
            Rect::from_center_size((selector.x1 - gap - 7.0, selector.center().y), (14.0, 14.0)),
            if self.destination_open {
                Icon::ChevronUp
            } else {
                Icon::ChevronDown
            },
            colors.text_dim,
            LUCIDE_DEFAULT_STROKE_PX,
        );
        self.push(selector, Target::DestinationList);
        y += row_h;
        if self.destination_open {
            y += f64_of(spacing.s1);
            for option in options {
                let row = Rect::from_origin_size((left, y), (inner, row_h));
                let target = Target::Destination(option.id.clone());
                let active = composer.destination.as_deref() == Some(option.id.as_str());
                let tier = selection_tier_on(
                    theme,
                    SelectionState::new(active, self.hovered(row) || self.focused(&target), false),
                    ui::chrome::shell::panel_ground(theme),
                );
                let shape = RoundedRect::from_rect(row, f64_of(theme.tokens.radii.lg));
                scene.fill(Fill::NonZero, transform, tier.fill, None, &shape);
                if let Some(border) = tier.border {
                    scene.stroke(&Stroke::new(1.0), transform, border, None, &shape);
                }
                Self::draw_destination_row(scene, transform, tr, theme, row, option, false);
                self.push(row, target);
                y += row_h;
            }
        }
        y += f64_of(spacing.s2);
        // Honest about what sending means: it cannot be taken back from
        // someone who already has it.
        paragraph(
            scene,
            transform,
            tr,
            &t("remark-retraction"),
            scale.xs,
            colors.text_dim,
            Point::new(left, y),
            inner,
        );
        y += note_h + gap;

        // What happened.
        y += section_header(
            scene,
            transform,
            tr,
            theme,
            &t("remark-show-what-happened"),
            Point::new(left, y),
            inner,
        ) + f64_of(spacing.s2);
        match &composer.flow {
            Flow::None => {
                let record = Rect::from_origin_size((left, y), (inner, 60.0));
                let card = ChoiceCardFace::new(
                    record,
                    Icon::Camera,
                    t("remark-record-now"),
                    t("remark-record-now-detail"),
                )
                .hovered(self.hovered(record))
                .focused(self.focused(&Target::RecordNow));
                card.paint(scene, transform, tr, theme);
                self.push(record, Target::RecordNow);
                y += 60.0 + f64_of(spacing.s2);
                let recent = Rect::from_origin_size((left, y), (inner, 60.0));
                let (title, detail, target) = if recent_buffer {
                    (
                        t("remark-use-recent"),
                        t("remark-use-recent-detail"),
                        Target::UseRecent,
                    )
                } else {
                    (
                        t("remark-recent-off"),
                        t("remark-recent-off-detail"),
                        Target::KeepRecent,
                    )
                };
                let card = ChoiceCardFace::new(recent, Icon::History, title, detail)
                    .hovered(self.hovered(recent))
                    .focused(self.focused(&target));
                card.paint(scene, transform, tr, theme);
                self.push(recent, target);
                y += 60.0 + f64_of(spacing.s2);
                let frames_row = Rect::from_origin_size((left, y), (inner, row_h));
                let toggle = Rect::from_center_size(
                    (frames_row.x1 - 22.0, frames_row.center().y),
                    (36.0, 20.0),
                );
                let target = Target::Frames(!composer.include_frames);
                Toggle::paint(
                    scene,
                    transform,
                    theme,
                    &ToggleFace::new(toggle, composer.include_frames)
                        .hovered(self.hovered(frames_row))
                        .focused(self.focused(&target)),
                );
                let (_, lh) =
                    tr.measure_weighted(&t("remark-frames"), scale.sm, None, TextWeight::Medium);
                line(
                    scene,
                    transform,
                    tr,
                    &t("remark-frames"),
                    Ink {
                        size: scale.sm,
                        color: colors.text_primary,
                        weight: TextWeight::Medium,
                    },
                    Point::new(left, frames_row.y0 + 4.0),
                    inner - 56.0,
                );
                paragraph(
                    scene,
                    transform,
                    tr,
                    &t("remark-frames-detail"),
                    scale.xs,
                    colors.text_dim,
                    Point::new(left, frames_row.y0 + 6.0 + f64::from(lh)),
                    inner - 56.0,
                );
                self.push(frames_row, target);
            }
            Flow::Recording { .. } | Flow::Capturing => {
                let row = Rect::from_origin_size((left, y), (inner, row_h));
                ui::surface::card(scene, transform, row, f64_of(theme.tokens.radii.lg), raised);
                let danger = colors.semantic.danger;
                let pulse = if theme.reduce_motion {
                    1.0
                } else {
                    let phase = Instant::now().elapsed().as_secs_f64();
                    0.6 + 0.4 * (phase * std::f64::consts::TAU / 1.6).cos().abs()
                };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    danger.with_alpha_factor(f32_of(pulse)),
                    None,
                    &Circle::new((row.x0 + gap + 5.0, row.center().y), 5.0),
                );
                let label = match (&composer.flow, now_recording_ms) {
                    (Flow::Capturing, _) => t("remark-capturing"),
                    (_, Some(ms)) => format!("{} · {}", t("remark-recording"), clock(ms)),
                    _ => t("remark-recording"),
                };
                let (_, lh) = tr.measure_weighted(&label, scale.sm, None, TextWeight::Medium);
                line(
                    scene,
                    transform,
                    tr,
                    &label,
                    Ink {
                        size: scale.sm,
                        color: colors.text_primary,
                        weight: TextWeight::Medium,
                    },
                    Point::new(row.x0 + gap + 16.0, row.center().y - f64::from(lh) / 2.0),
                    inner - 140.0,
                );
                if matches!(composer.flow, Flow::Recording { .. }) {
                    let stop_label = t("remark-stop");
                    let stop = Rect::from_center_size(
                        (row.x1 - gap - 40.0, row.center().y),
                        (80.0, target_h),
                    );
                    Button::paint(
                        scene,
                        transform,
                        tr,
                        theme,
                        &ButtonFace::new(stop, &stop_label)
                            .danger_outline()
                            .icon(Icon::Square)
                            .radius(f64_of(theme.tokens.radii.lg))
                            .hovered(self.hovered(stop))
                            .focused(self.focused(&Target::Stop)),
                    );
                    self.push(stop, Target::Stop);
                }
            }
            Flow::Attached { summary, .. } => {
                let row = Rect::from_origin_size((left, y), (inner, row_h));
                ui::surface::card(scene, transform, row, f64_of(theme.tokens.radii.lg), raised);
                draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size((row.x0 + gap + 9.0, row.center().y), (18.0, 18.0)),
                    Icon::ListVideo,
                    colors.text_primary,
                    LUCIDE_DEFAULT_STROKE_PX,
                );
                let label = t_args(
                    "remark-attached",
                    &[
                        ("steps", number(f64::from(summary.steps))),
                        ("duration", text(clock(summary.duration_ms))),
                    ],
                );
                let (_, lh) = tr.measure_weighted(&label, scale.sm, None, TextWeight::Medium);
                line(
                    scene,
                    transform,
                    tr,
                    &label,
                    Ink {
                        size: scale.sm,
                        color: colors.text_primary,
                        weight: TextWeight::Medium,
                    },
                    Point::new(row.x0 + gap + 26.0, row.center().y - f64::from(lh) / 2.0),
                    inner - 90.0,
                );
                let remove = Rect::from_center_size(
                    (row.x1 - gap - target_h / 2.0 + 4.0, row.center().y),
                    (target_h, target_h),
                );
                let remove_label = t("remark-remove-recording");
                Button::paint(
                    scene,
                    transform,
                    tr,
                    theme,
                    &ButtonFace::new(remove, "")
                        .ghost()
                        .icon(Icon::X)
                        .accessible_name(&remove_label)
                        .radius(f64_of(theme.tokens.radii.lg))
                        .hovered(self.hovered(remove))
                        .focused(self.focused(&Target::RemoveRecording)),
                );
                self.push(remove, Target::RemoveRecording);
                y += row_h + f64_of(spacing.s2);
                let review = Rect::from_origin_size((left, y), (inner, row_h));
                let reviewed = composer.review.is_some();
                let review_label = if reviewed {
                    let hidden = composer.review.as_ref().map_or(0, Review::redaction_count);
                    format!(
                        "{} · {}",
                        t("remark-review"),
                        t_args(
                            "remark-review-hidden",
                            &[("count", number(f64::from(hidden)))]
                        )
                    )
                } else {
                    t("remark-review")
                };
                Button::paint(
                    scene,
                    transform,
                    tr,
                    theme,
                    &ButtonFace::new(review, &review_label)
                        .icon(Icon::Eye)
                        .radius(f64_of(theme.tokens.radii.lg))
                        .hovered(self.hovered(review))
                        .focused(self.focused(&Target::Review)),
                );
                self.push(review, Target::Review);
            }
        }

        // Footer: why Send is waiting, discard, send.
        let footer_y = panel.y1 - pad - footer_h;
        let blocker = composer.send_blocker(options);
        let send_label = match (&composer.flow, &composer.review) {
            (Flow::Attached { .. }, None) => t("remark-review-and-send"),
            _ => t("remark-send"),
        };
        let (send_w, _) = tr.measure_weighted(&send_label, scale.sm, None, TextWeight::Medium);
        let send_w = f64::from(send_w) + 44.0;
        let send = Rect::from_origin_size((panel.x1 - pad - send_w, footer_y), (send_w, footer_h));
        let send_target =
            if matches!(composer.flow, Flow::Attached { .. }) && composer.review.is_none() {
                Target::Review
            } else {
                Target::Send
            };
        let sending = matches!(composer.sending, Sending::InFlight { .. });
        let send_ready = sending || blocker.is_none_or(|key| key == "remark-blocked-review");
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(send, if sending { "" } else { &send_label })
                .primary()
                .icon(Icon::Send)
                .radius(f64_of(theme.tokens.radii.lg))
                .disabled(!send_ready || sending)
                .hovered(self.hovered(send))
                .focused(self.focused(&send_target)),
        );
        if send_ready && !sending {
            self.push(send, send_target);
        }
        let discard_label = t("remark-discard");
        let (discard_w, _) =
            tr.measure_weighted(&discard_label, scale.sm, None, TextWeight::Medium);
        let discard_w = f64::from(discard_w) + 24.0;
        let discard = Rect::from_origin_size(
            (send.x0 - f64_of(spacing.s2) - discard_w, footer_y),
            (discard_w, footer_h),
        );
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(discard, &discard_label)
                .ghost()
                .radius(f64_of(theme.tokens.radii.lg))
                .hovered(self.hovered(discard))
                .focused(self.focused(&Target::Discard)),
        );
        self.push(discard, Target::Discard);
        let status = match (&composer.sending, blocker) {
            (Sending::InFlight { .. }, _) => Some((t("remark-sending"), colors.text_dim)),
            (Sending::Failed { reason }, _) => Some((reason.clone(), colors.semantic.danger)),
            (_, Some(key)) if key != "remark-blocked-review" => {
                Some((t(key), colors.text_tertiary))
            }
            _ => None,
        };
        if let Some((status, color)) = status {
            let (_, sh) = tr.measure(&status, scale.xs, None);
            line(
                scene,
                transform,
                tr,
                &status,
                Ink {
                    size: scale.xs,
                    color,
                    weight: TextWeight::Regular,
                },
                Point::new(left, footer_y + (footer_h - f64::from(sh)) / 2.0),
                discard.x0 - left - gap,
            );
        }
        let _ = y;
        scene.pop_layer();
    }

    fn draw_destination_row(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        option: &DestinationOption,
        trailing_chevron: bool,
    ) {
        let colors = &theme.tokens.colors;
        let scale = &theme.tokens.typography.scale;
        let gap = f64_of(theme.tokens.spacing.s3);
        let icon = f64_of(theme.tokens.metrics.icon_md);
        let icon_color = if option.is_public() {
            colors.semantic.info
        } else {
            colors.text_primary
        };
        draw_icon(
            scene,
            transform,
            Rect::from_center_size((row.x0 + gap + icon / 2.0, row.center().y), (icon, icon)),
            destination_icon(option),
            icon_color,
            LUCIDE_DEFAULT_STROKE_PX,
        );
        let x = row.x0 + gap + icon + f64_of(theme.tokens.metrics.icon_label_gap) + 2.0;
        let width = row.x1 - x - if trailing_chevron { gap + 18.0 } else { gap };
        let (_, name_h) = tr.measure_weighted(&option.label, scale.sm, None, TextWeight::Medium);
        let detail = destination_detail(option);
        let (_, detail_h) = tr.measure(&detail, scale.xs, None);
        let top = row.center().y - (f64::from(name_h) + f64::from(detail_h) + 1.0) / 2.0;
        line(
            scene,
            transform,
            tr,
            &option.label,
            Ink {
                size: scale.sm,
                color: colors.text_primary,
                weight: TextWeight::Medium,
            },
            Point::new(x, top),
            width,
        );
        line(
            scene,
            transform,
            tr,
            &detail,
            Ink {
                size: scale.xs,
                color: colors.text_dim,
                weight: TextWeight::Regular,
            },
            Point::new(x, top + f64::from(name_h) + 1.0),
            width,
        );
    }

    // ------------------------------------------------------------------
    // Review
    // ------------------------------------------------------------------

    /// The review sheet: the steps down the left, the chosen step's state
    /// and texts on the right, the model in use at the top.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered layout-and-paint pass over the review sheet"
    )]
    pub fn draw_review(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        review: Option<&Review>,
        states: &[StepState],
    ) {
        self.targets.clear();
        let colors = &theme.tokens.colors;
        let spacing = &theme.tokens.spacing;
        let scale = &theme.tokens.typography.scale;
        let metrics = &theme.tokens.metrics;
        let pad = f64_of(spacing.s4);
        let gap = f64_of(spacing.s3);
        let target_h = f64_of(metrics.min_pointer_target);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.scrim.with_alpha_factor(0.45),
            None,
            &viewport,
        );
        let width = (viewport.width() - f64_of(spacing.s8) * 2.0).clamp(320.0, 940.0);
        let height = (viewport.height() - f64_of(spacing.s8) * 2.0).clamp(360.0, 660.0);
        let enter = ease_out(entrance(self.opened, theme));
        let sheet = Rect::from_center_size(
            (
                viewport.center().x,
                viewport.center().y - 10.0 * (1.0 - enter),
            ),
            (width, height),
        );
        self.panel = sheet;
        let radius = f64_of(theme.tokens.radii.xl);
        ui::surface::shadow(scene, transform, sheet, radius, &theme.tokens.elevation.e4);
        ui::surface::panel(scene, transform, sheet, radius, theme);
        self.push(sheet, Target::Absorb);
        let left = sheet.x0 + pad;
        let mut y = sheet.y0 + pad;
        let inner = width - pad * 2.0;

        // Header: the title, and which model reviews (with setting one up
        // when there is none) beside it, or below it when the sheet is too
        // narrow for both.
        let title = t("remark-review-title");
        let subtitle = t("remark-review-subtitle");
        let (title_w, th) = tr.measure_weighted(&title, scale.lg, None, TextWeight::Semibold);
        let (subtitle_w, _) = tr.measure(&subtitle, scale.sm, None);
        let heading_w = f64::from(title_w.max(subtitle_w));
        let badge_w = 320.0_f64.min(inner);
        let setup = review
            .filter(|review| matches!(review.model, ModelUse::Floor))
            .map(|_| {
                let label = t("remark-review-model-setup-action");
                let (lw, _) = tr.measure_weighted(&label, scale.sm, None, TextWeight::Medium);
                (label, f64::from(lw) + 44.0)
            });
        let setup_w = setup.as_ref().map_or(0.0, |(_, w)| *w + gap);
        let beside = review.is_none() || heading_w + gap + setup_w + badge_w <= inner;
        let heading_max = if beside {
            inner - setup_w - badge_w - gap
        } else {
            inner
        };
        line(
            scene,
            transform,
            tr,
            &title,
            Ink {
                size: scale.lg,
                color: colors.text_primary,
                weight: TextWeight::Semibold,
            },
            Point::new(left, y),
            heading_max,
        );
        line(
            scene,
            transform,
            tr,
            &subtitle,
            Ink {
                size: scale.sm,
                color: colors.text_dim,
                weight: TextWeight::Regular,
            },
            Point::new(left, y + f64::from(th) + 2.0),
            heading_max,
        );
        if !beside {
            y += 52.0;
        }
        if let Some(review) = review {
            let badge = if beside {
                Rect::new(sheet.x1 - pad - badge_w, y, sheet.x1 - pad, y + 44.0)
            } else {
                Rect::new(left, y, left + (inner - setup_w).min(badge_w), y + 44.0)
            };
            Self::draw_model_badge(scene, transform, tr, theme, badge, &review.model);
            // No model yet: set the recommended one up here, through the
            // hub's own action and its own confirmation of size and license.
            if let Some((label, w)) = setup.as_ref() {
                let x = if beside {
                    badge.x0 - gap - w
                } else {
                    sheet.x1 - pad - w
                };
                let button = Rect::from_origin_size((x, y + 4.0), (*w, target_h));
                Button::paint(
                    scene,
                    transform,
                    tr,
                    theme,
                    &ButtonFace::new(button, label)
                        .icon(Icon::CloudDownload)
                        .radius(f64_of(theme.tokens.radii.lg))
                        .hovered(self.hovered(button))
                        .focused(self.focused(&Target::ReviewSetUp)),
                );
                self.push(button, Target::ReviewSetUp);
            }
        }
        y += 52.0;
        ui::surface::separator(
            scene,
            transform,
            sheet.x0,
            sheet.x1,
            y,
            gap,
            ui::chrome::shell::panel_ground(theme),
        );
        y += gap;

        let footer_h = target_h;
        let body = Rect::new(left, y, sheet.x1 - pad, sheet.y1 - pad - footer_h - gap);
        let Some(review) = review else {
            let (_, h) = tr.measure(&t("remark-review-verdict-pending"), scale.sm, None);
            line(
                scene,
                transform,
                tr,
                &t("remark-review-verdict-pending"),
                Ink {
                    size: scale.sm,
                    color: colors.text_dim,
                    weight: TextWeight::Regular,
                },
                Point::new(body.center().x - 80.0, body.center().y - f64::from(h) / 2.0),
                240.0,
            );
            self.draw_review_footer(scene, transform, tr, theme, sheet, None);
            return;
        };
        if review.steps.is_empty() {
            let list = Rect::new(
                body.x0,
                body.y0,
                body.x0 + (body.width() * 0.34).clamp(200.0, 300.0),
                body.y1,
            );
            Self::draw_protected_rows(scene, transform, tr, theme, list, 0, &review.protected);
            let message = t("remark-review-empty");
            let (w, h) = tr.measure(&message, scale.sm, None);
            line(
                scene,
                transform,
                tr,
                &message,
                Ink {
                    size: scale.sm,
                    color: colors.text_dim,
                    weight: TextWeight::Regular,
                },
                Point::new(
                    body.center().x - f64::from(w) / 2.0,
                    body.center().y - f64::from(h) / 2.0,
                ),
                inner,
            );
            self.draw_review_footer(scene, transform, tr, theme, sheet, Some(review));
            self.draw_review_offer(scene, transform, tr, theme, sheet, body, review);
            return;
        }

        // Steps, and below them whether a replay reproduces the frames.
        let list_w = (body.width() * 0.34).clamp(200.0, 300.0);
        let step_h = 52.0;
        let verdict_h = if matches!(review.verdict, Verdict::Unchecked) {
            0.0
        } else {
            64.0
        };
        Self::draw_verdict_badge(
            scene,
            transform,
            tr,
            theme,
            Rect::new(
                body.x0 + gap,
                body.y1 - verdict_h + gap,
                body.x0 + list_w,
                body.y1,
            ),
            &review.verdict,
        );
        let ground = ui::chrome::shell::panel_ground(theme);
        for (index, step) in review.steps.iter().enumerate() {
            let row = Rect::from_origin_size(
                (
                    body.x0,
                    body.y0 + ui::num::f64_of_usize(index) * (step_h + 2.0),
                ),
                (list_w, step_h),
            );
            if row.y1 > body.y1 - verdict_h {
                break;
            }
            let target = Target::ReviewStep(step.index);
            let selected = review.selected == step.index;
            let tier = selection_tier_on(
                theme,
                SelectionState::new(selected, self.hovered(row) || self.focused(&target), false),
                ground,
            );
            let shape = RoundedRect::from_rect(row, f64_of(theme.tokens.radii.lg));
            scene.fill(Fill::NonZero, transform, tier.fill, None, &shape);
            if let Some(border) = tier.border {
                scene.stroke(&Stroke::new(1.0), transform, border, None, &shape);
            }
            let cut = review.is_cut(step.index);
            let dot = Circle::new((row.x0 + gap + 10.0, row.center().y), 10.0);
            let dot_color = if step.failed() {
                colors.semantic.danger
            } else {
                colors.text_tertiary
            };
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                dot_color.with_alpha_factor(if cut { 0.4 } else { 1.0 }),
                None,
                &dot,
            );
            let n = index.saturating_add(1).to_string();
            let (nw, nh) = tr.measure_weighted(&n, scale.xs, None, TextWeight::Semibold);
            tr.draw_text_weighted(
                scene,
                &n,
                scale.xs,
                dot_color,
                transform.pre_translate(
                    (
                        dot.center.x - f64::from(nw) / 2.0,
                        dot.center.y - f64::from(nh) / 2.0,
                    )
                        .into(),
                ),
                None,
                TextWeight::Semibold,
            );
            let x = row.x0 + gap + 28.0;
            // Replay drew this step differently.
            let flagged = !review.divergent_regions(step.index).is_empty();
            let text_x1 = if flagged {
                draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size((row.x1 - gap - 7.0, row.center().y), (14.0, 14.0)),
                    Icon::TriangleAlert,
                    colors.semantic.warning,
                    LUCIDE_DEFAULT_STROKE_PX,
                );
                row.x1 - gap - 18.0
            } else {
                row.x1
            };
            let title = review.title(step.index);
            let title_color = if cut {
                colors.text_tertiary
            } else {
                colors.text_primary
            };
            let (_, title_h) = tr.measure_weighted(&title, scale.sm, None, TextWeight::Medium);
            line(
                scene,
                transform,
                tr,
                &title,
                Ink {
                    size: scale.sm,
                    color: title_color,
                    weight: TextWeight::Medium,
                },
                Point::new(x, row.y0 + 8.0),
                text_x1 - x - gap,
            );
            if cut {
                let shown = tr.elide_to_width_weighted(
                    &title,
                    scale.sm,
                    f32_of((text_x1 - x - gap).max(0.0)),
                    TextWeight::Medium,
                );
                let (title_w, _) = tr.measure_weighted(&shown, scale.sm, None, TextWeight::Medium);
                let mid = (row.y0 + 8.0 + f64::from(title_h) / 2.0).round();
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.text_tertiary,
                    None,
                    &Rect::new(x, mid, x + f64::from(title_w), mid + 1.0),
                );
            }
            let meta = if cut {
                t("remark-review-cut-state")
            } else {
                format!("{} · {}", step.app_label, clock(step.at_ms()))
            };
            line(
                scene,
                transform,
                tr,
                &meta,
                Ink {
                    size: scale.xs,
                    color: colors.text_dim,
                    weight: TextWeight::Regular,
                },
                Point::new(x, row.y0 + 12.0 + f64::from(title_h)),
                text_x1 - x - gap,
            );
            self.push(row, target);
        }
        Self::draw_protected_rows(
            scene,
            transform,
            tr,
            theme,
            Rect::new(body.x0, body.y0, body.x0 + list_w, body.y1 - verdict_h),
            review.steps.len(),
            &review.protected,
        );

        // The chosen step.
        let detail = Rect::new(body.x0 + list_w + gap * 1.5, body.y0, body.x1, body.y1);
        let step = review.selected;
        let mut dy = detail.y0;
        let title = review.title(step);
        let (_, title_h) = tr.measure_weighted(&title, scale.base, None, TextWeight::Semibold);
        line(
            scene,
            transform,
            tr,
            &title,
            Ink {
                size: scale.base,
                color: colors.text_primary,
                weight: TextWeight::Semibold,
            },
            Point::new(detail.x0, dy),
            detail.width() - 150.0,
        );
        if review.titled_by_model(step) {
            draw_icon(
                scene,
                transform,
                Rect::from_origin_size((detail.x0 - 18.0, dy + 1.0), (14.0, 14.0)),
                Icon::Sparkles,
                colors.text_dim,
                LUCIDE_DEFAULT_STROKE_PX,
            );
        }
        let cut = review.is_cut(step);
        let cut_label = if cut {
            t("remark-review-keep")
        } else {
            t("remark-review-cut")
        };
        let (cw, _) = tr.measure_weighted(&cut_label, scale.sm, None, TextWeight::Medium);
        let cut_rect = Rect::from_origin_size(
            (detail.x1 - f64::from(cw) - 40.0, dy - 4.0),
            (f64::from(cw) + 40.0, target_h),
        );
        let cut_target = Target::ReviewCut { step, cut: !cut };
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(cut_rect, &cut_label)
                .icon(if cut { Icon::Undo2 } else { Icon::Scissors })
                .radius(f64_of(theme.tokens.radii.lg))
                .hovered(self.hovered(cut_rect))
                .focused(self.focused(&cut_target)),
        );
        self.push(cut_rect, cut_target);
        dy += f64::from(title_h) + 4.0;
        if let Some(meta) = review.steps.iter().find(|s| s.index == step) {
            let outcome = if meta.failed() {
                format!(" · {}", t("remark-review-failed"))
            } else {
                String::new()
            };
            let meta_line = format!("{} · {}{}", meta.app_label, clock(meta.at_ms()), outcome);
            line(
                scene,
                transform,
                tr,
                &meta_line,
                Ink {
                    size: scale.xs,
                    color: if meta.failed() {
                        colors.semantic.danger
                    } else {
                        colors.text_dim
                    },
                    weight: TextWeight::Regular,
                },
                Point::new(detail.x0, dy),
                detail.width() - 150.0,
            );
        }
        dy += 24.0;

        // The step as it looked, with what is hidden drawn in place.
        let state = states.iter().find(|state| state.step == step);
        let preview_h = ((detail.y1 - dy) * 0.6).clamp(140.0, 320.0);
        let preview = Rect::from_origin_size((detail.x0, dy), (detail.width(), preview_h));
        ui::surface::card(
            scene,
            transform,
            preview,
            f64_of(theme.tokens.radii.lg),
            colors.surfaces.raised,
        );
        if let Some(state) = state {
            let failed = review
                .steps
                .iter()
                .find(|candidate| candidate.index == step)
                .is_some_and(super::review::Step::failed);
            self.draw_step_preview(
                scene,
                transform,
                tr,
                theme,
                preview.inset(-gap),
                state,
                failed,
            );
        }
        dy += preview_h + gap;

        // What is included: each field's content as it will leave, word by
        // word, so a word can be hidden. A control's own name is not
        // content and is not listed.
        dy += section_header(
            scene,
            transform,
            tr,
            theme,
            &t("remark-review-included"),
            Point::new(detail.x0, dy),
            detail.width(),
        ) + f64_of(spacing.s2);
        let caption_w = 104.0;
        let mut listed = 0_usize;
        let masker = session_trace::redact::Masker::new(review.hidden_values());
        for item in review.shown_texts(step) {
            let text_item = item.text;
            if dy > detail.y1 - 20.0 {
                break;
            }
            let base = text_item.node.split('#').next().unwrap_or_default();
            let node = state.and_then(|state| state.nodes.iter().find(|node| node.id == base));
            let is_value = !text_item.node.contains('#');
            if !(is_value || node.is_some_and(|node| node.content)) {
                continue;
            }
            listed = listed.saturating_add(1);
            let caption = node
                .and_then(|node| node.name.clone())
                .unwrap_or_else(|| t("remark-review-shown"));
            line(
                scene,
                transform,
                tr,
                &caption,
                Ink {
                    size: scale.xs,
                    color: colors.text_tertiary,
                    weight: TextWeight::Medium,
                },
                Point::new(detail.x0, dy + 2.0),
                caption_w - gap,
            );
            // As it leaves: what review hid here, and every hidden value
            // wherever else it appears.
            let shown = masker.mask(&review.redacted(step, &text_item.node).unwrap_or_default());
            dy = self.draw_words(
                &mut ui::paint::PaintCtx::new(scene, transform, tr, theme),
                Point::new(detail.x0 + caption_w, dy),
                detail.width() - caption_w,
                step,
                &text_item.node,
                (&text_item.text, &shown),
                scale.sm,
            ) + f64_of(spacing.s2);
        }
        if listed == 0 {
            line(
                scene,
                transform,
                tr,
                &t("remark-review-nothing-included"),
                Ink {
                    size: scale.sm,
                    color: colors.text_dim,
                    weight: TextWeight::Regular,
                },
                Point::new(detail.x0, dy),
                detail.width(),
            );
        }

        self.draw_review_footer(scene, transform, tr, theme, sheet, Some(review));
        self.draw_review_offer(scene, transform, tr, theme, sheet, body, review);
    }

    /// The hub's offer to set the review model up, over the sheet below its
    /// header: the toolkit's shared set-up offer, as Settings shows it, in
    /// the hub's own words. It stands in for the steps and the footer until
    /// the person answers it; its buttons are this sheet's targets.
    fn draw_review_offer(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        sheet: Rect,
        body: Rect,
        review: &Review,
    ) {
        let ModelUse::Offer { choices, expanded } = &review.model else {
            return;
        };
        let Some(offer) = review_offer(choices) else {
            return;
        };
        // As tall as the offer, at most the sheet below its header.
        let inset = f64_of(theme.tokens.spacing.s4);
        let bottom = sheet.y1 - f64_of(theme.tokens.spacing.s4);
        let layout = offer.layout_fitting(
            Point::new(body.x0 + inset, body.y0 + inset),
            (body.width() - inset * 2.0).max(120.0),
            bottom - body.y0 - inset * 2.0,
            *expanded,
            theme,
            tr,
        );
        let card = Rect::new(
            body.x0,
            body.y0,
            body.x1,
            (layout.rect.y1 + inset).min(bottom),
        );
        let radius = f64_of(theme.tokens.radii.lg);
        ui::surface::shadow(scene, transform, card, radius, &theme.tokens.elevation.e3);
        ui::surface::panel(scene, transform, card, radius, theme);
        self.push(card, Target::Absorb);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &RoundedRect::from_rect(card, radius),
        );
        let targets = layout.targets();
        let hovered = targets
            .iter()
            .find(|(rect, _)| self.hovered(*rect))
            .map(|(_, target)| *target);
        let focused = targets
            .iter()
            .find(|(_, target)| self.focused(&Target::ReviewOffer(*target)))
            .map(|(_, target)| *target);
        layout.paint(scene, transform, tr, theme, hovered, focused);
        scene.pop_layer();
        for (rect, target) in targets {
            // What the card clips away cannot be pressed either.
            if card.contains(Point::new(rect.x0, rect.y1)) {
                self.push(rect, Target::ReviewOffer(target));
            }
        }
    }

    /// One row per capture-protected app, below the `after` step rows of
    /// `list`: the capture placeholder where its steps would be, and that
    /// nothing of it was recorded.
    fn draw_protected_rows(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        list: Rect,
        after: usize,
        protected: &[(String, String)],
    ) {
        let step_h = 52.0;
        let gap = gap(theme);
        let scale = &theme.tokens.typography.scale;
        let colors = &theme.tokens.colors;
        for (index, (_, name)) in protected.iter().enumerate() {
            let row = Rect::from_origin_size(
                (
                    list.x0,
                    list.y0 + ui::num::f64_of_usize(after.saturating_add(index)) * (step_h + 2.0),
                ),
                (list.width(), step_h),
            );
            if row.y1 > list.y1 {
                break;
            }
            let mark = Rect::from_origin_size(
                (row.x0 + gap, row.y0 + 6.0),
                (step_h - 12.0, step_h - 12.0),
            );
            crate::shell::capture_protection::paint_placeholder(
                scene,
                transform,
                mark,
                f64_of(theme.tokens.radii.sm),
                theme,
            );
            let x = mark.x1 + gap;
            let (_, name_h) = tr.measure_weighted(name, scale.sm, None, TextWeight::Medium);
            line(
                scene,
                transform,
                tr,
                name,
                Ink {
                    size: scale.sm,
                    color: colors.text_primary,
                    weight: TextWeight::Medium,
                },
                Point::new(x, row.y0 + 8.0),
                row.x1 - x - gap,
            );
            line(
                scene,
                transform,
                tr,
                &t("remark-review-protected"),
                Ink {
                    size: scale.xs,
                    color: colors.text_dim,
                    weight: TextWeight::Regular,
                },
                Point::new(x, row.y0 + 12.0 + f64::from(name_h)),
                row.x1 - x - gap,
            );
        }
    }

    /// One text, word by word: a hidden word is its mask, a shown one is
    /// pressable to hide it. Returns the y below the text.
    fn draw_words(
        &mut self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        at: Point,
        width: f64,
        step: u32,
        node: &str,
        (original, shown): (&str, &str),
        size: f32,
    ) -> f64 {
        let (scene, transform, tr, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let colors = &theme.tokens.colors;
        let (space_w, line_h) = tr.measure(" ", size, None);
        let space_w = f64::from(space_w).max(3.0);
        let line_h = f64::from(line_h);
        let mut x = at.x;
        let mut y = at.y;
        // Walk the original's words: the shown text has the same character
        // boundaries, one mask per hidden character.
        let shown_chars: Vec<char> = shown.chars().collect();
        let mut char_index = 0_usize;
        for (offset, word) in word_spans(original) {
            let skipped = original.get(..offset).map_or(0, |s| s.chars().count());
            char_index = char_index.max(skipped);
            let count = word.chars().count();
            let visible: String = shown_chars.iter().skip(char_index).take(count).collect();
            char_index = char_index.saturating_add(count);
            let hidden = visible.contains(tilo_feedback_review::MASK);
            let (w, _) = tr.measure(&visible, size, None);
            let w = f64::from(w);
            if x > at.x && x + w > at.x + width {
                x = at.x;
                y += line_h + 2.0;
            }
            let rect = Rect::new(x - 1.0, y - 1.0, x + w + 1.0, y + line_h + 1.0);
            let hover = self.hovered(rect);
            if hidden {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.text_primary.with_alpha_factor(0.08),
                    None,
                    &RoundedRect::from_rect(rect, 3.0),
                );
            } else if hover {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.semantic.warning.with_alpha_factor(0.22),
                    None,
                    &RoundedRect::from_rect(rect, 3.0),
                );
            }
            tr.draw_text(
                scene,
                &visible,
                size,
                if hidden {
                    colors.text_dim
                } else {
                    colors.text_primary
                },
                transform.pre_translate((x, y).into()),
                None,
            );
            if !hidden {
                self.push(
                    rect,
                    Target::ReviewWord {
                        step,
                        node: node.to_string(),
                        at: offset,
                    },
                );
            }
            x += w + space_w;
        }
        y + line_h
    }

    fn draw_model_badge(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        model: &ModelUse,
    ) {
        let colors = &theme.tokens.colors;
        let (icon, tint, title, detail) = match model {
            ModelUse::OnDevice { model } => (
                Icon::Sparkles,
                colors.semantic.success,
                t_args(
                    "remark-review-model-device",
                    &[("model", text(model.clone()))],
                ),
                t("remark-review-model-device-detail"),
            ),
            ModelUse::Hosted { model, location } => (
                Icon::Globe,
                colors.semantic.warning,
                t_args(
                    "remark-review-model-hosted",
                    &[
                        ("model", text(model.clone())),
                        ("location", text(location.clone())),
                    ],
                ),
                t("remark-review-model-hosted-detail"),
            ),
            ModelUse::SettingUp { model, progress } => (
                Icon::CloudDownload,
                colors.accent[5],
                t_args(
                    "remark-review-model-setup",
                    &[("model", text(model.clone()))],
                ),
                progress.clone().unwrap_or_default(),
            ),
            ModelUse::Failed { reason } => (
                Icon::TriangleAlert,
                colors.semantic.warning,
                t("remark-review-model-failed"),
                reason.clone(),
            ),
            ModelUse::Floor | ModelUse::Pending | ModelUse::Offer { .. } => (
                Icon::Shield,
                colors.text_dim,
                t("remark-review-model-floor"),
                t("remark-review-model-floor-detail"),
            ),
        };
        Self::draw_badge(
            &mut ui::paint::PaintCtx::new(scene, transform, tr, theme),
            rect,
            icon,
            tint,
            &title,
            &detail,
        );
    }

    /// Whether a replay of the session reproduces its frames; nothing when
    /// the recording kept none.
    fn draw_verdict_badge(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        verdict: &Verdict,
    ) {
        let colors = &theme.tokens.colors;
        let (icon, tint, title, detail) = match verdict {
            Verdict::Unchecked => return,
            Verdict::Unavailable => (
                Icon::Film,
                colors.text_dim,
                t("remark-review-verdict-unavailable"),
                t("remark-review-verdict-unavailable-detail"),
            ),
            Verdict::Checking => (
                Icon::Film,
                colors.accent[5],
                t("remark-review-verdict-pending"),
                t("remark-review-verdict-pending-detail"),
            ),
            Verdict::Failed(reason) => (
                Icon::TriangleAlert,
                colors.semantic.warning,
                t("remark-review-verdict-failed"),
                reason.clone(),
            ),
            Verdict::Checked(report) => {
                let regions = super::review::diverging_places(report);
                if regions.is_empty() {
                    (
                        Icon::Check,
                        colors.semantic.success,
                        t("remark-review-verdict-logic"),
                        t("remark-review-verdict-logic-detail"),
                    )
                } else {
                    let unsemantic = regions.iter().all(|region| {
                        matches!(
                            region.divergence,
                            session_trace::divergence::RegionDivergence::Unexplained
                        )
                    });
                    (
                        Icon::TriangleAlert,
                        colors.semantic.warning,
                        if unsemantic {
                            t("remark-review-verdict-unsemantic")
                        } else {
                            t("remark-review-verdict-below")
                        },
                        t_args(
                            "remark-review-verdict-below-detail",
                            &[(
                                "count",
                                number(u32::try_from(regions.len()).unwrap_or(u32::MAX)),
                            )],
                        ),
                    )
                }
            }
        };
        Self::draw_badge(
            &mut ui::paint::PaintCtx::new(scene, transform, tr, theme),
            rect,
            icon,
            tint,
            &title,
            &detail,
        );
    }

    fn draw_badge(
        ctx: &mut ui::paint::PaintCtx<'_>,
        rect: Rect,
        icon: Icon,
        tint: Color,
        title: &str,
        detail: &str,
    ) {
        let (scene, transform, tr, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let colors = &theme.tokens.colors;
        let scale = &theme.tokens.typography.scale;
        let icon_rect = Rect::from_center_size((rect.x0 + 10.0, rect.y0 + 10.0), (16.0, 16.0));
        draw_icon(
            scene,
            transform,
            icon_rect,
            icon,
            tint,
            LUCIDE_DEFAULT_STROKE_PX,
        );
        let x = rect.x0 + 26.0;
        let (_, h) = tr.measure_weighted(title, scale.sm, None, TextWeight::Medium);
        line(
            scene,
            transform,
            tr,
            title,
            Ink {
                size: scale.sm,
                color: colors.text_primary,
                weight: TextWeight::Medium,
            },
            Point::new(x, rect.y0 + 2.0),
            rect.x1 - x,
        );
        paragraph(
            scene,
            transform,
            tr,
            detail,
            scale.xs,
            colors.text_dim,
            Point::new(x, rect.y0 + 4.0 + f64::from(h)),
            rect.x1 - x,
        );
    }

    /// The step as it looked: the recorded frame of the app when frames
    /// were kept (already masked where the app declared something
    /// personal), else the app's own tree drawn as a sketch of its
    /// controls. What review hid is drawn in place as the mask it leaves
    /// as, the node the step acted on is ringed, and pressing a text hides
    /// it whole.
    #[allow(
        clippy::too_many_lines,
        reason = "the preview is laid out against the step, its state and the frame together"
    )]
    fn draw_step_preview(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        state: &StepState,
        failed: bool,
    ) {
        let colors = &theme.tokens.colors;
        let (content_w, content_h, unit) = if let Some(frame) = &state.frame {
            (f64::from(frame.width), f64::from(frame.height), frame.scale)
        } else {
            let bounds = state
                .nodes
                .iter()
                .map(|node| node.bounds)
                .reduce(|a, b| a.union(b))
                .unwrap_or(Rect::ZERO);
            (bounds.x1, bounds.y1, 1.0)
        };
        if content_w <= 0.0 || content_h <= 0.0 {
            return;
        }
        let fit = (area.width() / content_w).min(area.height() / content_h);
        let shown = Rect::from_center_size(area.center(), (content_w * fit, content_h * fit));
        let map = |rect: Rect| {
            let k = unit * fit;
            Rect::new(
                shown.x0 + rect.x0 * k,
                shown.y0 + rect.y0 * k,
                shown.x0 + rect.x1 * k,
                shown.y0 + rect.y1 * k,
            )
        };
        let radius = f64_of(theme.tokens.radii.md);
        if let Some(frame) = &state.frame {
            let index = self
                .frame_images
                .iter()
                .position(|(step, _)| *step == state.step);
            let index = index.unwrap_or_else(|| {
                self.frame_images.push((
                    state.step,
                    ui::widget::Image::from_rgba(
                        std::sync::Arc::clone(&frame.rgba),
                        frame.width,
                        frame.height,
                    )
                    .with_fit(ui::widget::ImageFit::Fill),
                ));
                self.frame_images.len().saturating_sub(1)
            });
            if let Some((_, image)) = self.frame_images.get_mut(index) {
                image.paint_at_on(scene, transform, shown, theme);
            }
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                ui::surface::edge_on(colors.surfaces.raised),
                None,
                &RoundedRect::from_rect(shown, 2.0),
            );
        } else {
            // A sketch of the app from its own tree: fields as fields,
            // buttons as buttons, alerts as alerts.
            ui::surface::card(scene, transform, shown, radius, colors.surfaces.base);
            for node in state.nodes.iter().filter(|node| node.leaf) {
                let rect = map(node.bounds);
                if rect.width() < 2.0 || rect.height() < 2.0 {
                    continue;
                }
                let text = node.text.as_deref().unwrap_or_default();
                let size = f32_of((rect.height() * 0.42).clamp(7.0, 13.0));
                let (fill, ink, centred) = match node.role.as_str() {
                    role if is_control(role) => {
                        (Some(colors.surfaces.raised), colors.text_primary, true)
                    }
                    "text_input" | "combo_box" | "search_box" => {
                        (Some(colors.surfaces.overlay), colors.text_primary, false)
                    }
                    "alert" | "alert_dialog" | "banner" | "status" => (
                        Some(colors.semantic.danger.with_alpha_factor(0.16)),
                        colors.semantic.danger,
                        false,
                    ),
                    _ => (None, colors.text_dim, false),
                };
                if let Some(fill) = fill {
                    ui::surface::card(
                        scene,
                        transform,
                        rect,
                        (rect.height() / 2.0).min(radius),
                        fill,
                    );
                }
                if !text.is_empty() {
                    let fitted =
                        tr.elide_to_width(text, size, f32_of((rect.width() - 8.0).max(0.0)));
                    let (w, h) = tr.measure(&fitted, size, None);
                    if f64::from(h) <= rect.height() + 2.0 {
                        let x = if centred {
                            rect.center().x - f64::from(w) / 2.0
                        } else {
                            rect.x0 + 6.0
                        };
                        tr.draw_text(
                            scene,
                            &fitted,
                            size,
                            ink,
                            transform
                                .pre_translate((x, rect.center().y - f64::from(h) / 2.0).into()),
                            None,
                        );
                    }
                }
            }
        }
        // What review hid, drawn where it was, as it will leave; and the
        // texts that can still be hidden, pressable in place.
        for node in state.nodes.iter().filter(|node| node.content) {
            let rect = map(node.bounds);
            if rect.width() < 2.0 || rect.height() < 2.0 {
                continue;
            }
            let hidden = node
                .text
                .as_deref()
                .is_some_and(|text| text.contains(tilo_feedback_review::MASK));
            if hidden && state.frame.is_some() {
                let text = node.text.as_deref().unwrap_or_default();
                let size = f32_of((rect.height() * 0.42).clamp(7.0, 13.0));
                // Over the frame's own mask, edge to edge, so none of it
                // shows around the placeholder.
                ui::surface::card(
                    scene,
                    transform,
                    rect.inflate(1.0, 1.0),
                    2.0,
                    colors.surfaces.overlay,
                );
                let fitted = tr.elide_to_width(text, size, f32_of((rect.width() - 8.0).max(0.0)));
                let (_, h) = tr.measure(&fitted, size, None);
                tr.draw_text(
                    scene,
                    &fitted,
                    size,
                    colors.text_dim,
                    transform.pre_translate(
                        (rect.x0 + 6.0, rect.center().y - f64::from(h) / 2.0).into(),
                    ),
                    None,
                );
            }
            let target = Target::ReviewHideText {
                step: state.step,
                node: node.id.clone(),
            };
            if self.hovered(rect) {
                scene.stroke(
                    &Stroke::new(1.5),
                    transform,
                    colors.semantic.warning,
                    None,
                    &RoundedRect::from_rect(rect.inflate(1.0, 1.0), 3.0),
                );
            }
            self.push(rect, target);
        }
        // Where a replay of the session drew something else.
        for region in &state.divergent {
            scene.stroke(
                &Stroke::new(1.5).with_dashes(0.0, [4.0, 3.0]),
                transform,
                colors.semantic.warning,
                None,
                &RoundedRect::from_rect(map(*region).inflate(3.0, 3.0), 3.0),
            );
        }
        // The node the step acted on.
        if let Some(node) = state.nodes.iter().find(|node| node.acted) {
            let ring = if failed {
                colors.semantic.danger
            } else {
                colors.accent[5]
            };
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                ring,
                None,
                &RoundedRect::from_rect(map(node.bounds).inflate(2.0, 2.0), radius),
            );
        }
    }

    fn draw_review_footer(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        sheet: Rect,
        review: Option<&Review>,
    ) {
        let colors = &theme.tokens.colors;
        let spacing = &theme.tokens.spacing;
        let scale = &theme.tokens.typography.scale;
        let pad = f64_of(spacing.s4);
        let h = f64_of(theme.tokens.metrics.min_pointer_target);
        let y = sheet.y1 - pad - h;
        let back_label = t("remark-review-back");
        let (bw, _) = tr.measure_weighted(&back_label, scale.sm, None, TextWeight::Medium);
        let back = Rect::from_origin_size((sheet.x0 + pad, y), (f64::from(bw) + 44.0, h));
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(back, &back_label)
                .ghost()
                .icon(Icon::ArrowLeft)
                .radius(f64_of(theme.tokens.radii.lg))
                .hovered(self.hovered(back))
                .focused(self.focused(&Target::ReviewBack)),
        );
        self.push(back, Target::ReviewBack);
        let send_label = t("remark-send");
        let (sw, _) = tr.measure_weighted(&send_label, scale.sm, None, TextWeight::Medium);
        let send = Rect::from_origin_size(
            (sheet.x1 - pad - f64::from(sw) - 44.0, y),
            (f64::from(sw) + 44.0, h),
        );
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(send, &send_label)
                .primary()
                .icon(Icon::Send)
                .radius(f64_of(theme.tokens.radii.lg))
                .disabled(review.is_none())
                .hovered(self.hovered(send))
                .focused(self.focused(&Target::ReviewSend)),
        );
        if review.is_some() {
            self.push(send, Target::ReviewSend);
        }
        if let Some(review) = review {
            let summary = format!(
                "{} · {}",
                t_args(
                    "remark-attached",
                    &[
                        (
                            "steps",
                            number(ui::num::f64_of_usize(review.kept().count()))
                        ),
                        ("duration", text(clock(review.duration.as_millis())))
                    ]
                ),
                t_args(
                    "remark-review-hidden",
                    &[("count", number(f64::from(review.redaction_count())))]
                ),
            );
            let (sw2, sh) = tr.measure(&summary, scale.xs, None);
            let x = send.x0 - gap(theme) - f64::from(sw2);
            line(
                scene,
                transform,
                tr,
                &summary,
                Ink {
                    size: scale.xs,
                    color: colors.text_dim,
                    weight: TextWeight::Regular,
                },
                Point::new(x.max(back.x1 + gap(theme)), y + (h - f64::from(sh)) / 2.0),
                send.x0 - back.x1 - gap(theme) * 2.0,
            );
            if matches!(review.model, ModelUse::Floor) {
                let setup_label = t("remark-review-model-floor");
                let _ = setup_label;
            }
        }
    }

    // ------------------------------------------------------------------
    // Recording pill
    // ------------------------------------------------------------------

    /// The pill that says the Recorder is in the room, and stops it. It
    /// hangs from the collaborator roster, where the Recorder is listed as
    /// an agent, so the one indicator is the roster row and this its
    /// control.
    pub fn draw_pill(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        anchor: Point,
        elapsed_ms: u64,
    ) -> Rect {
        let colors = &theme.tokens.colors;
        let scale = &theme.tokens.typography.scale;
        let label = format!("{} {}", t("remark-pill-recording"), clock(elapsed_ms));
        let (lw, lh) = tr.measure_weighted(&label, scale.xs, None, TextWeight::Semibold);
        let stop_label = t("remark-stop");
        let (sw, _) = tr.measure_weighted(&stop_label, scale.xs, None, TextWeight::Medium);
        let h = f64_of(theme.tokens.metrics.selector_height);
        let stop_w = f64::from(sw) + 20.0;
        let w = 12.0 + 8.0 + 6.0 + f64::from(lw) + 8.0 + stop_w + 3.0;
        let pill = Rect::from_origin_size((anchor.x - w, anchor.y), (w, h));
        let shape = RoundedRect::from_rect(pill, f64_of(theme.tokens.radii.full));
        ui::surface::shadow(
            scene,
            transform,
            pill,
            f64_of(theme.tokens.radii.full),
            &theme.tokens.elevation.e2,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.semantic.danger.with_alpha_factor(0.55),
            None,
            &shape,
        );
        let pulse = if theme.reduce_motion {
            1.0
        } else {
            0.55 + 0.45
                * ((ui::num::f64_of_i64(i64::try_from(elapsed_ms).unwrap_or(0)) / 1000.0)
                    * std::f64::consts::PI)
                    .cos()
                    .abs()
        };
        scene.fill(
            Fill::NonZero,
            transform,
            colors.semantic.danger.with_alpha_factor(f32_of(pulse)),
            None,
            &Circle::new((pill.x0 + 12.0, pill.center().y), 4.0),
        );
        tr.draw_text_weighted(
            scene,
            &label,
            scale.xs,
            colors.text_primary,
            transform.pre_translate((pill.x0 + 22.0, pill.center().y - f64::from(lh) / 2.0).into()),
            None,
            TextWeight::Semibold,
        );
        let stop = Rect::new(
            pill.x1 - 3.0 - stop_w,
            pill.y0 + 3.0,
            pill.x1 - 3.0,
            pill.y1 - 3.0,
        );
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(stop, &stop_label)
                .danger()
                .radius(f64_of(theme.tokens.radii.full))
                .text_size(scale.xs)
                .hovered(self.hovered(stop)),
        );
        // While recording, the pill is all of the remark surface on screen.
        self.targets.clear();
        self.panel = pill;
        self.targets.push((stop, Target::PillStop));
        pill
    }

    /// The semantic tree of what is on screen, for AccessKit and agents.
    pub fn accessibility(&self, composer: &Composer, stage_name: &str) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut root = AccessNode::new(Role::Dialog)
            .with_name(stage_name.to_string())
            .with_bounds(self.panel)
            .with_state(AccessState::new().modal(composer.stage != Stage::Picking));
        let focused = self.focused_target();
        for (rect, target) in &self.targets {
            if matches!(
                target,
                Target::Absorb | Target::ReviewWord { .. } | Target::ReviewHideText { .. }
            ) {
                continue;
            }
            let role = match target {
                Target::Text => Role::TextInput,
                Target::Frames(_) => Role::Checkbox,
                Target::Destination(_) | Target::ReviewStep(_) => Role::ListItem,
                _ => Role::Button,
            };
            let mut node = AccessNode::new(role)
                .with_name(target.name(composer))
                .with_bounds(*rect)
                .with_state(AccessState {
                    focusable: true,
                    focused: focused.as_ref() == Some(target),
                    ..AccessState::default()
                });
            if let Target::Text = target {
                node = node.with_optional_value(Some(composer.text.clone()));
            }
            root.children.push(node);
        }
        root
    }
}

/// Where the pick tag goes: inside the app it names, so it never covers
/// shell chrome, and off the node it names, so it never hides what is
/// being picked. Above the node, else below it, else inside its top edge,
/// else wherever it fits in the app.
fn tag_rect(node: Rect, app: Rect, size: (f64, f64), gap: f64) -> Rect {
    let (w, h) = size;
    let x = node
        .x0
        .clamp(app.x0 + gap, (app.x1 - w - gap).max(app.x0 + gap));
    let fits = |y: f64| y >= app.y0 + gap && y + h <= app.y1 - gap;
    let above = node.y0 - gap - h;
    let below = node.y1 + gap;
    let inside = node.y0 + gap;
    let y = if fits(above) {
        above
    } else if fits(below) {
        below
    } else if node.height() >= h + gap * 2.0 && fits(inside) {
        inside
    } else {
        (app.y0 + gap).max(app.y1 - h - gap).min(app.y0 + gap)
    };
    Rect::from_origin_size((x, y), (w, h))
}

fn gap(theme: &Theme) -> f64 {
    f64_of(theme.tokens.spacing.s3)
}

/// `(byte offset, word)` for each run of non-whitespace in `text`.
fn word_spans(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (offset, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(begin) = start.take()
                && let Some(word) = text.get(begin..offset)
            {
                out.push((begin, word));
            }
        } else if start.is_none() {
            start = Some(offset);
        }
    }
    if let Some(begin) = start
        && let Some(word) = text.get(begin..)
    {
        out.push((begin, word));
    }
    out
}

/// What the review preview draws for one step: each node the app described
/// then, in its own coordinates, with its text as it will leave.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepState {
    pub step: u32,
    pub nodes: Vec<StateNode>,
    /// The recorded frame of the step's app, when frames were kept.
    pub frame: Option<StepFrame>,
    /// Where a replay of the session drew something else, in the app's
    /// surface units.
    pub divergent: Vec<Rect>,
}

/// One recorded frame, already masked where the app declared personal or
/// secret content.
#[derive(Clone, Debug, PartialEq)]
pub struct StepFrame {
    pub rgba: std::sync::Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    /// Frame pixels per surface unit.
    pub scale: f64,
}

/// Roles whose name is the control's, not content someone wrote.
pub fn is_control(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "toggle_button"
            | "link"
            | "menu_item"
            | "menu_item_checkbox"
            | "menu_item_radio"
            | "tab"
            | "checkbox"
            | "radio_button"
            | "switch"
            | "toolbar"
            | "menu"
            | "menu_bar"
            | "tab_list"
            | "window"
    )
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StateNode {
    /// The node's stable id, as review addresses its texts.
    pub id: String,
    /// The field's name (a text input's label), for captioning its value.
    pub name: Option<String>,
    /// Only a node with no children has its text drawn: a container's name
    /// would be drawn across its children.
    pub leaf: bool,
    pub role: String,
    /// Its text is content someone wrote or the app showed, not a
    /// control's own name.
    pub content: bool,
    /// The step acted on this node.
    pub acted: bool,
    pub bounds: Rect,
    pub text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A member is told who sees a public remark; only a person who left
    /// the community is told that sending joins it again.
    #[test]
    fn the_community_destination_says_what_is_true_for_the_person() {
        let community = |joined, left| DestinationOption {
            id: "community".into(),
            label: "Tilo Community".into(),
            kind: DestinationKind::Community {
                room: "room".into(),
                channel: "c".into(),
                joined,
                left,
            },
        };
        let member = t("remark-dest-community-detail");
        assert_eq!(destination_detail(&community(true, false)), member);
        assert_eq!(
            destination_detail(&community(false, false)),
            member,
            "a member the host has not admitted yet is still a member"
        );
        assert_eq!(
            destination_detail(&community(false, true)),
            t("remark-dest-community-join")
        );
    }

    #[test]
    fn every_target_names_the_action_a_click_on_it_runs() {
        for (target, id) in [
            (Target::Close, "remark.discard"),
            (Target::PickMore, "remark.pick.start"),
            (Target::RecordNow, "remark.record.start"),
            (Target::UseRecent, "remark.record.recent"),
            (Target::KeepRecent, "feedback.recent-buffer.set"),
            (Target::PillStop, "remark.record.stop"),
            (Target::Send, "remark.send"),
            (Target::PickDone, "remark.pick.done"),
            (
                Target::ReviewCut { step: 1, cut: true },
                "remark.review.cut",
            ),
        ] {
            assert_eq!(target.action().map(|(action, _)| action), Some(id));
        }
        assert_eq!(Target::DestinationList.action(), None);
        // The offer: confirming the recommendation is choice 0, an
        // alternative its rank; neither downloads anything by itself.
        assert_eq!(
            Target::ReviewOffer(OfferTarget::SetUp).action(),
            Some(("remark.review.model.confirm", json!({ "choice": 0 })))
        );
        assert_eq!(
            Target::ReviewOffer(OfferTarget::Alternative(1)).action(),
            Some(("remark.review.model.confirm", json!({ "choice": 2 })))
        );
        assert_eq!(
            Target::ReviewOffer(OfferTarget::ToggleMore)
                .action()
                .map(|(action, _)| action),
            Some("remark.review.model.more")
        );
    }

    #[test]
    fn the_review_offer_is_the_hubs_choice_in_the_shared_face() {
        let choice = |action: &str, label: &str, fit: &str| SetUpChoice {
            action: action.into(),
            label: label.into(),
            terms: "2.5 GiB download · License Apache-2.0 · Fast here, about 90 words a second"
                .into(),
            fit: Some(fit.into()),
            speed: Some("speed-interactive".into()),
        };
        let offer = review_offer(&[
            choice(
                "set-up-on-device",
                "Set up Qwen3 4B (AWQ) with vLLM on NVIDIA GeForce RTX 4070 Laptop GPU for Feedback review",
                "fit-good",
            ),
            choice(
                "set-up-on-device-1",
                "Set up Qwen3 4B (GGUF) with llama.cpp on AMD Radeon 780M Graphics for Feedback review",
                "fit-excellent",
            ),
        ])
        .expect("an offer");
        assert!(offer.question.contains("vLLM on NVIDIA GeForce RTX 4070"));
        assert!(offer.recommended.terms.contains("Apache-2.0"));
        assert_eq!(offer.recommended.pills.len(), 2, "fit and speed");
        assert_eq!(offer.alternatives.len(), 1);
        assert!(offer.show_more.contains('1'));
        let mut renderer = TextRenderer::new();
        let expanded = offer.layout(
            Point::new(0.0, 0.0),
            700.0,
            true,
            &Theme::dark(),
            &mut renderer,
        );
        assert!(
            expanded
                .texts()
                .iter()
                .any(|line| line.contains("llama.cpp on AMD Radeon 780M"))
        );
        assert!(
            expanded
                .targets()
                .iter()
                .any(|(_, target)| *target == OfferTarget::Alternative(0))
        );
        assert!(review_offer(&[]).is_none());
    }

    #[test]
    fn the_pick_tag_stays_inside_the_app_and_off_the_node() {
        let app = Rect::new(0.0, 100.0, 800.0, 700.0);
        // Room above: above.
        let tag = tag_rect(Rect::new(10.0, 300.0, 90.0, 330.0), app, (120.0, 24.0), 8.0);
        assert_eq!(tag.y1, 292.0);
        // At the app's top edge: below, never over the chrome above the app.
        let top = Rect::new(10.0, 104.0, 90.0, 134.0);
        let tag = tag_rect(top, app, (120.0, 24.0), 8.0);
        assert!(tag.y0 >= top.y1, "{tag:?}");
        assert!(app.contains(tag.origin()));
        // A node filling the app: inside its top edge.
        let whole = Rect::new(0.0, 100.0, 800.0, 700.0);
        let tag = tag_rect(whole, app, (120.0, 24.0), 8.0);
        assert!(app.contains(tag.origin()) && tag.y1 <= app.y1);
    }

    #[test]
    fn a_word_span_is_its_byte_offset() {
        assert_eq!(word_spans("café  noir"), vec![(0, "café"), (7, "noir")]);
    }

    #[test]
    fn the_composer_sits_beside_what_it_is_about() {
        let viewport = Rect::new(0.0, 0.0, 1200.0, 800.0);
        let anchor = Rect::new(100.0, 100.0, 200.0, 140.0);
        let origin = RemarkView::composer_origin(viewport, Some(anchor), (400.0, 500.0), 16.0);
        assert_eq!(origin, Point::new(216.0, 100.0));
        let right_edge = Rect::new(1000.0, 100.0, 1180.0, 140.0);
        let origin = RemarkView::composer_origin(viewport, Some(right_edge), (400.0, 500.0), 16.0);
        assert_eq!(
            origin.x,
            1000.0 - 16.0 - 400.0,
            "it moves to the other side"
        );
    }
}
