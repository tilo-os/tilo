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
import inspect
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
# A surface that answers an action before it draws its first frame needs longer
# than a settle: the quick settings panel in a dev session paints seconds later,
# and later still on a loaded machine. These are the seconds to keep looking,
# and the frame that drew the most is the one kept.
LATE = 20
# A desk that still differs from the empty one by more than this has something
# left on it: a sheet, a popover, a panel the next story must not start under.
LEFTOVER = 12000
# Doors that open an app of their own rather than a surface of the desk: a run
# installs what its stories open, and these open an app without naming it.
DOOR_APPS = {"desk.settings": "settings", "desk.settings-pane": "settings",
             "desk.files-shared": "files"}


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


# A menu is a panel that fits beside the window it is for. One as wide as the
# strip holding paragraph-long hints at the rows' ends is not a menu a person
# reads as one, whatever its overlay says.
MENU_WIDTH_MAX = 460
MENU_HEIGHT_MAX = 760
# A menu offers this many rows at least: a single line that came up is a hint,
# a toast or the window's own text, not the list of what can be done.
MENU_ROWS_MIN = 4


def menu_frame(before: Path, shot: Path, pressed, rows_min: int = MENU_ROWS_MIN) -> dict:
    """What a menu story's frame shows, measured on the pixels: the panel that
    came up, the rows of text standing in it, and whether a person reading the
    screen would find a menu there, at the point they pressed.

    The rows are the bright text bands the frame did not have before, and a
    menu's rows start at one inset: their left edges line up in a column."""
    found: dict = {"ok": False, "why": None}
    if before is None or shot is None or not (Path(before).exists() and Path(shot).exists()):
        found["why"] = "no frame to read"
        return found
    first = Image.open(before).convert("RGB")
    second = Image.open(shot).convert("RGB")
    if first.size != second.size:
        found["why"] = "the frames are not the same size"
        return found
    difference = ImageChops.difference(first, second).convert("L")
    box = difference.getbbox()
    if not box:
        found["why"] = "nothing drew where the story pressed"
        return found
    found["box"] = list(box)
    found["width"], found["height"] = box[2] - box[0], box[3] - box[1]
    mask = difference.load()
    frame = second.load()

    def lit(x: int, y: int) -> bool:
        red, green, blue = frame[x, y]
        return mask[x, y] > 6 and 0.299 * red + 0.587 * green + 0.114 * blue > 90

    bands: list[list[int]] = []
    for y in range(box[1], box[3]):
        xs = [x for x in range(box[0], box[2]) if lit(x, y)]
        # A window edge lighting one column of every row is not a row of text:
        # a line of words has several lit pixels across it.
        if len(xs) < 6:
            continue
        if bands and y - bands[-1][1] <= 2:
            bands[-1][1] = y
            bands[-1][2] = min(bands[-1][2], xs[0])
            bands[-1][3] = max(bands[-1][3], xs[-1])
        else:
            bands.append([y, y, xs[0], xs[-1]])
    found["rows"] = len(bands)
    if len(bands) >= rows_min:
        lefts = [band[2] for band in bands]
        common = max(set(lefts), key=lefts.count)
        aligned = sum(1 for left in lefts if abs(left - common) <= 3)
        found["label_x"] = common
        found["aligned"] = aligned
    if pressed is not None:
        x, y = pressed
        inside = box[0] - 8 <= x <= box[2] + 8 and box[1] - 8 <= y <= box[3] + 8
        found["at_pointer"] = inside
        if not inside:
            found["why"] = (f"the panel ({box}) does not stand at the press "
                            f"({x:.0f}, {y:.0f})")
            return found
    if len(bands) < rows_min:
        found["why"] = f"{len(bands)} row(s) of text came up where a menu's list belongs"
        return found
    if found["aligned"] * 2 < len(bands):
        found["why"] = (f"the {len(bands)} rows do not line up: their left edges "
                        f"spread from {min(lefts)} to {max(lefts)}")
        return found
    if found["width"] > MENU_WIDTH_MAX:
        found["why"] = (f"the panel is {found['width']} px wide, as wide as the strip: "
                        f"a menu stands beside its window, not across it")
        return found
    if found["height"] > MENU_HEIGHT_MAX:
        found["why"] = f"the panel is {found['height']} px tall, taller than a menu goes"
        return found
    found["ok"] = True
    return found


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

    def apps(self, chosen=None) -> str:
        """Every app the run launches, for the session that installs apps."""
        found = []
        for _, scenario in (self.order() if chosen is None else chosen):
            for step in scenario["steps"]:
                app = step.get("app") or step.get("launch") \
                    or DOOR_APPS.get(step.get("door"))
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

    @staticmethod
    def centre(rect):
        return rect[0] + rect[2] / 2, rect[1] + rect[3] / 2

    # -- capture --

    def capture(self, name: str, settle: float = 0.4, park: bool = True) -> Path:
        # Parking the pointer keeps it out of the picture; a story about a hover
        # keeps it where the person left it.
        if park:
            try:
                self.doors.inert()
            except Failed:
                pass
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
        # A side can stop answering frames — a locked session does — and the
        # story still runs: what it answers is the finding, not a reason to end
        # the whole audit.
        try:
            before = self.capture(f"_{name}-before", settle=0.2)
        except Failed as error:
            before = None
            row["frames"] = f"no frame before the story: {str(error)[:300]}"
        started = time.monotonic()
        self.doors.pressed = None
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
            if before is not None:
                row["changed_px"] = changed_pixels(before, shot)
                row["changed_bbox"] = diff_bbox(before, shot)
            row["targets"] = {entry.get("name"): [round(value, 1) for value in entry["rect"]]
                              for entry in self.side.targets()}
            row["target_count"] = len(row["targets"])
            row["windows"] = len(self.side.windows())
            row["overlays"] = self.side.overlays()
            # An action that answered without the desk changing may have been
            # answered before its surface drew: ask once more, later, so the
            # pair tells "slow to draw" apart from "nothing drew". An overlay
            # the desk starts as a program of its own (a window's menu, quick
            # settings) paints its first frame seconds after the door opened,
            # whatever the story waited: its story says so, and the frame that
            # drew is the one the story is judged on.
            showing = [key for key, value in row["overlays"].items() if value]
            answered_early = (row.get("changed_px") or 0) < QUIET
            if showing and (answered_early or scenario.get("drawn_late")):
                # The surface answered before it drew. Look for the whole window
                # and keep the frame that drew the most: the first seconds can
                # hold a press highlight or a hover, and stopping at the first
                # change would judge that instead of the surface.
                late = pair / f"{self.side.name}-late.png"
                late_px, late_s = 0, 0
                for waited in range(1, LATE + 1):
                    time.sleep(1.0)
                    frame = self.capture(f"_{name}-late", settle=0.2)
                    drawn = changed_pixels(before, frame)
                    if drawn > late_px:
                        late_px, late_s = drawn, waited
                        shutil.copy2(frame, late)
                    if late_px > (row.get("changed_px") or 0) + QUIET and drawn <= late_px:
                        # Something drew and the frame now stands still: this
                        # is the picture the person is left looking at.
                        break
                row["late_px"] = late_px
                row["late_s"] = late_s
                if late_px > (row.get("changed_px") or 0) + QUIET:
                    # What the story drew is this frame, not the empty desk the
                    # action answered on: it is the picture the pair is judged
                    # on, and the early one is kept beside it.
                    early = pair / f"{self.side.name}-early.png"
                    shutil.move(str(target), early)
                    shutil.copy2(late, target)
                    late.unlink()
                    row["early_png"] = str(early)
                    row["changed_px_early"] = row.get("changed_px")
                    row["changed_px"] = late_px
                    row["changed_bbox"] = diff_bbox(before, target)
                else:
                    if late.exists():
                        row["late_png"] = str(late)
                drew = row["changed_px_early"] if "changed_px_early" in row \
                    else row.get("changed_px")
                print(f"    nothing drew yet ({drew} px); "
                      f"{late_px} px after {late_s} s", flush=True)

            # What the story says a person must see is read off the frame, not
            # off the manifest: an overlay listed as showing is not a menu on
            # the screen until its rows stand in the picture.
            if scenario.get("see") == "menu":
                desk = row.get("mid", {}).get("menu-before")
                seen = menu_frame(Path(desk) if desk else before, Path(row["png"]),
                                  self.doors.pressed,
                                  int(scenario.get("menu_rows", MENU_ROWS_MIN)))
                row["menu"] = seen
                if seen["ok"]:
                    print(f"    menu: {seen['rows']} rows at x {seen.get('label_x')}, "
                          f"{seen['width']}x{seen['height']} at the press", flush=True)
                else:
                    print(f"    no menu a person sees: {seen['why']}", flush=True)
                    if row["outcome"] == "captured":
                        row["outcome"] = "no_menu"

        # Back to a clean desk for the next story, and what was left behind.
        keeps_windows = scenario.get("desk", "empty") == "keep"
        cleans = scenario.get("reset", "clean") != "keep"
        if cleans:
            row["cleared"] = self.clean(close_windows=not keeps_windows)
        try:
            residue = self.capture(f"_{name}-after", settle=0.7)
            row["residual_px"] = changed_pixels(self.idle, residue)
            row["residual_bbox"] = diff_bbox(self.idle, residue)
            # Something still on the desk would be the ground of the next
            # story: dismiss again, and say so when it stays.
            if cleans and not keeps_windows and (row["residual_px"] or 0) > LEFTOVER:
                print(f"    the desk was not clean ({row['residual_px']} px left); "
                      f"dismissing again", flush=True)
                row["cleared_again"] = self.clean()
                residue = self.capture(f"_{name}-after-again", settle=0.7)
                row["residual_px"] = changed_pixels(self.idle, residue)
                row["residual_bbox"] = diff_bbox(self.idle, residue)
                if (row["residual_px"] or 0) > LEFTOVER:
                    print(f"    still {row['residual_px']} px of something on the desk",
                          flush=True)
        except Exception as error:
            row["residual_px"] = None
            row["residual_bbox"] = None
            self.problems.append({"state": name, "error": f"no after frame: {error}"})
        return row

    def clean(self, tries: int = 4, *, close_windows: bool = True) -> str:
        """Back to a known desk: every anchored and modal surface dismissed, the
        windows closed and the pointer out of the way.

        Escape is what a person presses, and it is pressed whether or not the
        session lists an overlay as showing: a sheet, a magnifier, a shortcut
        card or a menu the desk draws is not in that list, and a story that
        starts under one is a story judged against the wrong picture.
        """
        closed = []
        pressed = 0
        while pressed < tries:
            try:
                showing = [key for key, value in self.side.overlays().items() if value]
            except Exception:
                showing = []
            # Two presses even on a desk that lists nothing: one takes the
            # layer on top, the next the one under it.
            if not showing and pressed >= 2:
                break
            try:
                self.doors.escape()
            except Exception as error:
                closed.append(str(error)[:120])
                break
            closed += showing
            pressed += 1
            time.sleep(0.2)
        if close_windows:
            try:
                closed.append(self.doors.window_close_all())
            except Exception as error:
                closed.append(f"windows left: {str(error)[:120]}")
        try:
            self.doors.inert()
        except Exception:
            pass
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
                try:
                    row = self.scenario(area, scenario)
                except Exception as error:
                    # A side that stops answering loses this story, not the run.
                    row = {"id": scenario["id"], "area": area["id"], "steps": [],
                           "outcome": "failed", "error": str(error)[:400]}
                    print(f"    failed: {str(error)[:160]}", flush=True)
                self.rows.append(row)
        finally:
            if not self.keep:
                self.side.stop()
        return {"side": self.side.name, "ref": self.side.ref, "commit": self.side.sha,
                "start": start_info, "apps": self.side.apps, "theme": self.theme,
                "surfaces_missing": self.side.missing_surfaces()
                if hasattr(self.side, "missing_surfaces") else [],
                "seconds": round(time.monotonic() - started, 1),
                "rows": self.rows, "problems": self.problems}


