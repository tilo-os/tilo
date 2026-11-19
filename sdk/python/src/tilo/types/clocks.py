"""Types for Clocks (`so.tilo.clocks`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Collection` is the app's `clocks.collection` at `clocks/<resource>`, read with
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
    "Alarm",
    "Stopwatch",
    "Timer",
    "WeekdayKey",
    "WorldClock",
    "Collection",
    "AddWorldClockArgs",
    "RemoveWorldClockArgs",
    "AddAlarmArgs",
    "SetAlarmEnabledArgs",
    "EditAlarmArgs",
    "RemoveAlarmArgs",
    "AddTimerArgs",
    "PauseTimerArgs",
    "ResumeTimerArgs",
    "AddMinuteArgs",
    "RemoveTimerArgs",
    "StartStopwatchArgs",
    "PauseStopwatchArgs",
    "LapArgs",
    "ResetStopwatchArgs",
]

class Alarm(TypedDict):
    label: str
    hour: int
    minute: int
    repeat: int
    enabled: bool
    enabled_at_ms: int

class Stopwatch(TypedDict):
    accumulated_ms: int
    started_at_ms: int | None
    laps_ms: list[int]

class Timer(TypedDict):
    label: str
    duration_ms: int
    ends_at_ms: int | None
    paused_remaining_ms: int | None

WeekdayKey = Literal["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]

class WorldClock(TypedDict):
    city: str
    timezone: str

AddWorldClockArgs = TypedDict("AddWorldClockArgs", {
    "city": "str",
    "timezone": "str",
})
"""The arguments of `add-world-clock`."""

RemoveWorldClockArgs = TypedDict("RemoveWorldClockArgs", {
    "world_clock": "str",
})
"""The arguments of `remove-world-clock`."""

AddAlarmArgs = TypedDict("AddAlarmArgs", {
    "label": "str",
    "hour": "int",
    "minute": "int",
    "weekdays": "list[WeekdayKey]",
})
"""The arguments of `add-alarm`."""

SetAlarmEnabledArgs = TypedDict("SetAlarmEnabledArgs", {
    "alarm": "str",
    "enabled": "bool",
})
"""The arguments of `set-alarm-enabled`."""

EditAlarmArgs = TypedDict("EditAlarmArgs", {
    "alarm": "str",
    "label": "str",
    "hour": "int",
    "minute": "int",
    "weekdays": "list[WeekdayKey]",
})
"""The arguments of `edit-alarm`."""

RemoveAlarmArgs = TypedDict("RemoveAlarmArgs", {
    "alarm": "str",
})
"""The arguments of `remove-alarm`."""

AddTimerArgs = TypedDict("AddTimerArgs", {
    "label": "str",
    "seconds": "int",
})
"""The arguments of `add-timer`."""

PauseTimerArgs = TypedDict("PauseTimerArgs", {
    "timer": "str",
})
"""The arguments of `pause-timer`."""

ResumeTimerArgs = TypedDict("ResumeTimerArgs", {
    "timer": "str",
})
"""The arguments of `resume-timer`."""

AddMinuteArgs = TypedDict("AddMinuteArgs", {
    "timer": "str",
})
"""The arguments of `add-minute`."""

RemoveTimerArgs = TypedDict("RemoveTimerArgs", {
    "timer": "str",
})
"""The arguments of `remove-timer`."""

StartStopwatchArgs = TypedDict("StartStopwatchArgs", {
})
"""The arguments of `start-stopwatch`."""

PauseStopwatchArgs = TypedDict("PauseStopwatchArgs", {
})
"""The arguments of `pause-stopwatch`."""

LapArgs = TypedDict("LapArgs", {
})
"""The arguments of `lap`."""

ResetStopwatchArgs = TypedDict("ResetStopwatchArgs", {
})
"""The arguments of `reset-stopwatch`."""

class Collection(Resource):
    """Clocks — `clocks.collection`."""
    __surface__ = "so.tilo.clocks@43e52d1090e872f9"
    world_clocks: list[Entry[WorldClock]]
    alarms: list[Entry[Alarm]]
    timers: list[Entry[Timer]]
    @property
    def stopwatch(self) -> Stopwatch:
        """Read only."""
        return self.fields["stopwatch"]

    def add_world_clock(self,
        *,
        city: str,
        timezone: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-world-clock", city=city, timezone=timezone)

    def add_world_clock_many(self, items: list[AddWorldClockArgs]) -> list[dict[str, Any]]:
        """Runs `add-world-clock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-world-clock", items)

    def remove_world_clock(self, *, world_clock: str) -> None:
        """Runs the app's action."""
        return self.action("remove-world-clock", world_clock=world_clock)

    def remove_world_clock_many(self, items: list[RemoveWorldClockArgs]) -> list[dict[str, Any]]:
        """Runs `remove-world-clock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-world-clock", items)

    def add_alarm(self,
        *,
        label: str,
        hour: int,
        minute: int,
        weekdays: list[WeekdayKey],
    ) -> None:
        """Runs the app's action."""
        return self.action("add-alarm", label=label, hour=hour, minute=minute, weekdays=weekdays)

    def add_alarm_many(self, items: list[AddAlarmArgs]) -> list[dict[str, Any]]:
        """Runs `add-alarm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-alarm", items)

    def set_alarm_enabled(self,
        *,
        alarm: str,
        enabled: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-alarm-enabled", alarm=alarm, enabled=enabled)

    def set_alarm_enabled_many(self, items: list[SetAlarmEnabledArgs]) -> list[dict[str, Any]]:
        """Runs `set-alarm-enabled` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-alarm-enabled", items)

    def edit_alarm(self,
        *,
        alarm: str,
        label: str,
        hour: int,
        minute: int,
        weekdays: list[WeekdayKey],
    ) -> None:
        """Runs the app's action."""
        return self.action("edit-alarm", alarm=alarm, label=label, hour=hour, minute=minute, weekdays=weekdays)

    def edit_alarm_many(self, items: list[EditAlarmArgs]) -> list[dict[str, Any]]:
        """Runs `edit-alarm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-alarm", items)

    def remove_alarm(self, *, alarm: str) -> None:
        """Runs the app's action."""
        return self.action("remove-alarm", alarm=alarm)

    def remove_alarm_many(self, items: list[RemoveAlarmArgs]) -> list[dict[str, Any]]:
        """Runs `remove-alarm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-alarm", items)

    def add_timer(self,
        *,
        label: str,
        seconds: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-timer", label=label, seconds=seconds)

    def add_timer_many(self, items: list[AddTimerArgs]) -> list[dict[str, Any]]:
        """Runs `add-timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-timer", items)

    def pause_timer(self, *, timer: str) -> None:
        """Runs the app's action."""
        return self.action("pause-timer", timer=timer)

    def pause_timer_many(self, items: list[PauseTimerArgs]) -> list[dict[str, Any]]:
        """Runs `pause-timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pause-timer", items)

    def resume_timer(self, *, timer: str) -> None:
        """Runs the app's action."""
        return self.action("resume-timer", timer=timer)

    def resume_timer_many(self, items: list[ResumeTimerArgs]) -> list[dict[str, Any]]:
        """Runs `resume-timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resume-timer", items)

    def add_minute(self, *, timer: str) -> None:
        """Runs the app's action."""
        return self.action("add-minute", timer=timer)

    def add_minute_many(self, items: list[AddMinuteArgs]) -> list[dict[str, Any]]:
        """Runs `add-minute` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-minute", items)

    def remove_timer(self, *, timer: str) -> None:
        """Runs the app's action."""
        return self.action("remove-timer", timer=timer)

    def remove_timer_many(self, items: list[RemoveTimerArgs]) -> list[dict[str, Any]]:
        """Runs `remove-timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-timer", items)

    def start_stopwatch(self) -> None:
        """Runs the app's action."""
        return self.action("start-stopwatch")

    def start_stopwatch_many(self, items: list[StartStopwatchArgs]) -> list[dict[str, Any]]:
        """Runs `start-stopwatch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start-stopwatch", items)

    def pause_stopwatch(self) -> None:
        """Runs the app's action."""
        return self.action("pause-stopwatch")

    def pause_stopwatch_many(self, items: list[PauseStopwatchArgs]) -> list[dict[str, Any]]:
        """Runs `pause-stopwatch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pause-stopwatch", items)

    def lap(self) -> None:
        """Runs the app's action."""
        return self.action("lap")

    def lap_many(self, items: list[LapArgs]) -> list[dict[str, Any]]:
        """Runs `lap` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("lap", items)

    def reset_stopwatch(self) -> None:
        """Runs the app's action."""
        return self.action("reset-stopwatch")

    def reset_stopwatch_many(self, items: list[ResetStopwatchArgs]) -> list[dict[str, Any]]:
        """Runs `reset-stopwatch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset-stopwatch", items)

    def show(self, *, section: str) -> None:
        """Runs the app's action."""
        return self.action("show", section=section)

    def dismiss_notice(self) -> None:
        """Runs the app's action."""
        return self.action("dismiss-notice")

    def float_stopwatch(self) -> None:
        """Runs the app's action."""
        return self.action("float-stopwatch")

    def unfloat_stopwatch(self) -> None:
        """Runs the app's action."""
        return self.action("unfloat-stopwatch")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_world_clock", "add_world_clock_many", "remove_world_clock", "remove_world_clock_many", "add_alarm", "add_alarm_many", "set_alarm_enabled", "set_alarm_enabled_many", "edit_alarm", "edit_alarm_many", "remove_alarm", "remove_alarm_many", "add_timer", "add_timer_many", "pause_timer", "pause_timer_many", "resume_timer", "resume_timer_many", "add_minute", "add_minute_many", "remove_timer", "remove_timer_many", "start_stopwatch", "start_stopwatch_many", "pause_stopwatch", "pause_stopwatch_many", "lap", "lap_many", "reset_stopwatch", "reset_stopwatch_many", "show", "dismiss_notice", "float_stopwatch", "unfloat_stopwatch"})


