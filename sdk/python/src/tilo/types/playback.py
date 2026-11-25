"""Types for Playback (`so.tilo.playback`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `playback/<resource>`, read with
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
    "PlaybackState",
    "Notice",
    "Output",
    "Item",
    "Progress",
    "Playback",
    "TiloPlayback",
    "PlayListArgs",
    "SetCurrentArgs",
    "SetPlayingArgs",
    "SetSourceArgs",
    "SetLengthArgs",
    "RememberPositionArgs",
    "SetPositionArgs",
    "SetShareArgs",
    "ResumeArgs",
    "PlayArgs",
    "ToggleArgs",
    "PauseArgs",
    "StopArgs",
    "NextArgs",
    "PreviousArgs",
    "SelectArgs",
    "SeekByArgs",
    "SeekArgs",
    "SetVolumeArgs",
    "MuteArgs",
    "ShuffleToggleArgs",
    "CycleRepeatArgs",
    "QueueAddArgs",
    "QueuePlayNextArgs",
    "QueueRemoveArgs",
    "QueueMoveArgs",
    "AddOutputArgs",
    "RemoveOutputArgs",
    "MoveToArgs",
    "SetOutputVolumeArgs",
]

PlaybackState = Literal["stopped", "playing", "paused"]
"""Whether the player stands still, plays, or has nothing at all."""

PlayListArgs = TypedDict("PlayListArgs", {
    "items": "list[Item]",
    "at": NotRequired["int | None"],
})
"""The arguments of `play_list`."""

SetCurrentArgs = TypedDict("SetCurrentArgs", {
    "item": "Item",
})
"""The arguments of `set_current`."""

SetPlayingArgs = TypedDict("SetPlayingArgs", {
    "on": "bool",
})
"""The arguments of `set_playing`."""

SetSourceArgs = TypedDict("SetSourceArgs", {
    "source": "str",
})
"""The arguments of `set_source`."""

SetLengthArgs = TypedDict("SetLengthArgs", {
    "seconds": "float",
})
"""The arguments of `set_length`."""

RememberPositionArgs = TypedDict("RememberPositionArgs", {
    "seconds": "float",
    "when": "str",
})
"""The arguments of `remember_position`."""

SetPositionArgs = TypedDict("SetPositionArgs", {
    "seconds": "float",
})
"""The arguments of `set_position`."""

SetShareArgs = TypedDict("SetShareArgs", {
    "share": "float",
})
"""The arguments of `set_share`."""

ResumeArgs = TypedDict("ResumeArgs", {
    "ref": "str",
})
"""The arguments of `resume`."""

PlayArgs = TypedDict("PlayArgs", {
})
"""The arguments of `play`."""

ToggleArgs = TypedDict("ToggleArgs", {
})
"""The arguments of `toggle`."""

PauseArgs = TypedDict("PauseArgs", {
})
"""The arguments of `pause`."""

StopArgs = TypedDict("StopArgs", {
})
"""The arguments of `stop`."""

NextArgs = TypedDict("NextArgs", {
})
"""The arguments of `next`."""

PreviousArgs = TypedDict("PreviousArgs", {
})
"""The arguments of `previous`."""

SelectArgs = TypedDict("SelectArgs", {
    "at": "int",
})
"""The arguments of `select`."""

SeekByArgs = TypedDict("SeekByArgs", {
    "seconds": NotRequired["int | None"],
})
"""The arguments of `seek_by`."""

SeekArgs = TypedDict("SeekArgs", {
    "place": NotRequired["float | None"],
})
"""The arguments of `seek`."""

SetVolumeArgs = TypedDict("SetVolumeArgs", {
    "level": NotRequired["float | None"],
})
"""The arguments of `set_volume`."""

MuteArgs = TypedDict("MuteArgs", {
})
"""The arguments of `mute`."""

ShuffleToggleArgs = TypedDict("ShuffleToggleArgs", {
    "shuffle": NotRequired["bool | None"],
})
"""The arguments of `shuffle_toggle`."""

CycleRepeatArgs = TypedDict("CycleRepeatArgs", {
})
"""The arguments of `cycle_repeat`."""

QueueAddArgs = TypedDict("QueueAddArgs", {
    "item": "Item",
})
"""The arguments of `queue_add`."""

QueuePlayNextArgs = TypedDict("QueuePlayNextArgs", {
    "item": "Item",
})
"""The arguments of `queue_play_next`."""

QueueRemoveArgs = TypedDict("QueueRemoveArgs", {
    "index": "int",
    "queue_at": NotRequired["int | None"],
})
"""The arguments of `queue_remove`."""

QueueMoveArgs = TypedDict("QueueMoveArgs", {
    "from": "str",
    "to": "str",
})
"""The arguments of `queue_move`."""

AddOutputArgs = TypedDict("AddOutputArgs", {
    "output": "Output",
})
"""The arguments of `add_output`."""

RemoveOutputArgs = TypedDict("RemoveOutputArgs", {
    "index": "int",
})
"""The arguments of `remove_output`."""

MoveToArgs = TypedDict("MoveToArgs", {
    "from": "int",
    "to": "int",
})
"""The arguments of `move_to`."""

SetOutputVolumeArgs = TypedDict("SetOutputVolumeArgs", {
    "index": "int",
    "level": NotRequired["float | None"],
})
"""The arguments of `set_output_volume`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.playback@4640c21ab213a5f2"
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


