pub use crate::theme_document::{
    Preset, THEME_SCHEMA_VERSION, ThemeDensity, ThemeDocument, ThemeMaterial, ThemeMotion,
    ThemeShape, ThemeType,
};
pub use crate::theme_layers::{
    APPEARANCE_SECTION, AccessibilityLayer, CUSTOM_THEMES_KEY, ContentSurface, DARK_THEME_KEY,
    DENSITY_KEY, DISTRIBUTION_DEFAULTS_KEY, DISTRIBUTION_DEFAULTS_SCHEMA_VERSION,
    DISTRIBUTION_LAYER_ID, DistributionDefaults, DistributionPolicy, LIGHT_THEME_KEY, MODE_KEY,
    PIN_DENSITY, PIN_EMPHASIS, PIN_MATERIAL, PIN_MOTION, PIN_PALETTE, PIN_PRESENCE, PIN_SHAPE,
    PIN_TYPE, PINNABLE, ThemeLayerStack, WORKSPACE_TEMPERATURES_KEY, resolve_theme_layers,
    user_document, workspace_temperature_override,
};
pub use crate::tokens::contrast::{ContrastFinding, ContrastFloor, Role as ContrastRole};
use crate::tokens::{Colors, PresenceHue, Tokens};
pub use settings_store::day_schedule::{AUTOMATIC_MODE, is_automatic as is_automatic_mode};
use vello::peniko::Color;

fn mix_spring(
    a: crate::tokens::SpringParams,
    b: crate::tokens::SpringParams,
    t: f32,
) -> crate::tokens::SpringParams {
    crate::tokens::SpringParams {
        stiffness: a.stiffness + (b.stiffness - a.stiffness) * t,
        damping: a.damping + (b.damping - a.damping) * t,
        mass: a.mass + (b.mass - a.mass) * t,
    }
}

/// `1` leaves the stock springs. Below that the snappy spring moves toward
/// subtle; above it, toward bouncy. Heavy and tracking stay put.
fn apply_motion_character(motion: &mut crate::tokens::Motion, character: f32) {
    let stock = motion.springs;
    let calm = (1.0 - character).clamp(0.0, 1.0);
    let lively = (character - 1.0).clamp(0.0, 1.0);
    motion.springs.snappy = mix_spring(
        mix_spring(stock.snappy, stock.subtle, calm),
        stock.bouncy,
        lively,
    );
    motion.springs.bouncy = mix_spring(stock.bouncy, stock.snappy, calm);
}

fn clamp_to(value: f32, band: &std::ops::RangeInclusive<f32>) -> f32 {
    value.clamp(*band.start(), *band.end())
}

/// The colour tokens `document` paints with for the person wearing `you`,
/// repaired where a floor is missed.
fn colors_of(document: &ThemeDocument, you: PresenceHue) -> Colors {
    let mut colors =
        Colors::of_palette(&document.palette, document.emphasis, document.presence, you);
    colors.set_material(
        clamp_to(document.material.blur, &ThemeMaterial::BLUR_USABLE),
        clamp_to(
            document.material.tint_opacity,
            &ThemeMaterial::TINT_ADMITTED,
        ),
    );
    colors
}

/// Every token `document` produces, with values outside a role's usable
/// band clamped. The document is assumed admitted.
fn tokens_of(document: &ThemeDocument, you: PresenceHue) -> Tokens {
    let mut tokens = Tokens::around(colors_of(document, you));
    tokens.radii = crate::tokens::Radii::from_window_radius(clamp_to(
        document.shape.window_radius,
        &ThemeShape::USABLE,
    ));
    // Proportional on every step; `Metrics` stay at their defaults, so a
    // gutter preference never shrinks a pointer target.
    tokens.spacing = crate::tokens::Spacing::BASE
        .scaled(clamp_to(document.density.spacing, &ThemeDensity::USABLE));
    tokens.typography.set_face(&document.type_.face);
    tokens.typography.set_reading_face(&document.type_.reading);
    tokens
        .typography
        .set_ground(crate::text_render::TextGround::of(
            tokens.colors.surfaces.content,
        ));
    tokens.typography.distinct_letters = document.type_.distinguishes_letters();
    let size = clamp_to(document.type_.base_size, &ThemeType::USABLE) / ThemeType::BASE_SIZE;
    tokens.typography.scale = tokens.typography.scale.scaled(size);
    let character = clamp_to(document.motion.character, &ThemeMotion::ADMITTED);
    apply_motion_character(&mut tokens.motion, character);
    tokens.motion_character = character;
    tokens.selector = if document.palette.high_contrast {
        crate::tokens::SelectorFinish::OPAQUE
    } else {
        let selector = document.material.selector;
        crate::tokens::SelectorFinish {
            rim_span: clamp_to(
                selector.rim_span,
                &crate::tokens::SelectorFinish::RIM_SPAN_ADMITTED,
            ),
            rim_alpha: clamp_to(
                selector.rim_alpha,
                &crate::tokens::SelectorFinish::RIM_ALPHA_ADMITTED,
            ),
            hover_alpha: clamp_to(
                selector.hover_alpha,
                &crate::tokens::SelectorFinish::HOVER_ALPHA_ADMITTED,
            ),
            ..selector
        }
    };
    tokens
}

/// How many pages wide a pane is before a document's text grows
/// ([`Theme::document_zoom`]): a page then fills about two fifths of it.
pub const DOCUMENT_ROOM: f64 = 2.6;
/// The most a document's text grows in a wide pane: body text from 14 px to
/// 21 px, about what the same page reads at on a laptop held at arm's length
/// against a desktop display across the desk.
pub const MAX_DOCUMENT_ZOOM: f32 = 1.5;

#[derive(Debug, PartialEq)]
pub struct Theme {
    pub name: String,
    /// The document this theme was built from, as applied: after the
    /// workspace temperature and high contrast.
    pub document: ThemeDocument,
    pub tokens: Tokens,
    // Shortcuts — derived from tokens on construction. Grounds are not
    // among them: the framework paints `surfaces.content` and each region's
    // tier ([`Theme::region`]); a hover or pressed fill is the lift of the
    // ground the control sits on (`intent.selection_hover`/`selection`, or
    // `Colors::lift_on`).
    pub border_standard: Color,
    /// The focus ring: the person's own colour ([`crate::tokens::Intents::you`]).
    pub border_focus: Color,
    pub text_primary: Color,
    /// Second tier of the type hierarchy (captions, sub-labels).
    pub text_dim: Color,
    /// Quietest legible tier (timestamps, hints), below [`Theme::text_dim`].
    pub text_tertiary: Color,
    pub status_success: Color,
    /// The control radius ([`crate::tokens::Radii::control`]).
    pub radius_standard: f64,
    pub padding_standard: f32,
    pub gap_standard: f32,
    /// Carried on the theme (not passed through `draw`) so every widget
    /// reacts without new plumbing. Default `false`.
    pub reduce_motion: bool,
    /// Floating chrome draws its material opaque instead of over the blurred
    /// backdrop (`crate::floating_surface::Material`). Default `false`.
    pub reduce_transparency: bool,
    /// `1.0` = default. Baked into every [`Theme::text`] role by
    /// [`Theme::with_text_scale`]; kept here too so a widget can scale a
    /// size it computes itself.
    pub text_scale: f32,
    /// The "make everything bigger" knob — spacing, radii, and type scale
    /// together, distinct from [`Theme::text_scale`] (type only). Baked in
    /// by [`Theme::with_ui_scale`]. Composes with the compositor's
    /// per-output device scale: physical size = logical × `ui_scale` ×
    /// device-scale, text additionally × `text_scale`.
    pub ui_scale: f32,
    /// Width of an insertion caret in logical pixels. The configured theme
    /// folds in the accessibility preference once, keeping file I/O out of
    /// widget draw paths.
    pub caret_width: f64,
    /// High contrast or forced colours is the active track. Content surfaces
    /// ask [`Theme::colors_are_forced`] instead of comparing [`Theme::name`].
    pub forced_colors: bool,
    /// The shape of the surface the window is drawn on, in the coordinates
    /// of the scene being drawn, when its host says: rounded at the radius
    /// the compositor draws the window with, square while it is drawn edge
    /// to edge. `None` when no host said; nothing then keeps out of corners.
    /// Set where the window is drawn, not part of the configured theme.
    pub surface: Option<SurfaceShape>,
}

