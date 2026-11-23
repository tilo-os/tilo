"""Types for Shortcuts (`so.tilo.shortcuts`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workbench` is the app's `shortcuts.workbench` at `shortcuts/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers an `Invocation`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is. An undoable action that answers nothing also has a
`<action>_many`, which applies it to every argument object of a list in a few
all-or-nothing steps, all of them one invocation group.
"""

from __future__ import annotations

from typing import Any, Literal, NotRequired, TypedDict
from tilo import Entry, Invocation, Resource

__all__ = [
    "Draft",
    "Head",
    "StepNode",
    "Workbench",
    "DraftProposalArgs",
    "IgnoreProposalArgs",
    "AllowProposalArgs",
    "NewDraftArgs",
    "DuplicateDraftArgs",
    "DraftShortcutArgs",
    "OpenShortcutArgs",
    "DuplicateShortcutArgs",
    "AddTriggerArgs",
    "ReplaceTriggerArgs",
    "RemoveTriggerArgs",
    "DiscardArgs",
    "AddStepArgs",
    "ReplaceStepArgs",
    "BindArgumentArgs",
    "RemoveStepArgs",
    "MoveStepArgs",
    "SetStepPolicyArgs",
    "SetNestedArgs",
    "SetOutputArgs",
    "AddInputArgs",
    "RemoveInputArgs",
]

class Draft(TypedDict):
    name: str
    shortcut_id: str
    inputs: list[Entry[Any]]
    steps: list[Entry[StepNode]]
    output: Any | None
    triggers: list[Any]
    step_timeout_secs: int | None
    replaces_saved: bool

Head = dict[str, Any]
"""One of invoke, if, for_each, set_var, ask, ai, task, read: an object whose case the app tags it with."""

class StepNode(TypedDict):
    parent: str
    branch: str
    id: str
    head: Head | None
    args: dict[str, Any]
    on_error: Any

DraftProposalArgs = TypedDict("DraftProposalArgs", {
    "shortcut": "Any",
})
"""The arguments of `draft-proposal`."""

IgnoreProposalArgs = TypedDict("IgnoreProposalArgs", {
    "key": "str",
})
"""The arguments of `ignore-proposal`."""

AllowProposalArgs = TypedDict("AllowProposalArgs", {
    "key": "str",
})
"""The arguments of `allow-proposal`."""

NewDraftArgs = TypedDict("NewDraftArgs", {
    "name": "str",
})
"""The arguments of `new-draft`."""

DuplicateDraftArgs = TypedDict("DuplicateDraftArgs", {
    "draft": "str",
})
"""The arguments of `duplicate-draft`."""

DraftShortcutArgs = TypedDict("DraftShortcutArgs", {
    "shortcut": "Any",
})
"""The arguments of `draft-shortcut`."""

OpenShortcutArgs = TypedDict("OpenShortcutArgs", {
    "shortcut": "Any",
})
"""The arguments of `open-shortcut`."""

DuplicateShortcutArgs = TypedDict("DuplicateShortcutArgs", {
    "shortcut": "Any",
})
"""The arguments of `duplicate-shortcut`."""

AddTriggerArgs = TypedDict("AddTriggerArgs", {
    "draft": "str",
    "trigger": "Any",
})
"""The arguments of `add-trigger`."""

ReplaceTriggerArgs = TypedDict("ReplaceTriggerArgs", {
    "draft": "str",
    "index": "int",
    "trigger": "Any",
})
"""The arguments of `replace-trigger`."""

RemoveTriggerArgs = TypedDict("RemoveTriggerArgs", {
    "draft": "str",
    "index": "int",
})
"""The arguments of `remove-trigger`."""

DiscardArgs = TypedDict("DiscardArgs", {
    "draft": "str",
})
"""The arguments of `discard`."""

AddStepArgs = TypedDict("AddStepArgs", {
    "draft": "str",
    "block": "str",
    "step": "Any",
})
"""The arguments of `add-step`."""

ReplaceStepArgs = TypedDict("ReplaceStepArgs", {
    "draft": "str",
    "path": "str",
    "step": "Any",
})
"""The arguments of `replace-step`."""

BindArgumentArgs = TypedDict("BindArgumentArgs", {
    "draft": "str",
    "path": "str",
    "param": "str",
    "binding": "str",
})
"""The arguments of `bind-argument`."""

RemoveStepArgs = TypedDict("RemoveStepArgs", {
    "draft": "str",
    "path": "str",
})
"""The arguments of `remove-step`."""

