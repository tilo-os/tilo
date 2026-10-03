//! The **shared guided-customization panel** — the one component that collects
//! a Tilo install profile (identity, region, options) for *both* imager flows:
//! writing a pre-configured image to a stick, and installing Tilo onto a disk.
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
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::local_state::LocalState;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Button, ButtonFace, ControlSize, Dropdown, TextInput, Widget};

use crate::Gaps;
use crate::actions::{IMAGER_ACTION_TYPE, ImagerAction};
use crate::model::{Filesystem, InstallProfile, installer_locales};

const EXPORT_LABEL: &str = "Preview embedded setup";

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
    Filesystem,
}

impl CzField {
    const ALL: [CzField; 7] = [
        CzField::Hostname,
        CzField::FullName,
        CzField::Username,
        CzField::Locale,
        CzField::Timezone,
        CzField::Keyboard,
        CzField::Filesystem,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Hostname => "Hostname",
            Self::FullName => "Full name",
            Self::Username => "Username",
            Self::Locale => "Language and locale",
            Self::Timezone => "Time zone",
            Self::Keyboard => "Keyboard layout",
            Self::Filesystem => "Filesystem",
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
            Self::Filesystem => "filesystem",
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
    filesystem: Rect,
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
            CzField::Filesystem => self.filesystem,
        }
    }
}

/// What the host measures for the panel: where it starts under the step
/// title, the section and caption line boxes, the control height, the
/// Preview button's width and the theme's gaps.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CzAnchors {
    top: f64,
    section_h: f64,
    label_h: f64,
    field_h: f64,
    export_w: f64,
    gaps: Gaps,
    /// The theme's text scale, so the panel's compact decision is the
    /// wizard's (`crate::compact_content`).
    text_scale: f64,
}

impl Default for CzAnchors {
    /// The unmeasured estimate on the unscaled scale, replaced by the first
    /// measurement.
    fn default() -> Self {
        let gaps = Gaps::default();
        Self {
            top: gaps.group * 2.0,
            section_h: gaps.group,
            label_h: gaps.section,
            field_h: gaps.region,
            export_w: gaps.region * 6.0,
            gaps,
            text_scale: 1.0,
        }
    }
}

/// What the host app should do after handing an event to the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CzResponse {
    /// The panel consumed the event.
    pub handled: bool,
    /// The user asked to export the secret-free media preconfiguration.
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
    filesystem: TextInput,
    locale_choice: Dropdown,
    timezone_choice: Dropdown,
    keyboard_choice: Dropdown,
    filesystem_choice: Dropdown,
    timezone_catalog: Vec<String>,
    focused: Option<CzField>,
    export_focused: bool,
    last_mouse: Point,
    anchors: CzAnchors,
}

