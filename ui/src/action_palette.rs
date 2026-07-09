//! `ActionPalette` — the bridge from the [`qos_actions`] resource model to the
//! [`CommandPalette`](crate::widget::CommandPalette) widget, and back.
//!
//! It does three things:
//!
//! 1. **Scopes Ctrl+K to open resources.** Given the resource forest the focused
//!    app instance publishes (`ui::App` collects it each frame), it flattens
//!    every exported action into a palette row and keeps a parallel `key →
//!    action` index so a chosen row resolves back to a replayable action.
//! 2. **Supports a general scope.** The same palette can list the OS-wide
//!    registry of action *types* (the seam the FS / global-search backend plugs
//!    into) — [`SearchScope::Workspace`] = this instance's open resources,
//!    [`SearchScope::All`] = everything registered.
//! 3. **Executes on select.** Draining the palette outcome, it builds the
//!    action's [`EncodedAction`] (immediate, or from a typed argument), decodes
//!    it through the [`ActionRegistry`] — the *same* path a remote peer uses to
//!    replay — and [`dispatch`](qos_actions::dispatch)es it against the doc +
//!    transport, honoring the action's [`ExecutionLocus`].

use std::collections::HashMap;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId};
use winit::event::{Modifiers, WindowEvent};

use crate::widget::command_palette::{CommandEntry, CommandPalette, SearchScope};
use qos_actions::{
    dispatch, flatten, ActionInput, ActionRegistry, ActionTransport, Dispatched, EncodedAction,
    ExecutionLocus, Resource,
};

/// A palette row's resolved action: either a ready-to-fire encoded action, or a
/// builder that turns the user's typed argument into one.
#[derive(Clone)]
enum Resolved {
    Immediate(EncodedAction),
    Prompted(Arc<dyn Fn(&str) -> EncodedAction + Send + Sync>),
}

/// One entry for the general (OS-wide) scope: an action *type* contributed by
/// some app, not tied to a currently-open resource. This is the seam the global
/// search / FS backend fills; the palette renders and dispatches them exactly
/// like open-resource rows.
#[derive(Clone)]
pub struct GeneralAction {
    /// Stable, unique row key.
    pub key: String,
    pub label: String,
    pub description: Option<String>,
    pub locus: ExecutionLocus,
    /// How the row produces its encoded action.
    pub input: ActionInput,
}

/// The Ctrl+K action palette: a [`CommandPalette`] scoped to the focused app
/// instance's open resources (or the general registry), plus the index needed
/// to resolve a selection back to a replayable action.
pub struct ActionPalette {
    palette: CommandPalette,
    index: HashMap<String, Resolved>,
    scope: SearchScope,
}

impl Default for ActionPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionPalette {
    /// A closed command-launcher palette (Commands chrome, focus-trapping).
    pub fn new() -> Self {
        let mut palette = CommandPalette::commands();
        palette.close();
        Self {
            palette,
            index: HashMap::new(),
            scope: SearchScope::Workspace,
        }
    }

    /// Whether the palette is currently open.
    pub fn is_open(&self) -> bool {
        self.palette.is_open()
    }

    /// Open (focus) the palette. Load entries first.
    pub fn open(&mut self) {
        self.palette.open();
    }

    /// Close the palette.
    pub fn close(&mut self) {
        self.palette.close();
    }

    /// The active scope.
    pub fn scope(&self) -> SearchScope {
        self.scope
    }

    /// Read/paint access to the underlying palette (the host draws it).
    pub fn palette(&self) -> &CommandPalette {
        &self.palette
    }
    pub fn palette_mut(&mut self) -> &mut CommandPalette {
        &mut self.palette
    }

    /// Populate from the focused instance's open-resource forest
    /// ([`SearchScope::Workspace`]). Rebuilds the row list + resolve index.
    pub fn load_open_resources(&mut self, resources: &[Resource]) {
        self.scope = SearchScope::Workspace;
        self.palette.search_filter_mut().set_scope(SearchScope::Workspace);
        let flat = flatten(resources);
        let mut entries = Vec::with_capacity(flat.len());
        let mut index = HashMap::with_capacity(flat.len());
        for fa in &flat {
            entries.push(CommandEntry::from_flat_action(fa));
            let resolved = match &fa.action.input {
                ActionInput::Immediate(enc) => Resolved::Immediate(enc.clone()),
                ActionInput::Prompted { build, .. } => Resolved::Prompted(build.clone()),
            };
            index.insert(fa.key.clone(), resolved);
        }
        self.palette.set_entries(entries);
        self.index = index;
    }

    /// Populate from the general (OS-wide) action-type registry
    /// ([`SearchScope::All`]). This is what a global-search host feeds in.
    pub fn load_general(&mut self, actions: &[GeneralAction]) {
        self.scope = SearchScope::All;
        self.palette.search_filter_mut().set_scope(SearchScope::All);
        let mut entries = Vec::with_capacity(actions.len());
        let mut index = HashMap::with_capacity(actions.len());
        for ga in actions {
            let mut entry = CommandEntry::new(ga.key.clone(), ga.label.clone());
            if let Some(d) = &ga.description {
                entry = entry.description(d.clone());
            }
            let resolved = match &ga.input {
                ActionInput::Immediate(enc) => Resolved::Immediate(enc.clone()),
                ActionInput::Prompted {
                    prompt,
                    placeholder,
                    build,
                } => {
                    entry = entry.with_argument(prompt.clone(), placeholder.clone());
                    Resolved::Prompted(build.clone())
                }
            };
            entries.push(entry);
            index.insert(ga.key.clone(), resolved);
        }
        self.palette.set_entries(entries);
        self.index = index;
    }