impl Clone for Theme {
    /// Out of line: a derived clone is inlined, with the token tables it copies, so every app that
    /// cloned a theme compiled one of its own.
    #[inline(never)]
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            document: self.document.clone(),
            tokens: self.tokens.clone(),
            border_standard: self.border_standard,
            border_focus: self.border_focus,
            text_primary: self.text_primary,
            text_dim: self.text_dim,
            text_tertiary: self.text_tertiary,
            status_success: self.status_success,
            radius_standard: self.radius_standard,
            padding_standard: self.padding_standard,
            gap_standard: self.gap_standard,
            reduce_motion: self.reduce_motion,
            reduce_transparency: self.reduce_transparency,
            text_scale: self.text_scale,
            ui_scale: self.ui_scale,
            caret_width: self.caret_width,
            forced_colors: self.forced_colors,
            surface: self.surface,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

/// Every shipped theme, so an assertion about "a theme's surfaces" covers
/// all of them. Crate-visible (test builds only) because sibling-module
/// widget tests sweep the same list.
#[cfg(test)]
pub(crate) const ALL_PRESETS_FOR_TEST: &[fn() -> Theme] = &[
    Theme::dark,
    || Theme::preset(Preset::DarkCool),
    || Theme::preset(Preset::DarkGraphite),
    || Theme::preset(Preset::DarkSoft),
    Theme::light,
    Theme::high_contrast,
    || Theme::preset(Preset::HighContrastLight),
];

/// Polled by [`crate::app::App`] once per frame and reconciled against the
/// live theme. Level-triggered like `fullscreen_intent`: a *steady* value is
/// correct, since the App only re-themes when a field differs from what's
/// applied, so a settings pane can return the same intent every open frame.
///
/// `base_theme` is the colour theme **only**; the App layers high-contrast,
/// UI scale, and text scale on top itself, keeping that bookkeeping in one
/// place.
#[derive(Clone, Debug)]
pub struct AppearanceIntent {
    pub base_theme: Theme,
    pub high_contrast: bool,
    pub reduce_motion: bool,
    pub text_scale: f32,
    pub ui_scale: f32,
}

/// The local wall-clock moment the day/night schedule is evaluated at, read
/// through the toolkit clock.
#[must_use]
pub fn local_moment() -> settings_store::day_schedule::LocalMoment {
    use chrono::{Datelike, Offset, Timelike};
    // Through the epoch offset: chrono converts only std's clock type, which
    // is not the toolkit clock in the browser.
    let since_ms = crate::time::unix_now_ms();
    let local = chrono::DateTime::from_timestamp(
        i64::try_from(since_ms / 1000).unwrap_or(i64::MAX),
        u32::try_from(since_ms % 1000).unwrap_or(0) * 1_000_000,
    )
    .unwrap_or_default()
    .with_timezone(&chrono::Local);
    settings_store::day_schedule::LocalMoment {
        day_of_year: u16::try_from(local.ordinal()).unwrap_or(1),
        minute_of_day: u16::try_from(local.hour() * 60 + local.minute()).unwrap_or(0),
        utc_offset_minutes: local.offset().fix().local_minus_utc() / 60,
    }
}

impl Theme {
    /// Derived on every colour change, so the terminal and every
    /// syntax-highlighting surface track the OS theme instead of a
    /// hard-coded one.
    fn derive_palettes(tokens: &mut Tokens) {
        tokens.terminal_palette = crate::tokens::TerminalPalette::from_colors(&tokens.colors);
        tokens.syntax = crate::tokens::SyntaxPalette::from_colors(&tokens.colors);
    }

    /// The theme `document` describes, worn by `you`. The document is
    /// assumed admitted; values outside a role's usable band are clamped and
    /// a missed contrast floor is repaired.
    fn built(document: ThemeDocument, you: PresenceHue) -> Self {
        let mut tokens = tokens_of(&document, you);
        Self::derive_palettes(&mut tokens);
        let c = &tokens.colors;
        Self {
            name: document.name.clone(),
            border_standard: c.border_standard,
            border_focus: c.border_focus,
            text_primary: c.text_primary,
            text_dim: c.text_dim,
            text_tertiary: c.text_tertiary,
            status_success: c.semantic.success,
            radius_standard: f64::from(tokens.radii.control),
            padding_standard: tokens.spacing.s3,
            gap_standard: tokens.spacing.s3,
            reduce_motion: false,
            reduce_transparency: false,
            text_scale: 1.0,
            ui_scale: 1.0,
            caret_width: 2.0,
            forced_colors: document.palette.high_contrast,
            surface: None,
            document,
            tokens,
        }
    }

    /// The theme a document describes, or `None` for one this build does not
    /// admit ([`ThemeDocument::validate`]). Values outside a role's usable
    /// band are clamped, and a pairing that misses a contrast floor is
    /// repaired, so every theme that materializes is legible.
    #[must_use]
    pub fn from_document(document: &ThemeDocument) -> Option<Self> {
        document.validate().ok()?;
        Some(Self::built(document.clone(), PresenceHue::Blue))
    }

    /// A shipped theme.
    #[must_use]
    pub fn preset(preset: Preset) -> Self {
        Self::built(preset.document(), PresenceHue::Blue)
    }

    /// The default: pure neutral dark.
    pub fn dark() -> Self {
        Self::preset(Preset::Dark)
    }

    pub fn light() -> Self {
        Self::preset(Preset::Light)
    }

    /// Black ground under near-white ink, every text pairing at WCAG AAA.
    /// Selected when [`A11ySettings::high_contrast`](crate::a11y::A11ySettings)
    /// is set on a dark theme.
    pub fn high_contrast() -> Self {
        Self::preset(Preset::HighContrast)
    }

    /// Ink tiers on the content surface — the window ground every app
    /// paints on.
    pub fn on_content(&self) -> crate::tokens::OnSurface {
        self.tokens.colors.on_content()
    }

    /// The ground of `region` (HIG §9.2): `surfaces.content`,
    /// `surfaces.sidebar` or `surfaces.inspector`. The framework paints it
    /// ([`crate::surface::region`]); a caller reads it to measure against.
    #[must_use]
    pub fn region(&self, region: crate::surface::Region) -> Color {
        let s = &self.tokens.colors.surfaces;
        match region {
            crate::surface::Region::Content => s.content,
            crate::surface::Region::Sidebar => s.sidebar,
            crate::surface::Region::Inspector => s.inspector,
        }
    }

    /// This theme as seen by a widget standing on `ground`: a region tier, a
    /// raised well, a document's paper. `surfaces.content` becomes `ground`,
    /// the text tiers are re-measured against it ([`Self::on_surface`]) and
    /// the selection lift steps from it, so a widget that paints "ink on
    /// the window ground" or "the selected row" is right on whatever ground
    /// its container handed it. `surfaces.raised` stays off the ground
    /// ([`crate::tokens::Colors::raised_on`]), so a control's face never
    /// sinks into a lighter panel. When `ground` has the other polarity
    /// from the palette (the near-black `media.letterbox` under a light
    /// theme) and the emphasis is inverted, the primary fill is re-derived
    /// too: primary ink on that ground as the fill and the ground as its
    /// label, so a `Primary` button over media still reads. Every other
    /// token is unchanged.
    #[must_use]
    pub fn on_ground(&self, ground: Color) -> Self {
        let mut theme = self.clone();
        if ground == self.tokens.colors.surfaces.content {
            return theme;
        }
        let on = self.tokens.colors.on_surface(ground);
        let c = &mut theme.tokens.colors;
        c.surfaces.raised = c.raised_on(ground);
        c.surfaces.content = ground;
        c.text_primary = on.ink;
        c.text_dim = on.ink_dim;
        c.text_tertiary = on.ink_tertiary;
        c.intent.selection = c.selected_on(ground);
        c.intent.selection_hover = c.lift_on(ground, 0.5);
        c.intent.value_fill =
            crate::tokens::colors::value_fill_on(c.intent.accent_ink, ground, c.intent.selection);
        let ground_dark = crate::color::relative_luminance(ground) < Self::POLARITY_LUMINANCE;
        if ground_dark != c.palette.dark && matches!(c.emphasis, crate::tokens::Emphasis::Inverted)
        {
            // The same construction the palette uses on its own ground:
            // primary ink as the fill, hover 12 % of the way back toward
            // the ground, the ground as the label.
            c.intent.accent = on.ink;
            c.intent.accent_hover = crate::tokens::colors::over(on.ink, ground, 0.12);
            c.intent.accent_pressed = crate::tokens::colors::over(on.ink, ground, 0.24);
            c.intent.on_accent = ground;
        }
        theme
            .tokens
            .typography
            .set_ground(crate::text_render::TextGround::of(ground));
        theme.refresh_colour_shortcuts();
        theme
    }

    /// Relative luminance below which a ground counts as dark: the point
    /// where black and white ink contrast equally with it.
    const POLARITY_LUMINANCE: f64 = 0.179;

    /// [`Self::on_ground`] for one of the window's regions.
    #[must_use]
    pub fn in_region(&self, region: crate::surface::Region) -> Self {
        self.on_ground(self.region(region))
    }

    /// [`Self::on_content`] for any surface a widget actually paints on — a
    /// document's paper, a tinted placeholder box, a chart panel. See
    /// [`crate::tokens::Colors::on_surface`] for why the surface is a
    /// parameter.
    pub fn on_surface(&self, surface: Color) -> crate::tokens::OnSurface {
        self.tokens.colors.on_surface(surface)
    }

