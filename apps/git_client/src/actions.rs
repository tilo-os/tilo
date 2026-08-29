//! The git client's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! ## The split — almost everything here is external
//!
//! The qOS action-model recipe (`docs/action-model-recipe.md`) sorts every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an [`Action`] (a serializable value that deterministically changes
//! the shared Automerge document, so undo and multi-peer replay come for free),
//! while presentation / view state stays plain app code. The git client is a
//! near-textbook case of a mostly-**external** app:
//!
//! - **External — never actions.** Every git operation — stage / unstage,
//!   commit, branch create / switch / delete, checkout, fetch / pull / push,
//!   stash, discard, per-hunk staging — mutates the *on-disk* repository through
//!   libgit2 (see [`crate::repo`]). None of it touches the shared CRDT, so none
//!   of it is an action. The live repository is *projected* into the in-memory
//!   [`RepoModel`](crate::model::RepoModel) and re-read after each op; that model
//!   is not persisted to the document either.
//!
//! - **Presentation — never actions.** Which screen is showing, the selected
//!   file or commit, scroll offsets, the sidebar flap, the armed-discard
//!   confirmation, keyboard focus — all local, per-instance view state.
//!
//! ## What genuinely lives in the shared document
//!
//! That leaves exactly one thing in the CRDT: the app's **durable view
//! preferences**, stored as a small map under `root["settings"]` (see
//! [`crate::settings`]) — the diff layout (unified / side-by-side), and whether
//! the diff shows whitespace, syntax-highlights, and soft-wraps long lines.
//! These are user-authored, they survive restarts, and they replicate to
//! collaborators, so toggling one is a document-mutating operation: an
//! [`Action`]. Each toggle carries the **absolute** target value in its payload,
//! so [`Action::apply`] is a pure function of `(payload, document)` — no reading
//! of the prior value, no wall-clock, no RNG — and replays byte-identically on
//! every replica. A malformed diff-layout string normalises to the canonical
//! form rather than erroring, so a bad replay can't wedge a peer.
//!
//! The resource tree mirrors that: the open repository is the presented
//! resource, and the four preference toggles are the [`ActionSpec`]s Ctrl+K
//! offers for it. When no repository is open the app publishes nothing.

use automerge::AutoCommit;
use automerge::transaction::Transactable;
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};

use crate::settings::{self, DiffLayout, Settings};

/// Registry `type_id` for every git-client action.
pub const GIT_CLIENT_ACTION_TYPE: &str = "git_client.action";

/// Every collaborative operation the git client exports on the shared document.
///
/// All four are absolute *sets* of a durable view preference in
/// `root["settings"]` — never toggles — so the payload fully determines the
/// resulting state and replay is deterministic. The prior value is read (once,
/// presentation-side) at *authoring* time to pick the opposite value; the
/// action itself carries only the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitClientAction {
    /// Set the diff pane layout. `layout` is the document's string form
    /// (`"unified"` | `"split"`); any other value normalises to `"unified"`.
    SetDiffLayout { layout: String },
    /// Set whether the diff renders whitespace.
    SetShowWhitespace { on: bool },
    /// Set whether the diff is syntax-highlighted.
    SetSyntaxHighlight { on: bool },
    /// Set whether the diff soft-wraps long lines.
    SetWrapLines { on: bool },
}

impl Action for GitClientAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let obj = settings::settings_obj(doc);
        match self {
            GitClientAction::SetDiffLayout { layout } => {
                // Normalise through the enum so an unknown payload lands on a
                // valid setting instead of wedging.
                let norm = DiffLayout::from_str(layout).as_str();
                doc.put(&obj, settings::keys::DIFF_LAYOUT, norm)?;
            }
            GitClientAction::SetShowWhitespace { on } => {
                doc.put(&obj, settings::keys::SHOW_WHITESPACE, *on)?;
            }
            GitClientAction::SetSyntaxHighlight { on } => {
                doc.put(&obj, settings::keys::SYNTAX_HIGHLIGHT, *on)?;
            }
            GitClientAction::SetWrapLines { on } => {
                doc.put(&obj, settings::keys::WRAP_LINES, *on)?;
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // A personal view preference: this client applies + replicates, peers
        // replay.
        ExecutionLocus::ClientLocal
    }
}

/// Register the git client's action decoder for palette execution + remote
/// replay. Called once at app start.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<GitClientAction>(GIT_CLIENT_ACTION_TYPE);
}

