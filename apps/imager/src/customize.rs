//! The **shared guided-customization panel** — the one component that collects
//! a qOS install profile (identity, region, options) for *both* imager flows:
//! writing a pre-configured image to a stick, and installing qOS onto a disk.
//!
//! ## Why a self-contained component
//!
//! The imager and the installer both need the same "who is this machine, and
//! who logs in" step, over the same [`InstallProfile`]. Rather than each app
//! hand-rolling its own form (with subtly different validation, focus, and
//! a11y), this panel owns the whole thing once:
//!
//! * every text field is a real shared [`TextInput`] — caret, selection,
//!   clipboard, IME and a11y come for free. Secrets stay off the host and are
//!   collected only on the target machine (HIG §3: never hand-roll editing);
//! * every edit dispatches the matching [`ImagerAction`] through the app's
//!   [`ui::ActionHost`], so a change is one undo boundary, replicates to
//!   collaborators, and is byte-identical to the action the Ctrl+K palette
//!   would dispatch — the single source of truth in [`crate::actions`].
//!
//! The panel holds no profile of its own: it renders from an [`InstallProfile`]
//! snapshot each frame and writes changes back through actions, so the document
//! stays authoritative. It is deliberately free of any imager-wizard coupling
//! (no step/`Hit` types leak in) so the installer can embed the same panel.

use automerge::AutoCommit;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::local_state::LocalState;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Dropdown, TextInput, Widget};

use crate::actions::{IMAGER_ACTION_TYPE, ImagerAction};
use crate::model::InstallProfile;

const LOCALES: &[&str] = &[
    "ar_SA.UTF-8",
    "cs_CZ.UTF-8",
    "da_DK.UTF-8",
    "de_DE.UTF-8",
    "en_GB.UTF-8",
    "en_US.UTF-8",
    "es_ES.UTF-8",
    "fi_FI.UTF-8",
    "fr_FR.UTF-8",
    "it_IT.UTF-8",
    "ja_JP.UTF-8",
    "ko_KR.UTF-8",
    "nb_NO.UTF-8",
    "nl_NL.UTF-8",
    "pl_PL.UTF-8",
    "pt_BR.UTF-8",
    "pt_PT.UTF-8",
    "ru_RU.UTF-8",
    "sv_SE.UTF-8",
    "tr_TR.UTF-8",
    "uk_UA.UTF-8",
    "zh_CN.UTF-8",
    "zh_TW.UTF-8",
];

const KEYBOARDS: &[(&str, &str)] = &[
    ("English (US)", "us"),
    ("English (UK)", "gb"),
    ("German (QWERTZ)", "de"),
    ("French (AZERTY)", "fr"),
    ("Spanish", "es"),
    ("Italian", "it"),
    ("Portuguese", "pt"),
    ("Portuguese (Brazil)", "br"),
    ("Russian (ЙЦУКЕН)", "ru"),
    ("Swedish", "se"),
    ("Norwegian", "no"),
    ("Danish", "dk"),
    ("Swiss", "ch"),
    ("Polish", "pl"),
    ("Dvorak", "dvorak"),
    ("Colemak", "colemak"),
];

const IANA_TIMEZONES: &str = include_str!("iana_timezones.txt");

fn keyboard_label(id: &str) -> &str {
    KEYBOARDS
        .iter()
        .find(|(_, candidate)| *candidate == id)
        .map_or(id, |(label, _)| *label)
}

fn keyboard_id(label: &str) -> Option<&'static str> {
    KEYBOARDS
        .iter()
        .find(|(candidate, _)| *candidate == label)
        .map(|(_, id)| *id)
}

fn timezones() -> Vec<String> {
    // A comprehensive canonical catalog is compiled into every build. Runtime
    // discovery does not determine whether Windows/macOS users get a worldwide
    // chooser, and cannot introduce host-only aliases that the live image may
    // reject at its zoneinfo trust boundary.
    let mut zones = IANA_TIMEZONES
        .lines()
        .map(str::trim)
        .filter(|zone| !zone.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    zones.sort();
    zones.dedup();
    zones
}

/// One editable text field of the profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CzField {
    Hostname,
    FullName,
    Username,
    Locale,
    Timezone,
    Keyboard,
}

impl CzField {
    const ALL: [CzField; 6] = [
        CzField::Hostname,
        CzField::FullName,
        CzField::Username,
        CzField::Locale,
        CzField::Timezone,
        CzField::Keyboard,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Hostname => "Hostname",
            Self::FullName => "Full name",
            Self::Username => "Username",
            Self::Locale => "Language and locale",
            Self::Timezone => "Time zone",
            Self::Keyboard => "Keyboard layout",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Hostname => "hostname",
            Self::FullName => "full-name",
            Self::Username => "username",
            Self::Locale => "locale",
            Self::Timezone => "timezone",
            Self::Keyboard => "keyboard",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.key() == key)
    }
}

