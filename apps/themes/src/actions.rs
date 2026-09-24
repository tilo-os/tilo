//! Themes' exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every document-mutating operation Themes performs on its shared catalog is a
//! [`ThemesAction`]: a small serializable value that deterministically mutates
//! the CRDT (see [`crate::doc`], where each custom theme is a native keyed
//! Automerge record and the active theme id is a scalar). Because that state already lives in the
//! shared document, an action replicates and replays for free. The exact same
//! [`EncodedAction`] is what the on-screen affordances (the top-bar Apply / New,
//! the customizer Duplicate / Reset / Delete, the sidebar context menu) dispatch
//! and what the Ctrl+K palette dispatches — one source of truth, one replay path.
//!
//! The resource tree mirrors the data model: the library owns its themes, and
//! each theme exports the [`ActionSpec`]s Ctrl+K offers for it (apply it as the
//! OS theme, rename it, recolour it, duplicate it, delete it).
//!
//! Presentation — selection, colour-picker drags until commit, open editor
//! tab, scroll position, and command pulses — is per-window local state. It is
//! never stored in the shared document, action history, or presence stream.
//!
//! ## Determinism contract
//!
//! Ids are minted at *authoring* time and carried in the payload (a New /
//! Duplicate mints its uuid in the affordance closure, never in
//! [`Action::apply`]). [`Action::apply`] never touches the RNG or the wall-clock,
//! and a payload that names a built-in preset or a missing theme is a **no-op**
//! rather than an error, so a bad replay can't wedge a peer.

use automerge::{AutoCommit, ROOT};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use tilo_actions::{
    Action, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec,
    ConfirmationPolicy, DiscoveryScope, Doc, EncodedAction, ExecutionLocus, OperationKind,
    ParamType, Resource, ResourceProvider, RetryPolicy, RiskLevel,
};

use crate::doc;
use crate::model::{ThemeSpec, TokenId, presets};

/// Registry `type_id` for every Themes action.
pub const THEMES_ACTION_TYPE: &str = "themes.action";

/// Every collaborative operation Themes exports on its catalog / active-theme
/// store.
///
/// Each variant is a whole-item write, a delete, or a scalar set carrying enough
/// payload to replay deterministically from a shared base state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ThemesAction {
    /// Make the theme named by `id` the active OS theme: writes the active id
    /// into the shared document (the in-app "Active" badge and the next-launch
    /// look). Persisting the full spec to `~/.config/tilo/settings.json` is an
    /// app-level side effect, not part of this replayable action. A malformed id
    /// (naming no theme in the catalog) is a no-op.
    SetActiveTheme {
        id: String,
    },
    /// Create a custom theme once. Existing ids are a no-op; field edits use
    /// the granular variants below so stale snapshots cannot overwrite peers.
    CreateCustomTheme {
        spec: ThemeSpec,
    },
    SetCustomThemeName {
        id: String,
        name: String,
    },
    SetCustomThemeToken {
        id: String,
        token: TokenId,
        rgb: crate::model::Rgb,
    },
    SetCustomThemeRadius {
        id: String,
        radius_norm: f32,
    },
    SetCustomThemeSpacing {
        id: String,
        spacing_norm: f32,
    },
    SetCustomThemeFont {
        id: String,
        font: String,
    },
    /// Reset deliberately writes the independent token/shape/font fields, not
    /// a replacement record, so unrelated concurrent edits remain mergeable.
    ResetCustomTheme {
        id: String,
    },
    /// Delete the custom theme `id`. Built-in presets are protected (a no-op) and
    /// the catalog is never emptied (deleting the last surviving theme is a
    /// no-op), mirroring the on-screen Delete guard.
    DeleteCustomTheme {
        id: String,
    },
}