# --- pairing -----------------------------------------------------------------


def take_side(sources, out: Path, side: str, chosen, catalogue: Catalogue) -> None:
    """Take one side's rows and pair pictures from earlier runs.

    A run that drives one side only — the new session against the same
    catalogue, on the same stories — is still judged side by side, against what
    the other side did when it was run. With more than one earlier run, the
    first that holds a story gives it: a side re-run for a few stories, with a
    fresh home or a newer build, is taken for those and the rest come from the
    run before it.
    """
    order = [scenario["id"] for _, scenario in chosen]
    wanted = set(order)
    manifests = []
    for source in sources:
        path = source / f"manifest-{side}.json"
        if not path.exists():
            raise SystemExit(f"no {path}: --from names a run's output directory")
        manifests.append((source, json.loads(path.read_text())))
    rows: dict[str, dict] = {}
    problems = []
    for _, manifest in manifests:
        for row in manifest.get("rows", []):
            if row.get("id") in wanted and row["id"] not in rows:
                rows[row["id"]] = row
        problems += [problem for problem in manifest.get("problems", [])
                     if str(problem.get("state")) in wanted]
    source, first = manifests[0]
    taken = {key: value for key, value in first.items() if key not in ("rows", "problems")}
    taken["rows"] = [rows[scenario_id] for scenario_id in order if scenario_id in rows]
    taken["problems"] = problems
    taken["taken_from"] = [str(source) for source, _ in manifests]
    pictures = 0
    for scenario_id in order:
        for source, _ in manifests:
            picture = source / "pairs" / scenario_id / f"{side}.png"
            if picture.exists():
                target = out / "pairs" / scenario_id / f"{side}.png"
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(picture, target)
                pictures += 1
                break
    (out / f"manifest-{side}.json").write_text(json.dumps(taken, indent=1))
    print(f"took {len(taken['rows'])} {side} rows and {pictures} pictures from "
          f"{', '.join(taken['taken_from'])}")