impl Default for CustomizePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl CustomizePanel {
    pub fn new() -> Self {
        let field = TextInput::local;
        let locale_state = LocalState::new("en_US.UTF-8".to_string());
        let timezone_state = LocalState::new("UTC".to_string());
        let keyboard_state = LocalState::new("English (US)".to_string());
        let filesystem_state = LocalState::new(Filesystem::Btrfs.label().to_string());
        let timezone_catalog = timezones();
        Self {
            hostname: field(),
            full_name: field(),
            username: field(),
            locale: field(),
            timezone: field(),
            keyboard: field(),
            filesystem: field(),
            locale_choice: Dropdown::from_state(
                installer_locales().map(str::to_string).collect(),
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
            filesystem_choice: Dropdown::from_state(
                Filesystem::all()
                    .into_iter()
                    .map(|value| value.label().to_string())
                    .collect(),
                &filesystem_state,
            )
            .with_id("imager.filesystem.choice")
            .with_accessible_name("Filesystem")
            .with_placeholder("Choose a filesystem"),
            timezone_catalog,
            focused: None,
            export_focused: false,
            last_mouse: Point::ZERO,
            anchors: CzAnchors::default(),
        }
    }

    fn is_choice(f: CzField) -> bool {
        matches!(
            f,
            CzField::Locale | CzField::Timezone | CzField::Keyboard | CzField::Filesystem
        )
    }

    fn choice(&mut self, f: CzField) -> Option<&mut Dropdown> {
        match f {
            CzField::Locale => Some(&mut self.locale_choice),
            CzField::Timezone => Some(&mut self.timezone_choice),
            CzField::Keyboard => Some(&mut self.keyboard_choice),
            CzField::Filesystem => Some(&mut self.filesystem_choice),
            CzField::Hostname | CzField::FullName | CzField::Username => None,
        }
    }

    fn choice_ref(&self, f: CzField) -> Option<&Dropdown> {
        match f {
            CzField::Locale => Some(&self.locale_choice),
            CzField::Timezone => Some(&self.timezone_choice),
            CzField::Keyboard => Some(&self.keyboard_choice),
            CzField::Filesystem => Some(&self.filesystem_choice),
            CzField::Hostname | CzField::FullName | CzField::Username => None,
        }
    }

    fn choice_display_value(profile: &InstallProfile, f: CzField) -> String {
        match f {
            CzField::Keyboard => keyboard_label(&profile.keyboard).to_string(),
            CzField::Filesystem => profile.filesystem.label().to_string(),
            _ => Self::profile_value(profile, f).to_string(),
        }
    }

    fn choice_profile_value(f: CzField, displayed: &str) -> Option<String> {
        match f {
            CzField::Keyboard => keyboard_id(displayed).map(str::to_string),
            CzField::Filesystem => Filesystem::all()
                .into_iter()
                .find(|filesystem| filesystem.label() == displayed)
                .map(|filesystem| filesystem.key().to_string()),
            CzField::Locale => installer_locales()
                .any(|locale| locale == displayed)
                .then(|| displayed.to_string()),
            CzField::Timezone => Some(displayed.to_string()),
            _ => None,
        }
    }

    fn choice_is_valid(&self, f: CzField) -> bool {
        let Some(value) = self.choice_ref(f).map(Dropdown::selected) else {
            return false;
        };
        match f {
            CzField::Locale => installer_locales().any(|locale| locale == value),
            CzField::Timezone => self.timezone_catalog.iter().any(|zone| zone == value),
            CzField::Keyboard => keyboard_id(value).is_some(),
            CzField::Filesystem => Filesystem::all()
                .into_iter()
                .any(|filesystem| filesystem.label() == value),
            _ => true,
        }
    }

    /// Normalize an assistive-technology value through the same catalogs the
    /// visible choosers expose. Free-text identity fields remain unchanged.
    pub(crate) fn canonical_value(&self, f: CzField, value: &str) -> Option<String> {
        match f {
            CzField::Locale => installer_locales()
                .any(|locale| locale == value)
                .then(|| value.to_string()),
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
            CzField::Filesystem => Filesystem::from_key(&value.to_ascii_lowercase())
                .or_else(|| {
                    Filesystem::all()
                        .into_iter()
                        .find(|filesystem| filesystem.label() == value)
                })
                .map(|filesystem| filesystem.key().to_string()),
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
            CzField::Filesystem => &mut self.filesystem,
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
            CzField::Filesystem => &self.filesystem,
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
            CzField::Filesystem => profile.filesystem.key(),
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
            CzField::Filesystem => ImagerAction::SetFilesystem {
                filesystem: Filesystem::from_key(&value).unwrap_or_default(),
            },
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

    /// Refresh the anchors from the renderer and theme that will paint the
    /// panel; `top` is the host's title band. Returns whether the geometry
    /// moved.
    pub fn measure_layout(&mut self, tr: &mut TextRenderer, theme: &Theme, top: f64) -> bool {
        let line = |tr: &mut TextRenderer, role: TextRole| {
            f64::from(tr.measure_style("Ag", None, &theme.text(role)).1)
        };
        let next = CzAnchors {
            top,
            section_h: line(tr, TextRole::Subheading),
            label_h: line(tr, TextRole::Label),
            field_h: ButtonFace::height(ControlSize::Regular, theme),
            export_w: ButtonFace::new(Rect::ZERO, EXPORT_LABEL).intrinsic_width(tr, theme),
            gaps: Gaps::of(&theme.tokens.spacing),
            text_scale: f64::from(theme.text_scale),
        };
        let changed = next != self.anchors;
        self.anchors = next;
        changed
    }

    /// The bottom of the panel.
    pub fn extent(&self, content: Rect) -> f64 {
        self.geom(content).export.y1
    }

    /// Shared vertical rhythm so section titles, field captions and inputs
    /// never touch: a caption sits a `tight` step above its field, fields of
    /// one section follow each other a `section` step apart, the next
    /// section (and the Preview action) a `group` later. Pure function of
    /// `content` and the measured anchors, so draw + hit-test agree.
    fn geom(&self, content: Rect) -> CzGeom {
        let x0 = content.x0;
        let x1 = content.x1;
        let a = self.anchors;
        let g = a.gaps;
        // The host's breakpoint: the form stacks exactly when the wizard
        // chrome is compact.
        let compact = crate::compact_content(content.width(), a.text_scale);
        let fh = a.field_h;
        let caption = a.label_h + g.tight;
        let row_pitch = fh + g.section + caption;
        let identity_label_y = content.y0 + a.top;
        let host_y = identity_label_y + a.section_h + g.related + caption;
        let hostname = Rect::new(x0, host_y, x1, host_y + fh);
        let r2 = host_y + row_pitch;
        let fw = (content.width() - g.section) / 2.0;
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
            let r3 = r2 + row_pitch;
            (
                Rect::new(x0, r2, x0 + fw, r2 + fh),
                Rect::new(x0 + fw + g.section, r2, x1, r2 + fh),
                Rect::new(x0, r3, x0 + fw, r3 + fh),
                Rect::new(x0 + fw + g.section, r3, x1, r3 + fh),
                r3,
            )
        };
        let region_label_y = final_row_y + fh + g.group;
        let kb_y = region_label_y + a.section_h + g.related + caption;
        let (keyboard, filesystem, option_bottom) = if compact {
            let filesystem_y = kb_y + row_pitch;
            (
                Rect::new(x0, kb_y, x1, kb_y + fh),
                Rect::new(x0, filesystem_y, x1, filesystem_y + fh),
                filesystem_y + fh,
            )
        } else {
            (
                Rect::new(x0, kb_y, x0 + fw, kb_y + fh),
                Rect::new(x0 + fw + g.section, kb_y, x1, kb_y + fh),
                kb_y + fh,
            )
        };
        let export_w = a.export_w.min(content.width());
        let export = Rect::new(
            x1 - export_w,
            option_bottom + g.group,
            x1,
            option_bottom + g.group + fh,
        );
        CzGeom {
            identity_label_y,
            hostname,
            full_name,
            username,
            locale,
            timezone,
            region_label_y,
            keyboard,
            filesystem,
            export,
        }
    }

    // ---- drawing ----------------------------------------------------------

    fn section_title(ctx: &mut PaintCtx<'_>, s: &str, at: Point) {
        let ink = ctx.theme.text_primary;
        ctx.text_role(s, TextRole::Subheading, ink, at, None);
    }

    /// One field under its caption: the toolkit's own input or choice.
    fn draw_field(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx, f: CzField, r: Rect) {
        let choice_valid = !Self::is_choice(f) || self.choice_is_valid(f);
        // The caption sits on its own line box a `tight` step above the field.
        let label_top = r.y0 - self.anchors.gaps.tight - self.anchors.label_h;
        let dim = ctx.theme.text_dim;
        ctx.text_role(
            f.label(),
            TextRole::Label,
            dim,
            Point::new(r.x0, label_top),
            None,
        );
        if !choice_valid {
            // The field's problem, on its caption line at the trailing edge.
            let message = "Choose a value from the list";
            let style = ctx.theme.text(TextRole::Caption);
            let width = f64::from(ctx.text.measure_style(message, None, &style).0);
            let danger = ctx.theme.tokens.colors.semantic.danger;
            ctx.text(
                message,
                &style,
                danger,
                Point::new(r.x1 - width, label_top),
                None,
            );
        }
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        if Self::is_choice(f) {
            if let Some(choice) = self.choice(f) {
                choice.apply_layout_external(r);
                choice.draw(scene, t, tr, theme, presence);
            }
            return;
        }
        let input = self.input(f);
        input.apply_layout_external(r);
        Widget::draw(input, scene, t, tr, theme, presence);
    }

    /// Render the whole panel into `content` for the given profile snapshot.
    pub fn draw(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx, content: Rect) {
        let g = self.geom(content);
        Self::section_title(ctx, "Identity", Point::new(content.x0, g.identity_label_y));
        for field in [
            CzField::Hostname,
            CzField::FullName,
            CzField::Username,
            CzField::Timezone,
            CzField::Locale,
        ] {
            self.draw_field(ctx, presence, field, g.field(field));
        }
        // The last host-side preferences. Security, the storage target and
        // destructive choices are completed on the device.
        Self::section_title(
            ctx,
            "Keyboard and storage",
            Point::new(content.x0, g.region_label_y),
        );
        for field in [CzField::Keyboard, CzField::Filesystem] {
            self.draw_field(ctx, presence, field, g.field(field));
        }
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &ButtonFace::new(g.export, EXPORT_LABEL)
                .hovered(g.export.contains(self.last_mouse))
                .focused(self.export_focused),
        );
    }

    /// The open choice list, in the window's top layer so a long filtered
    /// time-zone list is never painted under the following fields or
    /// clipped by the scrolling step body.
    pub fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for field in [
            CzField::Locale,
            CzField::Timezone,
            CzField::Keyboard,
            CzField::Filesystem,
        ] {
            if let Some(choice) = self.choice(field) {
                choice.draw_overlay(scene, transform, text_renderer, theme, presence);
            }
        }
    }

