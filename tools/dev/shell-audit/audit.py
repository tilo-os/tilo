#!/usr/bin/env python3
"""Run the everyday-interaction catalogue against both desktops and pair the results.

  python3 tools/dev/shell-audit/audit.py                    # both sides, whole catalogue
  python3 tools/dev/shell-audit/audit.py --list             # what the catalogue holds
  python3 tools/dev/shell-audit/audit.py --only windows --side new
  python3 tools/dev/shell-audit/audit.py --old 2070cb898cf --new HEAD --out ~/shots/audit

Each side is started its own way (see `sides.py`), every scenario runs in order
on that side, and each capture lands in `<out>/pairs/<scenario>/<side>.png`
with its target list, timings and what the side answered in
`<out>/manifest-<side>.json`. `<out>/index.md` pairs the two sides: what each
one did, how long it took, how many elements it offered, and which door it
lacks. The audit judges nothing: the pictures are for a person to look at.

Captures and manifests stay out of git; point `--out` anywhere with room.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
import time
import traceback
from pathlib import Path

from PIL import Image, ImageChops

sys.path.insert(0, str(Path(__file__).resolve().parent))

import doors as doors_module  # noqa: E402
from sides import APPS, REPO, Failed, Side, open_side  # noqa: E402

HERE = Path(__file__).resolve().parent
CATALOGUE = HERE / "catalogue.json"
# The reference old shell: the tree just before the session replaced it. Any
# commit that still holds crates/session/shell works.
DEFAULT_OLD = os.environ.get("SHELL_AUDIT_OLD_REF", "5e0e173af2e^")
# A frame that changed by fewer pixels than this is a clock or a caret.
QUIET = 400


# --- pictures ----------------------------------------------------------------


def changed_pixels(a: Path | None, b: Path | None) -> int | None:
    if a is None or b is None or not (a.exists() and b.exists()):
        return None
    first = Image.open(a).convert("RGB")
    second = Image.open(b).convert("RGB")
    if first.size != second.size:
        return None
    red, green, blue = ImageChops.difference(first, second).split()
    busiest = ImageChops.lighter(ImageChops.lighter(red, green), blue)
    return sum(count for value, count in enumerate(busiest.histogram()) if value > 6)


def diff_bbox(a: Path | None, b: Path | None) -> list[int] | None:
    if a is None or b is None or not (a.exists() and b.exists()):
        return None
    first = Image.open(a).convert("RGB")
    second = Image.open(b).convert("RGB")
    if first.size != second.size:
        return None
    box = ImageChops.difference(first, second).getbbox()
    return list(box) if box else None


# --- the catalogue -----------------------------------------------------------


class Catalogue:
    def __init__(self, path: Path = CATALOGUE):
        self.doc = json.loads(path.read_text())
        self.areas: list[dict] = self.doc["areas"]
        self.scenarios: dict[str, dict] = {
            scenario["id"]: {"area": area["id"], **scenario}
            for area in self.areas for scenario in area["scenarios"]
        }

    def order(self) -> list[tuple[dict, dict]]:
        return [(area, scenario) for area in self.areas for scenario in area["scenarios"]]

    def select(self, only: str) -> list[tuple[dict, dict]]:
        if not only:
            return self.order()
        wanted = [part.strip() for part in only.split(",") if part.strip()]
        chosen = []
        for area, scenario in self.order():
            if area["id"] in wanted or scenario["id"] in wanted:
                chosen.append((area, scenario))
        unknown = set(wanted) - {area["id"] for area, _ in chosen} - \
            {scenario["id"] for _, scenario in chosen}
        if unknown:
            raise SystemExit(f"the catalogue has no {', '.join(sorted(unknown))}")
        return chosen

    def apps(self) -> str:
        """Every app the catalogue launches, for the session that installs apps."""
        found = []
        for _, scenario in self.order():
            for step in scenario["steps"]:
                app = step.get("app") if "door" in step else step.get("launch")
                if app == "files":
                    app = "file-explorer"
                if app and app not in found:
                    found.append(app)
        return " ".join(found) or APPS


# --- one side's run ----------------------------------------------------------


class Run:
    def __init__(self, side: Side, catalogue: Catalogue, out: Path, *, theme: str | None,
                 keep: bool = False):
        self.side = side
        self.doors = doors_module.Doors(side)
        self.catalogue = catalogue
        self.out = out
        self.raw = out / "raw" / side.name
        self.pairs = out / "pairs"
        self.theme = theme
        self.keep = keep
        self.rows: list[dict] = []
        self.problems: list[dict] = []
        self.idle: Path | None = None
        self.started = ""

    # -- steps --

    def perform(self, step: dict):
        if "door" in step:
            args = {key: value for key, value in step.items() if key != "door"}
            return self.doors.open(step["door"], **args)
        if "click" in step:
            return self.doors.press(step["click"])
        if "click_at" in step:
            x, y = step["click_at"]
            self.side.move(x, y)
            return self.side.click(x, y)
        if "right_click" in step:
            row = self.doors.target(step["right_click"])
            rect = row["rect"]
            self.side.right_click(rect[0] + rect[2] / 2, rect[1] + rect[3] / 2)
            return row
        if "right_click_at" in step:
            x, y = step["right_click_at"]
            return self.side.right_click(x, y)
        if "hover" in step:
            row = self.doors.target(step["hover"])
            rect = row["rect"]
            self.side.move(rect[0] + rect[2] / 2, rect[1] + rect[3] / 2)
            return row
        if "hover_at" in step:
            return self.side.move(*step["hover_at"])
        if "move" in step:
            return self.side.move(*step["move"])
        if "double_click" in step:
            row = self.doors.target(step["double_click"])
            rect = row["rect"]
            x, y = self.centre(rect)
            self.side.move(x, y)
            self.side.click(x, y)
            time.sleep(0.08)
            self.side.click(x, y)
            return row
        if "double_click_at" in step:
            x, y = step["double_click_at"]
            self.side.move(x, y)
            self.side.click(x, y)
            time.sleep(0.08)
            return self.side.click(x, y)
        if "middle_click" in step:
            row = self.doors.target(step["middle_click"])
            x, y = self.centre(row["rect"])
            self.side.move(x, y)
            time.sleep(0.12)
            self.side.button("middle", True)
            return self.side.button("middle", False)
        if "drag_tab_to" in step:
            row = self.doors.target(step["drag_tab_to"])
            x_from, y_from = self.doors.tab_point()
            x_to, y_to = self.centre(row["rect"])
            return self.side.drag(x_from, y_from, x_to, y_to, 14)
        if "drag_tab" in step:
            x_from, y_from = self.doors.tab_point()
            dx, dy = step["drag_tab"]
            return self.side.drag(x_from, y_from, x_from + dx, y_from + dy, 14)
        if "window_body" in step:
            return self.doors.click_window_body()
        if "key" in step:
            return self.side.key(step["key"])
        if "client_key" in step:
            return self.side.client_key(step["client_key"])
        if "chord" in step:
            self.chord(step["chord"])
            return step["chord"]
        if "type" in step:
            return self.side.type_text(step["type"])
        if "client_type" in step:
            self.side.client_type(step["client_type"])
            return step["client_type"]
        if "wait" in step:
            time.sleep(float(step["wait"]))
            return "waited"
        if "drag" in step:
            return self.side.drag(*step["drag"])
        if "drag_between" in step:
            return self.drag_between(step["drag_between"])
        if "scroll" in step:
            return self.side.scroll(*step["scroll"])
        if "scroll_at" in step:
            x, y, dx, dy = step["scroll_at"]
            self.side.move(x, y)
            return self.side.scroll(dx, dy)
        if "launch" in step:
            started = time.monotonic()
            window = self.side.launch_app(step["launch"])
            return {"window": window, "ms": round((time.monotonic() - started) * 1000, 1)}
        if "notify" in step:
            options = step["notify"]
            return self.side.notify(options.get("summary", "A note"), options.get("body", ""),
                                    options.get("app", "Notes"))
        if "snapshot" in step:
            return str(self.capture(f"{step['snapshot']}-{self.side.name}"))
        raise SystemExit(f"unknown step {json.dumps(step)[:200]}")

    def chord(self, text: str) -> None:
        parts = [part for part in text.replace("+", " ").split() if part]
        key = parts[-1]
        held = {part.lower() for part in parts[:-1]}
        self.side.modifiers(ctrl="ctrl" in held or "control" in held,
                            shift="shift" in held, alt="alt" in held,
                            logo=bool({"super", "logo", "meta"} & held))
        try:
            self.side.key(key)
        finally:
            self.side.modifiers()

    def drag_between(self, spec: dict) -> dict:
        start = spec["from"]
        end = spec["to"]
        from_row = self.doors.target(start) if isinstance(start, str) else None
        to_row = self.doors.target(end) if isinstance(end, str) else None
        from_x, from_y = self.centre(from_row["rect"]) if from_row else start
        to_x, to_y = self.centre(to_row["rect"]) if to_row else end
        return self.side.drag(from_x, from_y, to_x, to_y, int(spec.get("steps", 14)))

    @staticmethod
    def centre(rect):
        return rect[0] + rect[2] / 2, rect[1] + rect[3] / 2

    # -- capture --

    def capture(self, name: str, settle: float = 0.4, park: bool = True) -> Path:
        # Parking the pointer keeps it out of the picture; a story about a hover
        # keeps it where the person left it.
        if park:
            self.doors.inert()
        time.sleep(settle)
        return self.side.snapshot(self.raw / f"{name}.png")

    def size(self) -> tuple[int, int]:
        return self.side.size

    # -- one scenario --

    def scenario(self, area: dict, scenario: dict) -> dict:
        name = scenario["id"]
        row = {"id": name, "area": area["id"], "story": scenario.get("story", ""),
               "capture": scenario.get("capture", ""), "measure": scenario.get("measure", []),
               "outcome": "captured", "door_missing": None, "error": None,
               "steps": [], "started_s": round(time.monotonic() - self.side.started_at, 2)}
        before = self.capture(f"_{name}-before", settle=0.2)
        started = time.monotonic()
        print(f"  [{name}] {scenario.get('story', '')}"[:150], flush=True)
        for index, step in enumerate(scenario["steps"]):
            step_started = time.monotonic()
            label = json.dumps(step)[:120]
            try:
                result = self.perform(step)
                elapsed = round((time.monotonic() - step_started) * 1000, 1)
                row["steps"].append({"step": label, "ms": elapsed,
                                     "result": str(result)[:240]})
            except doors_module.DoorMissing as error:
                row["steps"].append({"step": label,
                                     "ms": round((time.monotonic() - step_started) * 1000, 1),
                                     "door_missing": str(error)[:400]})
                row["outcome"] = "door_missing"
                row["door_missing"] = f"{label}: {str(error)[:400]}"
                print(f"    door missing: {str(error)[:160]}", flush=True)
                break
            except Exception as error:
                row["steps"].append({"step": label,
                                     "ms": round((time.monotonic() - step_started) * 1000, 1),
                                     "error": str(error)[:400]})
                row["outcome"] = "failed"
                row["error"] = f"{label}: {str(error)[:400]}"
                print(f"    failed: {str(error)[:160]}", flush=True)
                break
            if "snapshot" in step:
                row.setdefault("mid", {})[step["snapshot"]] = str(
                    self.raw / f"{step['snapshot']}-{self.side.name}.png")
        row["total_ms"] = round((time.monotonic() - started) * 1000, 1)

        # The motion frames, when the scenario asks for them: 16 ms apart as
        # asked, with the send times recorded because a snapshot round trip
        # costs more than 16 ms on both sides.
        motion = scenario.get("motion")
        if motion:
            stamps = []
            start = time.monotonic()
            for index in range(int(motion.get("frames", 8))):
                time.sleep(float(motion.get("interval", 0.016)))
                sent = time.monotonic()
                try:
                    self.side.snapshot(self.raw / f"{name}-motion-{index}.png")
                except Failed as error:
                    self.problems.append({"state": name, "error": str(error)[:400]})
                    break
                stamps.append(round((sent - start) * 1000, 1))
            row["motion_ms"] = stamps
            row["motion"] = str(self.raw / f"{name}-motion-*.png")

        try:
            shot = self.capture(name, settle=float(scenario.get("settle", 0.5)),
                                park=scenario.get("pointer", "park") != "keep")
        except Exception as error:
            row["outcome"] = "no_frame" if row["outcome"] == "captured" else row["outcome"]
            row["error"] = row.get("error") or f"no frame: {str(error)[:400]}"
            print(f"    no frame: {str(error)[:160]}", flush=True)
            shot = None
        if shot is not None:
            pair = self.pairs / name
            pair.mkdir(parents=True, exist_ok=True)
            target = pair / f"{self.side.name}.png"
            shutil.copy2(shot, target)
            row["png"] = str(target)
            row["changed_px"] = changed_pixels(before, shot)
            row["changed_bbox"] = diff_bbox(before, shot)
            row["targets"] = {entry.get("name"): [round(value, 1) for value in entry["rect"]]
                              for entry in self.side.targets()}
            row["target_count"] = len(row["targets"])
            row["windows"] = len(self.side.windows())
            row["overlays"] = self.side.overlays()

        # Back to a clean desk for the next story, and what was left behind.
        if scenario.get("reset", "clean") != "keep":
            row["cleared"] = self.clean(close_windows=scenario.get("desk", "empty") != "keep")
        try:
            residue = self.capture(f"_{name}-after", settle=0.7)
            row["residual_px"] = changed_pixels(self.idle, residue)
            row["residual_bbox"] = diff_bbox(self.idle, residue)
        except Exception as error:
            row["residual_px"] = None
            row["residual_bbox"] = None
            self.problems.append({"state": name, "error": f"no after frame: {error}"})
        return row

    def clean(self, tries: int = 4, *, close_windows: bool = True) -> str:
        closed = []
        for _ in range(tries):
            try:
                showing = [key for key, value in self.side.overlays().items() if value]
            except Failed:
                showing = []
            if not showing:
                break
            try:
                self.doors.escape()
            except Failed as error:
                closed.append(str(error)[:120])
            closed += showing
            time.sleep(0.25)
        if close_windows:
            try:
                closed.append(self.doors.window_close_all())
            except Failed as error:
                closed.append(f"windows left: {str(error)[:120]}")
        self.doors.inert()
        time.sleep(0.2)
        return ", ".join(entry for entry in closed if entry)

    # -- one area --

    def area(self, area: dict) -> None:
        if area.get("restart") and self.rows:
            print(f"[area {area['id']}] restarting the session", flush=True)
            try:
                self.side.restart(fresh=True)
            except Exception as error:
                self.problems.append({"state": f"restart-{area['id']}", "error": str(error)})
                raise
        print(f"[area {area['id']}] {area.get('about', '')}"[:160], flush=True)
        try:
            cleared = self.doors.window_close_all()
            print(f"    desk cleared: {cleared}", flush=True)
        except Failed as error:
            self.problems.append({"state": f"clear-{area['id']}", "error": str(error)[:300]})
        try:
            self.idle = self.capture("idle-desk", settle=1.0)
            self.rows.append({"state": f"idle-desk-after:{area['id']}",
                              "targets": {entry.get("name"): [round(v, 1) for v in entry["rect"]]
                                          for entry in self.side.targets()},
                              "overlays": self.side.overlays()})
        except Exception as error:
            self.problems.append({"state": f"idle-desk-after:{area['id']}", "error": str(error)})

    # -- the whole run --

    def execute(self, chosen) -> dict:
        started = time.monotonic()
        print(f"[{self.side.name}] starting", flush=True)
        start_info = self.side.start(fresh=True)
        print(f"[{self.side.name}] started in {start_info.get('seconds')}s: "
              f"{json.dumps(start_info)[:300]}", flush=True)
        current = None
        try:
            for area, scenario in chosen:
                if area["id"] != current:
                    self.area(area)
                    current = area["id"]
                row = self.scenario(area, scenario)
                self.rows.append(row)
        finally:
            if not self.keep:
                self.side.stop()
        return {"side": self.side.name, "ref": self.side.ref, "commit": self.side.sha,
                "start": start_info, "apps": self.side.apps, "theme": self.theme,
                "seconds": round(time.monotonic() - started, 1),
                "rows": self.rows, "problems": self.problems}


# --- pairing -----------------------------------------------------------------


def pair(out: Path, catalogue: Catalogue) -> Path:
    manifests = {}
    for name in ("old", "new"):
        path = out / f"manifest-{name}.json"
        if path.exists():
            manifests[name] = json.loads(path.read_text())
    lines = ["# The everyday interactions, old shell and new session", ""]
    for name, manifest in manifests.items():
        lines.append(f"- {name}: `{manifest.get('ref')}` at `{str(manifest.get('commit'))[:12]}`, "
                     f"started in {manifest.get('start', {}).get('seconds')}s, "
                     f"{sum(1 for row in manifest['rows'] if 'outcome' in row)} scenarios")
        if manifest.get("problems"):
            lines.append(f"  - {len(manifest['problems'])} run problem(s), at the end")
    lines += ["", "A door one side lacks is in the table as `door missing` with what that",
              "side answered instead. Timings are wall milliseconds for the whole scenario,",
              "including the capture; `px` is how much of the desk the scenario changed.",
              "", "| scenario | area | old | new | pairs |", "| --- | --- | --- | --- | --- |"]

    def describe(manifest, scenario_id):
        if not manifest:
            return "not run"
        for row in manifest["rows"]:
            if row.get("id") != scenario_id:
                continue
            if row["outcome"] != "captured":
                detail = row.get("door_missing") or row.get("error") or row["outcome"]
                return f"{row['outcome']}: {detail[:150]}"
            return (f"{row['total_ms']} ms, {row.get('changed_px')} px, "
                    f"{row.get('target_count')} elements, {row.get('windows')} windows, "
                    f"residue {row.get('residual_px')}")
        return "not run"

    for area, scenario in catalogue.order():
        old = describe(manifests.get("old"), scenario["id"])
        new = describe(manifests.get("new"), scenario["id"])
        pair_dir = out / "pairs" / scenario["id"]
        files = ", ".join(f"[{path.name}]({path})" for path in sorted(pair_dir.glob("*.png"))) \
            if pair_dir.exists() else "-"
        lines.append(f"| {scenario['id']} | {area['id']} | {old} | {new} | {files} |")

    # Door parity: for every door the catalogue asks for, which sides have it.
    lines += ["", "## Doors each side has", "",
              "| door | old | new |", "| --- | --- | --- |"]
    asked: dict[str, set[str]] = {}
    for name, manifest in manifests.items():
        for row in manifest["rows"]:
            if "outcome" not in row:
                continue
            for step in row["steps"]:
                door = None
                label = step["step"]
                if label.startswith('{"door": "'):
                    door = label.split('"')[3]
                if door:
                    asked.setdefault(door, set())
                    if "door_missing" in step:
                        asked[door].add(name)
    for door in sorted(asked):
        missing = asked[door]
        old = "missing" if "old" in missing else ("have" if "old" in manifests else "-")
        new = "missing" if "new" in missing else ("have" if "new" in manifests else "-")
        lines.append(f"| {door} | {old} | {new} |")

    # What could not be captured, per side.
    lines += ["", "## What a side could not do", ""]
    for name, manifest in manifests.items():
        lines.append(f"### {name}")
        lines.append("")
        anything = False
        for row in manifest["rows"]:
            if "outcome" not in row or row["outcome"] == "captured":
                anything = True
                continue
            lines.append(f"- {row['id']}: {row['outcome']} — "
                         f"{(row.get('door_missing') or row.get('error') or '')[:300]}")
        if not anything:
            lines.append("- nothing: every scenario ran")
        for problem in manifest.get("problems", [])[:20]:
            lines.append(f"- run: {problem.get('state')}: {str(problem.get('error'))[:200]}")
        lines.append("")

    path = out / "index.md"
    path.write_text("\n".join(lines) + "\n")
    return path


# --- the catalogue, without starting anything --------------------------------

VERBS = {"door", "click", "click_at", "right_click", "right_click_at", "hover", "hover_at",
         "move", "double_click", "double_click_at", "middle_click", "drag_tab_to", "drag_tab",
         "window_body", "key", "client_key", "chord", "type", "client_type", "wait", "drag",
         "drag_between", "scroll", "scroll_at", "launch", "notify", "snapshot"}


def check(catalogue: "Catalogue") -> list[str]:
    """Everything a run would only discover after a start: unknown steps and doors."""
    class Stand:
        name = "new"
        size = (1280, 800)

    known = doors_module.Doors(Stand()).table
    problems: list[str] = []
    seen: set[str] = set()
    for area, scenario in catalogue.order():
        if scenario["id"] in seen:
            problems.append(f"{scenario['id']}: the id is used twice")
        seen.add(scenario["id"])
        for step in scenario["steps"]:
            verbs = set(step) & VERBS
            if not verbs:
                problems.append(f"{scenario['id']}: no step verb in {json.dumps(step)[:120]}")
            elif len(verbs) > 1:
                problems.append(f"{scenario['id']}: several step verbs in {json.dumps(step)[:120]}")
            if "door" in step and step["door"] not in known:
                problems.append(f"{scenario['id']}: no door `{step['door']}`")
    return problems


# --- command line ------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run the everyday-interaction catalogue against the old shell and "
                    "the new session, and pair what each did.",
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--old", default=DEFAULT_OLD,
                        help=f"git ref of a tree that still has crates/session/shell "
                             f"(default: {DEFAULT_OLD})")
    parser.add_argument("--new", default="HEAD", help="git ref of the session tree (default: HEAD)")
    parser.add_argument("--old-bin", type=Path,
                        help="the old side's shell and app binaries, instead of building --old")
    parser.add_argument("--new-build", type=Path,
                        help="a directory of nested-session-build.sh output, instead of "
                             "building --new")
    parser.add_argument("--old-apps", type=Path,
                        help="a directory of .tapp packages the old shell installs")
    parser.add_argument("--apps", help="apps the new session installs (default: the "
                                      "catalogue's own)")
    parser.add_argument("--side", choices=("both", "old", "new"), default="both")
    parser.add_argument("--only", default="", help="areas or scenario ids, comma separated")
    parser.add_argument("--theme", choices=("dark", "light"), default="dark")
    parser.add_argument("--size", default="1280x800", help="display size, WxH")
    parser.add_argument("--out", type=Path,
                        default=REPO / "target" / "shell-audit" / "run",
                        help="where captures, manifests and the index go")
    parser.add_argument("--keep", action="store_true",
                        help="leave the sessions running and the profiles in place")
    parser.add_argument("--index-only", action="store_true",
                        help="rebuild index.md from the manifests already in --out")
    parser.add_argument("--list", action="store_true", help="print the catalogue")
    parser.add_argument("--validate", action="store_true",
                        help="check the catalogue's steps and doors without starting a side")
    options = parser.parse_args()

    catalogue = Catalogue()
    if options.validate:
        problems = check(catalogue)
        for problem in problems:
            print(problem)
        print(f"{len(catalogue.scenarios)} scenarios in {len(catalogue.areas)} areas, "
              f"{len(problems)} problem(s)")
        return 1 if problems else 0
    if options.list:
        for area in catalogue.areas:
            print(f"{area['id']}: {area.get('about', '')}")
            for scenario in area["scenarios"]:
                print(f"  {scenario['id']}: {scenario.get('story', '')}")
        print(f"{len(catalogue.scenarios)} scenarios in {len(catalogue.areas)} areas")
        return 0

    out = options.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    if options.index_only:
        print(pair(out, catalogue))
        return 0

    width, height = (int(part) for part in options.size.lower().split("x"))
    chosen = catalogue.select(options.only)
    apps = options.apps or catalogue.apps()
    root = out / "profiles"
    results = {}
    for name in (("old", "new") if options.side == "both" else (options.side,)):
        side = open_side(name, options.old if name == "old" else options.new,
                         root, size=(width, height), theme=options.theme, apps=apps,
                         bins=options.old_bin if name == "old" else None,
                         build=options.new_build if name == "new" else None,
                         app_collection=options.old_apps if name == "old" else None,
                         keep=options.keep)
        run = Run(side, catalogue, out, theme=options.theme, keep=options.keep)
        try:
            results[name] = run.execute(chosen)
        except Exception as error:
            traceback.print_exc()
            results[name] = {"side": name, "ref": side.ref, "commit": side.sha,
                             "start": {}, "rows": run.rows,
                             "problems": [{"state": "run", "error": str(error)[:1000]}]}
        (out / f"manifest-{name}.json").write_text(json.dumps(results[name], indent=1))
    print(pair(out, catalogue))
    return 0


if __name__ == "__main__":
    sys.exit(main())
