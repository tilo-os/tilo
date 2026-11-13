"""Types for Shortcuts (`so.tilo.shortcuts`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workbench` is the app's `shortcuts.workbench` at `shortcuts/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers a `Run`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is.
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict
from tilo import Resource, Run

__all__ = [
    "Draft",
    "Head",
    "StepNode",
    "Workbench",
]

class Draft(TypedDict):
    name: str
    shortcut_id: str
    inputs: list[Any]
    steps: list[StepNode]
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

class Workbench(Resource):
    """Shortcuts — `shortcuts.workbench`."""
    __surface__ = "so.tilo.shortcuts@720723bfb1ff289c"
    drafts: list[Draft]
    ignored: list[str]

    def draft_proposal(self, *, shortcut: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("draft-proposal", shortcut=shortcut)

    def ignore_proposal(self, *, key: str) -> Run | None:
        """Runs the app's action."""
        return self.action("ignore-proposal", key=key)

    def allow_proposal(self, *, key: str) -> Run | None:
        """Runs the app's action."""
        return self.action("allow-proposal", key=key)

    def new_draft(self, *, name: str) -> Run | None:
        """Runs the app's action."""
        return self.action("new-draft", name=name)

    def duplicate_draft(self, *, draft: str) -> Run | None:
        """Runs the app's action."""
        return self.action("duplicate-draft", draft=draft)

    def draft_shortcut(self, *, shortcut: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("draft-shortcut", shortcut=shortcut)

    def open_shortcut(self, *, shortcut: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("open-shortcut", shortcut=shortcut)

    def duplicate_shortcut(self, *, shortcut: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("duplicate-shortcut", shortcut=shortcut)

    def set_schedule(self,
        *,
        draft: str,
        enabled: bool,
        hour: int,
        minute: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-schedule", draft=draft, enabled=enabled, hour=hour, minute=minute)

    def discard(self, *, draft: str) -> Run | None:
        """Runs the app's action."""
        return self.action("discard", draft=draft)

    def add_step(self,
        *,
        draft: str,
        block: str,
        step: Any,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-step", draft=draft, block=block, step=step)

    def replace_step(self,
        *,
        draft: str,
        path: str,
        step: Any,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("replace-step", draft=draft, path=path, step=step)

    def bind_argument(self,
        *,
        draft: str,
        path: str,
        param: str,
        binding: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("bind-argument", draft=draft, path=path, param=param, binding=binding)

    def remove_step(self,
        *,
        draft: str,
        path: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-step", draft=draft, path=path)

    def move_step(self,
        *,
        draft: str,
        path: str,
        earlier: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("move-step", draft=draft, path=path, earlier=earlier)

    def set_step_policy(self,
        *,
        draft: str,
        path: str,
        policy: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-step-policy", draft=draft, path=path, policy=policy)

    def set_nested(self,
        *,
        draft: str,
        path: str,
        block: str,
        index: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-nested", draft=draft, path=path, block=block, index=index)

    def set_output(self,
        *,
        draft: str,
        binding: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-output", draft=draft, binding=binding)

    def add_input(self,
        *,
        draft: str,
        name: str,
        kind: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-input", draft=draft, name=name, kind=kind)

    def remove_input(self,
        *,
        draft: str,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-input", draft=draft, name=name)


