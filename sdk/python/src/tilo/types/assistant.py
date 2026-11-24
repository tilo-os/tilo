"""Types for Assistant (`so.tilo.assistant`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Window` is the app's `assistant.window` at `assistant/<resource>`, read with
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
    "Agent",
    "AgentState",
    "Coding",
    "CodingHarness",
    "CodingProcess",
    "NewCoding",
    "NewSubAgent",
    "Note",
    "Produced",
    "ProducedAt",
    "RunsOn",
    "Spent",
    "Stands",
    "Sub",
    "Window",
    "WorkspaceAgent",
    "CreateAgentArgs",
    "WaitToRunAgentArgs",
    "RunAgentOnArgs",
    "SetAgentSpendLimitArgs",
    "RecordAgentSpendArgs",
    "RenameAgentArgs",
    "SetAgentAvatarArgs",
    "SetAgentModelArgs",
    "SetAgentInstructionArgs",
    "PauseAgentArgs",
    "ResumeAgentArgs",
    "RetireAgentArgs",
    "StartSubAgentArgs",
    "NoteSubAgentArgs",
    "StopSubAgentArgs",
    "SubAgentStandsArgs",
    "SubAgentProducedArgs",
    "SubAgentCodingArgs",
    "SetSubAgentLimitsArgs",
]

class Agent(TypedDict):
    id: str
    name: str
    avatar: str | None
    acts_for: str
    model: str | None
    mcp_client: str | None
    spend_limit_micros: int | None
    spent: Spent | None
    state: AgentState
    created_at: int
    instruction: str | None
    sub: Sub | None
    sub_depth: int | None
    sub_at_once: int | None
    runs_on: list[RunsOn]

AgentState = Literal["active", "retired", "paused"]

class Coding(TypedDict):
    harness: str
    harness_name: str
    repository: str
    directory: str
    branch: str
    base: str
    session: str
    process: CodingProcess | None
    spent_micros: int

class CodingHarness(TypedDict):
    id: str
    name: str

class CodingProcess(TypedDict):
    started_ms: int
    ended_ms: int | None
    ended: str
    exit: int | None
    words: str
    stopped_by: str

class NewCoding(TypedDict):
    harness: str | None
    repository: str
    branch: str | None
    base: str | None

class NewSubAgent(TypedDict):
    name: str
    job: str
    role: str | None
    instruction: str | None
    kind: str | None
    model: str | None
    task: str | None
    id: str | None
    coding: NewCoding | None

class Note(TypedDict):
    id: str
    by: str
    text: str
    at: int

class Produced(TypedDict):
    id: str
    kind: str
    label: str
    url: str | None
    at: ProducedAt | None
    at_ms: int

class ProducedAt(TypedDict):
    app: str
    document: str
    resource: str

class RunsOn(TypedDict):
    device: str
    ticket: str | None
    certificate: str | None

class Spent(TypedDict):
    day: str
    cost_micros: int

Stands = Literal["working", "needs_person", "done", "failed", "stopped"]

class Sub(TypedDict):
    started_by: str
    job: str
    role: str
    kind: str
    depth: int
    task: str | None
    stands: Stands
    since: int
    line: str
    notes: list[Note]
    produced: list[Produced]
    coding: Coding | None

CreateAgentArgs = TypedDict("CreateAgentArgs", {
    "name": "str",
    "avatar": NotRequired["str | None"],
    "id": NotRequired["str | None"],
    "model": NotRequired["str | None"],
    "mcp_client": NotRequired["str | None"],
})
"""The arguments of `create-agent`."""

WaitToRunAgentArgs = TypedDict("WaitToRunAgentArgs", {
    "agent": "str",
    "device": "str",
    "ticket": "str",
})
"""The arguments of `wait-to-run-agent`."""

RunAgentOnArgs = TypedDict("RunAgentOnArgs", {
    "agent": "str",
    "device": "str",
    "certificate": "str",
})
"""The arguments of `run-agent-on`."""

SetAgentSpendLimitArgs = TypedDict("SetAgentSpendLimitArgs", {
    "agent": "str",
    "limit_micros": NotRequired["int | None"],
})
"""The arguments of `set-agent-spend-limit`."""

RecordAgentSpendArgs = TypedDict("RecordAgentSpendArgs", {
    "agent": "str",
    "day": "str",
    "cost_micros": "int",
})
"""The arguments of `record-agent-spend`."""

RenameAgentArgs = TypedDict("RenameAgentArgs", {
    "agent": "str",
    "name": "str",
})
"""The arguments of `rename-agent`."""

SetAgentAvatarArgs = TypedDict("SetAgentAvatarArgs", {
    "agent": "str",
    "avatar": NotRequired["str | None"],
})
"""The arguments of `set-agent-avatar`."""

SetAgentModelArgs = TypedDict("SetAgentModelArgs", {
    "agent": "str",
    "model": NotRequired["str | None"],
})
"""The arguments of `set-agent-model`."""

SetAgentInstructionArgs = TypedDict("SetAgentInstructionArgs", {
    "agent": "str",
    "instruction": NotRequired["str | None"],
})
"""The arguments of `set-agent-instruction`."""

PauseAgentArgs = TypedDict("PauseAgentArgs", {
    "agent": "str",
})
"""The arguments of `pause-agent`."""

ResumeAgentArgs = TypedDict("ResumeAgentArgs", {
    "agent": "str",
})
"""The arguments of `resume-agent`."""

RetireAgentArgs = TypedDict("RetireAgentArgs", {
    "agent": "str",
})
"""The arguments of `retire-agent`."""

StartSubAgentArgs = TypedDict("StartSubAgentArgs", {
    "name": "str",
    "job": "str",
    "role": NotRequired["str | None"],
    "instruction": NotRequired["str | None"],
    "kind": NotRequired["str | None"],
    "model": NotRequired["str | None"],
    "task": NotRequired["str | None"],
    "id": NotRequired["str | None"],
    "coding": NotRequired["NewCoding | None"],
})
"""The arguments of `start-sub-agent`."""

NoteSubAgentArgs = TypedDict("NoteSubAgentArgs", {
    "agent": "str",
    "text": "str",
})
"""The arguments of `note-sub-agent`."""

StopSubAgentArgs = TypedDict("StopSubAgentArgs", {
    "agent": "str",
})
"""The arguments of `stop-sub-agent`."""

SubAgentStandsArgs = TypedDict("SubAgentStandsArgs", {
    "agent": "str",
    "stands": "Stands",
    "line": NotRequired["str | None"],
})
"""The arguments of `sub-agent-stands`."""

SubAgentProducedArgs = TypedDict("SubAgentProducedArgs", {
    "agent": "str",
    "kind": "str",
    "label": "str",
    "url": NotRequired["str | None"],
    "at": NotRequired["ProducedAt | None"],
})
"""The arguments of `sub-agent-produced`."""

SubAgentCodingArgs = TypedDict("SubAgentCodingArgs", {
    "agent": "str",
    "process": NotRequired["CodingProcess | None"],
    "session": NotRequired["str | None"],
    "spent_micros": NotRequired["int | None"],
    "harness": NotRequired["CodingHarness | None"],
})
"""The arguments of `sub-agent-coding`."""

SetSubAgentLimitsArgs = TypedDict("SetSubAgentLimitsArgs", {
    "agent": "str",
    "depth": NotRequired["int | None"],
    "at_once": NotRequired["int | None"],
})
"""The arguments of `set-sub-agent-limits`."""

class Window(Resource):
    """Assistant window — `assistant.window`."""
    __surface__ = "so.tilo.assistant@2cd0d3ff8fc75ce5"

class WorkspaceAgent(Resource):
    """Agents — `tilo.workspace.agent`."""
    __surface__ = "so.tilo.assistant@2cd0d3ff8fc75ce5"
    agents: list[Entry[Agent]]

    def create_agent(self,
        *,
        name: str,
        avatar: str | None = None,
        id: str | None = None,
        model: str | None = None,
        mcp_client: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        if avatar is not None:
            args["avatar"] = avatar
        if id is not None:
            args["id"] = id
        if model is not None:
            args["model"] = model
        if mcp_client is not None:
            args["mcp_client"] = mcp_client
        return self.action("create-agent", **args)

    def create_agent_many(self, items: list[CreateAgentArgs]) -> list[dict[str, Any]]:
        """Runs `create-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("create-agent", items)

    def wait_to_run_agent(self,
        *,
        agent: str,
        device: str,
        ticket: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("wait-to-run-agent", agent=agent, device=device, ticket=ticket)

    def wait_to_run_agent_many(self, items: list[WaitToRunAgentArgs]) -> list[dict[str, Any]]:
        """Runs `wait-to-run-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("wait-to-run-agent", items)

    def run_agent_on(self,
        *,
        agent: str,
        device: str,
        certificate: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("run-agent-on", agent=agent, device=device, certificate=certificate)

    def run_agent_on_many(self, items: list[RunAgentOnArgs]) -> list[dict[str, Any]]:
        """Runs `run-agent-on` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("run-agent-on", items)

    def set_agent_spend_limit(self,
        *,
        agent: str,
        limit_micros: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        if limit_micros is not None:
            args["limit_micros"] = limit_micros
        return self.action("set-agent-spend-limit", **args)

    def set_agent_spend_limit_many(self, items: list[SetAgentSpendLimitArgs]) -> list[dict[str, Any]]:
        """Runs `set-agent-spend-limit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-agent-spend-limit", items)

    def record_agent_spend(self,
        *,
        agent: str,
        day: str,
        cost_micros: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("record-agent-spend", agent=agent, day=day, cost_micros=cost_micros)

    def record_agent_spend_many(self, items: list[RecordAgentSpendArgs]) -> list[dict[str, Any]]:
        """Runs `record-agent-spend` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-agent-spend", items)

    def rename_agent(self,
        *,
        agent: str,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("rename-agent", agent=agent, name=name)

    def rename_agent_many(self, items: list[RenameAgentArgs]) -> list[dict[str, Any]]:
        """Runs `rename-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename-agent", items)

    def set_agent_avatar(self,
        *,
        agent: str,
        avatar: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        if avatar is not None:
            args["avatar"] = avatar
        return self.action("set-agent-avatar", **args)

    def set_agent_avatar_many(self, items: list[SetAgentAvatarArgs]) -> list[dict[str, Any]]:
        """Runs `set-agent-avatar` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-agent-avatar", items)

    def set_agent_model(self,
        *,
        agent: str,
        model: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        if model is not None:
            args["model"] = model
        return self.action("set-agent-model", **args)

    def set_agent_model_many(self, items: list[SetAgentModelArgs]) -> list[dict[str, Any]]:
        """Runs `set-agent-model` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-agent-model", items)

    def set_agent_instruction(self,
        *,
        agent: str,
        instruction: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        if instruction is not None:
            args["instruction"] = instruction
        return self.action("set-agent-instruction", **args)

    def set_agent_instruction_many(self, items: list[SetAgentInstructionArgs]) -> list[dict[str, Any]]:
        """Runs `set-agent-instruction` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-agent-instruction", items)

    def pause_agent(self, *, agent: str) -> None:
        """Runs the app's action."""
        return self.action("pause-agent", agent=agent)

    def pause_agent_many(self, items: list[PauseAgentArgs]) -> list[dict[str, Any]]:
        """Runs `pause-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pause-agent", items)

    def resume_agent(self, *, agent: str) -> None:
        """Runs the app's action."""
        return self.action("resume-agent", agent=agent)

    def resume_agent_many(self, items: list[ResumeAgentArgs]) -> list[dict[str, Any]]:
        """Runs `resume-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resume-agent", items)

    def retire_agent(self, *, agent: str) -> None:
        """Runs the app's action."""
        return self.action("retire-agent", agent=agent)

    def retire_agent_many(self, items: list[RetireAgentArgs]) -> list[dict[str, Any]]:
        """Runs `retire-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("retire-agent", items)

    def start_sub_agent(self,
        *,
        name: str,
        job: str,
        role: str | None = None,
        instruction: str | None = None,
        kind: str | None = None,
        model: str | None = None,
        task: str | None = None,
        id: str | None = None,
        coding: NewCoding | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        args["job"] = job
        if role is not None:
            args["role"] = role
        if instruction is not None:
            args["instruction"] = instruction
        if kind is not None:
            args["kind"] = kind
        if model is not None:
            args["model"] = model
        if task is not None:
            args["task"] = task
        if id is not None:
            args["id"] = id
        if coding is not None:
            args["coding"] = coding
        return self.action("start-sub-agent", **args)

    def start_sub_agent_many(self, items: list[StartSubAgentArgs]) -> list[dict[str, Any]]:
        """Runs `start-sub-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start-sub-agent", items)

    def note_sub_agent(self,
        *,
        agent: str,
        text: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("note-sub-agent", agent=agent, text=text)

    def note_sub_agent_many(self, items: list[NoteSubAgentArgs]) -> list[dict[str, Any]]:
        """Runs `note-sub-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("note-sub-agent", items)

    def stop_sub_agent(self, *, agent: str) -> None:
        """Runs the app's action."""
        return self.action("stop-sub-agent", agent=agent)

    def stop_sub_agent_many(self, items: list[StopSubAgentArgs]) -> list[dict[str, Any]]:
        """Runs `stop-sub-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop-sub-agent", items)

    def sub_agent_stands(self,
        *,
        agent: str,
        stands: Stands,
        line: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        args["stands"] = stands
        if line is not None:
            args["line"] = line
        return self.action("sub-agent-stands", **args)

    def sub_agent_stands_many(self, items: list[SubAgentStandsArgs]) -> list[dict[str, Any]]:
        """Runs `sub-agent-stands` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sub-agent-stands", items)

    def sub_agent_produced(self,
        *,
        agent: str,
        kind: str,
        label: str,
        url: str | None = None,
        at: ProducedAt | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        args["kind"] = kind
        args["label"] = label
        if url is not None:
            args["url"] = url
        if at is not None:
            args["at"] = at
        return self.action("sub-agent-produced", **args)

    def sub_agent_produced_many(self, items: list[SubAgentProducedArgs]) -> list[dict[str, Any]]:
        """Runs `sub-agent-produced` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sub-agent-produced", items)

    def sub_agent_coding(self,
        *,
        agent: str,
        process: CodingProcess | None = None,
        session: str | None = None,
        spent_micros: int | None = None,
        harness: CodingHarness | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        if process is not None:
            args["process"] = process
        if session is not None:
            args["session"] = session
        if spent_micros is not None:
            args["spent_micros"] = spent_micros
        if harness is not None:
            args["harness"] = harness
        return self.action("sub-agent-coding", **args)

    def sub_agent_coding_many(self, items: list[SubAgentCodingArgs]) -> list[dict[str, Any]]:
        """Runs `sub-agent-coding` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sub-agent-coding", items)

    def set_sub_agent_limits(self,
        *,
        agent: str,
        depth: int | None = None,
        at_once: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        if depth is not None:
            args["depth"] = depth
        if at_once is not None:
            args["at_once"] = at_once
        return self.action("set-sub-agent-limits", **args)

    def set_sub_agent_limits_many(self, items: list[SetSubAgentLimitsArgs]) -> list[dict[str, Any]]:
        """Runs `set-sub-agent-limits` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-sub-agent-limits", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"create_agent", "create_agent_many", "wait_to_run_agent", "wait_to_run_agent_many", "run_agent_on", "run_agent_on_many", "set_agent_spend_limit", "set_agent_spend_limit_many", "record_agent_spend", "record_agent_spend_many", "rename_agent", "rename_agent_many", "set_agent_avatar", "set_agent_avatar_many", "set_agent_model", "set_agent_model_many", "set_agent_instruction", "set_agent_instruction_many", "pause_agent", "pause_agent_many", "resume_agent", "resume_agent_many", "retire_agent", "retire_agent_many", "start_sub_agent", "start_sub_agent_many", "note_sub_agent", "note_sub_agent_many", "stop_sub_agent", "stop_sub_agent_many", "sub_agent_stands", "sub_agent_stands_many", "sub_agent_produced", "sub_agent_produced_many", "sub_agent_coding", "sub_agent_coding_many", "set_sub_agent_limits", "set_sub_agent_limits_many"})