impl Action for ThemesAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            ThemesAction::SetActiveTheme { id } => {
                // Only name a theme that actually exists in the catalog, so a
                // stale / malformed id can't point the desktop at nothing.
                let exists =
                    doc::load_catalog(doc, &ROOT).is_some_and(|c| c.iter().any(|s| &s.id == id));
                if exists {
                    doc::put_str(doc, &ROOT, doc::KEY_ACTIVE, id);
                }
            }
            ThemesAction::CreateCustomTheme { spec } => {
                let _ = doc::create_custom_theme(doc, &ROOT, spec);
            }
            ThemesAction::SetCustomThemeName { id, name } => {
                let _ = doc::set_name(doc, &ROOT, id, name);
            }
            ThemesAction::SetCustomThemeToken { id, token, rgb } => {
                let _ = doc::set_token(doc, &ROOT, id, *token, *rgb);
            }
            ThemesAction::SetCustomThemeRadius { id, radius_norm } => {
                let _ = doc::set_radius(doc, &ROOT, id, *radius_norm);
            }
            ThemesAction::SetCustomThemeSpacing { id, spacing_norm } => {
                let _ = doc::set_spacing(doc, &ROOT, id, *spacing_norm);
            }
            ThemesAction::SetCustomThemeFont { id, font } => {
                let _ = doc::set_font(doc, &ROOT, id, font);
            }
            ThemesAction::ResetCustomTheme { id } => {
                if let Some(current) = doc::load_catalog(doc, &ROOT)
                    .and_then(|c| c.into_iter().find(|s| s.id == *id && !s.builtin))
                {
                    let base = reset_spec(&current);
                    let _ = doc::set_token(doc, &ROOT, id, TokenId::Accent, base.accent);
                    let _ = doc::set_token(doc, &ROOT, id, TokenId::AccentHover, base.accent_hover);
                    let _ = doc::set_token(doc, &ROOT, id, TokenId::SurfaceBase, base.surface_base);
                    let _ =
                        doc::set_token(doc, &ROOT, id, TokenId::SurfaceRaised, base.surface_raised);
                    let _ = doc::set_token(doc, &ROOT, id, TokenId::TextPrimary, base.text_primary);
                    let _ = doc::set_token(doc, &ROOT, id, TokenId::TextDim, base.text_dim);
                    let _ = doc::set_token(doc, &ROOT, id, TokenId::Border, base.border);
                    let _ = doc::set_token(doc, &ROOT, id, TokenId::Success, base.success);
                    let _ = doc::set_token(doc, &ROOT, id, TokenId::Warning, base.warning);
                    let _ = doc::set_token(doc, &ROOT, id, TokenId::Danger, base.danger);
                    let _ = doc::set_token(doc, &ROOT, id, TokenId::Info, base.info);
                    let _ = doc::set_radius(doc, &ROOT, id, base.radius_norm);
                    let _ = doc::set_spacing(doc, &ROOT, id, base.spacing_norm);
                    let _ = doc::set_font(doc, &ROOT, id, &base.font);
                }
            }
            ThemesAction::DeleteCustomTheme { id } => {
                let _ = doc::delete_custom_theme(doc, &ROOT, id);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register Themes' action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<ThemesAction>(THEMES_ACTION_TYPE);
}