/// Encode any [`GitClientAction`] into the wire form the palette and the
/// on-screen affordances both dispatch — the single point that turns a git
/// action into a replayable envelope.
pub fn encode(action: &GitClientAction) -> EncodedAction {
    EncodedAction::encode(GIT_CLIENT_ACTION_TYPE, action)
        .expect("GitClientAction is always serializable")
}

/// Build the "set the diff layout to `layout`" action — the single source of
/// truth shared by the app-bar segmented control (see [`crate::app`]) and the
/// Ctrl+K resource tree below.
pub fn set_diff_layout_action(layout: DiffLayout) -> GitClientAction {
    GitClientAction::SetDiffLayout {
        layout: layout.as_str().to_string(),
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// The four preference toggles, exported as Ctrl+K actions on the open
/// repository resource. Each is an immediate action that sets the preference to
/// the *opposite* of its current document value, so choosing it in the palette
/// flips the setting exactly as the on-screen control does. Level-triggered:
/// recomputed from `doc` each frame, so the labels and target values always
/// track the live state.
pub fn provide_resources(repo_name: &str, doc: &AutoCommit) -> Vec<Resource> {
    let s = Settings::read_only(doc);
    let label = if repo_name.trim().is_empty() {
        "Repository".to_string()
    } else {
        repo_name.to_string()
    };
    let mut repo = Resource::new(
        format!("git_client.repo:{repo_name}"),
        "git_client.repo",
        label,
    );

    // Diff layout — flip to the other layout.
    let next_layout = match s.diff_layout {
        DiffLayout::Unified => DiffLayout::SideBySide,
        DiffLayout::SideBySide => DiffLayout::Unified,
    };
    let layout_label = match next_layout {
        DiffLayout::SideBySide => "Diff layout: side-by-side",
        DiffLayout::Unified => "Diff layout: unified",
    };
    repo = repo.with_action(
        ActionSpec::immediate(
            "diff-layout",
            layout_label,
            GIT_CLIENT_ACTION_TYPE,
            &set_diff_layout_action(next_layout),
        )
        .expect("encode diff-layout action"),
    );

    // Syntax highlighting.
    repo = repo.with_action(
        ActionSpec::immediate(
            "syntax-highlight",
            if s.syntax_highlight {
                "Turn off syntax highlighting"
            } else {
                "Turn on syntax highlighting"
            },
            GIT_CLIENT_ACTION_TYPE,
            &GitClientAction::SetSyntaxHighlight {
                on: !s.syntax_highlight,
            },
        )
        .expect("encode syntax-highlight action"),
    );

    // Whitespace rendering.
    repo = repo.with_action(
        ActionSpec::immediate(
            "show-whitespace",
            if s.show_whitespace {
                "Hide whitespace"
            } else {
                "Show whitespace"
            },
            GIT_CLIENT_ACTION_TYPE,
            &GitClientAction::SetShowWhitespace {
                on: !s.show_whitespace,
            },
        )
        .expect("encode show-whitespace action"),
    );

    // Soft line wrapping.
    repo = repo.with_action(
        ActionSpec::immediate(
            "wrap-lines",
            if s.wrap_lines {
                "Stop wrapping long lines"
            } else {
                "Wrap long lines"
            },
            GIT_CLIENT_ACTION_TYPE,
            &GitClientAction::SetWrapLines { on: !s.wrap_lines },
        )
        .expect("encode wrap-lines action"),
    );

    vec![repo]
}

impl ResourceProvider for crate::app::GitClientApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        match self.resource_repo_label() {
            Some(name) => provide_resources(&name, doc),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_actions::{ActionInput, LocalTransport};

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// Dispatch an action through the executor, exactly as the on-screen
    /// affordance path does (decode from the wire, apply through a local
    /// transport).
    fn dispatch(doc: &mut AutoCommit, action: &GitClientAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("expected an immediate action: {}", spec.id),
        }
    }

    /// A base document with the default settings seeded, as the app's first
    /// `sync` leaves it — so toggling a preference is a genuine change.
    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        Settings::default().write(&mut doc);
        doc.commit();
        doc
    }

    /// Each preference action writes its absolute value into `root["settings"]`.
    #[test]
    fn preference_actions_write_the_settings() {
        let mut doc = seeded();
        assert_eq!(Settings::read_only(&doc).diff_layout, DiffLayout::Unified);
        assert!(Settings::read_only(&doc).syntax_highlight);

        dispatch(&mut doc, &set_diff_layout_action(DiffLayout::SideBySide));
        dispatch(&mut doc, &GitClientAction::SetSyntaxHighlight { on: false });
        dispatch(&mut doc, &GitClientAction::SetShowWhitespace { on: true });
        dispatch(&mut doc, &GitClientAction::SetWrapLines { on: true });

        let s = Settings::read_only(&doc);
        assert_eq!(s.diff_layout, DiffLayout::SideBySide);
        assert!(!s.syntax_highlight);
        assert!(s.show_whitespace);
        assert!(s.wrap_lines);
    }

    /// A malformed diff-layout payload normalises to the canonical form rather
    /// than erroring, so an out-of-range replay lands on a valid setting.
    #[test]
    fn malformed_diff_layout_normalises_instead_of_wedging() {
        let mut doc = seeded();
        dispatch(
            &mut doc,
            &GitClientAction::SetDiffLayout {
                layout: "nonsense".into(),
            },
        );
        assert_eq!(
            Settings::read_only(&doc).diff_layout,
            DiffLayout::Unified,
            "an unknown layout falls back to the canonical default"
        );
    }

    /// Ctrl+K on an open repository publishes the repo resource exporting the
    /// four preference actions, and the one palette lists them as rows. Drives
    /// the real resource-builder and the real [`ui::ActionPalette`].
    #[test]
    fn ctrl_k_publishes_the_repo_and_preference_actions() {
        let doc = seeded();
        let resources = provide_resources("qos", &doc);
        assert_eq!(resources.len(), 1, "one repository root");
        let repo = &resources[0];
        assert_eq!(repo.kind, "git_client.repo");
        assert_eq!(repo.label, "qos");
        assert_eq!(repo.id.as_str(), "git_client.repo:qos");

        let ids: Vec<&str> = repo.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "diff-layout",
                "syntax-highlight",
                "show-whitespace",
                "wrap-lines"
            ],
        );

        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("side-by-side")),
            "the palette offers the diff-layout toggle: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("syntax highlighting")),
            "{labels:?}"
        );
    }

    /// The palette's exported actions track the live document: with a non-default
    /// state seeded, each toggle targets the opposite of the current value.
    #[test]
    fn palette_actions_target_the_opposite_of_current_state() {
        let mut doc = AutoCommit::new();
        let mut s = Settings::default();
        s.diff_layout = DiffLayout::SideBySide;
        s.syntax_highlight = false;
        s.write(&mut doc);
        doc.commit();

        let resources = provide_resources("qos", &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "diff-layout")
            .unwrap();
        // Currently side-by-side, so the exported action flips back to unified.
        assert_eq!(
            immediate_encode(spec),
            encode(&set_diff_layout_action(DiffLayout::Unified)),
        );
        let syn = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "syntax-highlight")
            .unwrap();
        assert_eq!(
            immediate_encode(syn),
            encode(&GitClientAction::SetSyntaxHighlight { on: true }),
        );
    }

    /// The full undo covenant on the real executor path: dispatched through the
    /// executor the action mutates the document and records exactly one undo
    /// boundary, and Ctrl+Z (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut state = ui::State::new();
        // Seed defaults as the app's first sync does, then bracket.
        Settings::default().write(&mut state.doc);
        state.doc.commit();
        assert_eq!(
            Settings::read_only(&state.doc).diff_layout,
            DiffLayout::Unified
        );
        let before_heads = state.doc.get_heads();

        let encoded = encode(&set_diff_layout_action(DiffLayout::SideBySide));
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            Settings::read_only(&state.doc).diff_layout,
            DiffLayout::SideBySide,
            "the action switched the layout"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            Settings::read_only(&state.doc).diff_layout,
            DiffLayout::Unified,
            "undo reverted the layout change and nothing else"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a
    /// second independent replica (from the same base state) lands on the
    /// byte-identical document — same head hashes, not merely the same value.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let mut a = seeded();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = set_diff_layout_action(DiffLayout::SideBySide);
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(Settings::read_only(&b).diff_layout, DiffLayout::SideBySide);
    }

    /// Single source of truth: the palette's "diff-layout" spec encodes
    /// byte-identically to the builder the on-screen segmented control calls, for
    /// the same target layout.
    #[test]
    fn palette_and_builder_encode_identically() {
        let doc = seeded(); // unified → the exported toggle targets side-by-side
        let resources = provide_resources("qos", &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "diff-layout")
            .unwrap();

        let from_spec = immediate_encode(spec);
        let from_builder = encode(&set_diff_layout_action(DiffLayout::SideBySide));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "diff-layout is one source of truth"
        );
    }
}
