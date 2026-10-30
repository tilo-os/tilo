//! The transient readout for the hardware keys: volume, brightness, the
//! keyboard's backlight, playback and the machine toggles.
//!
//! A key that changes something invisible (the volume) or unreadable (the
//! brightness, *because* it is what changed) needs an answer on screen: a
//! slab that appears, shows the new level, and gets out of the way. It never
//! takes focus or the pointer and never outlives the moment it was about.
//! It is deliberately not a notification, which persists and stacks:
//! pressing volume up eight times shows one slab counting up, not eight
//! banners.
//!
//! Whoever handled the key shows it through its published action
//! (`desktop:osd/show`), as any caller would.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use taffy::{NodeId, Style, TaffyTree};
use tilo_actions::{
    ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionMode, OperationKind,
    ParamType, ResourceSpec, ResultSpec,
};
use ui::a11y::{AccessNode, Politeness, Role};
use ui::presence::PresenceCtx;
use ui::text_render::{TextKind, TextWeight};
use ui::tokens::TextRole;
use ui::widget::{TaffyTreeExt as _, Widget};
use ui::{TextRenderer, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;

use crate::i18n::{t, t_args};

/// The operation type the readout's action carries.
pub const OPERATION: &str = "tilo.desktop.osd";
/// The readout's resource.
pub const OSD_RESOURCE: &str = "desktop:osd";

/// How long the readout stays up after the last change, fade included:
/// long enough to read after one tap, short enough that a burst of presses
/// does not leave it hanging around.
pub const OSD_HOLD: Duration = Duration::from_millis(1_500);

/// The slab's size, logical pixels: a host places it this big.
pub const OSD_SIZE: (u32, u32) = (320, 56);

/// What the readout is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OsdKind {
    Volume,
    Brightness,
    KeyboardBacklight,
    Microphone,
    AirplaneMode,
    Touchpad,
    Play,
    Pause,
    NextTrack,
    PreviousTrack,
    Stop,
}

impl OsdKind {
    /// Every kind, as its published name spells it.
    pub const ALL: [Self; 11] = [
        Self::Volume,
        Self::Brightness,
        Self::KeyboardBacklight,
        Self::Microphone,
        Self::AirplaneMode,
        Self::Touchpad,
        Self::Play,
        Self::Pause,
        Self::NextTrack,
        Self::PreviousTrack,
        Self::Stop,
    ];

    /// Whether a level bar shows beside the caption.
    #[must_use]
    pub const fn has_level(self) -> bool {
        matches!(
            self,
            Self::Volume | Self::Brightness | Self::KeyboardBacklight
        )
    }
}

/// One readout: what it is about, its level (`0.0 ..= 1.0`) and whether
/// the thing is on (sound not muted, the radio or touchpad on).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Osd {
    pub kind: OsdKind,
    #[serde(default)]
    pub level: f32,
    #[serde(default = "on")]
    pub on: bool,
}

const fn on() -> bool {
    true
}

/// `level` as a whole percentage.
fn percent(level: f32) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the level is clamped to 0..=1, so the percentage is 0..=100"
    )]
    let percent = (level.clamp(0.0, 1.0) * 100.0).round() as u32;
    percent
}

impl Osd {
    /// The one line the slab reads.
    #[must_use]
    pub fn caption(&self) -> String {
        let level = || [("percent", percent(self.level).into())];
        match self.kind {
            OsdKind::Volume if !self.on => t("osd-muted"),
            OsdKind::Volume => t_args("osd-volume", &level()),
            OsdKind::Brightness => t_args("osd-brightness", &level()),
            OsdKind::KeyboardBacklight if self.level <= 0.0 => t("osd-keyboard-backlight-off"),
            OsdKind::KeyboardBacklight => t_args("osd-keyboard-backlight", &level()),
            OsdKind::Microphone if self.on => t("osd-microphone-on"),
            OsdKind::Microphone => t("osd-microphone-muted"),
            OsdKind::AirplaneMode if self.on => t("osd-airplane-mode-on"),
            OsdKind::AirplaneMode => t("osd-airplane-mode-off"),
            OsdKind::Touchpad if self.on => t("osd-touchpad-on"),
            OsdKind::Touchpad => t("osd-touchpad-off"),
            OsdKind::Play => t("osd-play"),
            OsdKind::Pause => t("osd-pause"),
            OsdKind::NextTrack => t("osd-next-track"),
            OsdKind::PreviousTrack => t("osd-previous-track"),
            OsdKind::Stop => t("osd-stop"),
        }
    }
}

