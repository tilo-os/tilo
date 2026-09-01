//! Themes' exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every document-mutating operation Themes performs on its shared catalog is a
//! [`ThemesAction`]: a small serializable value that deterministically mutates
//! the CRDT (see [`crate::doc`], where the catalog lives as a JSON `Text` blob
//! and the active theme id as a scalar). Because that state already lives in the
//! shared document, an action replicates and replays for free. The exact same
//! [`EncodedAction`] is what the on-screen affordances (the top-bar Apply / New,
//! the customizer Duplicate / Reset / Delete, the sidebar context menu) dispatch
//! and what the Ctrl+K palette dispatches — one source of truth, one replay path.
//!
//! The resource tree mirrors the data model: the library owns its themes, and
//! each theme exports the [`ActionSpec`]s Ctrl+K offers for it (apply it as the
//! OS theme, rename it, recolour it, duplicate it, delete it).
//!
//! Presentation — which theme is selected, the colour picker's live drag until
//! commit, the open editor tab, scroll position — stays out of the document as
//! shared *intent* and never becomes an action. (Selection does live in the
//! document as scratch view-state, but it is never dispatched as an action: it
//! is not a collaborative mutation and never an undo boundary.)
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

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
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
    /// look). Persisting the full spec to `~/.config/qos/settings.json` is an
    /// app-level side effect, not part of this replayable action. A malformed id
    /// (naming no theme in the catalog) is a no-op.
    SetActiveTheme { id: String },
    /// Create or replace a **custom** theme in the catalog. One variant covers
    /// New, Duplicate, Reset, a rename, and every committed colour / shape /
    /// typography edit — it carries the whole [`ThemeSpec`]. Built-in presets are
    /// read-only: an upsert whose `spec` is marked built-in, or whose id already
    /// names a built-in preset, is a no-op.
    UpsertCustomTheme { spec: ThemeSpec },
    /// Delete the custom theme `id`. Built-in presets are protected (a no-op) and
    /// the catalog is never emptied (deleting the last surviving theme is a
    /// no-op), mirroring the on-screen Delete guard.
    DeleteCustomTheme { id: String },
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
            ThemesAction::UpsertCustomTheme { spec } => {
                // Never create or overwrite a read-only preset via an upsert.
                if spec.builtin {
                    return Ok(());
                }
                let mut catalog = doc::load_catalog(doc, &ROOT).unwrap_or_default();
                // Guard: an id that already names a built-in preset is protected
                // even if the incoming spec claims to be custom.
                if catalog.iter().any(|s| s.id == spec.id && s.builtin) {
                    return Ok(());
                }
                if let Some(slot) = catalog.iter_mut().find(|s| s.id == spec.id) {
                    *slot = spec.clone();
                } else {
                    catalog.push(spec.clone());
                }
                doc::save_catalog(doc, &ROOT, &catalog);
            }
            ThemesAction::DeleteCustomTheme { id } => {
                let mut catalog = doc::load_catalog(doc, &ROOT).unwrap_or_default();
                if let Some(i) = catalog.iter().position(|s| &s.id == id) {
                    // Built-in presets are protected; never empty the catalog.
                    let builtin = catalog.get(i).is_some_and(|s| s.builtin);
                    if !builtin && catalog.len() > 1 {
                        catalog.remove(i);
                        doc::save_catalog(doc, &ROOT, &catalog);
                    }
                }
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
/// the `build` closures [`ActionSpec::prompted`](qos_actions::ActionSpec::prompted)
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

/// Create or replace a custom theme (New, Duplicate, Reset, rename, recolour).
pub fn upsert_custom_theme_action(spec: &ThemeSpec) -> ThemesAction {
    ThemesAction::UpsertCustomTheme { spec: spec.clone() }
}

/// Delete a custom theme.
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
/// returns a plain `Vec<Resource>` (the [`qos_actions::ResourceProvider`]
/// trait shape), so there is no `Result` to propagate an encode failure into.
#[allow(
    clippy::expect_used,
    reason = "ThemesAction's fields are always JSON-serializable; see encode's doc comment"
)]
fn immediate_spec(id: &str, label: &str, action: &ThemesAction) -> ActionSpec {
    ActionSpec::immediate(id, label, THEMES_ACTION_TYPE, action).expect("encode themes action")
}

