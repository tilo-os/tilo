//! The resource + action model.
//!
//! Every user interaction in a qOS app is an **action** — a verb invoked on a
//! **resource**. When a widget tree dispatches its interactions as actions
//! (instead of ad-hoc handlers mutating state in place) three things fall out
//! of one refactor:
//!
//! - **Accessibility** — the resource tree is enumerable and each action is
//!   invokable, so an assistive technology (and the built-in reader) can list
//!   and trigger them. This is the same tree the a11y layer walks; a
//!   [`ResourceNode`] shares the [`Role`](crate::a11y::Role) vocabulary and an
//!   [`ActionSpec`] is the action-model twin of
//!   [`a11y::Action`](crate::a11y::Action).
//! - **Undo & collaboration** — every action routes through one executor
//!   ([`Widget::dispatch_action`](crate::widget::Widget::dispatch_action))
//!   that mutates the shared CRDT and raises
//!   [`needs_commit`](crate::widget::Widget::needs_commit), so the host records
//!   an undo boundary ([`State::commit_action_authored`](crate::state::State::commit_action_authored))
//!   and attributes the change to the local author. Ctrl+Z and multi-peer
//!   convergence then work uniformly, for free.
//! - **Command surfaces** — Ctrl+K / the command palette enumerate the same
//!   [`ActionSpec`]s the UI dispatches, so there is a single source of truth
//!   for "what can I do here".
//!
//! An app publishes its tree from
//! [`Widget::resources`](crate::widget::Widget::resources) and executes an
//! action by id from
//! [`Widget::dispatch_action`](crate::widget::Widget::dispatch_action); both
//! default to "nothing", so a widget opts in exactly like it opts into
//! `accessibility()`.

use crate::a11y::Role;

/// One invokable action a widget or app publishes: a stable `id` (the address
/// a command palette / AT / test uses to invoke it), a human `label`, whether
/// it is currently `enabled`, and whether dispatching it records an **undo
/// boundary** (a state change) as opposed to a pure view change (switching a
/// tab) that should not be undoable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionSpec {
    /// Stable, dispatchable id — e.g. `"request.send"`, `"collection.toggle.0"`.
    pub id: String,
    /// Human-facing label a palette / screen reader announces.
    pub label: String,
    /// Whether the action is available right now.
    pub enabled: bool,
    /// Whether invoking it mutates persisted state and should land on the undo
    /// stack. `false` for pure view changes (tab / response-view switches).
    pub undoable: bool,
}

impl ActionSpec {
    /// A state-changing (undoable) action.
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), enabled: true, undoable: true }
    }

    /// A pure view-change action (not undoable) — e.g. switching a tab.
    pub fn view(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), enabled: true, undoable: false }
    }

    /// Set the enabled state.
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
}

/// A node in an app's resource tree: a resource (noun) with the actions
/// (verbs) available on it and its child resources. The whole tree is what an
/// app returns from [`Widget::resources`](crate::widget::Widget::resources).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceNode {
    /// Stable resource id — e.g. `"request"`, `"collection.0"`.
    pub id: String,
    /// Human-facing label.
    pub label: String,
    /// Semantic role, shared with the accessibility model.
    pub role: Role,
    /// Actions available on this resource.
    pub actions: Vec<ActionSpec>,
    /// Child resources.
    pub children: Vec<ResourceNode>,
}

impl ResourceNode {
    pub fn new(id: impl Into<String>, label: impl Into<String>, role: Role) -> Self {
        Self { id: id.into(), label: label.into(), role, actions: Vec::new(), children: Vec::new() }
    }

    /// Attach an action (builder form).
    pub fn with_action(mut self, action: ActionSpec) -> Self {
        self.actions.push(action);
        self
    }

    /// Attach a child resource (builder form).
    pub fn with_child(mut self, child: ResourceNode) -> Self {
        self.children.push(child);
        self
    }

    /// Attach several actions.
    pub fn with_actions(mut self, actions: impl IntoIterator<Item = ActionSpec>) -> Self {
        self.actions.extend(actions);
        self
    }

    /// Every action in this subtree, depth-first — the flat command list a
    /// palette or an AT enumerates. Parents before children; a node's own
    /// actions before its children's.
    pub fn actions_deep(&self) -> Vec<&ActionSpec> {
        let mut out: Vec<&ActionSpec> = self.actions.iter().collect();
        for c in &self.children {
            out.extend(c.actions_deep());
        }
        out
    }

    /// Look up an action anywhere in the subtree by its id.
    pub fn find_action(&self, id: &str) -> Option<&ActionSpec> {
        self.actions_deep().into_iter().find(|a| a.id == id)
    }

    /// Total number of resource nodes in the subtree (self + descendants).
    pub fn node_count(&self) -> usize {
        1 + self.children.iter().map(|c| c.node_count()).sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_spec_undoable_vs_view() {
        assert!(ActionSpec::new("a", "A").undoable);
        assert!(!ActionSpec::view("v", "V").undoable);
        assert!(ActionSpec::new("a", "A").enabled(false).enabled == false);
    }

    #[test]
    fn resource_tree_flattens_and_finds_actions() {
        let tree = ResourceNode::new("root", "Root", Role::Document)
            .with_action(ActionSpec::new("root.a", "Root A"))
            .with_child(
                ResourceNode::new("child", "Child", Role::Group)
                    .with_action(ActionSpec::new("child.b", "Child B"))
                    .with_action(ActionSpec::view("child.c", "Child C")),
            );
        let flat = tree.actions_deep();
        // Parent action first, then the child's, in order.
        assert_eq!(
            flat.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec!["root.a", "child.b", "child.c"]
        );
        assert_eq!(tree.find_action("child.b").map(|a| a.label.as_str()), Some("Child B"));
        assert!(tree.find_action("missing").is_none());
        assert_eq!(tree.node_count(), 2);
    }
}