/// The readout's resource: `show`, with what to show.
#[must_use]
#[expect(clippy::expect_used, reason = "a readout always serializes to JSON")]
pub fn osd_resource() -> ResourceSpec {
    let mut spec = ActionSpec::typed_operation(
        "show",
        t("desktop-action-show-osd"),
        String::new(),
        OperationKind::Effect,
        OPERATION,
        vec![
            ActionParam::required_owned(
                "kind",
                ParamType::Text,
                t("desktop-action-param-osd-kind"),
            ),
            ActionParam::optional_owned(
                "level",
                ParamType::Number,
                t("desktop-action-param-osd-level"),
            ),
            ActionParam::optional_owned("on", ParamType::Boolean, t("desktop-action-param-osd-on")),
        ],
        |args| {
            let kind = args
                .text("kind")
                .and_then(|kind| {
                    serde_json::from_value::<OsdKind>(serde_json::Value::String(kind.into())).ok()
                })
                .unwrap_or(OsdKind::Volume);
            #[allow(clippy::cast_possible_truncation, reason = "a level between 0 and 1")]
            let level = args.number("level").unwrap_or(0.0) as f32;
            let osd = Osd {
                kind,
                level: level.clamp(0.0, 1.0),
                on: args.boolean("on").unwrap_or(true),
            };
            EncodedAction::encode_operation(OPERATION, &osd).expect("a readout always serializes")
        },
    );
    spec.type_mut().contract = ActionContract {
        execution: ExecutionMode::Immediate,
        result: ResultSpec::None,
        ..ActionContract::default()
    };
    ResourceSpec::new(OSD_RESOURCE, "desktop", t("desktop-actions-osd")).with_action(spec)
}

/// The readout up now, and since when.
#[derive(Clone, Copy, Debug)]
struct Raised {
    osd: Osd,
    /// When this run of readouts appeared: a burst of presses keeps it.
    shown_since: Instant,
    /// When it last changed: it stays [`OSD_HOLD`] after.
    changed_at: Instant,
}

/// The slab, as a widget filling the rectangle its host gives it.
pub struct OsdSlab {
    raised: Option<Raised>,
    rect: Rect,
}

impl Default for OsdSlab {
    fn default() -> Self {
        Self::new()
    }
}

impl OsdSlab {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            raised: None,
            rect: Rect::ZERO,
        }
    }

    /// Show `osd` now: a readout already up changes in place and stays up
    /// longer.
    pub fn show(&mut self, osd: Osd) {
        let now = ui::time::now();
        let shown_since = self
            .raised
            .filter(|raised| live(raised, now))
            .map_or(now, |raised| raised.shown_since);
        self.raised = Some(Raised {
            osd,
            shown_since,
            changed_at: now,
        });
    }

    /// The readout up now, if any.
    #[must_use]
    pub fn current(&self) -> Option<Osd> {
        let now = ui::time::now();
        self.raised
            .filter(|raised| live(raised, now))
            .map(|raised| raised.osd)
    }
}

/// When `raised` is gone.
fn expires(raised: &Raised) -> Instant {
    raised
        .changed_at
        .checked_add(OSD_HOLD)
        .unwrap_or(raised.changed_at)
}

/// Whether `raised` is still up at `now`.
fn live(raised: &Raised, now: Instant) -> bool {
    now < expires(raised)
}

/// How opaque `raised` is at `now`: easing in, held, easing out.
fn alpha(raised: &Raised, now: Instant) -> f32 {
    ui::motion::Transient::opacity(
        ui::motion::Transient::READOUT,
        (raised.shown_since, expires(raised)),
        now,
        ui::tokens::EffectiveMotion::default(),
    )
}

impl ui::resources::PublishesResources for OsdSlab {
    fn resources(&self, _: &ui::automerge::AutoCommit) -> Vec<ResourceSpec> {
        vec![osd_resource()]
    }
}

