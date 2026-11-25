"""Types for Clocks (`so.tilo.clocks`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `clocks/<resource>`, read with
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
    "Weekday",
    "Notice",
    "WorldClock",
    "Alarm",
    "Timer",
    "Lap",
    "Stopwatch",
    "Clocks",
    "TiloClocks",
    "EditAlarmArgs",
    "SetAlarmEnabledArgs",
    "SetAlarmDeviceArgs",
    "ToggleDayArgs",
    "SnoozeAlarmArgs",
    "RingAlarmArgs",
    "PauseTimerArgs",
    "ResumeTimerArgs",
    "AddMinuteArgs",
    "RemoveWorldClockArgs",
    "RemoveAlarmArgs",
    "RemoveTimerArgs",
    "StartStopwatchArgs",
    "PauseStopwatchArgs",
    "LapArgs",
    "ResetStopwatchArgs",
]

Weekday = Literal["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
"""A day of the week an alarm repeats on, Monday first."""

EditAlarmArgs = TypedDict("EditAlarmArgs", {
    "label": "str",
    "hour": "int",
    "minute": "int",
    "days": NotRequired["list[Weekday] | None"],
})
"""The arguments of `edit_alarm`."""

SetAlarmEnabledArgs = TypedDict("SetAlarmEnabledArgs", {
    "enabled": NotRequired["bool | None"],
})
"""The arguments of `set_alarm_enabled`."""

SetAlarmDeviceArgs = TypedDict("SetAlarmDeviceArgs", {
    "rings_on": "str",
})
"""The arguments of `set_alarm_device`."""

ToggleDayArgs = TypedDict("ToggleDayArgs", {
    "d": "Weekday",
})
"""The arguments of `toggle_day`."""

SnoozeAlarmArgs = TypedDict("SnoozeAlarmArgs", {
})
"""The arguments of `snooze_alarm`."""

RingAlarmArgs = TypedDict("RingAlarmArgs", {
})
"""The arguments of `ring_alarm`."""

PauseTimerArgs = TypedDict("PauseTimerArgs", {
})
"""The arguments of `pause_timer`."""

ResumeTimerArgs = TypedDict("ResumeTimerArgs", {
})
"""The arguments of `resume_timer`."""

AddMinuteArgs = TypedDict("AddMinuteArgs", {
})
"""The arguments of `add_minute`."""

RemoveWorldClockArgs = TypedDict("RemoveWorldClockArgs", {
    "w": "WorldClock",
})
"""The arguments of `remove_world_clock`."""

RemoveAlarmArgs = TypedDict("RemoveAlarmArgs", {
    "a": "Alarm",
})
"""The arguments of `remove_alarm`."""

RemoveTimerArgs = TypedDict("RemoveTimerArgs", {
    "t": "Timer",
})
"""The arguments of `remove_timer`."""

StartStopwatchArgs = TypedDict("StartStopwatchArgs", {
})
"""The arguments of `start_stopwatch`."""

PauseStopwatchArgs = TypedDict("PauseStopwatchArgs", {
})
"""The arguments of `pause_stopwatch`."""

LapArgs = TypedDict("LapArgs", {
})
"""The arguments of `lap`."""

ResetStopwatchArgs = TypedDict("ResetStopwatchArgs", {
})
"""The arguments of `reset_stopwatch`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.clocks@860b782a67ee26a5"
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


class WorldClock(Resource):
    """WorldClock — `tl.so.tilo.clocks.WorldClock`."""
    __surface__ = "so.tilo.clocks@860b782a67ee26a5"
    kbeao6ct: str  # city
    g5vsvo2p: str  # zone