MoveStepArgs = TypedDict("MoveStepArgs", {
    "draft": "str",
    "path": "str",
    "earlier": "bool",
})
"""The arguments of `move-step`."""

SetStepPolicyArgs = TypedDict("SetStepPolicyArgs", {
    "draft": "str",
    "path": "str",
    "policy": "str",
})
"""The arguments of `set-step-policy`."""

SetNestedArgs = TypedDict("SetNestedArgs", {
    "draft": "str",
    "path": "str",
    "block": "str",
    "index": "int",
})
"""The arguments of `set-nested`."""

SetOutputArgs = TypedDict("SetOutputArgs", {
    "draft": "str",
    "binding": "str",
})
"""The arguments of `set-output`."""

AddInputArgs = TypedDict("AddInputArgs", {
    "draft": "str",
    "name": "str",
    "kind": "str",
})
"""The arguments of `add-input`."""

RemoveInputArgs = TypedDict("RemoveInputArgs", {
    "draft": "str",
    "name": "str",
})
"""The arguments of `remove-input`."""

class Workbench(Resource):
    """Shortcuts — `shortcuts.workbench`."""
    __surface__ = "so.tilo.shortcuts@e0152581ca03b6ec"
    drafts: list[Entry[Draft]]
    ignored: list[str]

    def draft_proposal(self, *, shortcut: Any) -> None:
        """Runs the app's action."""
        return self.action("draft-proposal", shortcut=shortcut)

    def draft_proposal_many(self, items: list[DraftProposalArgs]) -> list[dict[str, Any]]:
        """Runs `draft-proposal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("draft-proposal", items)

    def ignore_proposal(self, *, key: str) -> None:
        """Runs the app's action."""
        return self.action("ignore-proposal", key=key)

    def ignore_proposal_many(self, items: list[IgnoreProposalArgs]) -> list[dict[str, Any]]:
        """Runs `ignore-proposal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ignore-proposal", items)

    def allow_proposal(self, *, key: str) -> None:
        """Runs the app's action."""
        return self.action("allow-proposal", key=key)

    def allow_proposal_many(self, items: list[AllowProposalArgs]) -> list[dict[str, Any]]:
        """Runs `allow-proposal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("allow-proposal", items)

    def new_draft(self, *, name: str) -> None:
        """Runs the app's action."""
        return self.action("new-draft", name=name)

    def new_draft_many(self, items: list[NewDraftArgs]) -> list[dict[str, Any]]:
        """Runs `new-draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-draft", items)

    def duplicate_draft(self, *, draft: str) -> None:
        """Runs the app's action."""
        return self.action("duplicate-draft", draft=draft)

    def duplicate_draft_many(self, items: list[DuplicateDraftArgs]) -> list[dict[str, Any]]:
        """Runs `duplicate-draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("duplicate-draft", items)

    def draft_shortcut(self, *, shortcut: Any) -> None:
        """Runs the app's action."""
        return self.action("draft-shortcut", shortcut=shortcut)

    def draft_shortcut_many(self, items: list[DraftShortcutArgs]) -> list[dict[str, Any]]:
        """Runs `draft-shortcut` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("draft-shortcut", items)

    def open_shortcut(self, *, shortcut: Any) -> None:
        """Runs the app's action."""
        return self.action("open-shortcut", shortcut=shortcut)

    def open_shortcut_many(self, items: list[OpenShortcutArgs]) -> list[dict[str, Any]]:
        """Runs `open-shortcut` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open-shortcut", items)

    def duplicate_shortcut(self, *, shortcut: Any) -> None:
        """Runs the app's action."""
        return self.action("duplicate-shortcut", shortcut=shortcut)

    def duplicate_shortcut_many(self, items: list[DuplicateShortcutArgs]) -> list[dict[str, Any]]:
        """Runs `duplicate-shortcut` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("duplicate-shortcut", items)

    def add_trigger(self,
        *,
        draft: str,
        trigger: Any,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-trigger", draft=draft, trigger=trigger)

    def add_trigger_many(self, items: list[AddTriggerArgs]) -> list[dict[str, Any]]:
        """Runs `add-trigger` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-trigger", items)

    def replace_trigger(self,
        *,
        draft: str,
        index: int,
        trigger: Any,
    ) -> None:
        """Runs the app's action."""
        return self.action("replace-trigger", draft=draft, index=index, trigger=trigger)

    def replace_trigger_many(self, items: list[ReplaceTriggerArgs]) -> list[dict[str, Any]]:
        """Runs `replace-trigger` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace-trigger", items)

    def remove_trigger(self,
        *,
        draft: str,
        index: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("remove-trigger", draft=draft, index=index)

    def remove_trigger_many(self, items: list[RemoveTriggerArgs]) -> list[dict[str, Any]]:
        """Runs `remove-trigger` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-trigger", items)

    def discard(self, *, draft: str) -> None:
        """Runs the app's action."""
        return self.action("discard", draft=draft)

    def discard_many(self, items: list[DiscardArgs]) -> list[dict[str, Any]]:
        """Runs `discard` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("discard", items)

    def add_step(self,
        *,
        draft: str,
        block: str,
        step: Any,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-step", draft=draft, block=block, step=step)

    def add_step_many(self, items: list[AddStepArgs]) -> list[dict[str, Any]]:
        """Runs `add-step` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-step", items)

    def replace_step(self,
        *,
        draft: str,
        path: str,
        step: Any,
    ) -> None:
        """Runs the app's action."""
        return self.action("replace-step", draft=draft, path=path, step=step)

    def replace_step_many(self, items: list[ReplaceStepArgs]) -> list[dict[str, Any]]:
        """Runs `replace-step` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace-step", items)

    def bind_argument(self,
        *,
        draft: str,
        path: str,
        param: str,
        binding: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("bind-argument", draft=draft, path=path, param=param, binding=binding)

    def bind_argument_many(self, items: list[BindArgumentArgs]) -> list[dict[str, Any]]:
        """Runs `bind-argument` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("bind-argument", items)

    def remove_step(self,
        *,
        draft: str,
        path: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("remove-step", draft=draft, path=path)

    def remove_step_many(self, items: list[RemoveStepArgs]) -> list[dict[str, Any]]:
        """Runs `remove-step` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-step", items)

    def move_step(self,
        *,
        draft: str,
        path: str,
        earlier: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("move-step", draft=draft, path=path, earlier=earlier)

    def move_step_many(self, items: list[MoveStepArgs]) -> list[dict[str, Any]]:
        """Runs `move-step` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move-step", items)

    def set_step_policy(self,
        *,
        draft: str,
        path: str,
        policy: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-step-policy", draft=draft, path=path, policy=policy)

    def set_step_policy_many(self, items: list[SetStepPolicyArgs]) -> list[dict[str, Any]]:
        """Runs `set-step-policy` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-step-policy", items)

    def set_nested(self,
        *,
        draft: str,
        path: str,
        block: str,
        index: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-nested", draft=draft, path=path, block=block, index=index)

    def set_nested_many(self, items: list[SetNestedArgs]) -> list[dict[str, Any]]:
        """Runs `set-nested` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-nested", items)

    def set_output(self,
        *,
        draft: str,
        binding: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-output", draft=draft, binding=binding)

    def set_output_many(self, items: list[SetOutputArgs]) -> list[dict[str, Any]]:
        """Runs `set-output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-output", items)

    def add_input(self,
        *,
        draft: str,
        name: str,
        kind: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-input", draft=draft, name=name, kind=kind)

    def add_input_many(self, items: list[AddInputArgs]) -> list[dict[str, Any]]:
        """Runs `add-input` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-input", items)

    def remove_input(self,
        *,
        draft: str,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("remove-input", draft=draft, name=name)

    def remove_input_many(self, items: list[RemoveInputArgs]) -> list[dict[str, Any]]:
        """Runs `remove-input` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-input", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"draft_proposal", "draft_proposal_many", "ignore_proposal", "ignore_proposal_many", "allow_proposal", "allow_proposal_many", "new_draft", "new_draft_many", "duplicate_draft", "duplicate_draft_many", "draft_shortcut", "draft_shortcut_many", "open_shortcut", "open_shortcut_many", "duplicate_shortcut", "duplicate_shortcut_many", "add_trigger", "add_trigger_many", "replace_trigger", "replace_trigger_many", "remove_trigger", "remove_trigger_many", "discard", "discard_many", "add_step", "add_step_many", "replace_step", "replace_step_many", "bind_argument", "bind_argument_many", "remove_step", "remove_step_many", "move_step", "move_step_many", "set_step_policy", "set_step_policy_many", "set_nested", "set_nested_many", "set_output", "set_output_many", "add_input", "add_input_many", "remove_input", "remove_input_many"})