/// The exact rects of the panel for one frame — computed once from the content
/// rect so drawing and hit-testing can never drift apart.
struct CzGeom {
    identity_label_y: f64,
    hostname: Rect,
    full_name: Rect,
    username: Rect,
    locale: Rect,
    timezone: Rect,
    region_label_y: f64,
    keyboard: Rect,
    export: Rect,
}

impl CzGeom {
    fn field(&self, f: CzField) -> Rect {
        match f {
            CzField::Hostname => self.hostname,
            CzField::FullName => self.full_name,
            CzField::Username => self.username,
            CzField::Locale => self.locale,
            CzField::Timezone => self.timezone,
            CzField::Keyboard => self.keyboard,
        }
    }
}

/// What the host app should do after handing an event to the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CzResponse {
    /// The panel consumed the event.
    pub handled: bool,
    /// The user asked to export the unattended-install config.
    pub export_requested: bool,
}

/// The shared guided-customization panel. Embeds one [`TextInput`] per
/// non-secret field and renders the media-profile preview around them.
pub struct CustomizePanel {
    hostname: TextInput,
    full_name: TextInput,
    username: TextInput,
    locale: TextInput,
    timezone: TextInput,
    keyboard: TextInput,
    locale_choice: Dropdown,
    timezone_choice: Dropdown,
    keyboard_choice: Dropdown,
    timezone_catalog: Vec<String>,
    focused: Option<CzField>,
    export_focused: bool,
    last_mouse: Point,
}

impl Default for CustomizePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl CustomizePanel {
    pub fn new() -> Self {
        let field = || {
            let mut t = TextInput::local();
            t.set_plain_chrome(true); // the panel draws the field box itself
            t
        };
        let locale_state = LocalState::new("en_US.UTF-8".to_string());
        let timezone_state = LocalState::new("UTC".to_string());
        let keyboard_state = LocalState::new("English (US)".to_string());
        let timezone_catalog = timezones();
        Self {
            hostname: field(),
            full_name: field(),
            username: field(),
            locale: field(),
            timezone: field(),
            keyboard: field(),
            locale_choice: Dropdown::from_state(
                LOCALES.iter().map(|value| (*value).to_string()).collect(),
                &locale_state,
            )
            .with_id("imager.locale.choice")
            .with_accessible_name("Language and locale")
            .with_placeholder("Choose a locale"),
            timezone_choice: Dropdown::from_state(timezone_catalog.clone(), &timezone_state)
                .with_id("imager.timezone.choice")
                .with_accessible_name("Time zone")
                .with_placeholder("Choose a time zone"),
            keyboard_choice: Dropdown::from_state(
                KEYBOARDS
                    .iter()
                    .map(|(label, _)| (*label).to_string())
                    .collect(),
                &keyboard_state,
            )
            .with_id("imager.keyboard.choice")
            .with_accessible_name("Keyboard layout")
            .with_placeholder("Choose a keyboard layout"),
            timezone_catalog,
            focused: None,
            export_focused: false,
            last_mouse: Point::ZERO,
        }
    }

    fn is_choice(f: CzField) -> bool {
        matches!(f, CzField::Locale | CzField::Timezone | CzField::Keyboard)
    }

    fn choice(&mut self, f: CzField) -> Option<&mut Dropdown> {
        match f {
            CzField::Locale => Some(&mut self.locale_choice),
            CzField::Timezone => Some(&mut self.timezone_choice),
            CzField::Keyboard => Some(&mut self.keyboard_choice),
            CzField::Hostname | CzField::FullName | CzField::Username => None,
        }
    }

    fn choice_ref(&self, f: CzField) -> Option<&Dropdown> {
        match f {
            CzField::Locale => Some(&self.locale_choice),
            CzField::Timezone => Some(&self.timezone_choice),
            CzField::Keyboard => Some(&self.keyboard_choice),
            CzField::Hostname | CzField::FullName | CzField::Username => None,
        }
    }

    fn choice_display_value(profile: &InstallProfile, f: CzField) -> String {
        match f {
            CzField::Keyboard => keyboard_label(&profile.keyboard).to_string(),
            _ => Self::profile_value(profile, f).to_string(),
        }
    }

    fn choice_profile_value(f: CzField, displayed: &str) -> Option<String> {
        match f {
            CzField::Keyboard => keyboard_id(displayed).map(str::to_string),
            CzField::Locale => LOCALES.contains(&displayed).then(|| displayed.to_string()),
            CzField::Timezone => Some(displayed.to_string()),
            _ => None,
        }
    }