class Alarm(Resource):
    """Alarm — `tl.so.tilo.clocks.Alarm`."""
    __surface__ = "so.tilo.clocks@860b782a67ee26a5"
    e6jhyy6h: str  # label
    cb6boqqg: int  # hour
    bvmw7lxl: int  # minute
    s4ppnbtv: list[Weekday]  # days
    adpv3jco: bool  # enabled
    fbxt6ruu: str | None  # at
    @property
    def _3bdkffqt(self) -> str:
        """rings_on (`3bdkffqt` on the wire)."""
        return self.fields["3bdkffqt"]

    @_3bdkffqt.setter
    def _3bdkffqt(self, value: str) -> None:
        self.edit("3bdkffqt", value=value)
    uizteftt: bool  # snoozed

    def edit_alarm(self,
        *,
        label: str,
        hour: int,
        minute: int,
        days: list[Weekday] | None = None,
    ) -> None:
        """Changes an alarm's words, time and days. Its on/off state stays as it was, and one that is on rings next at the time it now names."""
        args: dict[str, Any] = {}
        args["label"] = label
        args["hour"] = hour
        args["minute"] = minute
        if days is not None:
            args["days"] = days
        return self.action("edit_alarm", **args)

    def edit_alarm_many(self, items: list[EditAlarmArgs]) -> list[dict[str, Any]]:
        """Runs `edit_alarm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit_alarm", items)

    def set_alarm_enabled(self,
        *,
        enabled: bool | None = None,
    ) -> None:
        """Turns an alarm on or off: off, it does not ring; on, it rings next at its time."""
        args: dict[str, Any] = {}
        if enabled is not None:
            args["enabled"] = enabled
        return self.action("set_alarm_enabled", **args)

    def set_alarm_enabled_many(self, items: list[SetAlarmEnabledArgs]) -> list[dict[str, Any]]:
        """Runs `set_alarm_enabled` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_alarm_enabled", items)

    def set_alarm_device(self, *, rings_on: str) -> None:
        """Names the device an alarm rings on: one device's id, or none for every device of the person's."""
        return self.action("set_alarm_device", rings_on=rings_on)

    def set_alarm_device_many(self, items: list[SetAlarmDeviceArgs]) -> list[dict[str, Any]]:
        """Runs `set_alarm_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_alarm_device", items)

    def toggle_day(self, *, d: Weekday) -> None:
        """Repeats an alarm on `d`, or stops repeating on it: a day it names rings, a day it does not skips. Its next ring moves to the next day it names."""
        return self.action("toggle_day", d=d)

    def toggle_day_many(self, items: list[ToggleDayArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_day", items)

    def snooze_alarm(self) -> None:
        """Snoozes the alarm ringing now: it rings again in five minutes."""
        return self.action("snooze_alarm")

    def snooze_alarm_many(self, items: list[SnoozeAlarmArgs]) -> list[dict[str, Any]]:
        """Runs `snooze_alarm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("snooze_alarm", items)

    def ring_alarm(self) -> None:
        """Rings an alarm when its moment comes, on the device it names, window or no window. A repeating alarm rings again the next day it names; one that rings once turns itself off."""
        return self.action("ring_alarm")

    def ring_alarm_many(self, items: list[RingAlarmArgs]) -> list[dict[str, Any]]:
        """Runs `ring_alarm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ring_alarm", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_alarm", "edit_alarm_many", "set_alarm_enabled", "set_alarm_enabled_many", "set_alarm_device", "set_alarm_device_many", "toggle_day", "toggle_day_many", "snooze_alarm", "snooze_alarm_many", "ring_alarm", "ring_alarm_many"})


