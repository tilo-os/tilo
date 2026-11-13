"""Types for Clocks (`so.tilo.clocks`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Collection` is the app's `clocks.collection` at `clocks/<resource>`, read with
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
    "Collection",
    "OnDevices",
]

class Collection(Resource):
    """Clocks — `clocks.collection`."""
    __surface__ = "so.tilo.clocks@d3069d6f03debbee"
    world_clocks: Any
    alarms: Any
    timers: Any
    @property
    def stopwatch(self) -> Any:
        """Read only."""
        return self.fields["stopwatch"]

    def add_world_clock(self,
        *,
        city: str,
        timezone: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-world-clock", city=city, timezone=timezone)

    def remove_world_clock(self, *, world_clock: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-world-clock", world_clock=world_clock)

    def add_alarm(self,
        *,
        label: str,
        hour: int,
        minute: int,
        weekdays: list[str],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-alarm", label=label, hour=hour, minute=minute, weekdays=weekdays)

    def set_alarm_enabled(self,
        *,
        alarm: str,
        enabled: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-alarm-enabled", alarm=alarm, enabled=enabled)

    def edit_alarm(self,
        *,
        alarm: str,
        label: str,
        hour: int,
        minute: int,
        weekdays: list[str],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("edit-alarm", alarm=alarm, label=label, hour=hour, minute=minute, weekdays=weekdays)

    def remove_alarm(self, *, alarm: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-alarm", alarm=alarm)

    def add_timer(self,
        *,
        label: str,
        seconds: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-timer", label=label, seconds=seconds)

    def pause_timer(self, *, timer: str) -> Run | None:
        """Runs the app's action."""
        return self.action("pause-timer", timer=timer)

    def resume_timer(self, *, timer: str) -> Run | None:
        """Runs the app's action."""
        return self.action("resume-timer", timer=timer)

    def add_minute(self, *, timer: str) -> Run | None:
        """Runs the app's action."""
        return self.action("add-minute", timer=timer)

    def remove_timer(self, *, timer: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-timer", timer=timer)

    def start_stopwatch(self) -> Run | None:
        """Runs the app's action."""
        return self.action("start-stopwatch")

    def pause_stopwatch(self) -> Run | None:
        """Runs the app's action."""
        return self.action("pause-stopwatch")

    def lap(self) -> Run | None:
        """Runs the app's action."""
        return self.action("lap")

    def reset_stopwatch(self) -> Run | None:
        """Runs the app's action."""
        return self.action("reset-stopwatch")


class OnDevices(Resource):
    """Clocks on each device — `clocks.on-devices`."""
    __surface__ = "so.tilo.clocks@d3069d6f03debbee"
    devices: Any

    def keep(self,
        *,
        device: str,
        kept: Any,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("keep", device=device, kept=kept)