class Output(Resource):
    """Output — `tl.so.tilo.playback.Output`."""
    __surface__ = "so.tilo.playback@4640c21ab213a5f2"
    mtgbfbe3: str  # kind
    q2afggjg: str  # device
    duy3y22z: str  # name
    sz4b5nfh: float  # volume
    ewrhlhvs: bool  # synced


class Item(Resource):
    """Item — `tl.so.tilo.playback.Item`."""
    __surface__ = "so.tilo.playback@4640c21ab213a5f2"
    jsytyrgy: str  # ref
    hbrdxjws: str  # title
    @property
    def _3sty6xvi(self) -> str:
        """artist (`3sty6xvi` on the wire)."""
        return self.fields["3sty6xvi"]

    @_3sty6xvi.setter
    def _3sty6xvi(self, value: str) -> None:
        self.edit("3sty6xvi", value=value)
    wzxeznnc: str  # cover
    iueovkzr: str  # source
    ggr7nnxj: float  # length


class Progress(Resource):
    """Progress — `tl.so.tilo.playback.Progress`."""
    __surface__ = "so.tilo.playback@4640c21ab213a5f2"
    ijcc2upy: str  # ref
    owteykf6: float  # position
    q64zycak: str  # updated


class Playback(Resource):
    """Playback — `tl.so.tilo.playback.Playback`."""
    __surface__ = "so.tilo.playback@4640c21ab213a5f2"
    hnwowp42: str  # track
    sx7vtcfl: str  # title
    eejl5scp: str  # artist
    rjbz4skr: str  # cover
    tafuqjic: str  # source
    xcyilpu4: PlaybackState  # state
    p2dmm6gi: list[Entry[Output]]  # outputs
    pkexjxh3: float  # at
    min4lfee: float  # length
    udayujnj: float  # volume
    @property
    def _4t7z5bco(self) -> bool:
        """shuffle (`4t7z5bco` on the wire)."""
        return self.fields["4t7z5bco"]

    @_4t7z5bco.setter
    def _4t7z5bco(self, value: bool) -> None:
        self.edit("4t7z5bco", value=value)
    @property
    def _2l7pa2rs(self) -> str:
        """repeat (`2l7pa2rs` on the wire)."""
        return self.fields["2l7pa2rs"]

    @_2l7pa2rs.setter
    def _2l7pa2rs(self, value: str) -> None:
        self.edit("2l7pa2rs", value=value)
    wwdrlrqy: list[Entry[Item]]  # queue
    krqh6s6b: int  # queue_at
    eixby7cm: list[Entry[Progress]]  # progress

    def play_list(self,
        *,
        items: list[Item],
        at: int | None = None,
    ) -> None:
        """Plays the items `items` names, in the order given, from the one at `at`: the queue is what follows, and the item at `at` plays. The app that calls this fills the title, artist, cover, source and length it resolves from its own record; the shared actions below move the queue without reading another app's records."""
        args: dict[str, Any] = {}
        args["items"] = items
        if at is not None:
            args["at"] = at
        return self.action("play_list", **args)

    def play_list_many(self, items: list[PlayListArgs]) -> list[dict[str, Any]]:
        """Runs `play_list` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("play_list", items)

    def set_current(self, *, item: Item) -> None:
        """The session shows `item`: its fields are the item's, so a shared next or previous never leaves the title, cover or source of the one before it."""
        return self.action("set_current", item=item)

    def set_current_many(self, items: list[SetCurrentArgs]) -> list[dict[str, Any]]:
        """Runs `set_current` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_current", items)

    def set_playing(self, *, on: bool) -> None:
        """Plays or pauses what plays, as a player's own control and a media key ask it."""
        return self.action("set_playing", on=on)

    def set_playing_many(self, items: list[SetPlayingArgs]) -> list[dict[str, Any]]:
        """Runs `set_playing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_playing", items)

    def set_source(self, *, source: str) -> None:
        """Where what plays comes from, as the app that knows it answered again: a video's other quality, or the address it was asked for once more. The player opens it, and the live card and another device name the same bytes."""
        return self.action("set_source", source=source)

    def set_source_many(self, items: list[SetSourceArgs]) -> list[dict[str, Any]]:
        """Runs `set_source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_source", items)

    def set_length(self, *, seconds: float) -> None:
        """What the player reports of its length, once the decoder knows it."""
        return self.action("set_length", seconds=seconds)

    def set_length_many(self, items: list[SetLengthArgs]) -> list[dict[str, Any]]:
        """Runs `set_length` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_length", items)

    def remember_position(self,
        *,
        seconds: float,
        when: str,
    ) -> None:
        """Keeps the session's place and the item's as `when` records them. It has no effect of its own, so an app's action that plays can call it inline: an action that needs a moment is a step of its own, which a nested call only suggests."""
        return self.action("remember_position", seconds=seconds, when=when)

    def remember_position_many(self, items: list[RememberPositionArgs]) -> list[dict[str, Any]]:
        """Runs `remember_position` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remember_position", items)

    def set_position(self, *, seconds: float) -> None:
        """Where the player stands, as it reports it, so a later opening resumes there: the session's place and the item's, kept so another device or a later opening continues from it."""
        return self.action("set_position", seconds=seconds)

    def set_position_many(self, items: list[SetPositionArgs]) -> list[dict[str, Any]]:
        """Runs `set_position` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_position", items)

    def set_share(self, *, share: float) -> None:
        """Where the player stands, as the share of its length a player reports (`0..=1`), kept so a later opening resumes there."""
        return self.action("set_share", share=share)

    def set_share_many(self, items: list[SetShareArgs]) -> list[dict[str, Any]]:
        """Runs `set_share` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_share", items)

    def resume(self, *, ref: str) -> None:
        """Moves what plays to where `ref` was left, so a video or a song continues where the person stopped instead of beginning again."""
        return self.action("resume", ref=ref)

    def resume_many(self, items: list[ResumeArgs]) -> list[dict[str, Any]]:
        """Runs `resume` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resume", items)

    def play(self) -> None:
        """Plays what is chosen, or begins again where it stands."""
        return self.action("play")

    def play_many(self, items: list[PlayArgs]) -> list[dict[str, Any]]:
        """Runs `play` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("play", items)

    def toggle(self) -> None:
        """Plays or pauses what plays."""
        return self.action("toggle")

    def toggle_many(self, items: list[ToggleArgs]) -> list[dict[str, Any]]:
        """Runs `toggle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle", items)

    def pause(self) -> None:
        """Pauses what plays and starts it again where it stood ([`toggle`]'s own job)."""
        return self.action("pause")

    def pause_many(self, items: list[PauseArgs]) -> list[dict[str, Any]]:
        """Runs `pause` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pause", items)

    def stop(self) -> None:
        """Stops what plays: it plays on no device, and the shell shows it no more."""
        return self.action("stop")

    def stop_many(self, items: list[StopArgs]) -> list[dict[str, Any]]:
        """Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop", items)

    def next(self) -> None:
        """The item after this one: the queue's next, or the first when repeat is on."""
        return self.action("next")

    def next_many(self, items: list[NextArgs]) -> list[dict[str, Any]]:
        """Runs `next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("next", items)

    def previous(self) -> None:
        """The item before this one."""
        return self.action("previous")

    def previous_many(self, items: list[PreviousArgs]) -> list[dict[str, Any]]:
        """Runs `previous` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("previous", items)

    def select(self, *, at: int) -> None:
        """Plays the queued item at `at`, keeping the queue as it stands."""
        return self.action("select", at=at)

    def select_many(self, items: list[SelectArgs]) -> list[dict[str, Any]]:
        """Runs `select` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("select", items)

    def seek_by(self,
        *,
        seconds: int | None = None,
    ) -> None:
        """Moves what plays by `seconds`, for the live card's two seek controls."""
        args: dict[str, Any] = {}
        if seconds is not None:
            args["seconds"] = seconds
        return self.action("seek_by", **args)

    def seek_by_many(self, items: list[SeekByArgs]) -> list[dict[str, Any]]:
        """Runs `seek_by` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("seek_by", items)

    def seek(self,
        *,
        place: float | None = None,
    ) -> None:
        """Moves what plays to `place`, a fraction of its length, and keeps the place so a later opening continues there."""
        args: dict[str, Any] = {}
        if place is not None:
            args["place"] = place
        return self.action("seek", **args)

    def seek_many(self, items: list[SeekArgs]) -> list[dict[str, Any]]:
        """Runs `seek` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("seek", items)

    def set_volume(self,
        *,
        level: float | None = None,
    ) -> None:
        """How loud the session is."""
        args: dict[str, Any] = {}
        if level is not None:
            args["level"] = level
        return self.action("set_volume", **args)

    def set_volume_many(self, items: list[SetVolumeArgs]) -> list[dict[str, Any]]:
        """Runs `set_volume` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_volume", items)

    def mute(self) -> None:
        """Silences the session, or brings back the volume it had."""
        return self.action("mute")

    def mute_many(self, items: list[MuteArgs]) -> list[dict[str, Any]]:
        """Runs `mute` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mute", items)

    def shuffle_toggle(self,
        *,
        shuffle: bool | None = None,
    ) -> None:
        """Shuffles the queue, or puts its order back. The item that plays stays first."""
        args: dict[str, Any] = {}
        if shuffle is not None:
            args["shuffle"] = shuffle
        return self.action("shuffle_toggle", **args)

    def shuffle_toggle_many(self, items: list[ShuffleToggleArgs]) -> list[dict[str, Any]]:
        """Runs `shuffle_toggle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("shuffle_toggle", items)

    def cycle_repeat(self) -> None:
        """Moves repeat on: off, all, one."""
        return self.action("cycle_repeat")

    def cycle_repeat_many(self, items: list[CycleRepeatArgs]) -> list[dict[str, Any]]:
        """Runs `cycle_repeat` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cycle_repeat", items)

    def queue_add(self, *, item: Item) -> None:
        """Adds `item` after what plays."""
        return self.action("queue_add", item=item)

    def queue_add_many(self, items: list[QueueAddArgs]) -> list[dict[str, Any]]:
        """Runs `queue_add` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("queue_add", items)

    def queue_play_next(self, *, item: Item) -> None:
        """Adds `item` right after what plays."""
        return self.action("queue_play_next", item=item)

    def queue_play_next_many(self, items: list[QueuePlayNextArgs]) -> list[dict[str, Any]]:
        """Runs `queue_play_next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("queue_play_next", items)

    def queue_remove(self,
        *,
        index: int,
        queue_at: int | None = None,
    ) -> None:
        """Takes the item at `index` out of the queue."""
        args: dict[str, Any] = {}
        args["index"] = index
        if queue_at is not None:
            args["queue_at"] = queue_at
        return self.action("queue_remove", **args)

    def queue_remove_many(self, items: list[QueueRemoveArgs]) -> list[dict[str, Any]]:
        """Runs `queue_remove` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("queue_remove", items)

    def queue_move(self,
        *,
        from_: str,
        to: str,
    ) -> None:
        """Moves the item called `from` to stand where `to` stands: the queue row a person dragged, and the ids its reorder reports."""
        args: dict[str, Any] = {}
        args["from"] = from_
        args["to"] = to
        return self.action("queue_move", **args)

    def queue_move_many(self, items: list[QueueMoveArgs]) -> list[dict[str, Any]]:
        """Runs `queue_move` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("queue_move", items)

    def add_output(self, *, output: Output) -> None:
        """Puts the sound on one more place: a speaker in a room, or a cast target."""
        return self.action("add_output", output=output)

    def add_output_many(self, items: list[AddOutputArgs]) -> list[dict[str, Any]]:
        """Runs `add_output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_output", items)

    def remove_output(self, *, index: int) -> None:
        """Takes the place at `index` out of the session; this device is never taken out."""
        return self.action("remove_output", index=index)

    def remove_output_many(self, items: list[RemoveOutputArgs]) -> list[dict[str, Any]]:
        """Runs `remove_output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_output", items)

    def move_to(self,
        *,
        from_: int,
        to: int,
    ) -> None:
        """Moves the place at `from` to where `to` is: 'continue on the speaker' is moving the session from this device to it."""
        args: dict[str, Any] = {}
        args["from"] = from_
        args["to"] = to
        return self.action("move_to", **args)

    def move_to_many(self, items: list[MoveToArgs]) -> list[dict[str, Any]]:
        """Runs `move_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_to", items)

    def set_output_volume(self,
        *,
        index: int,
        level: float | None = None,
    ) -> None:
        """Sets how loud the place at `index` is."""
        args: dict[str, Any] = {}
        args["index"] = index
        if level is not None:
            args["level"] = level
        return self.action("set_output_volume", **args)

    def set_output_volume_many(self, items: list[SetOutputVolumeArgs]) -> list[dict[str, Any]]:
        """Runs `set_output_volume` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_output_volume", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"play_list", "play_list_many", "set_current", "set_current_many", "set_playing", "set_playing_many", "set_source", "set_source_many", "set_length", "set_length_many", "remember_position", "remember_position_many", "set_position", "set_position_many", "set_share", "set_share_many", "resume", "resume_many", "play", "play_many", "toggle", "toggle_many", "pause", "pause_many", "stop", "stop_many", "next", "next_many", "previous", "previous_many", "select", "select_many", "seek_by", "seek_by_many", "seek", "seek_many", "set_volume", "set_volume_many", "mute", "mute_many", "shuffle_toggle", "shuffle_toggle_many", "cycle_repeat", "cycle_repeat_many", "queue_add", "queue_add_many", "queue_play_next", "queue_play_next_many", "queue_remove", "queue_remove_many", "queue_move", "queue_move_many", "add_output", "add_output_many", "remove_output", "remove_output_many", "move_to", "move_to_many", "set_output_volume", "set_output_volume_many"})


class TiloPlayback(Resource):
    """Playback — `tl.so.tilo.playback`."""
    __surface__ = "so.tilo.playback@4640c21ab213a5f2"

