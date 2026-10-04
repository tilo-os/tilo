//! Saved keyboard preferences, illustrated with immutable built-in layouts.
//! Layout definitions and typed/learned data are deliberately not recorded.
use crate::presentation::{KeyboardGeometry, KeyboardPresentation, keyboard_geometry};
use crate::widget::{key_row_height, keyboard_gutter, suggestion_row_height, toolbar_row_height};
use crate::{KeyboardLayout, OskToolbarItem, ShiftState};
use automerge::{AutoCommit, ROOT, ReadDoc};
use ui::history_mode::restore::{
    HistoryRestoreRoot, HistoryRestoreSchema, RestoreValueType, ScalarType,
};
use ui::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
    PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView,
};
use ui::paint::PaintCtx;
use ui::{TextRenderer, TextRole, Theme};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
    peniko::{BlendMode, Fill},
};

pub const SCOPE: &str = "preferences";
pub(crate) const LAYOUT: &str = "osk.preferred_layout";
pub(crate) const SUGGESTIONS: &str = "osk.suggestions_enabled";
pub(crate) const PERSONALIZATION: &str = "osk.personalization_enabled";
/// Recently picked emoji: learned from use, so a restore keeps the current
/// ones rather than rewinding them.
pub(crate) const EMOJI_RECENTS: &str = "osk.emoji_recents";
const ROOTS: &[HistoryRestoreRoot] = &[
    HistoryRestoreRoot {
        key: LAYOUT,
        required: false,
        expected_type: RestoreValueType::Scalar(ScalarType::String),
    },
    HistoryRestoreRoot {
        key: SUGGESTIONS,
        required: false,
        expected_type: RestoreValueType::Scalar(ScalarType::Boolean),
    },
    HistoryRestoreRoot {
        key: PERSONALIZATION,
        required: false,
        expected_type: RestoreValueType::Scalar(ScalarType::Boolean),
    },
];
const fn schema<const MASK: u8>() -> HistoryRestoreSchema {
    HistoryRestoreSchema {
        id: "osk.preferences",
        version: 1,
        roots: ROOTS,
        selectors: &[],
        preserved_roots: &[EMOJI_RECENTS],
        preserved: &[],
        validate: validate::<MASK>,
        validate_result: validate::<MASK>,
    }
}
static SCHEMA_BOTH: HistoryRestoreSchema = schema::<3>();
static SCHEMA_Q: HistoryRestoreSchema = schema::<1>();
static SCHEMA_A: HistoryRestoreSchema = schema::<2>();
static SCHEMA_NONE: HistoryRestoreSchema = schema::<0>();
macro_rules! definition {
    ($name:ident,$mask:literal,$schema:ident) => {
        pub static $name: HistoryDefinition = HistoryDefinition {
            app_id: "so.tilo.osk",
            scopes: &[HistoryScopeDefinition {
                key: SCOPE,
                title: "Keyboard preferences",
                presentation: HistoryPresentation::Domain {
                    create: preview::<$mask>,
                },
                content_schema: Some(&$schema),
            }],
        };
    };
}
definition!(HISTORY, 3, SCHEMA_BOTH);
definition!(HISTORY_QWERTY, 1, SCHEMA_Q);
definition!(HISTORY_AZERTY, 2, SCHEMA_A);
definition!(HISTORY_CUSTOM, 0, SCHEMA_NONE);