def pair(out: Path, catalogue: Catalogue, chosen=None) -> Path:
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
        if manifest.get("taken_from"):
            sources = manifest["taken_from"]
            if isinstance(sources, str):
                sources = [sources]
            lines.append(f"  - taken from {', '.join(f'`{source}`' for source in sources)}, "
                         f"run separately on the same stories")
        missing = (manifest.get("surfaces_missing")
                   or manifest.get("start", {}).get("surfaces_missing") or [])
        if missing:
            lines.append(f"  - !! this session started without part of its desktop: "
                         f"{'; '.join(missing)}")
        if manifest.get("problems"):
            lines.append(f"  - {len(manifest['problems'])} run problem(s), at the end")
    lines += ["", "A door one side lacks is in the table as `door missing` with what that",
              "side answered instead. Timings are wall milliseconds for the whole scenario,",
              "including the capture; `px` is how much of the desk the scenario changed.",
              "", "## By area", "",
              "| area | scenarios | captured | door missing | failed | old ms | new ms |",
              "| --- | --- | --- | --- | --- | --- | --- |"]

    def side_of(manifest, area_id):
        if not manifest:
            return {"captured": 0, "door_missing": 0, "failed": 0, "ms": []}
        rows = [row for row in manifest["rows"]
                if row.get("area") == area_id and "outcome" in row]
        return {"captured": sum(1 for row in rows if row["outcome"] == "captured"),
                "door_missing": sum(1 for row in rows if row["outcome"] == "door_missing"),
                "failed": sum(1 for row in rows if row["outcome"] not in
                              ("captured", "door_missing")),
                "ms": [row["total_ms"] for row in rows if row["outcome"] == "captured"]}

    def mean(values):
        return f"{sum(values) / len(values):.0f}" if values else "-"

    areas = catalogue.areas if chosen is None else \
        [area for area in catalogue.areas if any(area["id"] == a["id"] for a, _ in chosen)]
    for area in areas:
        old = side_of(manifests.get("old"), area["id"])
        new = side_of(manifests.get("new"), area["id"])
        lines.append(f"| {area['id']} | {len(area['scenarios'])} | "
                     f"{old['captured']} / {new['captured']} | "
                     f"{old['door_missing']} / {new['door_missing']} | "
                     f"{old['failed']} / {new['failed']} | "
                     f"{mean(old['ms'])} | {mean(new['ms'])} |")
    lines += ["", "## Every scenario, both sides", "",
              "| scenario | area | old | new | pairs |", "| --- | --- | --- | --- | --- |"]

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
                    f"residue {row.get('residual_px')}"
                    + (f", drawn after {row['late_s']} s"
                       if row.get("changed_px_early") is not None else ""))
        return "not run"

    for area, scenario in (catalogue.order() if chosen is None else chosen):
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
    tried: dict[str, set[str]] = {}
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
                    tried.setdefault(door, set()).add(name)
                    if "door_missing" in step:
                        asked.setdefault(door, set()).add(name)

    def have(door, name):
        if name not in manifests:
            return "-"
        if name in asked.get(door, set()):
            return "missing"
        return "have" if name in tried.get(door, set()) else "not tried"

    for door in sorted(set(tried) | set(asked)):
        lines.append(f"| {door} | {have(door, 'old')} | {have(door, 'new')} |")

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
         "scroll", "scroll_at", "launch", "notify", "snapshot"}


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
            if "door" in step and step["door"] in known:
                # A step's keys are the door's arguments: one the door does not
                # take is a story that would break only when run.
                takes = inspect.signature(known[step["door"]]).parameters
                if not any(param.kind is param.VAR_KEYWORD for param in takes.values()):
                    extra = sorted(set(step) - {"door"} - set(takes))
                    if extra:
                        problems.append(f"{scenario['id']}: `{step['door']}` takes no "
                                        f"{', '.join(extra)}")
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
    parser.add_argument("--from", dest="taken_from", type=Path, metavar="DIR",
                        action="append",
                        help="an earlier run's output directory to take the side this run "
                             "does not drive from, so the pairs are still judged side by side; "
                             "give it again for a story only a later run holds")
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
    chosen = catalogue.select(options.only)
    if options.index_only:
        print(pair(out, catalogue, chosen if options.only else None))
        return 0

    width, height = (int(part) for part in options.size.lower().split("x"))
    apps = options.apps or catalogue.apps(chosen)
    root = out / "profiles"
    results = {}
    ran = ("old", "new") if options.side == "both" else (options.side,)
    for name in ran:
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
    if options.taken_from:
        for name in ("old", "new"):
            if name not in ran:
                take_side([path.resolve() for path in options.taken_from],
                          out, name, chosen, catalogue)
    print(pair(out, catalogue, chosen if options.taken_from else None))
    return 0


if __name__ == "__main__":
    sys.exit(main())
