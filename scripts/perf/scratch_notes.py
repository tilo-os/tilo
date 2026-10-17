"""A Notes notebook of a measurement's own, never the person's.

Every Notes window in a workspace opens that workspace's one notebook (the
room's record), so a script that types into "the Notes window" types into the
person's notes. A measurement instead creates a workspace of its own, which
mints a new room, opens Notes there through the ordinary application action,
and addresses every Notes action by that room and its record. Any room that
existed before the run belongs to the person and is refused, as is any row
that is not the scratch room's.

The transport stays the caller's: `state()` answers the compositor's state
dump, `invoke(provider, id, args)` runs one action to its end, and `rows()`
lists Notes' action rows.
"""
from __future__ import annotations

from dataclasses import dataclass
import time
from typing import Any, Callable

NOTES_APP = "so.tilo.notes"


class ScratchRoomError(RuntimeError):
    """The run could not prove it addresses a notebook of its own."""


@dataclass(frozen=True)
class ScratchNotes:
    """The workspace this run made, its room, and the notebook record Notes opened there."""

    workspace: Any
    room: str
    record: str
    # Rooms of the workspaces that existed before the run: the person's.
    protected: frozenset[str]

    def owns(self, row: dict[str, Any]) -> bool:
        """Whether an action row is Notes' in this run's room and record."""
        return (row.get("application") == NOTES_APP
                and row.get("room") == self.room
                and row.get("room") not in self.protected
                and row.get("record") == self.record)

    def surfaces(self, state: dict[str, Any]) -> list[Any]:
        """The Notes surfaces in this run's workspace, from a state dump."""
        return [tab["tile"]["Foreign"]["surface_id"]
                for workspace in workspaces(state) if workspace.get("room") == self.room
                for strip in workspace.get("strips", [])
                for group in strip.get("window_groups", []) for tab in group.get("tabs", [])
                if (tab.get("tile") or {}).get("Foreign", {}).get("app_id") == NOTES_APP]


def workspaces(state: dict[str, Any]) -> list[dict[str, Any]]:
    tree = state.get("workspace_tree")
    if not isinstance(tree, dict) or not isinstance(tree.get("workspaces"), list):
        raise ScratchRoomError("The state dump has no workspace tree")
    return tree["workspaces"]


def rooms(state: dict[str, Any]) -> frozenset[str]:
    """Every workspace's room. A workspace without a room cannot be told apart, so it fails."""
    found = [workspace.get("room") for workspace in workspaces(state)]
    if not all(isinstance(room, str) and room for room in found):
        raise ScratchRoomError("A workspace in the state dump has no room")
    return frozenset(found)


def open_scratch_notes(state: Callable[[], dict[str, Any]],
                       invoke: Callable[[str, str, dict[str, Any]], Any],
                       rows: Callable[[], list[dict[str, Any]]],
                       name: str, timeout: float = 30.0) -> ScratchNotes:
    """Create a workspace named `name`, open Notes in it, and return its notebook.

    Fails, after at most the workspace and the launch, unless exactly one new
    room appeared, it is the active workspace, and Notes there publishes
    actions for exactly one record."""
    protected = rooms(state())
    invoke("actions", "workspace.create", {"name": name})
    deadline = time.monotonic() + timeout
    while True:
        current = state()
        fresh = [workspace for workspace in workspaces(current)
                 if workspace.get("room") not in protected]
        if len(fresh) > 1:
            raise ScratchRoomError("More than one new workspace appeared; none is provably ours")
        if (len(fresh) == 1 and fresh[0].get("name") == name
                and current["workspace_tree"].get("active") == fresh[0].get("id")):
            break
        if time.monotonic() >= deadline:
            raise ScratchRoomError("The new scratch workspace never became the active one")
        time.sleep(.1)
    workspace, room = fresh[0].get("id"), fresh[0]["room"]
    if not isinstance(room, str) or not room or room in protected:
        raise ScratchRoomError("The scratch workspace has no room of its own")
    invoke("applications", NOTES_APP, {})
    while True:
        records = {row.get("record") for row in rows()
                   if row.get("application") == NOTES_APP and row.get("room") == room}
        if len(records) > 1:
            raise ScratchRoomError("Notes in the scratch room publishes more than one record")
        if len(records) == 1 and isinstance(next(iter(records)), str) and next(iter(records)):
            break
        if time.monotonic() >= deadline:
            raise ScratchRoomError("Notes never opened in the scratch room")
        time.sleep(.1)
    return ScratchNotes(workspace, room, records.pop(), protected)
