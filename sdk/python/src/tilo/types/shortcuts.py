"""Types for Shortcuts (`so.tilo.shortcuts`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `shortcuts/<resource>`, read with
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
    "Bind",
    "Cond",
    "Pick",
    "Branch",
    "Head",
    "InputType",
    "Policy",
    "Selector",
    "By",
    "Connected",
    "FocusMode",
    "FocusTransition",
    "TriggerKind",
    "Notice",
    "Param",
    "Member",
    "BindValue",
    "Condition",
    "Choice",
    "Arg",
    "Slot",
    "Problem",
    "Grant",
    "Step",
    "Input",
    "Trigger",
    "Offer",
    "Program",
    "Suggested",
    "Saved",
    "Draft",
    "Workbench",
    "Place",
    "Heard",
    "Repeated",
    "Shortcuts",
    "LoadValueArgs",
    "AdoptChoiceArgs",
    "CopyValueArgs",
    "LoadConditionArgs",
    "CopyConditionArgs",
    "LoadChoiceArgs",
    "LoadArgArgs",
    "ApplyOfferArgs",
    "ApplyTriggerArgs",
    "LoadSlotArgs",
    "LoadProgramArgs",
    "LoadBlockArgs",
    "LoadStepArgs",
    "LoadTriggersArgs",
    "LoadSavedArgs",
    "LoadShortcutArgs",
    "CopyBlockArgs",
    "DisarmArgs",
    "RemoveBelowArgs",
    "MatchCatalogueArgs",
    "NewDraftArgs",
    "DiscardArgs",
    "IgnoreProposalArgs",
    "AllowProposalArgs",
    "OpenPickerArgs",
    "ClosePickerArgs",
    "EditTriggerArgs",
    "CancelEditArgs",
    "DismissSuggestedArgs",
    "ToggleFoldArgs",
    "ShowSheetArgs",
    "ClearNewInputArgs",
    "ToggleDayArgs",
    "DismissNoticeArgs",
]

Bind = Literal["none", "literal", "input", "variable", "step", "template", "object", "list"]
"""How a value is held: typed, one of the shortcut's inputs, a variable, an earlier step's output, text with placeholders, or an object or list of values."""

Cond = Literal["truthy", "equals", "negate", "all", "any"]
"""What a branch asks."""

Pick = Literal["none", "choice", "literal"]
"""How a value a person chose is kept: not chosen, one of the values in scope, or a value typed."""

Branch = Literal["top", "then", "otherwise", "each"]
"""Which block of a step the step inside it runs in."""

Head = Literal["invoke", "branch", "each", "set", "ask", "ai", "task", "read"]
"""What a step does."""

InputType = Literal["text", "number", "integer", "boolean", "date_time", "resource"]
"""The kinds of value a shortcut takes when it is started."""

Policy = Literal["fail", "retry", "fallback", "carry_on"]
"""What a run does when a step fails."""

Selector = Literal["fixed", "kind", "binding"]
"""How a step names the resource its action is called on."""

By = Literal["anyone", "people", "not_bots"]
"""Whose changes a record trigger counts."""

Connected = Literal["any", "connected", "disconnected"]
"""Which network changes a network trigger counts."""

FocusMode = Literal["do_not_disturb", "focus"]
"""What a focus-mode trigger watches: Do Not Disturb or the focus schedule."""

FocusTransition = Literal["enter", "exit"]
"""Whether it watches the mode switching on or off."""

TriggerKind = Literal["keyboard", "app_opened", "schedule_day", "schedule_every", "once", "workspace_joined", "file_changed", "focus_mode", "network_changed", "device_connected", "resource_changed", "app_event", "incoming_message", "nearby_device", "resource_time", "source_changed"]
"""What starts a saved shortcut on its own."""

LoadValueArgs = TypedDict("LoadValueArgs", {
    "value": "Any",
})
"""The arguments of `load_value`."""

AdoptChoiceArgs = TypedDict("AdoptChoiceArgs", {
    "choice": "Choice",
})
"""The arguments of `adopt_choice`."""

CopyValueArgs = TypedDict("CopyValueArgs", {
    "from": "BindValue",
})
"""The arguments of `copy_value`."""

LoadConditionArgs = TypedDict("LoadConditionArgs", {
    "value": "Any",
})
"""The arguments of `load_condition`."""

CopyConditionArgs = TypedDict("CopyConditionArgs", {
    "from": "Condition",
})
"""The arguments of `copy_condition`."""

LoadChoiceArgs = TypedDict("LoadChoiceArgs", {
    "value": "Any",
})
"""The arguments of `load_choice`."""

LoadArgArgs = TypedDict("LoadArgArgs", {
    "name": "str",
    "value": "Any",
})
"""The arguments of `load_arg`."""

ApplyOfferArgs = TypedDict("ApplyOfferArgs", {
    "offered": "Any",
})
"""The arguments of `apply_offer`."""

ApplyTriggerArgs = TypedDict("ApplyTriggerArgs", {
    "from": "Trigger",
})
"""The arguments of `apply_trigger`."""

LoadSlotArgs = TypedDict("LoadSlotArgs", {
    "s": "Step",
    "name": "str",
    "value": "Any",
})
"""The arguments of `load_slot`."""

LoadProgramArgs = TypedDict("LoadProgramArgs", {
    "item": "Any",
})
"""The arguments of `load_program`."""

LoadBlockArgs = TypedDict("LoadBlockArgs", {
    "steps": NotRequired["Any | None"],
    "parent": NotRequired["Step | None"],
    "branch": "Branch",
})
"""The arguments of `load_block`."""

LoadStepArgs = TypedDict("LoadStepArgs", {
    "item": "Any",
    "parent": NotRequired["Step | None"],
    "branch": "Branch",
})
"""The arguments of `load_step`."""

LoadTriggersArgs = TypedDict("LoadTriggersArgs", {
    "triggers": "list[Any]",
})
"""The arguments of `load_triggers`."""

LoadSavedArgs = TypedDict("LoadSavedArgs", {
    "value": "Any",
})
"""The arguments of `load_saved`."""

LoadShortcutArgs = TypedDict("LoadShortcutArgs", {
    "program": "Any",
})
"""The arguments of `load_shortcut`."""

CopyBlockArgs = TypedDict("CopyBlockArgs", {
    "to": "Draft",
    "parent": NotRequired["Step | None"],
    "branch": "Branch",
})
"""The arguments of `copy_block`."""

DisarmArgs = TypedDict("DisarmArgs", {
})
"""The arguments of `disarm`."""

RemoveBelowArgs = TypedDict("RemoveBelowArgs", {
    "s": "Step",
})
"""The arguments of `remove_below`."""

MatchCatalogueArgs = TypedDict("MatchCatalogueArgs", {
})
"""The arguments of `match_catalogue`."""

NewDraftArgs = TypedDict("NewDraftArgs", {
    "name": NotRequired["str | None"],
})
"""The arguments of `new_draft`."""

DiscardArgs = TypedDict("DiscardArgs", {
    "d": "Draft",
})
"""The arguments of `discard`."""

IgnoreProposalArgs = TypedDict("IgnoreProposalArgs", {
    "key": "str",
})
"""The arguments of `ignore_proposal`."""

AllowProposalArgs = TypedDict("AllowProposalArgs", {
    "key": "str",
})
"""The arguments of `allow_proposal`."""

OpenPickerArgs = TypedDict("OpenPickerArgs", {
    "d": "Draft",
    "block": "str",
})
"""The arguments of `open_picker`."""

ClosePickerArgs = TypedDict("ClosePickerArgs", {
})
"""The arguments of `close_picker`."""

EditTriggerArgs = TypedDict("EditTriggerArgs", {
    "t": "Trigger",
})
"""The arguments of `edit_trigger`."""

CancelEditArgs = TypedDict("CancelEditArgs", {
})
"""The arguments of `cancel_edit`."""

DismissSuggestedArgs = TypedDict("DismissSuggestedArgs", {
    "s": "Suggested",
})
"""The arguments of `dismiss_suggested`."""

ToggleFoldArgs = TypedDict("ToggleFoldArgs", {
    "d": "Draft",
    "path": "str",
})
"""The arguments of `toggle_fold`."""

ShowSheetArgs = TypedDict("ShowSheetArgs", {
    "sheet": "str",
})
"""The arguments of `show_sheet`."""

ClearNewInputArgs = TypedDict("ClearNewInputArgs", {
})
"""The arguments of `clear_new_input`."""

ToggleDayArgs = TypedDict("ToggleDayArgs", {
    "day": "str",
})
"""The arguments of `toggle_day`."""

DismissNoticeArgs = TypedDict("DismissNoticeArgs", {
})
"""The arguments of `dismiss_notice`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    @property
    def notice_to(self) -> str:
        """to (`notice-to` on the wire)."""
        return self.fields["notice-to"]

    @notice_to.setter
    def notice_to(self, value: str) -> None:
        self.edit("notice-to", value=value)
    @property
    def notice_title(self) -> str:
        """title (`notice-title` on the wire)."""
        return self.fields["notice-title"]

    @notice_title.setter
    def notice_title(self, value: str) -> None:
        self.edit("notice-title", value=value)
    @property
    def notice_body(self) -> str:
        """body (`notice-body` on the wire)."""
        return self.fields["notice-body"]

    @notice_body.setter
    def notice_body(self, value: str) -> None:
        self.edit("notice-body", value=value)
    @property
    def notice_opens(self) -> Any:
        """opens (`notice-opens` on the wire)."""
        return self.fields["notice-opens"]

    @notice_opens.setter
    def notice_opens(self, value: Any) -> None:
        self.edit("notice-opens", value=value)
    @property
    def notice_seen(self) -> bool:
        """seen (`notice-seen` on the wire)."""
        return self.fields["notice-seen"]

    @notice_seen.setter
    def notice_seen(self, value: bool) -> None:
        self.edit("notice-seen", value=value)
    @property
    def notice_device(self) -> str:
        """device (`notice-device` on the wire)."""
        return self.fields["notice-device"]

    @notice_device.setter
    def notice_device(self, value: str) -> None:
        self.edit("notice-device", value=value)
    @property
    def notice_actions(self) -> list[str]:
        """actions (`notice-actions` on the wire)."""
        return self.fields["notice-actions"]

    @notice_actions.setter
    def notice_actions(self, value: list[str]) -> None:
        self.edit("notice-actions", value=value)