    /// [`Self::on_surface`] against a caller-chosen contrast floor.
    pub fn on_surface_at(&self, surface: Color, min_ratio: f64) -> crate::tokens::OnSurface {
        self.tokens.colors.on_surface_at(surface, min_ratio)
    }

    /// See [`crate::tokens::Colors::window_edge`] for why it's derived from
    /// the desktop ground rather than a constant.
    pub fn window_edge(&self) -> Color {
        self.tokens.colors.window_edge()
    }

    /// The same theme in a workspace whose temperature is `hue` (OKLCH
    /// degrees). Only the neutral material's hue moves — never its chroma,
    /// so a palette with chroma 0 (the default dark) shows no temperature
    /// at all, and the light palette, pure grey until now, takes its
    /// chroma at this hue. Presence, status, emphasis and the person's own
    /// colour keep their hues, and every contrast promise holds at every
    /// hue. A workspace's temperature comes from [`workspace_hue`].
    #[must_use]
    pub fn with_neutral_hue(mut self, hue: f32) -> Self {
        if !hue.is_finite() || self.document.palette.chroma <= 0.0 {
            return self;
        }
        self.document.palette.hue = Some(hue.rem_euclid(360.0));
        self.recolour();
        self
    }

    /// The person's own colour becomes their focus ring, text caret and
    /// text-selection tint, painted at this theme's presence lightness and
    /// chroma. A colour is identity, not theme: the host that knows who sits
    /// here applies it ([`PresenceHue::chosen_in`], [`PresenceHue::of_identity`]).
    #[must_use]
    pub fn with_your_colour(mut self, hue: PresenceHue) -> Self {
        if self.tokens.colors.you != hue {
            self.tokens.colors.you = hue;
            self.recolour();
        }
        self
    }

    /// The same theme on the high-contrast palette of its polarity: shape,
    /// density, type and motion stay, the person keeps their colour, and
    /// content surfaces are asked to force their colours.
    #[must_use]
    pub fn with_high_contrast(mut self) -> Self {
        if self.document.palette.high_contrast {
            return self;
        }
        self.document = self.document.in_high_contrast();
        self.name.clone_from(&self.document.name);
        self.forced_colors = true;
        self.recolour();
        self
    }

    /// Rebuild the colour tokens from [`Self::document`] after its palette,
    /// emphasis, presence or material changed, keeping every other token
    /// (scaled spacing and type stay scaled).
    fn recolour(&mut self) {
        self.tokens.colors = colors_of(&self.document, self.tokens.colors.you);
        self.refresh_colour_shortcuts();
    }

    /// Re-derive every shortcut and derived palette after the colour tokens
    /// changed in place.
    fn refresh_colour_shortcuts(&mut self) {
        Self::derive_palettes(&mut self.tokens);
        let c = &self.tokens.colors;
        self.border_standard = c.border_standard;
        self.border_focus = c.border_focus;
        self.text_primary = c.text_primary;
        self.text_dim = c.text_dim;
        self.text_tertiary = c.text_tertiary;
        self.status_success = c.semantic.success;
    }

    /// Reads `$XDG_CONFIG_HOME/tilo/session/settings.json` (falling back to
    /// `~/.config/tilo/session/settings.json`), the shared preference Settings and
    /// Theme Studio update through their respective domain stores.
    /// Unreadable, absent and unrecognised all mean the dark default — a
    /// machine with no preference and one whose settings file is corrupt
    /// both want a desktop that starts.
    pub fn configured() -> Self {
        if let Some(source) = settings_store::installed_source() {
            let text = serde_json::Value::Object(source.load()).to_string();
            let reduce_motion_override = crate::host::environment().accessibility.reduce_motion;
            return Self::configured_from_text(&text, reduce_motion_override);
        }
        Self::configured_from(settings_store::settings_path().as_deref())
    }

    /// [`Theme::configured`] from an explicit settings file; `None` or an
    /// unreadable path yields the dark default.
    pub fn configured_from(settings: Option<&std::path::Path>) -> Self {
        let text = settings
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        let reduce_motion_override = crate::host::environment().accessibility.reduce_motion;
        Self::configured_from_text(&text, reduce_motion_override)
    }

    fn configured_from_text(text: &str, reduce_motion_override: bool) -> Self {
        let mut stack =
            crate::theme_layers::ThemeLayerStack::from_settings_json(text, local_moment());
        // Hosts apply text and UI scale on top of this theme (`App::rebuild_theme`).
        // Baking the file's factors here would apply them twice. Reduced motion
        // is either source: the person's switch in Settings (the file), or the
        // host override — the compositor is itself a UI host, so an explicitly
        // reduced-motion session must not keep full-desktop transitions running
        // even when settings.json has not been written yet. Taking only the
        // override left the Settings switch unable to reach the shell's chrome.
        // A file schema this build rejects does not contribute contrast,
        // caret or motion. The host override is its own admitted layer.
        let file = stack.accessibility;
        let admitted = file.validate().is_ok();
        stack.accessibility = crate::theme_layers::AccessibilityLayer {
            high_contrast: admitted && file.high_contrast,
            forced_colors: admitted && file.forced_colors,
            thick_caret: admitted && file.thick_caret,
            reduce_transparency: admitted && file.reduce_transparency,
            hyperlegible: admitted && file.hyperlegible,
            distinct_letters: file.distinct_letters.filter(|_| admitted),
            text_scale: 1.0,
            ui_scale: 1.0,
            reduce_motion: reduce_motion_override || (admitted && file.reduce_motion),
            ..crate::theme_layers::AccessibilityLayer::default()
        };
        resolve_theme_layers(&stack)
    }

    /// The accessibility comforts the person chose in this process's
    /// settings document (the installed source, else the settings file), with
    /// what the launcher requested ([`crate::host::environment()`]) on top:
    /// high contrast, text size, interface scale and reduced motion. The
    /// layer [`Theme::configured`] leaves to its host, which applies it over
    /// that theme on every settings change, so a change in Settings reaches a
    /// running window without a restart.
    #[must_use]
    pub fn configured_accessibility() -> crate::theme_layers::AccessibilityLayer {
        Self::configured_comforts().or_default()
    }

    /// [`Self::configured_accessibility`], saying which comforts the
    /// document names at all: a host keeps its own value for one it does not.
    #[must_use]
    pub fn configured_comforts() -> ConfiguredComforts {
        let document = match settings_store::installed_source() {
            Some(source) => serde_json::Value::Object(source.load()),
            None => serde_json::Value::Object(
                settings_store::settings_path()
                    .map(|path| settings_store::load_at(&path))
                    .unwrap_or_default(),
            ),
        };
        Self::comforts_in(&document, crate::host::environment().accessibility)
    }

    /// [`Self::configured_accessibility`] of an explicit settings document
    /// and launch request.
    #[must_use]
    pub fn accessibility_in(
        document: &serde_json::Value,
        requested: crate::host::AccessibilityInputs,
    ) -> crate::theme_layers::AccessibilityLayer {
        Self::comforts_in(document, requested).or_default()
    }

    /// [`Self::configured_comforts`] of an explicit settings document and
    /// launch request. The launcher's request is a floor for contrast and
    /// motion (it can add them, never take them away) and gives the scales
    /// until the document names its own: the document is the person's
    /// current choice, the request a copy of it from when the window opened.
    /// A layer this build rejects names nothing.
    #[must_use]
    pub fn comforts_in(
        document: &serde_json::Value,
        requested: crate::host::AccessibilityInputs,
    ) -> ConfiguredComforts {
        let stack = crate::theme_layers::ThemeLayerStack::from_settings_json(
            &document.to_string(),
            local_moment(),
        );
        let admitted = stack.accessibility.materialize().ok();
        let appearance = document.get("appearance");
        let named = |key: &str| {
            admitted.is_some()
                && appearance.is_some_and(|section| {
                    section.get(key).is_some()
                        || section
                            .get(crate::theme_layers::ACCESSIBILITY_LAYER_KEY)
                            .is_some_and(|layer| layer.get(key).is_some())
                })
        };
        let file = admitted.unwrap_or_default();
        let flag = |key: &str, chosen: bool, floor: bool| {
            if named(key) {
                Some(chosen || floor)
            } else {
                floor.then_some(true)
            }
        };
        let scale = |key: &str, chosen: f32, launched: Option<f32>| {
            if named(key) {
                Some(chosen)
            } else {
                launched.filter(|value| value.is_finite())
            }
        };
        ConfiguredComforts {
            high_contrast: flag("high_contrast", file.high_contrast, requested.high_contrast),
            reduce_motion: flag("reduce_motion", file.reduce_motion, requested.reduce_motion),
            text_scale: scale("text_scale", file.text_scale, requested.text_scale),
            ui_scale: scale("ui_scale", file.ui_scale, requested.ui_scale),
            layer: file,
        }
    }

    /// The user's theme in a settings document, if it names one this build
    /// admits. Automatic mode resolves against the local time now.
    pub fn from_settings_json(text: &str) -> Option<Self> {
        Self::from_settings_json_at(text, local_moment())
    }