/// Build the resource tree Themes currently has open: the theme library (with a
/// "New theme" action) and each theme in it. Every theme can be applied as the
/// OS theme or duplicated; custom themes additionally export rename, recolour,
/// and delete. Every document-mutating operation is exported as the same action
/// the on-screen affordance dispatches.
pub fn provide_resources(doc: &AutoCommit) -> Vec<Resource> {
    let catalog = doc::load_catalog(doc, &ROOT).unwrap_or_default();

    let mut root = Resource::new("themes.library", "themes.library", "Themes");

    // Library-level: create a new theme from the base preset. The uuid is minted
    // once when the name argument is committed, then baked into the envelope, so
    // replay stays deterministic.
    let next_order = max_order(&catalog) + 1;
    root = root.with_action(ActionSpec::prompted(
        "new-theme",
        "New theme",
        "Name",
        "theme name…",
        ExecutionLocus::ClientLocal,
        move |name: &str| {
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
    ));

    for spec in &catalog {
        let mut child = Resource::new(
            format!("themes.theme:{}", spec.id),
            "themes.theme",
            spec.name.clone(),
        );

        // Every theme can become the active OS theme.
        child = child.with_action(immediate_spec(
            "apply",
            "Apply as system theme",
            &set_active_theme_action(spec.id.clone()),
        ));

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
            // Rename — a prompted action carrying the rest of the theme unchanged.
            let base = spec.clone();
            child = child.with_action(ActionSpec::prompted(
                "edit",
                "Rename theme",
                "Name",
                "theme name…",
                ExecutionLocus::ClientLocal,
                move |name: &str| encode(&upsert_custom_theme_action(&rename_spec(&base, name))),
            ));
            // Set the accent colour — a prompted colour edit (the committed form
            // of a picker drag), taking a hex string.
            let base = spec.clone();
            child = child.with_action(ActionSpec::prompted(
                "set-accent",
                "Set accent colour",
                "Colour",
                "#RRGGBB",
                ExecutionLocus::ClientLocal,
                move |hex: &str| {
                    let rgb = crate::model::parse_hex(hex).unwrap_or(base.accent);
                    encode(&upsert_custom_theme_action(&recolor_spec(
                        &base,
                        TokenId::Accent,
                        rgb,
                    )))
                },
            ));
            // Delete the custom theme.
            child = child.with_action(immediate_spec(
                "delete",
                "Delete theme",
                &delete_custom_theme_action(spec.id.clone()),
            ));
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
    use qos_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
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
            .find(|r| r.id.as_str() == "themes.theme:qos-blue")
            .expect("the qos-blue preset resource");
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
        assert!(c_actions.contains(&"delete"), "{c_actions:?}");

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
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
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

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

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
        let action = upsert_custom_theme_action(&recolor_spec(&sunset, TokenId::Accent, [1, 2, 3]));
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

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

        let action = delete_custom_theme_action("qos-blue");
        let encoded = encode(&action);
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, &action, encoded, &mut tx).unwrap();

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
        // Try to overwrite the qos-blue preset (claiming custom).
        let mut sneaky = presets()[0].clone();
        sneaky.builtin = false;
        sneaky.name = "Hijacked".into();
        let action = upsert_custom_theme_action(&sneaky);
        let encoded = encode(&action);
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, &action, encoded, &mut tx).unwrap();
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
        qos_actions::dispatch(&mut doc, &action, encoded, &mut tx).unwrap();
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

        // A preset's apply spec == the apply builder.
        let builtin = lib
            .children
            .iter()
            .find(|r| r.id.as_str() == "themes.theme:qos-blue")
            .unwrap();
        let apply_spec = builtin.actions.iter().find(|a| a.id == "apply").unwrap();
        let from_spec = immediate_encode(apply_spec);
        let from_builder = encode(&set_active_theme_action("qos-blue"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "apply is one source of truth"
        );

        // The rename spec == the rename builder for the same name.
        let sunset = catalog(&doc)
            .into_iter()
            .find(|s| s.id == "sunset")
            .unwrap();
        let edit_spec = custom.actions.iter().find(|a| a.id == "edit").unwrap();
        let from_spec = prompted_encode(edit_spec, "Twilight");
        let from_builder = encode(&upsert_custom_theme_action(&rename_spec(
            &sunset, "Twilight",
        )));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "rename is one source of truth"
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
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let cat = catalog(&doc);
        assert_eq!(cat.len(), before + 1, "the action added a theme");
        let created = cat
            .iter()
            .find(|s| s.name == "Aurora")
            .expect("named theme exists");
        assert!(!created.builtin, "the created theme is editable");
    }
}