/// Encode any [`ThemesAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Themes action into a
/// replayable envelope.
///
/// Returns `EncodedAction` rather than a `Result`: every call site — including
/// the `build` closures [`ActionSpec::prompted`](tilo_actions::ActionSpec::prompted)
/// requires to return `EncodedAction` directly — needs the value unconditionally,
/// and every [`ThemesAction`] variant carries only plain `String` / [`ThemeSpec`]
/// fields, which `serde_json` cannot fail to serialize.
///
/// # Panics
///
/// Panics if the action cannot be serialized (should never happen for valid actions).
#[allow(
    clippy::expect_used,
    reason = "ThemesAction's fields are always JSON-serializable; see doc comment"
)]
pub fn encode(action: &ThemesAction) -> EncodedAction {
    EncodedAction::encode(THEMES_ACTION_TYPE, action).expect("ThemesAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Apply `id` as the active OS theme.
pub fn set_active_theme_action(id: impl Into<String>) -> ThemesAction {
    ThemesAction::SetActiveTheme { id: id.into() }
}

/// Create a custom theme (New or Duplicate). Existing ids are deliberately a
/// no-op; use a field-specific builder for an edit.
pub fn upsert_custom_theme_action(spec: &ThemeSpec) -> ThemesAction {
    ThemesAction::CreateCustomTheme { spec: spec.clone() }
}
pub fn set_custom_theme_name_action(
    id: impl Into<String>,
    name: impl Into<String>,
) -> ThemesAction {
    ThemesAction::SetCustomThemeName {
        id: id.into(),
        name: name.into(),
    }
}
pub fn set_custom_theme_token_action(
    id: impl Into<String>,
    token: TokenId,
    rgb: crate::model::Rgb,
) -> ThemesAction {
    ThemesAction::SetCustomThemeToken {
        id: id.into(),
        token,
        rgb,
    }
}
pub fn set_custom_theme_radius_action(id: impl Into<String>, radius_norm: f32) -> ThemesAction {
    ThemesAction::SetCustomThemeRadius {
        id: id.into(),
        radius_norm,
    }
}
pub fn set_custom_theme_spacing_action(id: impl Into<String>, spacing_norm: f32) -> ThemesAction {
    ThemesAction::SetCustomThemeSpacing {
        id: id.into(),
        spacing_norm,
    }
}
pub fn set_custom_theme_font_action(
    id: impl Into<String>,
    font: impl Into<String>,
) -> ThemesAction {
    ThemesAction::SetCustomThemeFont {
        id: id.into(),
        font: font.into(),
    }
}
pub fn reset_custom_theme_action(id: impl Into<String>) -> ThemesAction {
    ThemesAction::ResetCustomTheme { id: id.into() }
}

/// Brokered default creation used by the visible New button. The app mints
/// the stable id when this operation is invoked, then dispatches the same
/// `CreateCustomTheme` action as the named palette flow.
#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "the unit payload always serializes"
)]
pub fn new_default_spec() -> ActionSpec {
    ActionSpec::operation("new", "New theme", OperationKind::Effect, "themes.new", &())
        .expect("unit payload serializes")
        .with_contract(ActionContract {
            discovery: DiscoveryScope::Launchable,
            tags: vec!["appearance".into(), "theme".into(), "create".into()],
            ..ActionContract::default()
        })
}

#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "the static resource has this nullary action"
)]
pub fn new_default_binding() -> tilo_actions::ActionBinding<()> {
    let resource =
        Resource::new("themes.library", "themes.library", "Themes").with_action(new_default_spec());
    tilo_actions::ActionBinding::unit_for(&resource, "new")
        .expect("the static New theme action accepts unit arguments")
}

/// Local brokered effect for exporting the selected theme. It has no document
/// payload and never enters collaboration/history.
#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "the unit payload always serializes"
)]
pub fn export_selected_spec() -> ActionSpec {
    ActionSpec::operation(
        "export-selected",
        "Export selected theme JSON",
        OperationKind::Effect,
        "themes.export",
        &(),
    )
    .expect("unit payload serializes")
    .with_contract(ActionContract {
        discovery: DiscoveryScope::Launchable,
        execution: tilo_actions::ExecutionMode::Background { cancellable: false },
        tags: vec!["appearance".into(), "theme".into(), "export".into()],
        ..ActionContract::default()
    })
}

#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "the static resource has this nullary action"
)]
pub fn export_selected_binding() -> tilo_actions::ActionBinding<()> {
    let resource = Resource::new("themes.library", "themes.library", "Themes")
        .with_action(export_selected_spec());
    tilo_actions::ActionBinding::unit_for(&resource, "export-selected")
        .expect("export-selected is nullary")
}

/// The selected-theme counterpart of a per-theme Apply operation. The top bar
/// binds this exact brokered effect while resource children carry stable ids.
#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "the unit payload always serializes"
)]
pub fn apply_selected_spec() -> ActionSpec {
    ActionSpec::operation(
        "apply-selected",
        "Apply selected theme",
        OperationKind::Effect,
        "themes.apply",
        &(),
    )
    .expect("unit payload serializes")
    .with_contract(ActionContract {
        discovery: DiscoveryScope::Launchable,
        retry: RetryPolicy::Idempotent,
        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
        result: tilo_actions::ResultSpec::Job,
        tags: vec!["appearance".into(), "theme".into()],
        ..ActionContract::default()
    })
}

#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "the static resource has this nullary action"
)]
pub fn apply_selected_binding() -> tilo_actions::ActionBinding<()> {
    let resource = Resource::new("themes.library", "themes.library", "Themes")
        .with_action(apply_selected_spec());
    tilo_actions::ActionBinding::unit_for(&resource, "apply-selected")
        .expect("apply-selected is nullary")
}