    /// [`Self::from_settings_json`] at an explicit local moment: automatic
    /// mode is the light theme by day and the dark theme by night, on the
    /// schedule night light shares (`settings_store::day_schedule`).
    pub fn from_settings_json_at(
        text: &str,
        moment: settings_store::day_schedule::LocalMoment,
    ) -> Option<Self> {
        let settings: serde_json::Value = serde_json::from_str(text).ok()?;
        let document = crate::theme_layers::user_document(&settings, moment)?;
        Self::from_document(&document)
    }

    /// The day/night schedule an automatic appearance in `settings` follows,
    /// or `None` when the mode is fixed. Hosts keep it to notice sunrise and
    /// sunset, which change the theme without changing the file.
    pub fn automatic_schedule_from(
        settings: Option<&std::path::Path>,
    ) -> Option<settings_store::day_schedule::Scheduled> {
        let document = settings_store::load_at(settings?);
        let mode = document
            .get("appearance")?
            .get(settings_store::day_schedule::MODE_KEY)?
            .as_str()?;
        settings_store::day_schedule::is_automatic(mode)
            .then(|| settings_store::day_schedule::Scheduled::from_settings(&document))
    }

    /// Every theme Tilo ships, by the name [`Theme::named`] resolves.
    ///
    /// Exists so a check can be run against *all* of them rather than against
    /// whichever two someone remembered.
    pub const ALL: [&'static str; 7] = [
        Preset::Dark.name(),
        Preset::DarkCool.name(),
        Preset::DarkGraphite.name(),
        Preset::DarkSoft.name(),
        Preset::Light.name(),
        Preset::HighContrast.name(),
        Preset::HighContrastLight.name(),
    ];

    /// A shipped theme by id or name, loosely ([`Preset::find`]), so the
    /// same lookup serves a picker, the persisted preference and the
    /// `--shots` theme override. `None` for an unknown name.
    pub fn named(name: &str) -> Option<Self> {
        Preset::find(name).map(Self::preset)
    }

    /// The motion widgets may play: the motion tokens with reduced motion
    /// applied. Every motion decision reads this, never `reduce_motion`
    /// alongside raw durations, so the rule lives in one place.
    #[must_use]
    pub fn motion(&self) -> crate::tokens::EffectiveMotion {
        crate::tokens::EffectiveMotion::new(self.tokens.motion, self.reduce_motion)
    }

    #[must_use]
    pub fn with_reduce_motion(mut self, on: bool) -> Self {
        self.reduce_motion = on;
        self
    }

    #[must_use]
    pub fn with_reduce_transparency(mut self, on: bool) -> Self {
        self.reduce_transparency = on;
        self
    }

    /// `1.0` = unchanged. Multiplies the whole type scale and base body
    /// size together, so layout (which measures the same scaled sizes)
    /// grows to fit and nothing clips. Not assumed idempotent — call once
    /// on a fresh theme.
    #[must_use]
    pub fn with_text_scale(mut self, factor: f32) -> Self {
        let factor = factor.clamp(0.5, 4.0);
        self.regrow_icons(self.text_scale, factor);
        self.text_scale = factor;
        self.multiply_type_sizes(factor);
        self
    }

    /// Glyph metrics follow the text scale by
    /// [`crate::tokens::Metrics::icon_growth`]: re-grow them from the scale
    /// they were grown for to `to`.
    fn regrow_icons(&mut self, from: f32, to: f32) {
        use crate::tokens::Metrics;
        let factor = Metrics::icon_growth(to) / Metrics::icon_growth(from);
        if (factor - 1.0).abs() > f32::EPSILON {
            self.tokens.metrics = self.tokens.metrics.with_icons_scaled(factor);
        }
    }

    fn multiply_type_sizes(&mut self, factor: f32) {
        self.tokens.typography.scale = self.tokens.typography.scale.scaled(factor);
    }

    /// This theme with the accessibility text scale taken back out: type at
    /// its 1× sizes (glyphs too), everything else unchanged. For content whose size is
    /// the document's own (a slide's text, laid out at the slide's size and
    /// then scaled as a whole), which must not grow a second time with the
    /// reader's text setting. [`Theme::with_text_scale`] multiplies, so it
    /// cannot undo a scale already applied.
    #[must_use]
    pub fn at_unit_text_scale(&self) -> Self {
        let mut theme = self.clone();
        if (theme.text_scale - 1.0).abs() > f32::EPSILON {
            theme.multiply_type_sizes(1.0 / theme.text_scale);
            theme.regrow_icons(theme.text_scale, 1.0);
            theme.text_scale = 1.0;
        }
        theme
    }

    /// How much larger a document draws its text in a pane `width` wide,
    /// whose page (reading column and its gutters) is `page` wide at this
    /// theme's size: `1.0` until the pane is [`DOCUMENT_ROOM`] pages wide,
    /// then growing with the pane up to [`MAX_DOCUMENT_ZOOM`], in steps of a
    /// sixteenth so resizing a window re-sets the text about once per pixel
    /// of type size rather than every frame.
    ///
    /// A page at its laptop size in a window on a large display is a narrow
    /// strip of small text in a field of empty space; grown, it keeps its
    /// measure and fills the screen the way a page held closer would. A
    /// laptop or phone window is never wide enough to grow it.
    #[must_use]
    pub fn document_zoom(width: f64, page: f64) -> f32 {
        if !(width.is_finite() && page.is_finite() && page > 0.0) {
            return 1.0;
        }
        let zoom = (width / (page * DOCUMENT_ROOM)).clamp(1.0, f64::from(MAX_DOCUMENT_ZOOM));
        crate::num::f32_of_f64((zoom * 16.0).floor() / 16.0)
    }

    /// This theme with its type grown by `zoom` on top of the reader's own
    /// text scale ([`Theme::document_zoom`]); `1.0` is this theme.
    #[must_use]
    pub fn with_document_zoom(&self, zoom: f32) -> Self {
        if (zoom - 1.0).abs() <= f32::EPSILON {
            return self.clone();
        }
        let scale = self.text_scale * zoom;
        self.at_unit_text_scale().with_text_scale(scale)
    }

    /// `1.0` = unchanged. Multiplies spacing, radii, and type together,
    /// distinct from [`Theme::with_text_scale`] which only resizes type.
    /// Not assumed idempotent — call once on a fresh theme.
    #[must_use]
    pub fn with_ui_scale(mut self, factor: f32) -> Self {
        let factor = factor.clamp(0.5, 3.0);
        self.ui_scale = factor;
        self.multiply_dimensions(factor);
        self.multiply_type_sizes(factor);
        self.radius_standard = f64::from(self.tokens.radii.control);
        self.padding_standard = self.tokens.spacing.s3;
        self.gap_standard = self.tokens.spacing.s3;
        self
    }

    /// The same theme at `density` (`crate::tokens::Density`): compact
    /// scales the spacing rhythm by 0.75 and shortens rows to 28 / 44 / 52;
    /// control heights and text stay. Set per window or per region (a
    /// sidebar, a table): a data-heavy app lays its content region out with
    /// `theme.with_density(Density::Compact)`.
    #[must_use]
    pub fn with_density(mut self, density: crate::tokens::Density) -> Self {
        let from = self.tokens.density;
        if from == density {
            return self;
        }
        // Undo the old factor, then apply the new one: dividing first keeps
        // a round trip exact.
        let (undo, apply) = (from.rhythm_factor(), density.rhythm_factor());
        let r = &mut self.tokens.spacing.rhythm;
        for gap in [
            &mut r.tight,
            &mut r.related,
            &mut r.sibling,
            &mut r.section,
            &mut r.group,
        ] {
            *gap = *gap / undo * apply;
        }
        let (was, now) = (from.rows(), density.rows());
        let m = &mut self.tokens.metrics;
        m.row_single = m.row_single / was.single * now.single;
        m.row_double = m.row_double / was.double * now.double;
        m.row_avatar = m.row_avatar / was.avatar * now.avatar;
        self.tokens.density = density;
        self
    }

    fn multiply_dimensions(&mut self, factor: f32) {
        self.tokens.spacing = self.tokens.spacing.scaled(factor);
        self.tokens.metrics = self.tokens.metrics.scaled(factor);

        let r = &mut self.tokens.radii;
        r.tile *= factor;
        r.panel *= factor;
        r.control *= factor;
        r.item *= factor;
        r.detail *= factor;
        // `full` is a "make it a pill" sentinel, not a real length — leave it.
    }