class Timer(Resource):
    """Timer — `tl.so.tilo.clocks.Timer`."""
    __surface__ = "so.tilo.clocks@860b782a67ee26a5"
    nhqnhguz: str  # label
    @property
    def _5b7nw57j(self) -> int:
        """seconds (`5b7nw57j` on the wire)."""
        return self.fields["5b7nw57j"]

    @_5b7nw57j.setter
    def _5b7nw57j(self, value: int) -> None:
        self.edit("5b7nw57j", value=value)
    @property
    def _6anly4z4(self) -> str | None:
        """ends_at (`6anly4z4` on the wire)."""
        return self.fields["6anly4z4"]

    @_6anly4z4.setter
    def _6anly4z4(self, value: str | None) -> None:
        self.edit("6anly4z4", value=value)
    ufpse6mi: int | None  # left

    def pause_timer(self) -> None:
        """Pauses a running timer where it stands."""
        return self.action("pause_timer")

    def pause_timer_many(self, items: list[PauseTimerArgs]) -> list[dict[str, Any]]:
        """Runs `pause_timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pause_timer", items)

    def resume_timer(self) -> None:
        """Resumes a paused timer, or starts a finished one over."""
        return self.action("resume_timer")

    def resume_timer_many(self, items: list[ResumeTimerArgs]) -> list[dict[str, Any]]:
        """Runs `resume_timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resume_timer", items)

    def add_minute(self) -> None:
        """Adds a minute to a timer, running or paused."""
        return self.action("add_minute")

    def add_minute_many(self, items: list[AddMinuteArgs]) -> list[dict[str, Any]]:
        """Runs `add_minute` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_minute", items)

    def ring_timer(self) -> None:
        """Rings a timer when it finishes, on every device of the person's, window or no window."""
        return self.action("ring_timer")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"pause_timer", "pause_timer_many", "resume_timer", "resume_timer_many", "add_minute", "add_minute_many", "ring_timer"})


class Lap(Resource):
    """Lap — `tl.so.tilo.clocks.Lap`."""
    __surface__ = "so.tilo.clocks@860b782a67ee26a5"
    mq5diuqb: int  # at


class Stopwatch(Resource):
    """Stopwatch — `tl.so.tilo.clocks.Stopwatch`."""
    __surface__ = "so.tilo.clocks@860b782a67ee26a5"
    yhijhjuh: int  # elapsed
    fg2gmze6: str | None  # since
    ngcmcd7g: list[Entry[Lap]]  # laps


class Clocks(Resource):
    """Clocks — `tl.so.tilo.clocks.Clocks`."""
    __surface__ = "so.tilo.clocks@860b782a67ee26a5"
    r2amisi7: list[Entry[WorldClock]]  # world_clocks
    gneumngu: list[Entry[Alarm]]  # alarms
    kahapraa: list[Entry[Timer]]  # timers
    rjh2x76r: Stopwatch  # stopwatch
    hnwcprw3: bool  # active

    def add_world_clock(self,
        *,
        city: str,
        zone: str,
    ) -> WorldClock:
        """Adds a city's clock to the workspace."""
        return self.action("add_world_clock", city=city, zone=zone)

    def remove_world_clock(self, *, w: WorldClock) -> None:
        """Removes a city's clock."""
        return self.action("remove_world_clock", w=w)

    def remove_world_clock_many(self, items: list[RemoveWorldClockArgs]) -> list[dict[str, Any]]:
        """Runs `remove_world_clock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_world_clock", items)

    def add_alarm(self,
        *,
        label: str,
        hour: int,
        minute: int,
        days: list[Weekday] | None = None,
        rings_on: str | None = None,
    ) -> Alarm:
        """Sets an alarm: at the time of day `when` names, on the days it repeats on, ringing on `rings_on`, every device of the person's when it names none. It rings first at the next such time."""
        args: dict[str, Any] = {}
        args["label"] = label
        args["hour"] = hour
        args["minute"] = minute
        if days is not None:
            args["days"] = days
        if rings_on is not None:
            args["rings_on"] = rings_on
        return self.action("add_alarm", **args)

    def remove_alarm(self, *, a: Alarm) -> None:
        """Removes an alarm."""
        return self.action("remove_alarm", a=a)

    def remove_alarm_many(self, items: list[RemoveAlarmArgs]) -> list[dict[str, Any]]:
        """Runs `remove_alarm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_alarm", items)

    def add_timer(self,
        *,
        label: str,
        seconds: int,
    ) -> Timer:
        """Starts a timer of `seconds`."""
        return self.action("add_timer", label=label, seconds=seconds)

    def remove_timer(self, *, t: Timer) -> None:
        """Removes a timer."""
        return self.action("remove_timer", t=t)

    def remove_timer_many(self, items: list[RemoveTimerArgs]) -> list[dict[str, Any]]:
        """Runs `remove_timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_timer", items)

    def start_stopwatch(self) -> None:
        """Starts the stopwatch."""
        return self.action("start_stopwatch")

    def start_stopwatch_many(self, items: list[StartStopwatchArgs]) -> list[dict[str, Any]]:
        """Runs `start_stopwatch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_stopwatch", items)

    def pause_stopwatch(self) -> None:
        """Pauses the stopwatch where it stands."""
        return self.action("pause_stopwatch")

    def pause_stopwatch_many(self, items: list[PauseStopwatchArgs]) -> list[dict[str, Any]]:
        """Runs `pause_stopwatch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pause_stopwatch", items)

    def lap(self) -> None:
        """Keeps a lap: the time the stopwatch stands at."""
        return self.action("lap")

    def lap_many(self, items: list[LapArgs]) -> list[dict[str, Any]]:
        """Runs `lap` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("lap", items)

    def reset_stopwatch(self) -> None:
        """Resets the stopwatch: it is at zero and stopped, with no laps."""
        return self.action("reset_stopwatch")

    def reset_stopwatch_many(self, items: list[ResetStopwatchArgs]) -> list[dict[str, Any]]:
        """Runs `reset_stopwatch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset_stopwatch", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_world_clock", "remove_world_clock", "remove_world_clock_many", "add_alarm", "remove_alarm", "remove_alarm_many", "add_timer", "remove_timer", "remove_timer_many", "start_stopwatch", "start_stopwatch_many", "pause_stopwatch", "pause_stopwatch_many", "lap", "lap_many", "reset_stopwatch", "reset_stopwatch_many"})


class TiloClocks(Resource):
    """Clocks — `tl.so.tilo.clocks`."""
    __surface__ = "so.tilo.clocks@860b782a67ee26a5"