    fn choice_is_valid(&self, f: CzField) -> bool {
        let Some(value) = self.choice_ref(f).map(Dropdown::selected) else {
            return false;
        };
        match f {
            CzField::Locale => LOCALES.contains(&value),
            CzField::Timezone => self.timezone_catalog.iter().any(|zone| zone == value),
            CzField::Keyboard => keyboard_id(value).is_some(),
            _ => true,
        }
    }

    /// Normalize an assistive-technology value through the same catalogs the
    /// visible choosers expose. Free-text identity fields remain unchanged.
    pub(crate) fn canonical_value(&self, f: CzField, value: &str) -> Option<String> {
        match f {
            CzField::Locale => LOCALES.contains(&value).then(|| value.to_string()),
            CzField::Timezone => self
                .timezone_catalog
                .iter()
                .find(|zone| zone.as_str() == value)
                .cloned(),
            CzField::Keyboard => keyboard_id(value)
                .or_else(|| {
                    KEYBOARDS
                        .iter()
                        .find(|(_, id)| *id == value)
                        .map(|(_, id)| *id)
                })
                .map(str::to_string),
            CzField::Hostname | CzField::FullName | CzField::Username => Some(value.to_string()),
        }
    }

    fn input(&mut self, f: CzField) -> &mut TextInput {
        match f {
            CzField::Hostname => &mut self.hostname,
            CzField::FullName => &mut self.full_name,
            CzField::Username => &mut self.username,
            CzField::Locale => &mut self.locale,
            CzField::Timezone => &mut self.timezone,
            CzField::Keyboard => &mut self.keyboard,
        }
    }

    fn input_ref(&self, f: CzField) -> &TextInput {
        match f {
            CzField::Hostname => &self.hostname,
            CzField::FullName => &self.full_name,
            CzField::Username => &self.username,
            CzField::Locale => &self.locale,
            CzField::Timezone => &self.timezone,
            CzField::Keyboard => &self.keyboard,
        }
    }

    fn profile_value(profile: &InstallProfile, f: CzField) -> &str {
        match f {
            CzField::Hostname => &profile.hostname,
            CzField::FullName => &profile.user.full_name,
            CzField::Username => &profile.user.username,
            CzField::Locale => &profile.locale,
            CzField::Timezone => &profile.timezone,
            CzField::Keyboard => &profile.keyboard,
        }
    }

    /// The [`ImagerAction`] that sets `f` to `value`. The single source of truth
    /// shared with the Ctrl+K field specs in [`crate::actions`].
    pub(crate) fn field_action(f: CzField, value: String) -> ImagerAction {
        match f {
            CzField::Hostname => ImagerAction::SetHostname { value },
            CzField::FullName => ImagerAction::SetFullName { value },
            CzField::Username => ImagerAction::SetUsername { value },
            CzField::Locale => ImagerAction::SetLocale { value },
            CzField::Timezone => ImagerAction::SetTimezone { value },
            CzField::Keyboard => ImagerAction::SetKeyboard { value },
        }
    }

    /// Whether the panel currently owns keyboard focus (a field is editing).
    pub fn is_focused(&self) -> bool {
        self.focused.is_some() || self.export_focused
    }

    /// Drop keyboard focus from whichever field held it (the host calls this on
    /// a step change so a stale caret does not linger).
    pub fn clear_focus(&mut self) {
        self.blur();
    }

    /// Stage one chooser open for the deterministic screenshot harness. This
    /// mutates peer-local widget state only; production opens the same chooser
    /// through its normal pointer/keyboard event path.
    pub(crate) fn open_timezone_for_shot(&mut self) {
        let state = LocalState::new(self.timezone_choice.selected().to_string());
        self.timezone_choice = Dropdown::from_state(self.timezone_catalog.clone(), &state)
            .with_id("imager.timezone.choice")
            .with_accessible_name("Time zone")
            .with_placeholder("Choose a time zone")
            .open();
        self.focused = Some(CzField::Timezone);
    }

    /// Pull each unfocused field's text from the profile so a loaded / peer-
    /// edited value shows. While a field is focused its [`TextInput`] is the
    /// source of truth and writes back through an action, so it is left alone.
    pub fn sync(&mut self, profile: &InstallProfile) {
        for f in CzField::ALL {
            let focused = self.focused == Some(f);
            if Self::is_choice(f) {
                let value = Self::choice_display_value(profile, f);
                if let Some(choice) = self.choice(f) {
                    choice.set_focus(focused);
                    if !focused {
                        choice.set_selected(value);
                    }
                }
                continue;
            }
            let value = Self::profile_value(profile, f).to_string();
            let input = self.input(f);
            input.set_focus(focused);
            if !focused {
                input.set_text(&value);
            }
        }
    }