    /// Replace factors already baked into an effective component snapshot.
    /// Its metadata must describe the supplied values; public builders remain cumulative.
    pub(crate) fn with_component_scales(mut self, text_scale: f32, ui_scale: f32) -> Self {
        let text_scale = if text_scale.is_finite() {
            text_scale.clamp(0.5, 4.0)
        } else {
            1.0
        };
        let ui_scale = if ui_scale.is_finite() {
            ui_scale.clamp(0.5, 3.0)
        } else {
            1.0
        };
        let ui_ratio = ui_scale / self.ui_scale;
        self.multiply_type_sizes(text_scale / self.text_scale * ui_ratio);
        self.multiply_dimensions(ui_ratio);
        self.regrow_icons(self.text_scale, text_scale);
        // Unlike the public builder, preserve independently customized aliases.
        self.radius_standard *= f64::from(ui_ratio);
        self.padding_standard *= ui_ratio;
        self.gap_standard *= ui_ratio;
        self.text_scale = text_scale;
        self.ui_scale = ui_scale;
        self
    }

    /// [`crate::tokens::Metrics::text_growth`] of this theme's
    /// [`Theme::text_scale`]: how much a text-sized extent grows, never
    /// below 1.
    #[must_use]
    pub fn text_growth(&self) -> f32 {
        crate::tokens::Metrics::text_growth(self.text_scale)
    }

    /// The one factor the responsive shells multiply an authored,
    /// text-sized breakpoint or pane width by: `ui_scale × text_growth()`.
    ///
    /// `NavShell` grows its metric widths by it, and a painted app's
    /// `SidebarDrawer` its authored ones (`breakpoint(760.0)`), so at 200 %
    /// text a 760 px breakpoint pushes at 1520 px — the window that has the
    /// room a 760 px window has at 1× — without the app multiplying
    /// anything. `1.0` on a stock theme, so 1× geometry is untouched. A width
    /// sized by icons rather than text (a rail) follows [`Theme::ui_scale`]
    /// alone.
    #[must_use]
    pub fn responsive_scale(&self) -> f64 {
        f64::from(self.ui_scale) * f64::from(self.text_growth())
    }

    /// UI scale first so it grows the whole system, then text scale layers
    /// additional type growth on top. High contrast keeps the theme's
    /// polarity, shape, density and type, and the person's own colour.
    #[must_use]
    pub fn with_a11y(
        self,
        settings: &crate::a11y::A11ySettings,
        text_scale: f32,
        ui_scale: f32,
    ) -> Self {
        let base = if settings.high_contrast {
            self.with_high_contrast()
        } else {
            self
        };
        base.with_ui_scale(ui_scale)
            .with_text_scale(text_scale)
            .with_reduce_motion(settings.reduce_motion)
    }
}

/// The comforts a settings document names ([`Theme::configured_comforts`]):
/// `None` where it names none, which leaves the host's own value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConfiguredComforts {
    pub high_contrast: Option<bool>,
    pub reduce_motion: Option<bool>,
    pub text_scale: Option<f32>,
    pub ui_scale: Option<f32>,
    /// The document's whole admitted layer, for the comforts not listed.
    pub layer: crate::theme_layers::AccessibilityLayer,
}

impl ConfiguredComforts {
    /// The layer with every comfort the document leaves unnamed at its
    /// default.
    #[must_use]
    pub fn or_default(self) -> crate::theme_layers::AccessibilityLayer {
        crate::theme_layers::AccessibilityLayer {
            high_contrast: self.high_contrast.unwrap_or(false),
            reduce_motion: self.reduce_motion.unwrap_or(false),
            text_scale: self.text_scale.unwrap_or(1.0),
            ui_scale: self.ui_scale.unwrap_or(1.0),
            ..self.layer
        }
    }
}

/// What the host showing a window knows about where, and for whom, it is
/// shown: the temperature of the workspace the window is in and the colour
/// of the person sitting at it. The shell resolves both and hands them to
/// its own chrome and, over the shell-ui channel, to every app window
/// (`crate::floating_surface::FloatingSurfaceHostMessage::Appearance`). A
/// window nobody told stays neutral and wears the default colour.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowAppearance {
    /// The workspace's temperature (OKLCH degrees), `None` while it has
    /// none: a grey wallpaper, or no shell.
    pub workspace_hue: Option<f32>,
    /// The person's own colour ("your colour"), `None` when the host does
    /// not know who sits here.
    pub your_colour: Option<PresenceHue>,
    /// How round the host draws the window's corners, in logical pixels:
    /// the theme's window radius (`radii.tile`) while the window stands
    /// framed among others, 0 while it is drawn edge to edge (a maximized
    /// window filling its display). `None` when no host said: a window on
    /// another desktop, whose shape is not known.
    pub corner_radius: Option<f32>,
}

/// The surface a window's content is drawn on, as the scene being drawn
/// sees it: its bounds and the radius of each of its corners, 0 where a
/// corner is square. A host that knows the shape it shows a window in puts
/// it on the window's theme ([`Theme::surface`]); what runs along the
/// window's edges (a scroll bar) keeps its ends out of a rounded corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceShape {
    pub bounds: vello::kurbo::Rect,
    pub radii: vello::kurbo::RoundedRectRadii,
}

impl SurfaceShape {
    #[must_use]
    pub fn rounded(bounds: vello::kurbo::Rect, radius: f64) -> Self {
        Self {
            bounds,
            radii: vello::kurbo::RoundedRectRadii::from_single_radius(radius.max(0.0)),
        }
    }

    #[must_use]
    pub fn is_rounded(&self) -> bool {
        let r = self.radii;
        [r.top_left, r.top_right, r.bottom_right, r.bottom_left]
            .into_iter()
            .any(|radius| radius > 0.0)
    }

    /// The shape where `transform` takes it: its bounds' box, and radii
    /// scaled by the transform's scale. Transforms that place surfaces
    /// translate and scale uniformly.
    #[must_use]
    pub fn transformed(&self, transform: vello::kurbo::Affine) -> Self {
        let scale = transform.determinant().abs().sqrt();
        let r = self.radii;
        Self {
            bounds: transform.transform_rect_bbox(self.bounds),
            radii: vello::kurbo::RoundedRectRadii::new(
                r.top_left * scale,
                r.top_right * scale,
                r.bottom_right * scale,
                r.bottom_left * scale,
            ),
        }
    }

    /// The shape as a widget that draws with `transform` sees it, in its
    /// own coordinates: the inverse of [`Self::transformed`].
    #[must_use]
    pub fn seen_through(&self, transform: vello::kurbo::Affine) -> Self {
        if transform.determinant().abs() <= f64::EPSILON {
            return *self;
        }
        self.transformed(transform.inverse())
    }
}

impl Theme {
    /// This theme as the window `appearance` describes sees it: the
    /// workspace's temperature on the neutrals (nothing on a palette with
    /// chroma 0, nothing while there is no temperature) and the person's
    /// colour as focus ring, caret and selection. Both are applied to the
    /// theme as configured, so a window moving to a neutral workspace loses
    /// the temperature of the one it left.
    #[must_use]
    pub fn in_window(self, appearance: &WindowAppearance) -> Self {
        let theme = match appearance.workspace_hue {
            Some(hue) => self.with_neutral_hue(hue),
            None => self,
        };
        match appearance.your_colour {
            Some(hue) => theme.with_your_colour(hue),
            None => theme,
        }
    }
}