class Param(Resource):
    """Param — `tl.so.tilo.shortcuts.Param`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    w7j2mzpu: str  # name
    ckvfwyfc: str  # ty
    lovculcl: bool  # required
    dley6sq2: str  # description


class Member(Resource):
    """Member — `tl.so.tilo.shortcuts.Member`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    mvlmdz3t: str  # at
    orfu2qci: BindValue  # value


class BindValue(Resource):
    """BindValue — `tl.so.tilo.shortcuts.BindValue`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    a43zwzxm: Bind  # kind
    nu47tafs: str  # ty
    q5rbcvgr: str  # name
    dzmxkvwn: str  # pointer
    bktowd37: str  # literal
    iu7blqby: list[Entry[Member]]  # members

    def load_value(self, *, value: Any) -> None:
        """Reads a value the engine wrote into `into`, and what an object or list holds inside it."""
        return self.action("load_value", value=value)

    def load_value_many(self, items: list[LoadValueArgs]) -> list[dict[str, Any]]:
        """Runs `load_value` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_value", items)

    def adopt_choice(self, *, choice: Choice) -> None:
        """Copies a value the checker offered into an argument's or a slot's own value."""
        return self.action("adopt_choice", choice=choice)

    def adopt_choice_many(self, items: list[AdoptChoiceArgs]) -> list[dict[str, Any]]:
        """Runs `adopt_choice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("adopt_choice", items)

    def copy_value(self, *, from_: BindValue) -> None:
        """Copies `from` into `into`, with what an object or a list holds."""
        args: dict[str, Any] = {}
        args["from"] = from_
        return self.action("copy_value", **args)

    def copy_value_many(self, items: list[CopyValueArgs]) -> list[dict[str, Any]]:
        """Runs `copy_value` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("copy_value", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"load_value", "load_value_many", "adopt_choice", "adopt_choice_many", "copy_value", "copy_value_many"})