    // ---- geometry ---------------------------------------------------------

    /// Shared vertical rhythm so section labels, field labels and inputs never
    /// overlap. Pure function of `content`, so draw + hit-test agree.
    fn geom(content: Rect) -> CzGeom {
        let x0 = content.x0;
        let x1 = content.x1;
        let compact = content.width() < 520.0;
        let fh = 38.0;
        let row_pitch = fh + if compact { 32.0 } else { 40.0 }; // input + label + gap
        let identity_label_y = content.y0 + if compact { 72.0 } else { 52.0 };
        let host_y = content.y0 + if compact { 104.0 } else { 92.0 };
        let hostname = Rect::new(x0, host_y, x1, host_y + fh);
        let r2 = host_y + row_pitch;
        let (full_name, username, timezone, locale, final_row_y) = if compact {
            let r3 = r2 + row_pitch;
            let r4 = r3 + row_pitch;
            let r5 = r4 + row_pitch;
            (
                Rect::new(x0, r2, x1, r2 + fh),
                Rect::new(x0, r3, x1, r3 + fh),
                Rect::new(x0, r4, x1, r4 + fh),
                Rect::new(x0, r5, x1, r5 + fh),
                r5,
            )
        } else {
            let fw = (content.width() - 16.0) / 2.0;
            let r3 = r2 + row_pitch;
            (
                Rect::new(x0, r2, x0 + fw, r2 + fh),
                Rect::new(x0 + fw + 16.0, r2, x1, r2 + fh),
                Rect::new(x0, r3, x0 + fw, r3 + fh),
                Rect::new(x0 + fw + 16.0, r3, x1, r3 + fh),
                r3,
            )
        };
        let region_label_y = final_row_y + fh + 26.0;
        let kb_y = region_label_y + 40.0;
        let keyboard = Rect::new(x0, kb_y, x1, kb_y + fh);
        let export = Rect::new(x1 - 250.0, kb_y + fh + 30.0, x1, kb_y + fh + 62.0);
        CzGeom {
            identity_label_y,
            hostname,
            full_name,
            username,
            locale,
            timezone,
            region_label_y,
            keyboard,
            export,
        }
    }