/// The neutral temperature a workspace takes from its wallpaper (RGBA8
/// pixels): the image's dominant hue, or `None` for a wallpaper with none
/// (a grey photograph, a missing file), which leaves the material neutral.
/// Only the hue is taken — the material never gains chroma from a
/// wallpaper.
#[must_use]
pub fn workspace_hue(wallpaper_rgba: &[u8]) -> Option<f32> {
    crate::color::dominant_hue(wallpaper_rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every control fill in every shipped theme, on every ground a control
    /// stands on, clears the non-text contrast floor there and is never the
    /// text colour; and a control's face (an unchecked box, a button, a
    /// track) is a surface that body text reads on, never ink. A filled
    /// progress bar or an unchecked box that renders as a black slab on a
    /// light window fails here.
    #[test]
    fn every_control_fill_reads_on_its_ground_and_is_never_the_text_colour() {
        use crate::a11y::contrast_ratio;
        use crate::color::Oklch;
        use crate::tokens::colors::{NON_TEXT_MIN_CONTRAST, composite};
        let lab = |c: Color| {
            let o = Oklch::from_color(c);
            let h = f64::from(o.h).to_radians();
            (
                f64::from(o.l),
                f64::from(o.c) * h.cos(),
                f64::from(o.c) * h.sin(),
            )
        };
        let distance = |a: Color, b: Color| {
            let ((l1, a1, b1), (l2, a2, b2)) = (lab(a), lab(b));
            ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
        };
        for preset in Preset::ALL {
            let base = Theme::preset(preset);
            let s = &base.tokens.colors.surfaces;
            for (ground_name, ground) in [
                ("content", s.content),
                ("sidebar", s.sidebar),
                ("inspector", s.inspector),
                ("raised", s.raised),
                ("overlay", composite(s.overlay, s.content)),
            ] {
                let theme = base.on_ground(ground);
                let c = &theme.tokens.colors;
                let ink = c.text_primary;
                for (token, fill) in [
                    ("value_fill", c.intent.value_fill),
                    ("on_state", c.intent.on_state),
                ] {
                    let ratio = contrast_ratio(fill, ground);
                    assert!(
                        ratio >= NON_TEXT_MIN_CONTRAST - 0.01,
                        "{}: {token} reaches only {ratio:.2}:1 on the {ground_name} ground",
                        base.name
                    );
                    assert!(
                        distance(fill, ink) >= 0.08,
                        "{}: {token} {fill:?} is the text colour {ink:?} on the {ground_name} ground",
                        base.name
                    );
                }
                let track = contrast_ratio(c.intent.value_fill, c.intent.selection);
                assert!(
                    track >= NON_TEXT_MIN_CONTRAST - 0.01,
                    "{}: value_fill reaches only {track:.2}:1 on its track on the {ground_name} ground",
                    base.name
                );
                assert!(
                    distance(c.intent.value_fill, c.border_focus) >= 0.08,
                    "{}: value_fill is the focus ring's colour on the {ground_name} ground",
                    base.name
                );
                let face = contrast_ratio(c.surfaces.raised, ink);
                assert!(
                    face >= 4.5 - 0.01,
                    "{}: a control face on the {ground_name} ground carries text at only {face:.2}:1, so it reads as ink",
                    base.name
                );
            }
        }
    }

    // Catches a note maximised on a 1440p-class display reading as a narrow
    // strip of laptop-sized text in a field of empty space.
    #[test]
    fn a_document_grows_its_text_only_in_a_pane_much_wider_than_its_page() {
        let page = 560.0;
        // Laptop, phone and a 1440p display at 1.5x keep the stock size.
        for width in [0.0, 360.0, 1000.0, 1427.0] {
            assert!((Theme::document_zoom(width, page) - 1.0).abs() < f32::EPSILON);
        }
        let grown = Theme::document_zoom(1700.0, page);
        assert!(grown > 1.0 && grown < MAX_DOCUMENT_ZOOM, "{grown}");
        // Whole sixteenths: a window resized by a pixel re-sets no text.
        assert!((grown * 16.0 - (grown * 16.0).round()).abs() < 1e-6);
        assert!((Theme::document_zoom(2280.0, page) - MAX_DOCUMENT_ZOOM).abs() < f32::EPSILON);
        assert!((Theme::document_zoom(f64::NAN, page) - 1.0).abs() < f32::EPSILON);

        // The zoom multiplies the reader's text scale, never replaces it.
        let large = Theme::dark().with_text_scale(1.25);
        let zoomed = large.with_document_zoom(1.5);
        assert!((zoomed.text_scale - 1.875).abs() < 1e-5);
        let body = |t: &Theme| t.text(crate::TextRole::Body).size;
        assert!((body(&zoomed) / body(&large) - 1.5).abs() < 1e-3);
        assert_eq!(large.with_document_zoom(1.0), large);
    }

    #[test]
    fn a_primary_control_on_a_flipped_ground_stays_legible() {
        use crate::a11y::contrast_ratio;
        for theme in [Theme::light(), Theme::dark()] {
            let letterbox = theme.tokens.colors.media.letterbox;
            let on = theme.on_ground(letterbox);
            let i = &on.tokens.colors.intent;
            assert!(
                contrast_ratio(i.on_accent, i.accent) >= 4.5,
                "the label on the primary fill must read over media",
            );
            assert!(
                contrast_ratio(i.accent, letterbox) >= 3.0,
                "the primary fill must stand off the media ground",
            );
            assert_ne!(i.accent, i.accent_hover);
        }
        let dark = Theme::dark();
        let sidebar = dark.tokens.colors.surfaces.sidebar;
        assert_eq!(
            dark.on_ground(sidebar).tokens.colors.intent.accent,
            dark.tokens.colors.intent.accent
        );
    }

    use crate::a11y::contrast_ratio;
    use crate::color::Oklch;
    use crate::color_compat::ColorCompat;
    use crate::tokens::Emphasis;

    fn quiet_ocean(spacing: f32) -> ThemeDocument {
        let mut doc = Preset::DarkCool.document();
        doc.id = "quiet-ocean".into();
        doc.name = "Quiet ocean".into();
        doc.palette.hue = Some(220.0);
        doc.density.spacing = spacing;
        doc.type_.face = "Inter".into();
        doc
    }

    #[test]
    fn the_shipped_themes_resolve_by_name_and_keep_every_floor() {
        // phase: every_name_round_trips
        {
            for name in Theme::ALL {
                let theme = Theme::named(name).unwrap_or_else(|| panic!("{name} resolves"));
                assert_eq!(theme.name, name);
            }
            assert_eq!(Theme::named("high-contrast").unwrap().name, "High contrast");
            assert_eq!(Theme::named("dark-soft").unwrap().name, "Dark soft");
            assert!(Theme::named("Dracula").is_none(), "no third-party palettes");
            assert_eq!(ALL_PRESETS_FOR_TEST.len(), Theme::ALL.len());
        }
        // phase: every_shipped_theme_keeps_every_floor
        {
            use crate::tokens::contrast::audit;
            for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let findings = audit(&t.tokens.colors);
                assert!(
                    findings.is_empty(),
                    "{} misses: {}",
                    t.name,
                    findings
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            }
        }
        // phase: the_shortcuts_mirror_the_tokens
        {
            for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let c = &t.tokens.colors;
                assert_eq!(t.text_primary, c.text_primary, "{}", t.name);
                assert_eq!(t.border_focus, c.intent.accent_ink, "{}", t.name);
                assert!((t.radius_standard - f64::from(t.tokens.radii.control)).abs() < 1e-9);
            }
            assert!(Theme::high_contrast().colors_are_forced());
            assert!(Theme::preset(Preset::HighContrastLight).colors_are_forced());
            assert!(!Theme::dark().colors_are_forced());
        }
    }

    #[test]
    fn the_window_edge_and_content_surface_register_against_their_ground() {
        // phase: window_edge_registers_against_the_desktop_on_every_theme
        {
            use crate::a11y::relative_luminance;
            for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let ground = t.tokens.colors.surfaces.base;
                let seen = crate::tokens::colors::composite(t.window_edge(), ground);
                let moved = (relative_luminance(seen) - relative_luminance(ground)).abs();
                assert!(moved >= 0.02, "{}: the window edge is not an edge", t.name);
            }
        }
        // phase: window_edge_flips_a_border_its_ground_would_swallow
        {
            let mut t = Theme::dark();
            t.tokens.colors.surfaces.base = Color::WHITE;
            t.tokens.colors.surfaces.content_edge = Color::from_rgba8(255, 255, 255, 51);
            let edge = t.window_edge();
            assert_eq!([edge.r(), edge.g(), edge.b(), edge.a()], [0, 0, 0, 51]);
        }
        // phase: the_content_surface_hosts_the_theme_s_own_ink
        {
            for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let on = t.on_content();
                assert_eq!(on.ink, t.text_primary, "{}", t.name);
                assert_eq!(on.ink_dim, t.text_dim, "{}", t.name);
                assert_eq!(on.ink_tertiary, t.text_tertiary, "{}", t.name);
                let r = contrast_ratio(on.tint, t.tokens.colors.surfaces.content);
                assert!(r < 1.6, "{}: the tint reads as a panel ({r:.2}:1)", t.name);
            }
        }
        // phase: a_region_theme_measures_ink_and_selection_against_its_ground
        {
            use crate::surface::Region;
            for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                for region in [Region::Content, Region::Sidebar, Region::Inspector] {
                    let ground = t.region(region);
                    let r = t.in_region(region);
                    let c = &r.tokens.colors;
                    assert_eq!(c.surfaces.content, ground, "{} {region:?}", t.name);
                    assert_eq!(r.text_primary, c.text_primary, "{} {region:?}", t.name);
                    for (tier, ink) in [("primary", r.text_primary), ("secondary", r.text_dim)] {
                        let ratio = contrast_ratio(ink, ground);
                        assert!(
                            ratio >= 4.5,
                            "{} {region:?}: {tier} ink {ratio:.2}:1",
                            t.name
                        );
                    }
                    let step = contrast_ratio(c.intent.selection, ground);
                    assert!(step >= 1.15, "{} {region:?}: lift {step:.3}:1", t.name);
                    // A region's raised face is re-derived so it stands off
                    // the region (`a_raised_face_stands_off_every_ground`).
                    assert_eq!(c.intent.accent, t.tokens.colors.intent.accent);
                }
                assert_eq!(
                    t.in_region(Region::Content).tokens.colors.intent.selection,
                    t.tokens.colors.intent.selection,
                    "{}: the content region is the theme itself",
                    t.name
                );
            }
        }
    }

    /// A control's face stands off every ground it is placed on the way it
    /// stands off the window: on a floating panel lighter than the raised
    /// tier, a secondary button or a field does not sink into the panel.
    #[test]
    fn a_raised_face_stands_off_every_ground() {
        use crate::color::Oklch;
        let l = |c: Color| Oklch::from_color(c).l;
        for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
            let colors = &t.tokens.colors;
            let step = l(colors.surfaces.raised) - l(colors.surfaces.content);
            if step.abs() <= f32::EPSILON {
                continue;
            }
            let panel = crate::chrome::panel_ground(&t);
            for ground in [
                colors.surfaces.content,
                colors.surfaces.sidebar,
                colors.surfaces.inspector,
                panel,
            ] {
                let raised = t.on_ground(ground).tokens.colors.surfaces.raised;
                let off = (l(raised) - l(ground)).abs();
                assert!(
                    off + 1e-4 >= step.abs() / 2.0,
                    "{}: a face on {ground:?} stands {off:.3} off it (step {step:.3})",
                    t.name
                );
            }
        }
    }

    #[test]
    fn temperature_and_your_colour_are_theme_builders() {
        // phase: a_tinted_palette_takes_the_workspace_hue
        {
            let warm = Theme::light().with_neutral_hue(60.0);
            let h = Oklch::from_color(warm.tokens.colors.surfaces.sidebar).h;
            assert!((h - 60.0).abs() <= 25.0, "sidebar hue {h:.0}");
            assert_eq!(
                warm.region(crate::surface::Region::Content),
                warm.tokens.colors.surfaces.content
            );
            assert_eq!(warm.text_dim, warm.tokens.colors.text_dim);
            assert_eq!(warm.name, "Light");
            assert_eq!(warm.document.palette.hue, Some(60.0));
            let same = Theme::light().with_neutral_hue(f32::NAN);
            assert_eq!(
                same.tokens.colors.surfaces.content,
                Theme::light().tokens.colors.surfaces.content,
                "a NaN hue is ignored"
            );
        }
        // phase: a_temperature_moves_the_hue_never_the_lightness_or_chroma
        // The stock material is hue 260 at chroma 0.006; a workspace's hue
        // replaces the hue only (design language, "Workspace temperature").
        {
            let dark = Theme::dark().with_neutral_hue(60.0);
            let stock = Theme::dark();
            for (warm, cool) in [
                (
                    dark.tokens.colors.surfaces.base,
                    stock.tokens.colors.surfaces.base,
                ),
                (
                    dark.tokens.colors.surfaces.content,
                    stock.tokens.colors.surfaces.content,
                ),
            ] {
                let (w, c) = (Oklch::from_color(warm), Oklch::from_color(cool));
                assert!((w.l - c.l).abs() <= 0.005, "lightness {} vs {}", w.l, c.l);
                assert!(w.c <= 0.010 + 0.004, "chroma {}", w.c);
            }
        }
        // phase: your_colour_is_the_focus_ring_and_survives_every_track
        {
            let mine = Theme::dark().with_your_colour(PresenceHue::Magenta);
            let you = mine.tokens.colors.intent.you;
            // The ring is the accent's ink: your hue at its contrast floor.
            assert_eq!(mine.border_focus, mine.tokens.colors.intent.accent_ink);
            let ring = (Oklch::from_color(mine.border_focus).h - Oklch::from_color(you).h)
                .rem_euclid(360.0);
            assert!(ring.min(360.0 - ring) < 15.0, "the ring is your hue");
            assert_ne!(you, Theme::dark().tokens.colors.intent.you);
            let warm = mine.clone().with_neutral_hue(40.0);
            assert_eq!(warm.tokens.colors.you, PresenceHue::Magenta);
            let hc = mine.with_a11y(
                &crate::a11y::A11ySettings {
                    high_contrast: true,
                    ..Default::default()
                },
                1.0,
                1.0,
            );
            assert_eq!(hc.tokens.colors.you, PresenceHue::Magenta);
            let hue = |c: Color| Oklch::from_color(c).h;
            let d = (hue(hc.border_focus) - hue(you)).rem_euclid(360.0);
            assert!(d.min(360.0 - d) < 5.0, "high contrast keeps your hue");
        }
        // phase: a_wallpaper_gives_its_hue_and_a_grey_one_none
        {
            let orange: Vec<u8> = [0xE0, 0x80, 0x30, 0xFF].repeat(64);
            let hue = workspace_hue(&orange).expect("an orange wallpaper has a hue");
            let want = Oklch::from_color(Color::from_rgb8(0xE0, 0x80, 0x30)).h;
            assert!((hue - want).abs() < 6.0, "{hue} vs {want}");
            let grey: Vec<u8> = [0x80, 0x80, 0x80, 0xFF].repeat(64);
            assert_eq!(workspace_hue(&grey), None);
            assert_eq!(workspace_hue(&[]), None);
        }
        // phase: light_without_a_workspace_is_the_stock_temperature_in_every_region
        // Cool, not grey-blue: chroma never past 0.010 (8-bit quantisation
        // adds a few thousandths).
        {
            let light = Theme::light();
            for region in [
                crate::surface::Region::Content,
                crate::surface::Region::Sidebar,
                crate::surface::Region::Inspector,
            ] {
                let ground = Oklch::from_color(light.region(region));
                assert!(ground.c <= 0.014, "{region:?} chroma {}", ground.c);
            }
            let base = Oklch::from_color(light.tokens.colors.surfaces.base);
            assert!(base.c <= 0.014, "base chroma {}", base.c);
        }
    }

    #[test]
    fn text_and_ui_scale_multiply_compose_and_clamp() {
        {
            let base = Theme::dark();
            let scaled = Theme::dark().with_text_scale(1.5);
            assert_eq!(scaled.text_scale, 1.5);
            for role in crate::tokens::TextRole::ALL {
                assert!((scaled.text(role).size - base.text(role).size * 1.5).abs() < 1e-4);
            }
        }

        {
            assert!(Theme::dark().with_text_scale(0.0).text_scale >= 0.5);
            assert!(Theme::dark().with_text_scale(100.0).text_scale <= 4.0);
        }

        {
            let base = Theme::dark();
            let bs = base.tokens.spacing.s3;
            let r = base.tokens.radii.clone();
            let bt = base.text(crate::tokens::TextRole::Body).size;

            let scaled = Theme::dark().with_ui_scale(1.5);
            assert_eq!(scaled.ui_scale, 1.5);
            assert!((scaled.tokens.spacing.s3 - bs * 1.5).abs() < 1e-4);
            let s = &scaled.tokens.radii;
            for (name, got, want) in [
                ("tile", s.tile, r.tile),
                ("panel", s.panel, r.panel),
                ("control", s.control, r.control),
                ("item", s.item, r.item),
                ("detail", s.detail, r.detail),
            ] {
                assert!((got - want * 1.5).abs() < 1e-4, "{name} grows");
            }
            assert_eq!(s.full, r.full, "radii.full is a sentinel, not scaled");
            assert!((scaled.text(crate::tokens::TextRole::Body).size - bt * 1.5).abs() < 1e-4);
            assert!((scaled.padding_standard - bs * 1.5).abs() < 1e-4);
            assert!((scaled.radius_standard - f64::from(r.control) * 1.5).abs() < 1e-4);
        }

        {
            assert!(Theme::dark().with_ui_scale(0.0).ui_scale >= 0.5);
            assert!(Theme::dark().with_ui_scale(100.0).ui_scale <= 3.0);
        }

        {
            let base = Theme::dark();
            let base_type = base.text(crate::tokens::TextRole::Body).size;
            let base_gap = base.tokens.spacing.s3;
            let composed = Theme::dark().with_ui_scale(1.25).with_text_scale(1.2);
            assert!(
                (composed.text(crate::tokens::TextRole::Body).size - base_type * 1.25 * 1.2).abs()
                    < 1e-3
            );
            assert!((composed.tokens.spacing.s3 - base_gap * 1.25).abs() < 1e-3);
        }

        {
            assert_eq!(Theme::dark().responsive_scale(), 1.0);
            assert_eq!(Theme::dark().with_text_scale(2.0).responsive_scale(), 2.0);
            assert_eq!(Theme::dark().with_text_scale(0.75).responsive_scale(), 1.0);
            assert_eq!(Theme::dark().with_ui_scale(1.5).responsive_scale(), 1.5);
            assert!((Theme::dark().with_ui_scale(0.5).responsive_scale() - 0.5).abs() < 1e-9);
        }
    }

    #[test]
    fn a11y_preferences_fold_into_the_theme() {
        {
            assert!(!Theme::dark().reduce_motion, "default is motion on");
            assert!(Theme::dark().with_reduce_motion(true).reduce_motion);
        }

        {
            assert!(Theme::configured_from_text("", true).reduce_motion);
            assert!(!Theme::configured_from_text("", false).reduce_motion);
        }

        // Case `the_settings_switch_reaches_a_host_without_the_override`.
        // The shell reads its chrome theme through `configured_from_text`
        // on every settings change, so the person's switch must be enough.
        {
            let on = r#"{"appearance":{"reduce_motion":true}}"#;
            let off = r#"{"appearance":{"reduce_motion":false}}"#;
            assert!(Theme::configured_from_text(on, false).reduce_motion);
            assert!(!Theme::configured_from_text(off, false).reduce_motion);
            assert!(Theme::configured_from_text(off, true).reduce_motion);
            let layer =
                r#"{"appearance":{"accessibility":{"schema_version":1,"reduce_motion":true}}}"#;
            assert!(Theme::configured_from_text(layer, false).reduce_motion);
            let rejected =
                r#"{"appearance":{"accessibility":{"schema_version":99,"reduce_motion":true}}}"#;
            assert!(!Theme::configured_from_text(rejected, false).reduce_motion);
        }

        // Case `with_a11y_picks_the_high_contrast_of_the_theme_s_polarity`.
        {
            use crate::a11y::A11ySettings;
            let settings = A11ySettings {
                high_contrast: true,
                reduce_motion: true,
                screen_reader: false,
            };
            let theme = Theme::dark().with_a11y(&settings, 2.0, 1.0);
            assert_eq!(theme.name, "High contrast");
            assert!(
                contrast_ratio(theme.text_primary, theme.tokens.colors.surfaces.content) >= 7.0
            );
            assert!(theme.reduce_motion);
            assert_eq!(theme.text_scale, 2.0);
            let light = Theme::light().with_a11y(&settings, 1.0, 1.0);
            assert_eq!(light.name, "High contrast light");
            assert!(light.colors_are_forced());
            assert!(contrast_ratio(light.text_dim, light.tokens.colors.surfaces.content) >= 7.0);
        }

        // Case `with_a11y_all_off_keeps_base_colours`.
        {
            let off = crate::a11y::A11ySettings::default();
            let theme = Theme::light().with_a11y(&off, 1.0, 1.0);
            assert_eq!(theme.name, "Light");
            assert!(!theme.reduce_motion);
        }
    }

    #[test]
    fn a_document_sets_every_role_it_names() {
        {
            let doc = quiet_ocean(1.25);
            let json = serde_json::json!({
                "appearance": {
                    "mode": "dark",
                    "dark_theme": "custom:quiet-ocean",
                    "custom_themes": [doc],
                }
            });
            let theme = Theme::from_settings_json(&json.to_string()).unwrap();
            assert_eq!(theme.name, "Quiet ocean");
            assert_eq!(theme.document.palette.hue, Some(220.0));
            assert!(theme.document.palette.dark);
            assert!((theme.tokens.spacing.s3 - 15.0).abs() < 1e-4);
            assert_eq!(theme.tokens.typography.sans_family, "Inter");
            assert_eq!(theme.tokens.radii, crate::tokens::Radii::default());
            assert!(crate::tokens::contrast::audit(&theme.tokens.colors).is_empty());
        }

        {
            let theme = Theme::from_document(&quiet_ocean(1.25)).unwrap();
            let s = theme.tokens.spacing;
            for (value, steps) in [(s.s1, 1.0), (s.s4, 4.0), (s.s16, 16.0)] {
                assert!((value - 4.0 * 1.25 * steps).abs() < 1e-3);
            }
            assert_eq!(theme.tokens.metrics, crate::tokens::Metrics::BASE);
            let dense = Theme::from_document(&quiet_ocean(0.5)).unwrap();
            assert!((dense.tokens.spacing.s3 - 12.0 * 0.75).abs() < 1e-4);
            assert!(Theme::from_document(&quiet_ocean(0.1)).is_none());
        }

        {
            let mut doc = quiet_ocean(1.0);
            doc.shape.window_radius = 28.0;
            doc.type_.base_size = 16.0;
            doc.motion.character = 0.0;
            let selector = crate::tokens::SelectorFinish {
                strip: crate::tokens::StripMaterial::Opaque,
                rim_span: 0.3,
                rim_alpha: 0.1,
                hover_alpha: 0.8,
            };
            doc.material = ThemeMaterial {
                blur: 40.0,
                tint_opacity: 0.9,
                selector,
            };
            doc.emphasis = Emphasis::Tinted { hue: 150.0 };
            let theme = Theme::from_document(&doc).unwrap();
            let stock = Theme::dark();
            assert!((theme.tokens.radii.tile - 28.0).abs() < 1e-4);
            assert!((theme.tokens.radii.control - 16.0).abs() < 1e-4);
            assert!(
                (theme.tokens.typography.size(crate::tokens::TextRole::Body) - 16.0).abs() < 1e-4
            );
            assert_eq!(
                theme.tokens.motion.springs.snappy.stiffness,
                stock.tokens.motion.springs.subtle.stiffness
            );
            assert!((theme.tokens.colors.surfaces.material_blur_px - 40.0).abs() < 1e-4);
            assert_eq!(theme.tokens.colors.surfaces.material.a(), 230);
            assert_eq!(theme.tokens.selector, selector);
            let fill = Oklch::from_color(theme.tokens.colors.intent.accent);
            assert!((fill.h - 150.0).abs() < 6.0 && fill.c > 0.08);
        }

        {
            let mut old = quiet_ocean(1.0);
            old.schema_version = 4;
            assert!(Theme::from_document(&old).is_none());
            let json = serde_json::json!({
                "appearance": { "dark_theme": "custom:wanted", "custom_themes": [quiet_ocean(1.0)] }
            });
            assert!(Theme::from_settings_json(&json.to_string()).is_none());
            let legacy =
                r#"{"appearance":{"theme":"custom:x","custom_theme":{"schema_version":4}}}"#;
            assert!(Theme::from_settings_json(legacy).is_none());
        }
    }

    #[test]
    fn the_automatic_mode_is_light_by_day_and_dark_by_night() {
        use settings_store::day_schedule::LocalMoment;
        let json = r#"{"appearance":{"mode":"automatic","dark_theme":"dark-soft","schedule":"custom","sunrise":"07:00","sunset":"19:00"}}"#;
        let at = |minute_of_day| LocalMoment {
            day_of_year: 100,
            minute_of_day,
            utc_offset_minutes: 0,
        };
        assert_eq!(
            Theme::from_settings_json_at(json, at(12 * 60))
                .unwrap()
                .name,
            Theme::light().name
        );
        assert_eq!(
            Theme::from_settings_json_at(json, at(22 * 60))
                .unwrap()
                .name,
            "Dark soft"
        );
    }
}