/// Delete a custom theme. A durable tombstone makes delete win over an edit
/// delivered later by an offline peer.
pub fn delete_custom_theme_action(id: impl Into<String>) -> ThemesAction {
    ThemesAction::DeleteCustomTheme { id: id.into() }
}

/// The highest `order` in `catalog`, so a New / Duplicate can append after every
/// existing row. Zero for an empty catalog.
pub fn max_order(catalog: &[ThemeSpec]) -> u32 {
    catalog.iter().map(|s| s.order).max().unwrap_or(0)
}

/// The first built-in preset, used as the starting tokens for a fresh theme
/// ([`new_theme_spec`]) and a reset ([`reset_spec`]). [`presets`] is built
/// from the fixed `PRESET_ENTRIES` table in [`crate::model`] and is never
/// empty; the indexing is centralised here with a documented reason rather
/// than repeated at each call site.
#[allow(
    clippy::indexing_slicing,
    reason = "presets() is built from the fixed, non-empty PRESET_ENTRIES table in crate::model"
)]
fn base_preset() -> ThemeSpec {
    presets()[0].clone()
}

/// A fresh editable theme starting from the first preset's tokens, carrying the
/// authored `id` / `name` / `order`. Shared by the top-bar New button and the
/// palette's "New theme" action so both dispatch the identical upsert.
pub fn new_theme_spec(id: &str, name: &str, order: u32) -> ThemeSpec {
    let mut s = base_preset();
    s.id = id.to_string();
    s.name = name.to_string();
    s.builtin = false;
    s.order = order;
    s
}

/// An editable copy of `src` carrying the authored `id` / `order` and a
/// "` copy`" name. Shared by the Duplicate affordances and the palette.
pub fn duplicate_spec(src: &ThemeSpec, id: &str, order: u32) -> ThemeSpec {
    let mut s = src.clone();
    s.id = id.to_string();
    s.name = format!("{} copy", src.name);
    s.builtin = false;
    s.order = order;
    s
}

/// The reset form of a custom theme: the first preset's tokens, keeping the
/// theme's own id / name / order so it stays the same catalog row. Shared by the
/// customizer Reset button and the palette.
pub fn reset_spec(current: &ThemeSpec) -> ThemeSpec {
    let mut s = base_preset();
    s.id.clone_from(&current.id);
    s.name.clone_from(&current.name);
    s.builtin = false;
    s.order = current.order;
    s
}

/// `current` with one colour token replaced — the committed form of a colour
/// edit. The live picker drag is presentation; only the committed value becomes
/// this upsert. Shared by the palette's "Set … colour" action.
pub fn recolor_spec(current: &ThemeSpec, token: TokenId, rgb: crate::model::Rgb) -> ThemeSpec {
    let mut s = current.clone();
    s.set_token(token, rgb);
    s
}

/// `current` with its name replaced — the committed form of a rename.
pub fn rename_spec(current: &ThemeSpec, name: &str) -> ThemeSpec {
    let mut s = current.clone();
    s.name = name.trim().to_string();
    s
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// Same non-fallible-in-practice contract as [`encode`]: `provide_resources`
/// returns a plain `Vec<Resource>` (the [`tilo_actions::ResourceProvider`]
/// trait shape), so there is no `Result` to propagate an encode failure into.
#[allow(
    clippy::expect_used,
    reason = "ThemesAction's fields are always JSON-serializable; see encode's doc comment"
)]
fn immediate_spec(id: &str, label: &str, action: &ThemesAction) -> ActionSpec {
    ActionSpec::immediate(id, label, THEMES_ACTION_TYPE, action)
        .expect("encode themes action")
        .with_contract(theme_contract())
}

fn theme_contract() -> ActionContract {
    ActionContract {
        retry: RetryPolicy::Idempotent,
        reversible: true,
        tags: vec!["appearance".into(), "theme".into()],
        ..ActionContract::default()
    }
}