    // ---- drawing ----------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn txt(
        tr: &mut TextRenderer,
        scene: &mut Scene,
        s: &str,
        size: f32,
        color: Color,
        x: f64,
        top: f64,
        max_w: Option<f32>,
        weight: TextWeight,
    ) {
        tr.draw_text_weighted(
            scene,
            s,
            size,
            color,
            Affine::translate((x, top)),
            max_w,
            weight,
        );
    }

    fn fill_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color) {
        scene.fill(
            Fill::NonZero,
            t,
            c,
            None,
            &RoundedRect::from_rect(r, radius),
        );
    }
    fn stroke_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color, w: f64) {
        scene.stroke(
            &Stroke::new(w),
            t,
            c,
            None,
            &RoundedRect::from_rect(r, radius),
        );
    }

    fn section_label(
        tr: &mut TextRenderer,
        scene: &mut Scene,
        theme: &Theme,
        s: &str,
        x: f64,
        y: f64,
    ) {
        Self::txt(
            tr,
            scene,
            s,
            11.0,
            theme.tokens.colors.text_dim,
            x,
            y,
            None,
            TextWeight::Semibold,
        );
    }

    /// Draw + hit-forward one text field (box + label + the shared input glyphs).
    #[allow(clippy::too_many_arguments)]
    fn draw_field(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        presence: &PresenceCtx,
        f: CzField,
        r: Rect,
        label: &str,
    ) {
        let c = &theme.tokens.colors;
        let choice_valid = !Self::is_choice(f) || self.choice_is_valid(f);
        Self::txt(
            tr,
            scene,
            label,
            11.0,
            if choice_valid {
                c.text_dim
            } else {
                c.semantic.danger
            },
            r.x0 + 2.0,
            r.y0 - 17.0,
            None,
            TextWeight::Semibold,
        );
        let focused = self.focused == Some(f);
        if Self::is_choice(f) {
            if let Some(choice) = self.choice(f) {
                choice.apply_layout_external(r);
                choice.draw(scene, t, tr, theme, presence);
            }
            if !choice_valid {
                Self::stroke_rr(scene, t, r, 9.0, c.semantic.danger, 2.0);
                let message = "Choose a listed value";
                let (width, _) = tr.measure(message, 10.0, None);
                Self::txt(
                    tr,
                    scene,
                    message,
                    10.0,
                    c.semantic.danger,
                    r.x1 - f64::from(width) - 4.0,
                    r.y0 - 16.0,
                    Some((r.width() - 8.0).max(0.0) as f32),
                    TextWeight::Regular,
                );
            }
            return;
        }
        Self::fill_rr(scene, t, r, 9.0, c.neutral[2]);
        Self::stroke_rr(
            scene,
            t,
            r,
            9.0,
            if focused {
                c.border_focus
            } else {
                c.border_standard
            },
            if focused { 1.6 } else { 1.0 },
        );
        // The plain-chrome input paints its own glyphs + caret inside the box.
        let inner = Rect::new(r.x0 + 12.0, r.y0, r.x1 - 12.0, r.y1);
        let input = self.input(f);
        input.apply_layout_external(inner);
        Widget::draw(input, scene, t, tr, theme, presence);
    }

    /// Render the whole panel into `content` for the given profile snapshot.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        content: Rect,
        profile: &InstallProfile,
        target_removable: bool,
    ) {
        let g = Self::geom(content);
        self.draw_identity_fields(scene, tr, theme, t, presence, content.x0, &g);
        self.draw_region_and_options(
            scene,
            tr,
            theme,
            t,
            presence,
            content.x0,
            &g,
            profile,
            target_removable,
        );
        self.draw_export_button(scene, tr, theme, t, &g);
        // Dropdown lists are a top-layer pass so a long filtered time-zone
        // result is never painted underneath the following fields.
        for field in [CzField::Locale, CzField::Timezone, CzField::Keyboard] {
            if let Some(choice) = self.choice(field) {
                choice.draw_overlay(scene, t, tr, theme, presence);
            }
        }
    }

    /// The IDENTITY section: hostname, name, username and time zone.
    #[allow(clippy::too_many_arguments)]
    fn draw_identity_fields(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        presence: &PresenceCtx,
        label_x: f64,
        g: &CzGeom,
    ) {
        Self::section_label(tr, scene, theme, "IDENTITY", label_x, g.identity_label_y);
        self.draw_field(
            scene,
            tr,
            theme,
            t,
            presence,
            CzField::Hostname,
            g.hostname,
            "Hostname",
        );
        self.draw_field(
            scene,
            tr,
            theme,
            t,
            presence,
            CzField::FullName,
            g.full_name,
            "Full name",
        );
        self.draw_field(
            scene,
            tr,
            theme,
            t,
            presence,
            CzField::Username,
            g.username,
            "Username",
        );
        self.draw_field(
            scene,
            tr,
            theme,
            t,
            presence,
            CzField::Timezone,
            g.timezone,
            "Time zone",
        );
        self.draw_field(
            scene,
            tr,
            theme,
            t,
            presence,
            CzField::Locale,
            g.locale,
            "Language & locale",
        );
    }

    /// The REGION section contains the last host-side preference. Security,
    /// storage target and destructive choices are completed on the device.
    #[allow(clippy::too_many_arguments)]
    fn draw_region_and_options(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        presence: &PresenceCtx,
        label_x: f64,
        g: &CzGeom,
        _profile: &InstallProfile,
        _target_removable: bool,
    ) {
        Self::section_label(
            tr,
            scene,
            theme,
            "REGION & OPTIONS",
            label_x,
            g.region_label_y,
        );
        self.draw_field(
            scene,
            tr,
            theme,
            t,
            presence,
            CzField::Keyboard,
            g.keyboard,
            "Keyboard",
        );
    }

    /// Preview the exact profile which will be embedded into the medium.
    fn draw_export_button(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        g: &CzGeom,
    ) {
        let c = &theme.tokens.colors;
        let hov = g.export.contains(self.last_mouse);
        Self::fill_rr(
            scene,
            t,
            g.export,
            9.0,
            if hov { c.neutral[3] } else { c.neutral[2] },
        );
        Self::stroke_rr(scene, t, g.export, 9.0, c.accent[6], 1.0);
        if self.export_focused {
            Self::stroke_rr(scene, t, g.export, 9.0, c.border_focus, 2.0);
        }
        let (ew, _) =
            tr.measure_weighted("Preview embedded setup", 12.0, None, TextWeight::Semibold);
        Self::txt(
            tr,
            scene,
            "Preview embedded setup",
            12.0,
            c.accent[7],
            f64::midpoint(g.export.x0, g.export.x1) - f64::from(ew) / 2.0,
            g.export.y0 + 8.0,
            None,
            TextWeight::Semibold,
        );
    }

    // ---- interaction ------------------------------------------------------

    /// Route a winit event into the panel. Text edits and toggles dispatch
    /// their [`ImagerAction`] through `host`; the returned [`CzResponse`] tells
    /// the host whether the event was consumed and whether export was asked for.
    #[allow(clippy::too_many_arguments)]
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        path: &automerge::ObjId,
        host: &mut ui::ActionHost,
        profile: &InstallProfile,
        target_removable: bool,
        content: Rect,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) -> CzResponse {
        let g = Self::geom(content);
        match event {
            WindowEvent::CursorMoved { position, .. } => self.handle_cursor_moved(
                event, position.x, position.y, scale, *modifiers, doc, path, &g, node_id, taffy,
            ),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_left_click(
                event,
                scale,
                *modifiers,
                doc,
                path,
                host,
                profile,
                target_removable,
                &g,
                node_id,
                taffy,
            ),
            WindowEvent::KeyboardInput { event: key_ev, .. }
                if key_ev.state == ElementState::Pressed =>
            {
                self.handle_key_event(
                    event, key_ev, scale, *modifiers, doc, path, host, &g, node_id, taffy,
                )
            }
            _ => CzResponse {
                handled: false,
                export_requested: false,
            },
        }
    }

    /// Track the pointer and forward it to a focused field so drag-selection
    /// tracks along.
    #[allow(clippy::too_many_arguments)]
    fn handle_cursor_moved(
        &mut self,
        event: &WindowEvent,
        px: f64,
        py: f64,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
        path: &automerge::ObjId,
        g: &CzGeom,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) -> CzResponse {
        self.last_mouse = Point::new(px / scale, py / scale);
        if let Some(f) = self.focused {
            let inner = Rect::new(
                g.field(f).x0 + 12.0,
                g.field(f).y0,
                g.field(f).x1 - 12.0,
                g.field(f).y1,
            );
            let (mut h, mut ft) = (false, None);
            if let Some(choice) = self.choice(f) {
                choice.apply_layout_external(g.field(f));
                choice.handle_event(
                    event, scale, &modifiers, doc, path, &mut h, &mut ft, node_id, taffy,
                );
            } else {
                let input = self.input(f);
                input.apply_layout_external(inner);
                input.handle_event(
                    event, scale, &modifiers, doc, path, &mut h, &mut ft, node_id, taffy,
                );
            }
        }
        CzResponse {
            handled: false,
            export_requested: false,
        }
    }

    /// A left click: focus a field and forward the press for caret placement,
    /// dispatch a toggle, request export, or (on empty space) drop focus.
    #[allow(clippy::too_many_arguments)]
    fn handle_left_click(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
        path: &automerge::ObjId,
        host: &mut ui::ActionHost,
        profile: &InstallProfile,
        _target_removable: bool,
        g: &CzGeom,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) -> CzResponse {
        let p = self.last_mouse;
        if let Some(field) = self.focused
            && Self::is_choice(field)
        {
            let before = Self::choice_display_value(profile, field);
            let mut handled = false;
            let mut focus_target = None;
            let selected = if let Some(choice) = self.choice(field) {
                choice.apply_layout_external(g.field(field));
                choice.handle_event(
                    event,
                    scale,
                    &modifiers,
                    doc,
                    path,
                    &mut handled,
                    &mut focus_target,
                    node_id,
                    taffy,
                );
                choice.selected().to_string()
            } else {
                String::new()
            };
            if selected != before
                && let Some(value) = Self::choice_profile_value(field, &selected)
            {
                Self::dispatch(doc, host, &Self::field_action(field, value));
            }
            if handled {
                return CzResponse {
                    handled: true,
                    export_requested: false,
                };
            }
        }
        for f in CzField::ALL {
            if g.field(f).contains(p) {
                self.focus_field(
                    f,
                    g.field(f),
                    event,
                    scale,
                    modifiers,
                    doc,
                    path,
                    node_id,
                    taffy,
                );
                return CzResponse {
                    handled: true,
                    export_requested: false,
                };
            }
        }
        if g.export.contains(p) {
            return CzResponse {
                handled: true,
                export_requested: true,
            };
        }
        // A click elsewhere drops field focus.
        self.blur();
        CzResponse {
            handled: false,
            export_requested: false,
        }
    }

    /// A key press while a field is focused: Enter/Escape blur it, anything
    /// else routes to the input and dispatches its fresh value.
    #[allow(clippy::too_many_arguments)]
    fn handle_key_event(
        &mut self,
        event: &WindowEvent,
        key_ev: &winit::event::KeyEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
        path: &automerge::ObjId,
        host: &mut ui::ActionHost,
        g: &CzGeom,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) -> CzResponse {
        if self.export_focused {
            return match &key_ev.logical_key {
                Key::Named(NamedKey::Enter) => CzResponse {
                    handled: true,
                    export_requested: true,
                },
                Key::Named(NamedKey::Escape) => {
                    self.blur();
                    CzResponse {
                        handled: true,
                        export_requested: false,
                    }
                }
                _ => CzResponse {
                    handled: false,
                    export_requested: false,
                },
            };
        }
        let Some(f) = self.focused else {
            return CzResponse {
                handled: false,
                export_requested: false,
            };
        };
        if Self::is_choice(f) {
            let mut handled = false;
            let mut focus_target = None;
            let rect = g.field(f);
            let (before, selected) = if let Some(choice) = self.choice(f) {
                let before = choice.selected().to_string();
                choice.apply_layout_external(rect);
                choice.handle_event(
                    event,
                    scale,
                    &modifiers,
                    doc,
                    path,
                    &mut handled,
                    &mut focus_target,
                    node_id,
                    taffy,
                );
                (before, choice.selected().to_string())
            } else {
                (String::new(), String::new())
            };
            if let Some(value) = Self::choice_profile_value(f, &selected)
                && selected != before
            {
                Self::dispatch(doc, host, &Self::field_action(f, value));
            }
            return CzResponse {
                handled,
                export_requested: false,
            };
        }
        if let Key::Named(NamedKey::Enter | NamedKey::Escape) = &key_ev.logical_key {
            self.blur();
            return CzResponse {
                handled: true,
                export_requested: false,
            };
        }
        // Route the keystroke to the focused input, then dispatch the field's
        // absolute-set action with its fresh value.
        let field_rect = g.field(f);
        let inner = Rect::new(
            field_rect.x0 + 12.0,
            field_rect.y0,
            field_rect.x1 - 12.0,
            field_rect.y1,
        );
        let input = self.input(f);
        input.set_focus(true);
        input.apply_layout_external(inner);
        let (mut h, mut ft) = (false, None);
        input.handle_event(
            event, scale, &modifiers, doc, path, &mut h, &mut ft, node_id, taffy,
        );
        let value = self.input(f).value().to_string();
        Self::dispatch(doc, host, &Self::field_action(f, value));
        CzResponse {
            handled: true,
            export_requested: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn focus_field(
        &mut self,
        f: CzField,
        field_rect: Rect,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
        path: &automerge::ObjId,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        self.blur();
        self.focused = Some(f);
        if let Some(choice) = self.choice(f) {
            choice.set_focus(true);
            choice.apply_layout_external(field_rect);
            let (mut handled, mut focus_target) = (false, None);
            choice.handle_event(
                event,
                scale,
                &modifiers,
                doc,
                path,
                &mut handled,
                &mut focus_target,
                node_id,
                taffy,
            );
            return;
        }
        let inner = Rect::new(
            field_rect.x0 + 12.0,
            field_rect.y0,
            field_rect.x1 - 12.0,
            field_rect.y1,
        );
        let input = self.input(f);
        input.set_focus(true);
        input.apply_layout_external(inner);
        let (mut h, mut ft) = (false, None);
        input.handle_event(
            event, scale, &modifiers, doc, path, &mut h, &mut ft, node_id, taffy,
        );
    }

    fn blur(&mut self) {
        self.export_focused = false;
        if let Some(f) = self.focused.take() {
            if let Some(choice) = self.choice(f) {
                choice.set_focus(false);
            } else {
                self.input(f).set_focus(false);
            }
        }
    }

    fn dispatch(doc: &mut AutoCommit, host: &mut ui::ActionHost, action: &ImagerAction) {
        host.dispatch_with_context(
            doc,
            IMAGER_ACTION_TYPE,
            action,
            qos_actions::ActionContext::for_action("org.qos.Imager", "Imager", action),
        );
    }

    /// Focus a field programmatically (test/host helper).
    #[cfg(test)]
    pub fn focus_for_test(&mut self, f: CzField) {
        self.blur();
        self.focused = Some(f);
        if let Some(choice) = self.choice(f) {
            choice.set_focus(true);
        } else {
            self.input(f).set_focus(true);
        }
    }

    #[cfg(test)]
    pub fn field_value(&mut self, f: CzField) -> String {
        self.choice_ref(f).map_or_else(
            || self.input_ref(f).value().to_string(),
            |choice| choice.selected().to_string(),
        )
    }

    /// Zero-based keyboard slot: six editable fields followed by Preview.
    pub(crate) fn keyboard_slot(&self) -> Option<usize> {
        self.focused
            .and_then(|field| {
                CzField::ALL
                    .iter()
                    .position(|candidate| *candidate == field)
            })
            .or_else(|| self.export_focused.then_some(CzField::ALL.len()))
    }

    pub(crate) fn focus_keyboard_slot(&mut self, slot: usize) {
        self.blur();
        if let Some(field) = CzField::ALL.get(slot).copied() {
            self.focused = Some(field);
            if let Some(choice) = self.choice(field) {
                choice.set_focus(true);
            } else {
                self.input(field).set_focus(true);
            }
        } else if slot == CzField::ALL.len() {
            self.export_focused = true;
        }
    }

    pub(crate) fn keyboard_slot_rect(content: Rect, slot: usize) -> Option<Rect> {
        let geom = Self::geom(content);
        CzField::ALL
            .get(slot)
            .map(|field| geom.field(*field))
            .or_else(|| (slot == CzField::ALL.len()).then_some(geom.export))
    }

    /// Semantic controls in the same order as keyboard traversal: six real
    /// text editors followed by the profile preview action.
    pub(crate) fn accessibility(&self, content: Rect) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let geom = Self::geom(content);
        let mut controls = CzField::ALL
            .into_iter()
            .map(|field| {
                let rect = geom.field(field);
                let target = format!("imager:profile:{}", field.key());
                let choice = Self::is_choice(field);
                let valid = !choice || self.choice_is_valid(field);
                let node = if choice {
                    self.choice_ref(field)
                        .and_then(Widget::accessibility)
                        .unwrap_or_else(|| AccessNode::new(Role::ComboBox))
                } else {
                    self.input_ref(field)
                        .accessibility()
                        .unwrap_or_else(|| AccessNode::new(Role::TextInput))
                };
                let node = node
                    .with_name(field.label())
                    .with_state(AccessState {
                        focused: self.focused == Some(field),
                        focusable: true,
                        required: matches!(field, CzField::Hostname | CzField::Username),
                        invalid: !valid
                            || (matches!(field, CzField::Hostname | CzField::Username)
                                && self.input_ref(field).value().trim().is_empty()),
                        ..Default::default()
                    })
                    .with_bounds(rect)
                    .with_action_target(target);
                if choice {
                    node.with_description("Choose a canonical value; type while open to filter")
                } else {
                    node
                }
            })
            .collect::<Vec<_>>();
        controls.push(
            AccessNode::new(Role::Button)
                .with_name("Preview embedded setup")
                .with_description("Review the non-secret setup profile stored on this medium")
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.export_focused),
                )
                .with_bounds(geom.export)
                .with_action_target("imager:profile:preview"),
        );
        controls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_timezone_catalog_is_worldwide_canonical_and_safe() {
        let zones = timezones();
        assert!(zones.len() >= 400, "catalog unexpectedly incomplete");
        for representative in [
            "Africa/Nairobi",
            "America/New_York",
            "Antarctica/McMurdo",
            "Asia/Kolkata",
            "Atlantic/Reykjavik",
            "Australia/Sydney",
            "Europe/Berlin",
            "Pacific/Auckland",
            "UTC",
        ] {
            assert!(zones.iter().any(|zone| zone == representative));
        }
        assert!(zones.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(zones.iter().all(|zone| {
            !zone.starts_with('/')
                && !zone.contains("..")
                && zone
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'/' | b'+' | b'-'))
        }));
    }

    #[test]
    fn canonical_values_match_the_embedded_profile_contract() {
        let panel = CustomizePanel::new();
        assert_eq!(
            panel.canonical_value(CzField::Keyboard, "German (QWERTZ)"),
            Some("de".into())
        );
        assert_eq!(
            panel.canonical_value(CzField::Keyboard, "de"),
            Some("de".into())
        );
        assert_eq!(
            panel.canonical_value(CzField::Timezone, "Europe/Berlin"),
            Some("Europe/Berlin".into())
        );
        assert_eq!(
            panel.canonical_value(CzField::Timezone, "Europe/DefinitelyMissing"),
            None
        );
        assert_eq!(panel.canonical_value(CzField::Locale, "not a locale"), None);
    }

    #[test]
    fn compact_layout_stacks_fields_at_full_width_for_large_text() {
        let content = Rect::new(24.0, 10.0, 344.0, 900.0);
        let geometry = CustomizePanel::geom(content);
        for field in CzField::ALL {
            let rect = geometry.field(field);
            assert_eq!(rect.x0, content.x0);
            assert_eq!(rect.x1, content.x1);
        }
        assert!(geometry.full_name.y1 < geometry.username.y0);
        assert!(geometry.username.y1 < geometry.timezone.y0);
        assert!(geometry.timezone.y1 < geometry.locale.y0);
        assert!(geometry.locale.y1 < geometry.keyboard.y0);
    }
}