#[cfg(test)]
mod configured_theme {
    use super::*;

    #[test]
    fn startup_theme_follows_the_settings_file_or_its_absence() {
        {
            let dir = tempfile::tempdir().expect("temp config dir");
            let settings = dir.path().join("settings.json");
            std::fs::write(
                &settings,
                r#"{"version":1,"appearance":{"dark_theme":"high-contrast"}}"#,
            )
            .expect("write settings");

            assert_eq!(
                Theme::configured_from(Some(&settings)).name,
                Theme::high_contrast().name,
                "a settings file asking for high contrast must produce it",
            );

            std::fs::write(
                &settings,
                r#"{"appearance":{"mode":"light","high_contrast":true}}"#,
            )
            .expect("write settings");
            assert_eq!(
                Theme::configured_from(Some(&settings)).name,
                "High contrast light",
                "appearance.high_contrast must reach the shell's chrome",
            );
            std::fs::write(
                &settings,
                r#"{"appearance":{"mode":"light","high_contrast":false}}"#,
            )
            .expect("write settings");
            assert_eq!(
                Theme::configured_from(Some(&settings)).name,
                Theme::light().name
            );
        }

        // Case `the_reduce_motion_switch_reaches_the_shells_chrome_live`:
        // the shell re-reads its theme through this door on every settings
        // change, so flipping the switch must flip the chrome's motion.
        if !crate::host::environment().accessibility.reduce_motion {
            let dir = tempfile::tempdir().expect("temp config dir");
            let settings = dir.path().join("settings.json");
            std::fs::write(&settings, r#"{"appearance":{"reduce_motion":true}}"#)
                .expect("write settings");
            assert!(Theme::configured_from(Some(&settings)).reduce_motion);
            std::fs::write(&settings, r#"{"appearance":{"reduce_motion":false}}"#)
                .expect("write settings");
            assert!(!Theme::configured_from(Some(&settings)).reduce_motion);
        }

        // Case `the_configured_theme_does_not_read_the_identity`: the
        // person's colour reaches a window through its host
        // (`Theme::in_window`), never through the theme.
        {
            let dir = tempfile::tempdir().expect("temp config dir");
            let settings = dir.path().join("settings.json");
            std::fs::write(&settings, r#"{"identity":{"colour":"teal"}}"#).expect("write");
            assert_eq!(
                Theme::configured_from(Some(&settings)).tokens.colors.you,
                Theme::dark().tokens.colors.you
            );
        }

        // Case `an_absent_settings_file_still_yields_a_usable_theme`:
        // No preference is not an error, and must not stop a desktop starting.
        {
            let dir = tempfile::tempdir().expect("temp config dir");
            let missing = dir.path().join("settings.json");
            assert_eq!(
                Theme::configured_from(Some(&missing)).name,
                Theme::dark().name
            );
            assert_eq!(Theme::configured_from(None).name, Theme::dark().name);
        }
    }
}