/// Build the resource tree Themes currently has open: the theme library (with a
/// "New theme" action) and each theme in it. Every theme can be applied as the
/// OS theme or duplicated; custom themes additionally export rename, recolour,
/// and delete. Every document-mutating operation is exported as the same action
/// the on-screen affordance dispatches.
///
/// # Panics
///
/// Panics only if a validated catalog id cannot be JSON-encoded for the
/// external apply-operation envelope.
#[allow(
    clippy::too_many_lines,
    reason = "one sequential routine kept together for review"
)]
#[allow(
    clippy::expect_used,
    reason = "Theme ids admitted by doc::load_catalog are bounded UTF-8 strings and always serialize"
)]
pub fn provide_resources(doc: &AutoCommit) -> Vec<Resource> {
    let catalog = doc::load_catalog(doc, &ROOT).unwrap_or_default();

    let mut root = Resource::new("themes.library", "themes.library", "Themes")
        .with_action(new_default_spec())
        .with_action(export_selected_spec())
        .with_action(apply_selected_spec());

    // Library-level: create a new theme from the base preset. The uuid is minted
    // once when the name argument is committed, then baked into the envelope, so
    // replay stays deterministic.
    let next_order = max_order(&catalog) + 1;
    root = root.with_action(
        ActionSpec::typed(
            "new-theme",
            "New theme",
            "theme name…",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required("name", ParamType::Text, "Theme name")],
            move |args| {
                let name = args.text("name").unwrap_or_default();
                let name = {
                    let n = name.trim();
                    if n.is_empty() { "New theme" } else { n }
                };
                encode(&upsert_custom_theme_action(&new_theme_spec(
                    &Uuid::new_v4().to_string(),
                    name,
                    next_order,
                )))
            },
        )
        .with_contract(theme_contract()),
    );

    for spec in &catalog {
        let mut child = Resource::new(
            format!("themes.theme:{}", spec.id),
            "themes.theme",
            spec.name.clone(),
        );

        // Every theme can become the active OS theme.
        child = child.with_action(
            ActionSpec::operation(
                "apply",
                "Apply as system theme",
                OperationKind::Effect,
                "themes.apply",
                &spec.id,
            )
            .expect("theme id is serializable")
            .with_contract(ActionContract {
                discovery: DiscoveryScope::Launchable,
                retry: RetryPolicy::Idempotent,
                execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                result: tilo_actions::ResultSpec::Job,
                tags: vec!["appearance".into(), "theme".into()],
                ..ActionContract::default()
            }),
        );

        // Agents can export a specific stable id; the visible control uses the
        // sibling selected-theme operation with the same broker type.
        child = child.with_action(
            ActionSpec::operation(
                "export",
                "Export theme JSON",
                OperationKind::Effect,
                "themes.export",
                &spec.id,
            )
            .expect("theme id is serializable")
            .with_contract(ActionContract {
                discovery: DiscoveryScope::Launchable,
                execution: tilo_actions::ExecutionMode::Background { cancellable: false },
                tags: vec!["appearance".into(), "theme".into(), "export".into()],
                ..ActionContract::default()
            }),
        );

        // Every theme can be duplicated into a new editable copy. The copy's uuid
        // is minted at authoring time so replay is deterministic.
        let dup_order = next_order;
        child = child.with_action(immediate_spec(
            "duplicate",
            "Duplicate",
            &upsert_custom_theme_action(&duplicate_spec(
                spec,
                &Uuid::new_v4().to_string(),
                dup_order,
            )),
        ));

        if !spec.builtin {
            child = child.with_action(
                immediate_spec(
                    "reset",
                    "Reset theme",
                    &reset_custom_theme_action(spec.id.clone()),
                )
                .with_contract(ActionContract {
                    confirmation: ConfirmationPolicy::Confirm,
                    ..theme_contract()
                }),
            );
            // Rename — a prompted action carrying the rest of the theme unchanged.
            let base = spec.clone();
            child = child.with_action(
                ActionSpec::typed(
                    "edit",
                    "Rename theme",
                    "theme name…",
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required("name", ParamType::Text, "Theme name")],
                    move |args| {
                        encode(&set_custom_theme_name_action(
                            base.id.clone(),
                            args.text("name").unwrap_or_default(),
                        ))
                    },
                )
                .with_contract(theme_contract()),
            );
            // Set the accent colour — a prompted colour edit (the committed form
            // of a picker drag), taking a hex string.
            let base = spec.clone();
            child = child.with_action(
                ActionSpec::typed(
                    "set-accent",
                    "Set accent colour",
                    "#RRGGBB",
                    ExecutionLocus::ClientLocal,
                    vec![
                        ActionParam::required("colour", ParamType::Text, "Hex RGB colour")
                            .with_schema(serde_json::json!({ "pattern": "^#[0-9A-Fa-f]{6}$" })),
                    ],
                    move |args| {
                        let hex = args.text("colour").unwrap_or_default();
                        let rgb = crate::model::parse_hex(hex).unwrap_or(base.accent);
                        encode(&set_custom_theme_token_action(
                            base.id.clone(),
                            TokenId::Accent,
                            rgb,
                        ))
                    },
                )
                .with_contract(theme_contract()),
            );
            // Delete the custom theme.
            child = child.with_action(
                immediate_spec(
                    "delete",
                    "Delete theme",
                    &delete_custom_theme_action(spec.id.clone()),
                )
                .with_contract(ActionContract {
                    risk: RiskLevel::Destructive,
                    confirmation: ConfirmationPolicy::Confirm,
                    ..theme_contract()
                }),
            );
        }

        root = root.with_child(child);
    }

    vec![root]
}