pub(crate) fn definition(qwerty: bool, azerty: bool) -> &'static HistoryDefinition {
    match (qwerty, azerty) {
        (true, true) => &HISTORY,
        (true, false) => &HISTORY_QWERTY,
        (false, true) => &HISTORY_AZERTY,
        (false, false) => &HISTORY_CUSTOM,
    }
}
fn preferences<const MASK: u8>(doc: &AutoCommit) -> Result<(String, bool, bool), String> {
    let layout = match doc.get(ROOT, LAYOUT).map_err(|e| e.to_string())? {
        None => "qwerty".to_owned(),
        Some((v, _)) => v
            .as_str()
            .ok_or("Saved keyboard layout must be a string")?
            .to_owned(),
    };
    if !matches!(
        (layout.as_str(), MASK),
        ("qwerty", 1 | 3) | ("azerty", 2 | 3)
    ) {
        let support = match MASK {
            1 => "This instance supports QWERTY (including an absent/default preference).",
            2 => {
                "This instance supports explicit AZERTY only; an absent preference defaults to unsupported QWERTY."
            }
            3 => "This instance supports QWERTY, AZERTY and an absent/default preference.",
            _ => {
                "This custom instance has no canonical text layout, so no layout snapshot can be previewed or restored."
            }
        };
        let recovery = if MASK == 0 {
            ""
        } else {
            " Choose a supported historical version. If the current saved preference is unsupported, return to live and select a supported layout first."
        };
        return Err(format!(
            "Saved layout {layout:?} is unsupported. {support}{recovery} Custom layout definitions are not recorded."
        ));
    }
    let boolean = |key| -> Result<bool, String> {
        match doc.get(ROOT, key).map_err(|e| e.to_string())? {
            None => Ok(true),
            Some((v, _)) => v
                .as_bool()
                .ok_or_else(|| format!("{key} must be a boolean")),
        }
    };
    Ok((layout, boolean(SUGGESTIONS)?, boolean(PERSONALIZATION)?))
}
fn validate<const MASK: u8>(doc: &AutoCommit) -> Result<(), String> {
    preferences::<MASK>(doc).map(|_| ())
}
fn preview<const MASK: u8>(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    let (name, suggestions, personalization) = preferences::<MASK>(&doc)?;
    let layout = if name == "azerty" {
        KeyboardLayout::azerty()
    } else {
        KeyboardLayout::qwerty()
    };
    let enabled = |value| if value { "on" } else { "off" };
    let mut layout_name = name;
    if let Some(first) = layout_name.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    let description = format!(
        "Layout: {layout_name}\nSuggestions: {}\nPersonalization: {}",
        enabled(suggestions),
        enabled(personalization)
    );
    Ok(Box::new(KeyboardPreview {
        layout,
        toolbar: OskToolbarItem::default_set(),
        description,
        view: input.initial_view,
        bounds: Rect::ZERO,
        geometry: None,
    }))
}
struct KeyboardPreview {
    layout: KeyboardLayout,
    toolbar: Vec<OskToolbarItem>,
    description: String,
    view: PreviewView,
    bounds: Rect,
    geometry: Option<KeyboardGeometry>,
}
impl HistoryPreview for KeyboardPreview {
    fn layout(
        &mut self,
        bounds: Rect,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Result<(), String> {
        self.bounds = bounds;
        let inset = f64::from(theme.tokens.spacing.s3);
        let (_, height) = text.measure_style(
            &self.description,
            Some((bounds.width() - 2.0 * inset).max(1.) as f32),
            &theme.text(TextRole::Body),
        );
        let y = bounds.y0 + f64::from(height) + 2.0 * inset;
        let toolbar = toolbar_row_height(&theme.tokens.spacing);
        let suggestions = suggestion_row_height(&theme.tokens.metrics);
        let rows =
            self.layout.row_count() as f64 * f64::from(key_row_height(&theme.tokens.metrics));
        let rect = Rect::new(
            bounds.x0,
            y,
            bounds.x1,
            y + f64::from(toolbar) + f64::from(suggestions) + rows,
        );
        self.geometry = Some(keyboard_geometry(
            rect,
            &self.layout,
            &self.toolbar,
            &[],
            ShiftState::Off,
            toolbar,
            suggestions,
            keyboard_gutter(&theme.tokens.spacing),
        ));
        Ok(())
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.,
            transform,
            &self.bounds,
        );
        let transform = transform * Affine::translate(self.view.scroll);
        let inset = f64::from(theme.tokens.spacing.s3);
        PaintCtx::new(scene, transform, text, theme).text_role(
            &self.description,
            TextRole::Body,
            theme.tokens.colors.text_primary,
            Point::new(self.bounds.x0 + inset, self.bounds.y0 + inset),
            Some((self.bounds.width() - 2.0 * inset).max(1.) as f32),
        );
        if let Some(g) = &self.geometry {
            KeyboardPresentation {
                rect: g.content_rect,
                layout: &self.layout,
                toolbar: &self.toolbar,
                suggestions: &[],
                toolbar_geom: &g.toolbar,
                suggestion_geom: &g.suggestions,
                key_geom: &g.keys,
                suggestion_area: g.suggestion_area,
                shift: ShiftState::Off,
                hovered_key: None,
                pressed_key: None,
                finger_keys: &[],
                hovered_tool: None,
                pressed_tool: None,
                hovered_sugg: None,
                pressed_sugg: None,
                empty_hint: "Typed text is never saved",
                emoji_panel_open: false,
                preview: None,
                alternates: None,
                cursor_mode: false,
                hit_cells: &[],
            }
            .draw(scene, transform, text, theme);
        }
        scene.pop_layer();
    }
    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        if let PreviewNavigation::Scroll { dx: _, dy } = command {
            self.view.scroll.1 = (self.view.scroll.1 + dy).min(0.);
            true
        } else {
            false
        }
    }
    fn view(&self) -> PreviewView {
        self.view.clone()
    }
    fn accessibility(&self) -> PreviewAccessibility {
        let keys = self
            .layout
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|k| k.display_label(ShiftState::Off))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n");
        PreviewAccessibility {
            description: format!("{}\n{keys}", self.description),
            pages: vec![],
        }
    }
}