    /// Feed a keyboard event to the palette while it is open. Returns whether it
    /// was consumed (the host should then stop propagating it).
    pub fn handle_key(
        &mut self,
        event: &WindowEvent,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        if !self.palette.is_open() {
            return false;
        }
        self.palette.handle_overlay_key(event, modifiers, doc, path)
    }

    /// Drain a pending selection (if the user pressed Enter) and dispatch its
    /// action against `doc` + `transport`, honoring its
    /// [`ExecutionLocus`](qos_actions::ExecutionLocus). Returns the
    /// [`Dispatched`] outcome, or `None` if nothing was selected.
    ///
    /// Executing goes through [`ActionRegistry::decode`] — the identical path a
    /// remote peer uses to [`replay`](qos_actions::replay) — so "play locally"
    /// and "replay anywhere" share one code path.
    pub fn take_dispatch(
        &mut self,
        doc: &mut AutoCommit,
        registry: &ActionRegistry,
        transport: &mut dyn ActionTransport,
    ) -> Option<Result<Dispatched, qos_actions::ActionError>> {
        let outcome = self.palette.take_outcome()?;
        let resolved = self.index.get(&outcome.id)?.clone();
        let encoded = match resolved {
            Resolved::Immediate(enc) => enc,
            Resolved::Prompted(build) => build(outcome.argument.as_deref().unwrap_or("")),
        };
        Some(self.dispatch_encoded(doc, &encoded, registry, transport))
    }

    fn dispatch_encoded(
        &self,
        doc: &mut AutoCommit,
        encoded: &EncodedAction,
        registry: &ActionRegistry,
        transport: &mut dyn ActionTransport,
    ) -> Result<Dispatched, qos_actions::ActionError> {
        let action = registry.decode(encoded)?;
        dispatch(doc, action.as_ref(), encoded.clone(), transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ReadDoc, ScalarValue, ROOT};
    use qos_actions::{Action, ActionError, ActionRegistry, Doc, LocalTransport};
    use serde::{Deserialize, Serialize};

    /// A minimal replayable action: set a boolean flag under a root key.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Mark {
        key: String,
    }
    const MARK_TYPE: &str = "test.mark";
    impl Action for Mark {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            doc.put(&ROOT, self.key.as_str(), ScalarValue::Boolean(true))?;
            Ok(())
        }
    }

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register::<Mark>(MARK_TYPE);
        r
    }

    /// The OS-wide (general) scope: an action *type* not tied to any open
    /// resource — the seam the FS / global-search backend fills — still lists,
    /// selects, and dispatches through the exact same registry path as a
    /// resource-scoped action. Proves the seam is real, not merely declared.
    #[test]
    fn general_scope_lists_and_dispatches_through_the_real_path() {
        let immediate = GeneralAction {
            key: "sys.mark".into(),
            label: "Mark it".into(),
            description: Some("set a flag".into()),
            locus: ExecutionLocus::ClientLocal,
            input: ActionInput::Immediate(
                EncodedAction::encode(
                    MARK_TYPE,
                    &Mark {
                        key: "marked".into(),
                    },
                )
                .unwrap(),
            ),
        };
        let prompted = GeneralAction {
            key: "sys.note".into(),
            label: "Note".into(),
            description: None,
            locus: ExecutionLocus::ClientLocal,
            input: ActionInput::Prompted {
                prompt: "Text".into(),
                placeholder: "…".into(),
                build: Arc::new(|s: &str| {
                    EncodedAction::encode(MARK_TYPE, &Mark { key: s.to_string() }).unwrap()
                }),
            },
        };

        let mut palette = ActionPalette::new();
        palette.load_general(&[immediate, prompted]);
        palette.open();
        assert_eq!(palette.scope(), SearchScope::All, "general scope is OS-wide");

        // Both the immediate and the (argument-collecting) prompted type list.
        let labels: Vec<String> = palette
            .palette()
            .visible_entries()
            .iter()
            .map(|e| e.label.clone())
            .collect();
        assert!(labels.iter().any(|l| l == "Mark it"), "{labels:?}");
        assert!(labels.iter().any(|l| l == "Note"), "{labels:?}");

        // Select + activate the immediate general action, then dispatch it.
        let mut doc = AutoCommit::new();
        assert!(doc.get(&ROOT, "marked").unwrap().is_none());
        let idx = palette
            .palette()
            .visible_entries()
            .iter()
            .position(|e| e.label == "Mark it")
            .expect("mark row present");
        palette.palette_mut().set_cursor(idx);
        let root = ROOT;
        palette.palette_mut().activate_selected(&mut doc, &root);
        let mut tx = LocalTransport::new();
        let d = palette
            .take_dispatch(&mut doc, &registry(), &mut tx)
            .expect("a selection was pending")
            .expect("dispatch succeeded");
        assert!(matches!(d, Dispatched::AppliedLocal(_)));
        assert_eq!(
            doc.get(&ROOT, "marked").unwrap().and_then(|(v, _)| v.to_bool()),
            Some(true),
            "the general-scope action mutated the CRDT"
        );
        assert_eq!(tx.replicated.len(), 1, "replicated once for peers to replay");
    }
}