    // ---- interaction ------------------------------------------------------

    /// Route a winit event into the panel. Text edits and toggles dispatch
    /// their [`ImagerAction`] through `host`; the returned [`CzResponse`] tells
    /// the host whether the event was consumed and whether export was asked for.
    pub(crate) fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        host: &mut crate::resource::ActionSink,
        profile: &InstallProfile,
        target_removable: bool,
        content: Rect,
    ) -> CzResponse {
        let g = self.geom(content);
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(event, ctx, position.x, position.y, &g)
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_left_click(event, ctx, host, profile, target_removable, &g),
            WindowEvent::KeyboardInput { event: key_ev, .. }
                if key_ev.state == ElementState::Pressed =>
            {
                self.handle_key_event(event, ctx, key_ev, host, &g)
            }
            _ => CzResponse {
                handled: false,
                export_requested: false,
            },
        }
    }

    /// Track the pointer and forward it to a focused field so drag-selection
    /// tracks along.
    fn handle_cursor_moved(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        px: f64,
        py: f64,
        g: &CzGeom,
    ) -> CzResponse {
        self.last_mouse = Point::new(px / ctx.scale_factor, py / ctx.scale_factor);
        if let Some(f) = self.focused {
            let inner = g.field(f);
            let (mut h, mut ft) = (false, None);
            if let Some(choice) = self.choice(f) {
                choice.apply_layout_external(g.field(f));
                choice.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        handled: &mut h,
                        focus_target: &mut ft,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
            } else {
                let input = self.input(f);
                input.apply_layout_external(inner);
                input.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        handled: &mut h,
                        focus_target: &mut ft,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
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
    fn handle_left_click(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        host: &mut crate::resource::ActionSink,
        profile: &InstallProfile,
        _target_removable: bool,
        g: &CzGeom,
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
                    &mut ui::widget::EventCtx {
                        handled: &mut handled,
                        focus_target: &mut focus_target,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
                choice.selected().to_string()
            } else {
                String::new()
            };
            if selected != before
                && let Some(value) = Self::choice_profile_value(field, &selected)
            {
                Self::dispatch(ctx.state, host, &Self::field_action(field, value));
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
                self.focus_field(event, ctx, f, g.field(f));
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
    fn handle_key_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        key_ev: &winit::event::KeyEvent,
        host: &mut crate::resource::ActionSink,
        g: &CzGeom,
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
                    &mut ui::widget::EventCtx {
                        handled: &mut handled,
                        focus_target: &mut focus_target,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
                (before, choice.selected().to_string())
            } else {
                (String::new(), String::new())
            };
            if let Some(value) = Self::choice_profile_value(f, &selected)
                && selected != before
            {
                Self::dispatch(ctx.state, host, &Self::field_action(f, value));
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
        let inner = field_rect;
        let input = self.input(f);
        input.set_focus(true);
        input.apply_layout_external(inner);
        let (mut h, mut ft) = (false, None);
        input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                handled: &mut h,
                focus_target: &mut ft,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        let value = self.input(f).value().to_string();
        Self::dispatch(ctx.state, host, &Self::field_action(f, value));
        CzResponse {
            handled: true,
            export_requested: false,
        }
    }

    fn focus_field(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        f: CzField,
        field_rect: Rect,
    ) {
        self.blur();
        self.focused = Some(f);
        if let Some(choice) = self.choice(f) {
            choice.set_focus(true);
            choice.apply_layout_external(field_rect);
            let (mut handled, mut focus_target) = (false, None);
            choice.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            return;
        }
        let inner = field_rect;
        let input = self.input(f);
        input.set_focus(true);
        input.apply_layout_external(inner);
        let (mut h, mut ft) = (false, None);
        input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                handled: &mut h,
                focus_target: &mut ft,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
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

    fn dispatch(
        doc: &mut AutoCommit,
        host: &mut crate::resource::ActionSink,
        action: &ImagerAction,
    ) {
        host.dispatch_with_context(
            doc,
            IMAGER_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.imager", "Imager", action),
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

    /// Zero-based keyboard slot: seven editable fields followed by Preview.
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

    pub(crate) fn keyboard_slot_rect(&self, content: Rect, slot: usize) -> Option<Rect> {
        let geom = self.geom(content);
        CzField::ALL
            .get(slot)
            .map(|field| geom.field(*field))
            .or_else(|| (slot == CzField::ALL.len()).then_some(geom.export))
    }

    /// Semantic controls in the same order as keyboard traversal: seven real
    /// text editors followed by the profile preview action.
    pub(crate) fn accessibility(&self, content: Rect) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let geom = self.geom(content);
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

    /// Timezone catalog and canonical values.
    #[test]
    fn customization_catalogs_match_the_embedded_profile_contract() {
        // Phase `built_in_timezone_catalog_is_worldwide_canonical_and_safe`.
        {
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
                    && zone.bytes().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, b'_' | b'/' | b'+' | b'-')
                    })
            }));
        }

        // Phase `canonical_values_match_the_embedded_profile_contract`.
        {
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
            assert_eq!(
                panel.canonical_value(CzField::Filesystem, "xfs"),
                Some("xfs".into())
            );
            assert_eq!(panel.canonical_value(CzField::Filesystem, "FAT32"), None);
        }
    }

    /// Customization form layout.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn customization_layout_keeps_rhythm_and_stacks_for_large_text() {
        // Phase `compact_layout_stacks_fields_at_full_width_for_large_text`.
        {
            let content = Rect::new(24.0, 10.0, 344.0, 900.0);
            let geometry = CustomizePanel::new().geom(content);
            for field in CzField::ALL {
                let rect = geometry.field(field);
                assert_eq!(rect.x0, content.x0);
                assert_eq!(rect.x1, content.x1);
            }
            assert!(geometry.full_name.y1 < geometry.username.y0);
            assert!(geometry.username.y1 < geometry.timezone.y0);
            assert!(geometry.timezone.y1 < geometry.locale.y0);
            assert!(geometry.locale.y1 < geometry.keyboard.y0);
            assert!(geometry.keyboard.y1 < geometry.filesystem.y0);
        }

        // Phase `captions_fields_and_sections_keep_their_rhythm`.
        // Every caption keeps a `tight` step above its field, every field of
        // a section a `section` step before the next caption, and the two
        // sections and the Preview action a `group` — at a wide and a
        // compact width, with the unmeasured and a measured anchor set.
        {
            let mut panel = CustomizePanel::new();
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            for measured in [false, true] {
                if measured {
                    assert!(panel.measure_layout(&mut tr, &theme, 61.0));
                }
                let a = panel.anchors;
                let g = a.gaps;
                let caption = a.label_h + g.tight;
                for content in [
                    Rect::new(190.0, 90.0, 1090.0, 700.0),
                    Rect::new(16.0, 90.0, 304.0, 700.0),
                ] {
                    let geom = panel.geom(content);
                    assert!(
                        (geom.hostname.y0
                            - (geom.identity_label_y + a.section_h + g.related + caption))
                            .abs()
                            < 1e-6
                    );
                    let mut tops: Vec<f64> = [
                        geom.hostname,
                        geom.full_name,
                        geom.username,
                        geom.timezone,
                        geom.locale,
                    ]
                    .iter()
                    .map(|r| r.y0)
                    .collect();
                    tops.dedup();
                    for pair in tops.windows(2) {
                        assert!(
                            (pair[1] - (pair[0] + a.field_h + g.section + caption)).abs() < 1e-6
                        );
                    }
                    assert!((geom.region_label_y - (geom.locale.y1 + g.group)).abs() < 1e-6);
                    let option_bottom = geom.keyboard.y1.max(geom.filesystem.y1);
                    assert!((geom.export.y0 - (option_bottom + g.group)).abs() < 1e-6);
                    let fields = [
                        geom.hostname,
                        geom.full_name,
                        geom.username,
                        geom.timezone,
                        geom.locale,
                        geom.keyboard,
                    ];
                    assert!(fields.iter().all(|f| (f.height() - a.field_h).abs() < 1e-6));
                    let mut rows: Vec<Rect> = fields.iter().copied().chain([geom.export]).collect();
                    rows.sort_by(|x, y| x.y0.total_cmp(&y.y0));
                    for pair in rows.windows(2) {
                        if pair[1].y0 > pair[0].y0 + 1e-6 {
                            assert!(
                                pair[1].y0 - pair[0].y1 >= g.section - 1e-6,
                                "rows never touch ({} to {})",
                                pair[0].y1,
                                pair[1].y0
                            );
                        }
                    }
                    assert!(geom.export.x0 >= content.x0, "Preview fits the column");
                }
            }
        }
    }
}