class Condition(Resource):
    """Condition — `tl.so.tilo.shortcuts.Condition`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    @property
    def _5qdigjgy(self) -> Cond:
        """kind (`5qdigjgy` on the wire)."""
        return self.fields["5qdigjgy"]

    @_5qdigjgy.setter
    def _5qdigjgy(self, value: Cond) -> None:
        self.edit("5qdigjgy", value=value)
    jm3nn7p2: BindValue  # left
    o4jmv2xp: BindValue  # right
    lwqjd6de: list[Entry[Condition]]  # inner

    def load_condition(self, *, value: Any) -> None:
        """Reads a condition the engine wrote into `c`."""
        return self.action("load_condition", value=value)

    def load_condition_many(self, items: list[LoadConditionArgs]) -> list[dict[str, Any]]:
        """Runs `load_condition` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_condition", items)

    def copy_condition(self, *, from_: Condition) -> None:
        """Copies a condition into `into`, with the conditions it joins."""
        args: dict[str, Any] = {}
        args["from"] = from_
        return self.action("copy_condition", **args)

    def copy_condition_many(self, items: list[CopyConditionArgs]) -> list[dict[str, Any]]:
        """Runs `copy_condition` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("copy_condition", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"load_condition", "load_condition_many", "copy_condition", "copy_condition_many"})


class Choice(Resource):
    """Choice — `tl.so.tilo.shortcuts.Choice`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    jjnl5c5u: Bind  # kind
    mf4qff6c: str  # ty
    kufc2lcn: str  # name
    @property
    def _3uy3tubv(self) -> str:
        """pointer (`3uy3tubv` on the wire)."""
        return self.fields["3uy3tubv"]

    @_3uy3tubv.setter
    def _3uy3tubv(self, value: str) -> None:
        self.edit("3uy3tubv", value=value)
    gouakd6l: str  # literal
    bi3zwtrn: str  # describes

    def load_choice(self, *, value: Any) -> None:
        """Reads one value the checker offered into `choice`, whose parts are the value's own."""
        return self.action("load_choice", value=value)

    def load_choice_many(self, items: list[LoadChoiceArgs]) -> list[dict[str, Any]]:
        """Runs `load_choice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_choice", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"load_choice", "load_choice_many"})


class Arg(Resource):
    """Arg — `tl.so.tilo.shortcuts.Arg`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    jrmg23bl: str  # name
    @property
    def _6cqaypb4(self) -> str:
        """ty (`6cqaypb4` on the wire)."""
        return self.fields["6cqaypb4"]

    @_6cqaypb4.setter
    def _6cqaypb4(self, value: str) -> None:
        self.edit("6cqaypb4", value=value)
    dqswtaus: Pick  # pick
    bqmee32h: BindValue  # value
    nq3xfb3y: list[Entry[Choice]]  # choices


class Slot(Resource):
    """Slot — `tl.so.tilo.shortcuts.Slot`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    @property
    def _6l2kcrfo(self) -> str:
        """name (`6l2kcrfo` on the wire)."""
        return self.fields["6l2kcrfo"]

    @_6l2kcrfo.setter
    def _6l2kcrfo(self, value: str) -> None:
        self.edit("6l2kcrfo", value=value)
    bwqrb4mc: str  # ty
    ns6t6yay: Pick  # pick
    a5ejwysp: BindValue  # value


class Problem(Resource):
    """Problem — `tl.so.tilo.shortcuts.Problem`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    ddjwwbo3: str  # path
    yyjdqeh7: str  # message