impl ui::host_operation::ResourceOperations for OsdSlab {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[OPERATION]
    }

    fn start_operation(
        &mut self,
        op: &ui::host_operation::ResourceOperation<'_>,
        _doc: &mut ui::widget::Edits<'_>,
        _path: &ui::automerge::ObjId,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped().filter(|request| request.type_id == OPERATION) else {
            return Ok(None);
        };
        let osd: Osd =
            serde_json::from_slice(request.payload).map_err(|error| error.to_string())?;
        self.show(osd);
        Ok(Some(ui::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

impl Widget for OsdSlab {
    ui::provides!(
        dyn ui::resources::PublishesResources,
        dyn ui::host_operation::ResourceOperations
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: taffy::Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let now = ui::time::now();
        let Some(raised) = self.raised else {
            return;
        };
        if !live(&raised, now) {
            self.raised = None;
            return;
        }
        let alpha = alpha(&raised, now);
        if alpha <= 0.0 {
            return;
        }
        let rect = self.rect.inset(-1.0);
        if alpha < 0.999 {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                alpha,
                transform,
                &self.rect,
            );
        }
        let painted = ui::chrome::shell::paint_panel(scene, transform, rect, theme, 1.0);
        let theme = &painted.theme;
        let colors = &theme.tokens.colors;
        let pad = f64::from(theme.tokens.spacing.s4);
        let inner = rect.inset(-pad);
        let style = theme
            .text(TextRole::Label)
            .with_weight(TextWeight::Medium)
            .with_kind(TextKind::Numeric);
        let caption = raised.osd.caption();
        let (_, height) = text.measure_style(&caption, None, &style);
        let caption_at = Point::new(inner.x0, inner.center().y - f64::from(height) / 2.0);
        if raised.osd.kind.has_level() {
            // The bar fills the right of the slab; the caption ends it. A
            // muted bar keeps its level in secondary ink.
            let height = ui::widget::ProgressBarFace::height(theme);
            let bar_width = inner.width() * 0.45;
            let bar = Rect::new(
                inner.x1 - bar_width,
                inner.center().y - height / 2.0,
                inner.x1,
                inner.center().y + height / 2.0,
            );
            let face = ui::widget::ProgressBarFace::new(raised.osd.level.clamp(0.0, 1.0));
            let face = if raised.osd.on {
                face
            } else {
                face.tone(ui::widget::Tone::Neutral)
            };
            face.paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
                bar,
            );
        }
        text.draw_style(
            scene,
            &caption,
            colors.text_primary,
            transform.pre_translate(caption_at.to_vec2()),
            None,
            &style,
        );
        if alpha < 0.999 {
            scene.pop_layer();
        }
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let osd = self.current()?;
        Some(
            AccessNode::new(Role::Label)
                .with_name(osd.caption())
                .with_live(Politeness::Polite),
        )
    }

    fn needs_redraw(&self) -> bool {
        self.raised
            .is_some_and(|raised| live(&raised, ui::time::now()))
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "OsdSlab"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captions_read_the_level_and_the_state() {
        ui::i18n::I18nManager::init("en-US");
        let volume = Osd {
            kind: OsdKind::Volume,
            level: 0.554,
            on: true,
        };
        assert_eq!(volume.caption(), "Volume 55%");
        assert_eq!(
            Osd {
                on: false,
                ..volume
            }
            .caption(),
            "Muted"
        );
        assert_eq!(
            Osd {
                kind: OsdKind::Touchpad,
                level: 0.0,
                on: false,
            }
            .caption(),
            "Touchpad off"
        );
    }

    #[test]
    fn a_readout_shows_by_its_action_changes_in_place_and_goes() {
        let _settled = ui::motion::testing::settled();
        let mut slab = OsdSlab::new();
        assert!(slab.current().is_none());
        let resource = osd_resource();
        let spec = &resource.actions[0];
        let mut arguments = serde_json::Map::new();
        arguments.insert("kind".into(), "brightness".into());
        arguments.insert("level".into(), 0.3.into());
        let encoded = tilo_actions::door::encode_json(
            std::slice::from_ref(&resource),
            &tilo_actions::ActionAddress::new(OSD_RESOURCE, spec.id.as_str()),
            &arguments,
        )
        .unwrap();
        let osd: Osd = serde_json::from_slice(&encoded.payload).unwrap();
        slab.show(osd);
        assert_eq!(
            slab.current().map(|osd| osd.kind),
            Some(OsdKind::Brightness)
        );
        slab.show(Osd { level: 0.4, ..osd });
        assert_eq!(slab.current().map(|osd| osd.level), Some(0.4));
    }
}
