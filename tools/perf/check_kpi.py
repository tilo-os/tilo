#!/usr/bin/env python3
"""Check recorded session measurements against the performance budgets.

The measurements come from the harnesses that run on a device; the budgets
here are what a person is owed on that class of device:

- `session_perf` (`crates/session/desktop/examples/session_perf.rs`) — build,
  first frame, a steady frame, a typing frame, idle frames that did work;
- `startup_marks` (`tools/perf/startup_marks.py`) — the native session's own
  startup marks, from process start to the first presented frame;
- `web_startup` (`tools/perf/web_startup.mjs`) — a page load in a browser, from
  navigation to the first presented desktop frame.

Usage:
    check_kpi.py check MEASUREMENT.json [MEASUREMENT.json ...]
    check_kpi.py budgets

A measurement names its device class (`device`) and its bundle; see
`tools/perf/kpi-budgets.toml` for the classes. Exit status is 1 when any
measurement exceeds its budget, so a test can call it directly.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
import tomllib

REPO = pathlib.Path(__file__).resolve().parents[2]
BUDGETS = REPO / "tools" / "perf" / "kpi-budgets.toml"

#: A measurement kind names one harness; each budget table lists limits for the
#: fields of that kind's JSON.
KINDS = ("session", "startup_marks", "web_startup")

#: The tile profiles a session measurement names: the native session draws
#: chrome over its windows, the page draws chrome and its apps.
PROFILES = ("arriving", "apps")


class Budgets:
    def __init__(self, text: str) -> None:
        data = tomllib.loads(text)
        self.devices: dict[str, dict[str, dict[str, float]]] = data.get("devices", {})
        if not self.devices:
            raise ValueError("no [devices.<class>] tables in the budgets")

    def limits(self, device: str, kind: str, profile: str | None = None) -> dict[str, float]:
        # A kind a host fills differently (a session's frames are chrome where
        # windows are the clients' own buffers, and chrome plus app paint in
        # the page) keeps one budget table per profile.
        try:
            table = self.devices[device]
        except KeyError:
            raise ValueError(
                f"no budgets for device {device!r}; known: {', '.join(sorted(self.devices))}"
            ) from None
        try:
            limits = table[kind]
        except KeyError:
            raise ValueError(f"no [{device}.{kind}] budgets in {BUDGETS}") from None
        if profile is not None:
            try:
                limits = limits[profile]
            except KeyError:
                raise ValueError(
                    f"no [{device}.{kind}.{profile}] budgets in {BUDGETS}"
                ) from None
        if isinstance(limits, dict) and any(name in limits for name in PROFILES):
            raise ValueError(
                f"[{device}.{kind}] keeps one budget per profile "
                f"({', '.join(PROFILES)}); the measurement names none"
            )
        return flatten(limits)


def flatten(table: dict, prefix: str = "") -> dict[str, float]:
    """`{steady: {p95_us: 9000}}` -> `{"steady.p95_us": 9000}`: a dotted budget
    key is how TOML writes a value nested in the measurement."""
    flat: dict[str, float] = {}
    for key, value in table.items():
        name = f"{prefix}{key}"
        if isinstance(value, dict):
            flat.update(flatten(value, f"{name}."))
        else:
            flat[name] = value
    return flat


def comparables(value: object) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def check(measurement: dict, budgets: Budgets) -> tuple[list[str], list[str]]:
    """`(failures, notes)`; empty failures means every budget held."""
    device = measurement.get("device")
    kind = measurement.get("kind")
    if not isinstance(device, str) or not device:
        return ["measurement names no device class"], []
    if kind not in KINDS:
        return [f"unknown measurement kind {kind!r}"], []
    profile = measurement.get("profile")
    if profile is not None and not isinstance(profile, str):
        return [f"measurement names a profile that is not a string ({profile!r})"], []
    try:
        limits = budgets.limits(device, kind, profile)
    except ValueError as error:
        return [str(error)], []
    failures: list[str] = []
    notes: list[str] = []
    for field, limit in sorted(limits.items()):
        # A dotted field reads one value out of a nested measurement object.
        value: object = measurement
        for part in field.split("."):
            if not isinstance(value, dict) or part not in value:
                failures.append(f"{device}: {field} missing from the {kind} measurement")
                value = None
                break
            value = value[part]
        if value is None:
            continue
        if not comparables(value):
            failures.append(f"{device}: {field} is not a number ({value!r})")
            continue
        if value > limit:
            failures.append(f"{device}: {field} is {value:.2f}, over its budget of {limit:.2f}")
        elif limit and value > 0.9 * limit:
            notes.append(f"{device}: {field} at {value:.2f} of {limit:.2f} (within 10 %)")
    return failures, notes


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    check_parser = commands.add_parser("check", help="fail when a measurement is over budget")
    check_parser.add_argument("measurements", nargs="+", type=pathlib.Path)
    commands.add_parser("budgets", help="print the budget tables")
    args = parser.parse_args()

    text = BUDGETS.read_text()
    if args.command == "budgets":
        sys.stdout.write(text)
        return 0

    budgets = Budgets(text)
    failed = False
    for path in args.measurements:
        try:
            measurement = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError) as error:
            print(f"{path}: {error}", file=sys.stderr)
            failed = True
            continue
        failures, notes = check(measurement, budgets)
        for note in notes:
            print(f"note: {path.name}: {note}")
        for failure in failures:
            print(f"{path.name}: {failure}", file=sys.stderr)
        failed = failed or bool(failures)
        if not failures:
            print(f"{path.name}: within budget for {measurement['device']}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