impl ResourceProvider for crate::app::ThemesApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ThemesApp;
    use tilo_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } | ActionInput::Deferred { .. } => {
                panic!("expected an immediate action: {}", spec.id)
            }
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => {
                panic!("expected a prompted action: {}", spec.id)
            }
        }
    }

    /// A document seeded with the demo catalog (the presets plus one editable
    /// custom "sunset"), the same shape the app persists.
    fn seeded_doc() -> AutoCommit {
        let mut doc = AutoCommit::new();
        doc::save_catalog(&mut doc, &ROOT, &crate::model::demo_catalog());
        doc
    }

    fn catalog(doc: &AutoCommit) -> Vec<ThemeSpec> {
        doc::load_catalog(doc, &ROOT).unwrap()
    }

    /// Ctrl+K on Themes yields the tree: the library with a "New theme" action,
    /// a `themes.theme` child per theme exporting apply + duplicate, and — for a
    /// custom theme — rename, set-accent and delete. Drives the real resource
    /// hook (`Widget::resources`).
    #[test]
    fn ctrl_k_publishes_the_themes_tree() {
        let mut app = ThemesApp::new_demo();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);

        let resources = Widget::resources(&app, &st.doc);
        assert_eq!(resources.len(), 1, "one library root");
        let lib = &resources[0];
        assert_eq!(lib.kind, "themes.library");
        let lib_actions: Vec<&str> = lib.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(lib_actions.contains(&"new-theme"), "{lib_actions:?}");

        // A built-in preset exports apply + duplicate but neither delete nor edit.
        let builtin = lib
            .children
            .iter()
            .find(|r| r.id.as_str() == "themes.theme:tilo-blue")
            .expect("the tilo-blue preset resource");
        let b_actions: Vec<&str> = builtin.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(b_actions.contains(&"apply"), "{b_actions:?}");
        assert!(b_actions.contains(&"duplicate"), "{b_actions:?}");
        assert!(
            !b_actions.contains(&"delete"),
            "a preset is not deletable: {b_actions:?}"
        );
        assert!(
            !b_actions.contains(&"edit"),
            "a preset is not editable: {b_actions:?}"
        );

        // The custom theme adds rename, recolour and delete.
        let custom = lib
            .children
            .iter()
            .find(|r| r.id.as_str() == "themes.theme:sunset")
            .expect("the sunset custom resource");
        let c_actions: Vec<&str> = custom.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(c_actions.contains(&"apply"), "{c_actions:?}");
        assert!(c_actions.contains(&"edit"), "{c_actions:?}");
        assert!(c_actions.contains(&"set-accent"), "{c_actions:?}");
        assert!(c_actions.contains(&"reset"), "{c_actions:?}");
        assert!(c_actions.contains(&"delete"), "{c_actions:?}");
        assert_eq!(
            custom
                .actions
                .iter()
                .find(|action| action.id == "delete")
                .expect("delete action")
                .contract
                .risk,
            RiskLevel::Destructive
        );
        let apply = custom
            .actions
            .iter()
            .find(|action| action.id == "apply")
            .expect("apply operation");
        assert_eq!(apply.kind, OperationKind::Effect);
        assert_eq!(apply.contract.discovery, DiscoveryScope::Launchable);
        assert_eq!(
            apply.contract.execution,
            tilo_actions::ExecutionMode::Background { cancellable: true }
        );
        assert_eq!(apply.contract.result, tilo_actions::ResultSpec::Job);

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(labels.iter().any(|l| l.contains("New theme")), "{labels:?}");
        assert!(
            labels.iter().any(|l| l.contains("Apply as system theme")),
            "{labels:?}"
        );
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut state = ui::State::new();
        state.doc = seeded_doc();
        let before_heads = state.doc.get_heads();
        let before_len = catalog(&state.doc).len();
        assert!(
            catalog(&state.doc).iter().any(|s| s.id == "sunset"),
            "custom present"
        );

        let action = delete_custom_theme_action("sunset");
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            catalog(&state.doc).len(),
            before_len - 1,
            "the action deleted a theme"
        );
        assert!(
            !catalog(&state.doc).iter().any(|s| s.id == "sunset"),
            "the right theme is gone"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            catalog(&state.doc).len(),
            before_len,
            "undo reverted the deletion"
        );
        assert!(
            catalog(&state.doc).iter().any(|s| s.id == "sunset"),
            "the theme is back after undo"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let mut a = seeded_doc();
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        // Create a new custom theme (id minted at authoring time, carried in the
        // payload — apply never touches the RNG).
        let spec = new_theme_spec("11112222-3333-4444-5555-666677778888", "Ocean", 200);
        let action = upsert_custom_theme_action(&spec);
        let encoded = encode(&action);

        let env = tilo_actions::apply_local(&mut a, &action, encoded).unwrap();
        tilo_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert!(
            catalog(&a)
                .iter()
                .any(|s| s.id == spec.id && s.name == "Ocean")
        );
        assert!(
            catalog(&b)
                .iter()
                .any(|s| s.id == spec.id && s.name == "Ocean")
        );
    }

    /// Recolouring a custom theme also replays byte-identically — the colour-edit
    /// path (committed), not just create/delete.
    #[test]
    fn recolor_replays_byte_identical() {
        let mut a = seeded_doc();
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let sunset = catalog(&a).into_iter().find(|s| s.id == "sunset").unwrap();
        let action = set_custom_theme_token_action(&sunset.id, TokenId::Accent, [1, 2, 3]);
        let encoded = encode(&action);

        let env = tilo_actions::apply_local(&mut a, &action, encoded).unwrap();
        tilo_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "recolour replayed identically"
        );
        assert_eq!(
            catalog(&b)
                .iter()
                .find(|s| s.id == "sunset")
                .unwrap()
                .accent,
            [1, 2, 3]
        );
    }

    /// Deleting a built-in preset through the action is a safe no-op: the catalog
    /// is unchanged and the document's heads do not move (no empty change).
    #[test]
    fn deleting_a_builtin_is_a_safe_no_op() {
        let mut doc = seeded_doc();
        doc.commit();
        let before_heads = doc.get_heads();
        let before = catalog(&doc);

        let action = delete_custom_theme_action("tilo-blue");
        let encoded = encode(&action);
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut doc, &action, encoded, &mut tx).unwrap();

        assert_eq!(catalog(&doc), before, "a built-in preset is protected");
        assert_eq!(
            doc.get_heads(),
            before_heads,
            "a no-op delete writes no change"
        );
    }

    /// An upsert that names a built-in preset id is likewise a no-op — a preset
    /// can't be overwritten into an editable clone in place.
    #[test]
    fn upsert_over_a_builtin_id_is_a_no_op() {
        let mut doc = seeded_doc();
        doc.commit();
        let before = catalog(&doc);
        // Try to overwrite the tilo-blue preset (claiming custom).
        let mut sneaky = presets()[0].clone();
        sneaky.builtin = false;
        sneaky.name = "Hijacked".into();
        let action = upsert_custom_theme_action(&sneaky);
        let encoded = encode(&action);
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut doc, &action, encoded, &mut tx).unwrap();
        assert_eq!(
            catalog(&doc),
            before,
            "a preset id can't be hijacked by an upsert"
        );
    }

    /// Applying a theme writes its id as the active theme in the shared document.
    #[test]
    fn set_active_theme_writes_the_active_id() {
        let mut doc = seeded_doc();
        let action = set_active_theme_action("dracula");
        let encoded = encode(&action);
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut doc, &action, encoded, &mut tx).unwrap();
        assert_eq!(
            doc::get_str(&doc, &ROOT, doc::KEY_ACTIVE).as_deref(),
            Some("dracula")
        );
    }

    /// Single source of truth: the palette's per-theme specs and the builders the
    /// on-screen affordances call encode byte-identically.
    #[test]
    fn palette_and_builder_encode_identically() {
        let doc = seeded_doc();
        let resources = provide_resources(&doc);
        let lib = &resources[0];

        // The custom theme's delete spec == the delete builder.
        let custom = lib
            .children
            .iter()
            .find(|r| r.id.as_str() == "themes.theme:sunset")
            .unwrap();
        let delete_spec = custom.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(delete_spec);
        let from_builder = encode(&delete_custom_theme_action("sunset"));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for delete"
        );

        // Applying is an external store effect; the spec carries the stable
        // theme id to the widget operation handler.
        let builtin = lib
            .children
            .iter()
            .find(|r| r.id.as_str() == "themes.theme:tilo-blue")
            .unwrap();
        let apply_spec = builtin.actions.iter().find(|a| a.id == "apply").unwrap();
        let from_spec = immediate_encode(apply_spec);
        assert_eq!(from_spec.type_id, "themes.apply");
        assert_eq!(
            serde_json::from_slice::<String>(&from_spec.payload).unwrap(),
            "tilo-blue"
        );

        // The rename spec == the rename builder for the same name.
        let sunset = catalog(&doc)
            .into_iter()
            .find(|s| s.id == "sunset")
            .unwrap();
        let edit_spec = custom.actions.iter().find(|a| a.id == "edit").unwrap();
        let from_spec = prompted_encode(edit_spec, "Twilight");
        let from_builder = encode(&set_custom_theme_name_action(&sunset.id, "Twilight"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "rename is one source of truth"
        );
    }

    #[test]
    fn every_theme_control_resource_uses_the_canonical_action_or_broker() {
        let doc = seeded_doc();
        let library = &provide_resources(&doc)[0];
        let sunset = library
            .children
            .iter()
            .find(|theme| theme.id.as_str() == "themes.theme:sunset")
            .unwrap();
        let decode = |id: &str| -> ThemesAction {
            let spec = sunset
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap();
            serde_json::from_slice(&immediate_encode(spec).payload).unwrap()
        };
        assert!(matches!(
            decode("duplicate"),
            ThemesAction::CreateCustomTheme { .. }
        ));
        assert!(matches!(
            decode("reset"),
            ThemesAction::ResetCustomTheme { id } if id == "sunset"
        ));
        assert!(matches!(
            decode("delete"),
            ThemesAction::DeleteCustomTheme { id } if id == "sunset"
        ));
        let accent = sunset
            .actions
            .iter()
            .find(|action| action.id == "set-accent")
            .unwrap();
        let encoded = prompted_encode(accent, "#010203");
        assert!(matches!(
            serde_json::from_slice::<ThemesAction>(&encoded.payload).unwrap(),
            ThemesAction::SetCustomThemeToken { id, token: TokenId::Accent, rgb: [1, 2, 3] } if id == "sunset"
        ));
        assert!(
            library
                .actions
                .iter()
                .any(|action| action.id == "new-theme")
        );
        assert!(
            library
                .actions
                .iter()
                .any(|action| action.id == "apply-selected")
        );
        assert!(
            library
                .actions
                .iter()
                .any(|action| action.id == "export-selected")
        );
    }

    /// Dispatching the "New theme" prompted action creates an editable theme in
    /// the document carrying the committed name.
    #[test]
    fn new_theme_action_creates_a_theme() {
        let mut doc = seeded_doc();
        let resources = provide_resources(&doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "new-theme")
            .unwrap();
        let before = catalog(&doc).len();

        let encoded = prompted_encode(spec, "Aurora");
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let cat = catalog(&doc);
        assert_eq!(cat.len(), before + 1, "the action added a theme");
        let created = cat
            .iter()
            .find(|s| s.name == "Aurora")
            .expect("named theme exists");
        assert!(!created.builtin, "the created theme is editable");
    }
}