class Grant(Resource):
    """Grant — `tl.so.tilo.shortcuts.Grant`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    @property
    def _44e6m2wx(self) -> str:
        """label (`44e6m2wx` on the wire)."""
        return self.fields["44e6m2wx"]

    @_44e6m2wx.setter
    def _44e6m2wx(self, value: str) -> None:
        self.edit("44e6m2wx", value=value)
    w6pqc4i2: str  # kind


class Step(Resource):
    """Step — `tl.so.tilo.shortcuts.Step`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    h3opbkx5: Step | None  # parent
    wb3ui23u: Branch  # branch
    so6ey5xl: str  # step_id
    vncpsqdq: Head  # head
    dcuko4cu: str  # app_id
    gc5jsux7: str  # action_kind
    srbrc53z: str  # action_name
    z7jjhgj3: str  # action_label
    cvrfcqnx: str  # resource_label
    m4zdkmap: Selector  # selector
    @property
    def _7sf54hyp(self) -> str:
        """target (`7sf54hyp` on the wire)."""
        return self.fields["7sf54hyp"]

    @_7sf54hyp.setter
    def _7sf54hyp(self, value: str) -> None:
        self.edit("7sf54hyp", value=value)
    @property
    def _7jcsp3l3(self) -> BindValue:
        """resource_binding (`7jcsp3l3` on the wire)."""
        return self.fields["7jcsp3l3"]

    @_7jcsp3l3.setter
    def _7jcsp3l3(self, value: BindValue) -> None:
        self.edit("7jcsp3l3", value=value)
    dpalqxeo: list[Entry[Param]]  # params
    shmtjvi6: list[Entry[Arg]]  # args
    lldhjl3c: list[Entry[Slot]]  # slots
    rpum3xhw: Condition  # condition
    e6xcw5u3: str  # items
    zz64m2bs: str  # variable
    su76kmlu: str  # name
    vsalsfek: str  # value
    szzje4pf: str  # ask_name
    lryaixlb: InputType  # ask_type
    nnrjiqq3: str  # ask_description
    wkqhz6gh: str  # prompt
    dlqum4fl: BindValue  # ai_input
    @property
    def _7kdxq5eg(self) -> str:
        """ai_output (`7kdxq5eg` on the wire)."""
        return self.fields["7kdxq5eg"]

    @_7kdxq5eg.setter
    def _7kdxq5eg(self, value: str) -> None:
        self.edit("7kdxq5eg", value=value)
    xnnuhmxj: str  # instruction
    rq6c5mlz: str  # allow
    ptov4iys: str  # task_output
    yyj53wzo: str  # read_app
    qd53kkgv: str  # read_record
    oihaj3wf: bool  # read_joined
    @property
    def _2f76vazs(self) -> Policy:
        """policy (`2f76vazs` on the wire)."""
        return self.fields["2f76vazs"]

    @_2f76vazs.setter
    def _2f76vazs(self, value: Policy) -> None:
        self.edit("2f76vazs", value=value)
    ydlvlxqi: float  # attempts
    w3tgejid: float  # backoff_ms
    b7i6c2gd: BindValue  # default_value

    def load_arg(self,
        *,
        name: str,
        value: Any,
    ) -> None:
        """Runs the app's action."""
        return self.action("load_arg", name=name, value=value)

    def load_arg_many(self, items: list[LoadArgArgs]) -> list[dict[str, Any]]:
        """Runs `load_arg` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_arg", items)

    def apply_offer(self, *, offered: Any) -> None:
        """What an offered action says about a step that calls it: its name, its resource, and what every parameter takes."""
        return self.action("apply_offer", offered=offered)

    def apply_offer_many(self, items: list[ApplyOfferArgs]) -> list[dict[str, Any]]:
        """Runs `apply_offer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("apply_offer", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"load_arg", "load_arg_many", "apply_offer", "apply_offer_many"})


class Input(Resource):
    """Input — `tl.so.tilo.shortcuts.Input`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    snpnqwju: str  # name
    tg4tli3b: InputType  # ty
    m7r4ya7c: bool  # required
    jgm4hgap: str  # description
    zwcz4mo7: str  # value


class Trigger(Resource):
    """Trigger — `tl.so.tilo.shortcuts.Trigger`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    @property
    def _5bbzhstm(self) -> TriggerKind:
        """kind (`5bbzhstm` on the wire)."""
        return self.fields["5bbzhstm"]

    @_5bbzhstm.setter
    def _5bbzhstm(self, value: TriggerKind) -> None:
        self.edit("5bbzhstm", value=value)
    nl7rjhv5: str  # chord
    h6z7jmu7: float  # hour
    pwego36a: float  # minute
    izj53iqb: list[str]  # weekdays
    w5cphrf7: bool  # catch_up
    jwrxcn3c: float  # period_secs
    mkcu4vtl: int  # once_ms
    e3uy6zru: str  # once_text
    @property
    def _4inykrds(self) -> str:
        """app_id (`4inykrds` on the wire)."""
        return self.fields["4inykrds"]

    @_4inykrds.setter
    def _4inykrds(self, value: str) -> None:
        self.edit("4inykrds", value=value)
    q7czkxzj: str  # workspace
    ehgpmzgw: str  # folder
    gcicb4ub: FocusMode  # focus_mode
    xumiu4qy: FocusTransition  # focus_transition
    wctuo7yb: str  # network
    oim6j5ww: Connected  # connected
    @property
    def _2yej4ltk(self) -> str:
        """device_class (`2yej4ltk` on the wire)."""
        return self.fields["2yej4ltk"]

    @_2yej4ltk.setter
    def _2yej4ltk(self, value: str) -> None:
        self.edit("2yej4ltk", value=value)
    bnuqxwzt: str  # record_kind
    pxnxp2in: list[str]  # actions
    uixffjmo: list[str]  # only_fields
    evffas55: str  # actions_text
    q5adiprn: str  # fields_text
    @property
    def _6zzuwtig(self) -> By:
        """by (`6zzuwtig` on the wire)."""
        return self.fields["6zzuwtig"]

    @_6zzuwtig.setter
    def _6zzuwtig(self, value: By) -> None:
        self.edit("6zzuwtig", value=value)
    muql6lws: str  # event_kind
    gavij27k: str  # from
    x7wd6xhm: str  # device
    @property
    def _7xi6qhpz(self) -> str:
        """time_field (`7xi6qhpz` on the wire)."""
        return self.fields["7xi6qhpz"]

    @_7xi6qhpz.setter
    def _7xi6qhpz(self, value: str) -> None:
        self.edit("7xi6qhpz", value=value)
    dcoqx7cm: float  # offset_secs
    kr5hkco2: str  # filter
    solyra7s: str  # source_at
    @property
    def _5pqw4rrp(self) -> float:
        """every_secs (`5pqw4rrp` on the wire)."""
        return self.fields["5pqw4rrp"]

    @_5pqw4rrp.setter
    def _5pqw4rrp(self, value: float) -> None:
        self.edit("5pqw4rrp", value=value)

    def apply_trigger(self, *, from_: Trigger) -> None:
        """Copies every field of one trigger onto another."""
        args: dict[str, Any] = {}
        args["from"] = from_
        return self.action("apply_trigger", **args)

    def apply_trigger_many(self, items: list[ApplyTriggerArgs]) -> list[dict[str, Any]]:
        """Runs `apply_trigger` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("apply_trigger", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"apply_trigger", "apply_trigger_many"})


class Offer(Resource):
    """Offer — `tl.so.tilo.shortcuts.Offer`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    xitbwh3f: str  # app_id
    @property
    def _2oava6yw(self) -> str:
        """app_label (`2oava6yw` on the wire)."""
        return self.fields["2oava6yw"]

    @_2oava6yw.setter
    def _2oava6yw(self, value: str) -> None:
        self.edit("2oava6yw", value=value)
    zuozugvm: str  # label
    jufgzesb: str  # resource
    bzlonbec: str  # resource_kind
    jqocub4q: str  # resource_id
    @property
    def _56tpoja7(self) -> str:
        """address (`56tpoja7` on the wire)."""
        return self.fields["56tpoja7"]

    @_56tpoja7.setter
    def _56tpoja7(self, value: str) -> None:
        self.edit("56tpoja7", value=value)
    gnvisywl: bool  # reads
    fzldwoda: list[Entry[Param]]  # params


class Program(Resource):
    """Program — `tl.so.tilo.shortcuts.Program`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    @property
    def _6ddumpv3(self) -> list[Entry[Input]]:
        """inputs (`6ddumpv3` on the wire)."""
        return self.fields["6ddumpv3"]

    @_6ddumpv3.setter
    def _6ddumpv3(self, value: list[Entry[Input]]) -> None:
        self.edit("6ddumpv3", value=value)
    zjl7ntoz: list[Entry[Step]]  # steps
    tndlzxkx: list[Entry[Trigger]]  # triggers
    nvg74t2f: BindValue  # output
    hzxfqcak: int  # step_timeout_secs
    ywxim34z: str  # grant

    def load_slot(self,
        *,
        s: Step,
        name: str,
        value: Any,
    ) -> None:
        """A value a step's block reads, as a slot: a literal typed, or the binding that holds it."""
        return self.action("load_slot", s=s, name=name, value=value)

    def load_slot_many(self, items: list[LoadSlotArgs]) -> list[dict[str, Any]]:
        """Runs `load_slot` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_slot", items)

    def load_program(self, *, item: Any) -> None:
        """Runs the app's action."""
        return self.action("load_program", item=item)

    def load_program_many(self, items: list[LoadProgramArgs]) -> list[dict[str, Any]]:
        """Runs `load_program` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_program", items)

    def load_block(self,
        *,
        branch: Branch,
        steps: Any | None = None,
        parent: Step | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if steps is not None:
            args["steps"] = steps
        if parent is not None:
            args["parent"] = parent
        args["branch"] = branch
        return self.action("load_block", **args)

    def load_block_many(self, items: list[LoadBlockArgs]) -> list[dict[str, Any]]:
        """Runs `load_block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_block", items)

    def load_step(self,
        *,
        item: Any,
        branch: Branch,
        parent: Step | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["item"] = item
        if parent is not None:
            args["parent"] = parent
        args["branch"] = branch
        return self.action("load_step", **args)

    def load_step_many(self, items: list[LoadStepArgs]) -> list[dict[str, Any]]:
        """Runs `load_step` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_step", items)

    def load_triggers(self, *, triggers: list[Any]) -> None:
        """Runs the app's action."""
        return self.action("load_triggers", triggers=triggers)

    def load_triggers_many(self, items: list[LoadTriggersArgs]) -> list[dict[str, Any]]:
        """Runs `load_triggers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_triggers", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"load_slot", "load_slot_many", "load_program", "load_program_many", "load_block", "load_block_many", "load_step", "load_step_many", "load_triggers", "load_triggers_many"})


class Suggested(Resource):
    """Suggested — `tl.so.tilo.shortcuts.Suggested`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    m53kboqe: str  # name
    duorb75u: Program  # program
    pshhv3vt: list[str]  # problems
    @property
    def _6e4ysabb(self) -> list[str]:
        """changes (`6e4ysabb` on the wire)."""
        return self.fields["6e4ysabb"]

    @_6e4ysabb.setter
    def _6e4ysabb(self, value: list[str]) -> None:
        self.edit("6e4ysabb", value=value)


class Saved(Resource):
    """Saved — `tl.so.tilo.shortcuts.Saved`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    l4rxg53i: str  # id
    @property
    def _2hqp5wnu(self) -> str:
        """name (`2hqp5wnu` on the wire)."""
        return self.fields["2hqp5wnu"]

    @_2hqp5wnu.setter
    def _2hqp5wnu(self, value: str) -> None:
        self.edit("2hqp5wnu", value=value)
    tpetr7if: Program  # program
    fydnutns: bool  # paused
    cc5kjsxw: str  # last_call
    @property
    def _44lyca4z(self) -> int:
        """last_started (`44lyca4z` on the wire)."""
        return self.fields["44lyca4z"]

    @_44lyca4z.setter
    def _44lyca4z(self, value: int) -> None:
        self.edit("44lyca4z", value=value)

    def load_saved(self, *, value: Any) -> None:
        """Reads the program a shortcut is made of into `p`. Reads one saved shortcut's entry in the library into `saved`."""
        return self.action("load_saved", value=value)

    def load_saved_many(self, items: list[LoadSavedArgs]) -> list[dict[str, Any]]:
        """Runs `load_saved` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_saved", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"load_saved", "load_saved_many"})


class Draft(Resource):
    """Draft — `tl.so.tilo.shortcuts.Draft`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    qodc2wld: str  # name
    imvb73zl: str  # shortcut_id
    jdjr5ygm: bool  # replaces_saved
    qhds5d5u: Program  # program
    kcuhy6xu: list[Entry[Problem]]  # problems
    uzhz35e3: list[Entry[Grant]]  # grants
    unbpnmsw: str  # folded
    @property
    def _2j6kqzzx(self) -> bool:
        """as_text (`2j6kqzzx` on the wire)."""
        return self.fields["2j6kqzzx"]

    @_2j6kqzzx.setter
    def _2j6kqzzx(self, value: bool) -> None:
        self.edit("2j6kqzzx", value=value)

    def load_shortcut(self, *, program: Any) -> None:
        """A shortcut written as JSON, loaded into `d` as its steps, inputs and triggers."""
        return self.action("load_shortcut", program=program)

    def load_shortcut_many(self, items: list[LoadShortcutArgs]) -> list[dict[str, Any]]:
        """Runs `load_shortcut` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_shortcut", items)

    def copy_block(self,
        *,
        to: Draft,
        branch: Branch,
        parent: Step | None = None,
    ) -> None:
        """Copies the steps of one block, and every block inside them, into another draft."""
        args: dict[str, Any] = {}
        args["to"] = to
        if parent is not None:
            args["parent"] = parent
        args["branch"] = branch
        return self.action("copy_block", **args)

    def copy_block_many(self, items: list[CopyBlockArgs]) -> list[dict[str, Any]]:
        """Runs `copy_block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("copy_block", items)

    def disarm(self) -> None:
        """A shortcut loaded as a draft arms nothing: a person saves it first, after the review."""
        return self.action("disarm")

    def disarm_many(self, items: list[DisarmArgs]) -> list[dict[str, Any]]:
        """Runs `disarm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("disarm", items)

    def remove_below(self, *, s: Step) -> None:
        """Runs the app's action."""
        return self.action("remove_below", s=s)

    def remove_below_many(self, items: list[RemoveBelowArgs]) -> list[dict[str, Any]]:
        """Runs `remove_below` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_below", items)

    def review(self) -> None:
        """Check the draft, and what each of its steps' arguments may be bound to, so the editor shows a person what saving would accept without the view reading anything itself."""
        return self.action("review")

    def match_catalogue(self) -> None:
        """Keeps every action step's parameters and the type of each argument what the catalogue says they are, reading the catalogue once."""
        return self.action("match_catalogue")

    def match_catalogue_many(self, items: list[MatchCatalogueArgs]) -> list[dict[str, Any]]:
        """Runs `match_catalogue` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("match_catalogue", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"load_shortcut", "load_shortcut_many", "copy_block", "copy_block_many", "disarm", "disarm_many", "remove_below", "remove_below_many", "review", "match_catalogue", "match_catalogue_many"})


class Workbench(Resource):
    """Workbench — `tl.so.tilo.shortcuts.Workbench`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    rwdjyqtz: list[Entry[Draft]]  # drafts
    daxqqmym: list[Entry[Saved]]  # saved
    fbadmr47: list[Entry[Suggested]]  # suggested
    dmzlirxv: str  # notice
    ke54krt6: str  # failure
    vjatpjs3: Trigger  # form
    td3nielz: Trigger | None  # editing
    jpfqt6cc: str  # sheet
    wb5qv6mt: str  # description
    t2nlvsmx: str  # comment
    sgiqblcg: str  # picker_query
    xmyhixxe: str  # new_input_name
    jnw4b5oc: InputType  # new_input_type
    olhnz4lr: list[str]  # ignored
    @property
    def _45ccshtx(self) -> list[Entry[Offer]]:
        """picker (`45ccshtx` on the wire)."""
        return self.fields["45ccshtx"]

    @_45ccshtx.setter
    def _45ccshtx(self, value: list[Entry[Offer]]) -> None:
        self.edit("45ccshtx", value=value)
    awchuh2j: Draft | None  # picker_for
    rxujaqef: str  # picker_block

    def new_draft(self,
        *,
        name: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("new_draft", **args)

    def new_draft_many(self, items: list[NewDraftArgs]) -> list[dict[str, Any]]:
        """Runs `new_draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new_draft", items)

    def rename_draft(self,
        *,
        d: Draft,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("rename_draft", d=d, name=name)

    def duplicate_draft(self, *, d: Draft) -> None:
        """Runs the app's action."""
        return self.action("duplicate_draft", d=d)

    def discard(self, *, d: Draft) -> None:
        """Runs the app's action."""
        return self.action("discard", d=d)

    def discard_many(self, items: list[DiscardArgs]) -> list[dict[str, Any]]:
        """Runs `discard` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("discard", items)

    def draft_shortcut(self, *, shortcut: str) -> None:
        """Runs the app's action."""
        return self.action("draft_shortcut", shortcut=shortcut)

    def open_shortcut(self, *, shortcut: str) -> None:
        """Runs the app's action."""
        return self.action("open_shortcut", shortcut=shortcut)

    def duplicate_shortcut(self, *, shortcut: str) -> None:
        """Runs the app's action."""
        return self.action("duplicate_shortcut", shortcut=shortcut)

    def draft_proposal(self, *, program: str) -> None:
        """Open a shortcut the app proposed, named after the work it noticed, as an unsaved draft."""
        return self.action("draft_proposal", program=program)

    def ignore_proposal(self, *, key: str) -> None:
        """Runs the app's action."""
        return self.action("ignore_proposal", key=key)

    def ignore_proposal_many(self, items: list[IgnoreProposalArgs]) -> list[dict[str, Any]]:
        """Runs `ignore_proposal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ignore_proposal", items)

    def allow_proposal(self, *, key: str) -> None:
        """Runs the app's action."""
        return self.action("allow_proposal", key=key)

    def allow_proposal_many(self, items: list[AllowProposalArgs]) -> list[dict[str, Any]]:
        """Runs `allow_proposal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("allow_proposal", items)

    def open_picker(self,
        *,
        d: Draft,
        block: str,
    ) -> None:
        """The actions the person is offered, read now, for the open picker. Nothing is kept once the picker closes: what a view draws it read in the action that opened it."""
        return self.action("open_picker", d=d, block=block)

    def open_picker_many(self, items: list[OpenPickerArgs]) -> list[dict[str, Any]]:
        """Runs `open_picker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_picker", items)

    def close_picker(self) -> None:
        """Runs the app's action."""
        return self.action("close_picker")

    def close_picker_many(self, items: list[ClosePickerArgs]) -> list[dict[str, Any]]:
        """Runs `close_picker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("close_picker", items)

    def add_offer(self,
        *,
        d: Draft,
        o: Offer,
    ) -> None:
        """Runs the app's action."""
        return self.action("add_offer", d=d, o=o)

    def add_block(self,
        *,
        d: Draft,
        head: Head,
    ) -> None:
        """Add a step that is not an app's action: `if`, `for_each`, `set_var`, `ask`, `ai` or `task`."""
        return self.action("add_block", d=d, head=head)

    def remove_step(self,
        *,
        d: Draft,
        s: Step,
    ) -> None:
        """Runs the app's action."""
        return self.action("remove_step", d=d, s=s)

    def move_step(self,
        *,
        d: Draft,
        s: Step,
        earlier: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("move_step", d=d, s=s, earlier=earlier)

    def nest_step(self,
        *,
        d: Draft,
        s: Step,
    ) -> None:
        """Runs the app's action."""
        return self.action("nest_step", d=d, s=s)

    def lift_step(self,
        *,
        d: Draft,
        s: Step,
    ) -> None:
        """Runs the app's action."""
        return self.action("lift_step", d=d, s=s)

    def set_policy(self,
        *,
        d: Draft,
        s: Step,
        policy: Policy,
    ) -> None:
        """Runs the app's action."""
        return self.action("set_policy", d=d, s=s, policy=policy)

    def set_output(self,
        *,
        d: Draft,
        choice: Choice,
    ) -> None:
        """Runs the app's action."""
        return self.action("set_output", d=d, choice=choice)

    def clear_output(self, *, d: Draft) -> None:
        """Runs the app's action."""
        return self.action("clear_output", d=d)

    def set_slot(self,
        *,
        d: Draft,
        s: Step,
        name: str,
        pick: Pick,
        choice: Choice,
    ) -> None:
        """Where one value a step reads comes from."""
        return self.action("set_slot", d=d, s=s, name=name, pick=pick, choice=choice)

    def type_slot(self,
        *,
        d: Draft,
        s: Step,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("type_slot", d=d, s=s, name=name)

    def set_argument(self,
        *,
        d: Draft,
        s: Step,
        a: Arg,
        pick: Pick,
        choice: Choice,
    ) -> None:
        """Runs the app's action."""
        return self.action("set_argument", d=d, s=s, a=a, pick=pick, choice=choice)

    def clear_argument(self,
        *,
        d: Draft,
        s: Step,
        a: Arg,
    ) -> None:
        """Runs the app's action."""
        return self.action("clear_argument", d=d, s=s, a=a)

    def clear_slot(self,
        *,
        d: Draft,
        s: Step,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("clear_slot", d=d, s=s, name=name)

    def type_argument(self,
        *,
        d: Draft,
        s: Step,
        a: Arg,
    ) -> None:
        """Runs the app's action."""
        return self.action("type_argument", d=d, s=s, a=a)

    def set_condition_typed(self,
        *,
        d: Draft,
        s: Step,
        text: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set_condition_typed", d=d, s=s, text=text)

    def add_input(self,
        *,
        d: Draft,
        name: str,
        ty: InputType,
    ) -> None:
        """Runs the app's action."""
        return self.action("add_input", d=d, name=name, ty=ty)

    def remove_input(self,
        *,
        d: Draft,
        i: Input,
    ) -> None:
        """Runs the app's action."""
        return self.action("remove_input", d=d, i=i)

    def add_trigger(self,
        *,
        d: Draft,
        t: Trigger,
    ) -> None:
        """Runs the app's action."""
        return self.action("add_trigger", d=d, t=t)

    def apply_form(self, *, d: Draft) -> None:
        """Add what the form holds to the open draft, or replace the trigger the form is editing."""
        return self.action("apply_form", d=d)

    def edit_trigger(self, *, t: Trigger) -> None:
        """Load a trigger the draft carries into the form, to change it."""
        return self.action("edit_trigger", t=t)

    def edit_trigger_many(self, items: list[EditTriggerArgs]) -> list[dict[str, Any]]:
        """Runs `edit_trigger` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit_trigger", items)

    def cancel_edit(self) -> None:
        """Runs the app's action."""
        return self.action("cancel_edit")

    def cancel_edit_many(self, items: list[CancelEditArgs]) -> list[dict[str, Any]]:
        """Runs `cancel_edit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cancel_edit", items)

    def remove_trigger(self,
        *,
        d: Draft,
        t: Trigger,
    ) -> None:
        """Runs the app's action."""
        return self.action("remove_trigger", d=d, t=t)

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def save(self, *, d: Draft) -> None:
        """Save the draft with the least grant saving would store, after the same check the editor shows."""
        return self.action("save", d=d)

    def run(self, *, s: Saved) -> None:
        """Run a saved shortcut with what its inputs hold."""
        return self.action("run", s=s)

    def delete_saved(self, *, s: Saved) -> None:
        """Runs the app's action."""
        return self.action("delete_saved", s=s)

    def pause_saved(self, *, s: Saved) -> None:
        """Runs the app's action."""
        return self.action("pause_saved", s=s)

    def resume_saved(self, *, s: Saved) -> None:
        """Runs the app's action."""
        return self.action("resume_saved", s=s)

    def stop_saved(self, *, s: Saved) -> None:
        """Runs the app's action."""
        return self.action("stop_saved", s=s)

    def stop_all(self) -> None:
        """Runs the app's action."""
        return self.action("stop_all")

    def describe(self, *, description: str) -> None:
        """Ask the agent to draft a shortcut doing what the description says."""
        return self.action("describe", description=description)

    def refine(self,
        *,
        s: Saved,
        comment: str,
    ) -> None:
        """Ask the agent what the shortcut should do differently."""
        return self.action("refine", s=s, comment=comment)

    def adopt(self,
        *,
        answer: str,
        changes: list[str],
    ) -> None:
        """Offer what the agent answered as a draft to open, with what the checker says of it."""
        return self.action("adopt", answer=answer, changes=changes)

    def open_suggested(self, *, s: Suggested) -> None:
        """Runs the app's action."""
        return self.action("open_suggested", s=s)

    def dismiss_suggested(self, *, s: Suggested) -> None:
        """Runs the app's action."""
        return self.action("dismiss_suggested", s=s)

    def dismiss_suggested_many(self, items: list[DismissSuggestedArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss_suggested` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss_suggested", items)

    def toggle_fold(self,
        *,
        d: Draft,
        path: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("toggle_fold", d=d, path=path)

    def toggle_fold_many(self, items: list[ToggleFoldArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_fold` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_fold", items)

    def show_sheet(self, *, sheet: str) -> None:
        """Runs the app's action."""
        return self.action("show_sheet", sheet=sheet)

    def show_sheet_many(self, items: list[ShowSheetArgs]) -> list[dict[str, Any]]:
        """Runs `show_sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_sheet", items)

    def clear_new_input(self) -> None:
        """Runs the app's action."""
        return self.action("clear_new_input")

    def clear_new_input_many(self, items: list[ClearNewInputArgs]) -> list[dict[str, Any]]:
        """Runs `clear_new_input` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_new_input", items)

    def toggle_day(self, *, day: str) -> None:
        """Runs the app's action."""
        return self.action("toggle_day", day=day)

    def toggle_day_many(self, items: list[ToggleDayArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_day", items)

    def dismiss_notice(self) -> None:
        """Runs the app's action."""
        return self.action("dismiss_notice")

    def dismiss_notice_many(self, items: list[DismissNoticeArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss_notice", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_draft", "new_draft_many", "rename_draft", "duplicate_draft", "discard", "discard_many", "draft_shortcut", "open_shortcut", "duplicate_shortcut", "draft_proposal", "ignore_proposal", "ignore_proposal_many", "allow_proposal", "allow_proposal_many", "open_picker", "open_picker_many", "close_picker", "close_picker_many", "add_offer", "add_block", "remove_step", "move_step", "nest_step", "lift_step", "set_policy", "set_output", "clear_output", "set_slot", "type_slot", "set_argument", "clear_argument", "clear_slot", "type_argument", "set_condition_typed", "add_input", "remove_input", "add_trigger", "apply_form", "edit_trigger", "edit_trigger_many", "cancel_edit", "cancel_edit_many", "remove_trigger", "save", "run", "delete_saved", "pause_saved", "resume_saved", "stop_saved", "stop_all", "describe", "refine", "adopt", "open_suggested", "dismiss_suggested", "dismiss_suggested_many", "toggle_fold", "toggle_fold_many", "show_sheet", "show_sheet_many", "clear_new_input", "clear_new_input_many", "toggle_day", "toggle_day_many", "dismiss_notice", "dismiss_notice_many"})


class Place(Resource):
    """Place — `tl.so.tilo.shortcuts.Place`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    uccg2srx: Step | None  # parent
    d5s4qkn6: Branch  # branch
    lmkmjowh: int  # index


class Heard(Resource):
    """Heard — `tl.so.tilo.shortcuts.Heard`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    gjnz55fw: str  # app_id
    ld52mtpg: str  # action_name
    mflabbsc: str  # kind
    obs5it3e: str  # resource
    v3scnpbm: str  # label


class Repeated(Resource):
    """Repeated — `tl.so.tilo.shortcuts.Repeated`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    jhdyurec: list[Heard]  # steps
    qyjikemy: int  # times


class Shortcuts(Resource):
    """Shortcuts — `tl.so.tilo.shortcuts`."""
    __surface__ = "so.tilo.shortcuts@68b6226c2ffe01a0"
    def agent_address(self) -> str:
        """The agent's action that drafts, when this workspace is offered it; empty when it is not."""
        return self.action("agent_address")

    def catalogue_summary(self) -> str:
        """The actions a draft may use, one line each, as the agent is told about them."""
        return self.action("catalogue_summary")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"agent_address", "catalogue_summary"})


