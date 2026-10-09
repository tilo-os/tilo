#!/usr/bin/env python3
"""Settings sidebar hover through an owned slot's kernel input and KMS feedback.

Creates one fresh Settings window, preserves every existing tab, and restores
diagnostic options. Never unlocks, closes windows, refreshes a session or takes
screenshots. GPU queries must already be enabled in the compositor; missing
capability fails. This does not measure palette opening, query or activation.
"""
import argparse
import importlib.util
import json
import math
from pathlib import Path
import re
import shlex
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/vm-acceptance"))
from acceptance.action_jobs import invoke
from acceptance.vm import TiloVm, first_json
from device_notes_latency import diagnostic_options, configure_diagnostics, private_write
from device_notes_trace import collect_settings_hover_trace
from fresh_notes_capture import FLAGS, STARTUP, fresh_binding, tabs
from physical_notes_stages import summarize_physical_settings_stages, require_gpu_stages

APP = "so.tilo.settings"
IDENTITY = r'''
import json, sys, uuid
from pathlib import Path
p = Path(sys.argv[1])
if p.stat().st_size > 32*1024*1024:
    raise SystemExit('Flight log exceeds observation bound')
rows = []
for line in p.read_text().splitlines():
    marker = 'tilo-physical-input '
    if marker not in line: continue
    row = json.loads(line.partition(marker)[2])
    if row.get('process_id') != int(sys.argv[2]): continue
    if (row.get('before') or {}).get('app') != 'so.tilo.settings': continue
    value = uuid.UUID(row['observer_session'])
    if value.version != 4 or str(value) != row['observer_session']:
        raise SystemExit('Invalid Settings observer')
    if type(row.get('id')) is not int or row['id'] < 0:
        raise SystemExit('Invalid Settings input boundary')
    rows.append((row['observer_session'], row['id']))
if not rows or len({r[0] for r in rows}) != 1:
    raise SystemExit('Exact Settings observer unavailable')
print(json.dumps(dict(observer_session=rows[-1][0], after_input_id=max(r[1] for r in rows))))
'''


def rect(value):
    if (not isinstance(value, list) or len(value) != 4
            or any(type(n) not in (int, float) or not math.isfinite(n) for n in value)):
        raise ValueError("Mounted rectangle unavailable")
    return value


def hover_point(inspection, targets, surface, pane):
    """Transform observed app bounds through the exact named foreign surface."""
    rows = [row for row in targets if row.get("name") == f"foreign-surface-{surface}"]
    if len(rows) != 1:
        raise ValueError("Exact mapped Settings surface target unavailable")
    x, y, width, height = rect(rows[0].get("rect"))  # public targets use xywh
    root = rect(inspection.get("input", {}).get("root_rect"))
    choices = [row for row in inspection.get("input", {}).get("rows", []) if row.get("pane") == pane]
    if len(choices) != 1:
        raise ValueError("Exact mounted Settings sidebar row unavailable")
    left, top, right, bottom = rect(choices[0].get("rect"))
    rw, rh = root[2] - root[0], root[3] - root[1]
    if min(width, height, rw, rh, right-left, bottom-top) <= 0:
        raise ValueError("Empty mounted Settings bounds")
    # Refuse scaling/letterboxing ambiguity; never guess pointer coordinates.
    if abs(width/rw-height/rh) > 1e-6:
        raise ValueError("Settings surface aspect differs from mounted root")
    if not (root[0] <= left < right <= root[2] and root[1] <= top < bottom <= root[3]):
        raise ValueError("Mounted sidebar row exceeds the app root")
    px, py = x + ((left+right)/2-root[0])*width/rw, y + ((top+bottom)/2-root[1])*height/rh
    if not x <= px < x+width or not y <= py < y+height or min(px, py) < 0:
        raise ValueError("Mounted sidebar row is outside its mapped surface")
    return round(px), round(py)


class SettingsDevice:
    def __init__(self, vm, cohort, preserved, out):
        self.vm, self.cohort, self.preserved, self.out = vm, cohort, preserved, out
        self.binding = None
        self.sequence = 0

    def control(self, arguments):
        return self.vm.control_json(*arguments)

    def execute(self, arguments):
        return self.vm.run("slot", "exec", "--timeout", "30", "--", *arguments)

    def scope(self):
        state = self.vm.state()
        if state["chrome"]["session_locked"] is not False:
            raise RuntimeError("Owner locked; no automatic unlock/input")
        if self.binding != fresh_binding(state, self.preserved, self.cohort, APP):
            raise RuntimeError("Owned Settings window/record/worker changed")
        return state

    def inspect(self):
        self.scope()
        bound = (APP, self.binding["room"], self.binding["record"])
        row = self.cohort.choose(self.cohort.catalog(self.vm, APP, bound, "settings.navigation"),
                                 APP, "settings.navigation", "inspect-navigation", {},
                                 bound=(*bound, "settings.navigation"),
                                 semantic=("settings.navigation", "settings.navigation"))
        self.sequence += 1
        result = invoke(self.vm, row, {}, self.out, f"inspect-{self.sequence}")
        self.scope()
        nav = result.get("input", {}).get("navigation", {}) if isinstance(result, dict) else {}
        if (result.get("app") != APP or result.get("ready") is not True
                or result.get("pending") is not False or result.get("content_visible") is not True
                or nav.get("selected_pane") != result.get("selected_pane")):
            raise ValueError("Exact mounted Settings navigation is not ready")
        return result

    def move(self, pane):
        value = self.inspect()
        self.scope()
        point = hover_point(value, self.vm.targets(), self.binding["surface"], pane)
        self.scope()
        self.vm.input("move", *map(str, point))

    def settled_hover(self, pane):
        deadline = time.monotonic() + 10
        while True:
            inspected = self.inspect()["input"]
            hover = inspected.get("hover", {})
            if (hover == inspected.get("rendered_hover") and hover.get("mounted") is True
                    and hover.get("control") == "settings.nav" and hover.get("pane") == pane):
                return
            if time.monotonic() >= deadline:
                raise ValueError("Preparation hover was not actually drawn")
            time.sleep(.1)


def capture(vm, cohort, options, shown, installed):
    before = vm.state()
    if before["chrome"]["session_locked"] is not False:
        raise RuntimeError("Unlocked owner required")
    device = SettingsDevice(vm, cohort, tabs(before), options.out)
    original = diagnostic_options(device.control(["actions", "invoke", "session-diagnostics",
                                                  "inspect@session-diagnostics", "--args-json", "{}"]))
    private_write(options.out / "diagnostics-original.json", json.dumps(original))
    try:
        configure_diagnostics(device, FLAGS)
        rows = [row for row in vm.actions("tab.open-beside")
                if row.get("provider") == "actions" and row.get("id") == "tab.open-beside"]
        if len(rows) != 1:
            raise RuntimeError("Unique public fresh-window action unavailable")
        invoke(vm, rows[0], {"app": APP, "background": True}, options.out, "fresh-open", require_job=False)
        deadline = time.monotonic() + 60
        while device.binding is None:
            device.binding = fresh_binding(vm.state(), device.preserved, cohort, APP)
            if time.monotonic() >= deadline:
                raise RuntimeError("Fresh Settings provenance unavailable; no launch replay")
            if device.binding is None:
                time.sleep(.1)
        bound = device.binding
        layer = shown["slot"]["components"]["apps+runtime"]
        files = [row for row in shown.get("files", []) if row.get("component") == "apps+runtime"
                 and row.get("layer") == layer and isinstance(row.get("path"), str)
                 and row["path"].startswith("/usr/share/tilo-apps/") and row["path"].endswith("/bin/settings")]
        if (len(files) != 1 or "link" in files[0]
                or not re.fullmatch("[0-9a-f]{64}", str(files[0].get("sha256")))):
            raise RuntimeError("Verified selected Settings executable unavailable")
        cohort.mapped(vm, "settings", installed, options.uid, options.out,
                      executable=files[0]["sha256"], selected=bound["subject"])
        subject = bound["subject"]
        startup = first_json(vm.root_exec(shlex.join(["python3", "-c", STARTUP,
                            str(subject["peer_pid"]), subject["peer_start_time_ticks"], str(options.uid)])))
        if (startup.get("pid") != subject["peer_pid"]
                or startup.get("start_time_ticks") != subject["peer_start_time_ticks"]
                or not isinstance(startup.get("nspid"), list) or not startup["nspid"]
                or startup["nspid"][0] != subject["peer_pid"]
                or any(type(value) is not int or value <= 0 for value in startup["nspid"])
                or len(startup.get("flags", {})) != 4
                or any(value is not True for value in startup["flags"].values())):
            raise RuntimeError("Exact startup GPU observation differs from pinned worker")
        private_write(options.out / "startup.json", json.dumps(startup))
        focus = [row for row in vm.actions(f"tab.focus:{bound['group']}:{bound['tab']}")
                 if row.get("provider") == "desktop-windows"
                 and row.get("id") == f"tab.focus:{bound['group']}:{bound['tab']}"]
        if len(focus) != 1:
            raise RuntimeError("Exact new Settings focus unavailable")
        invoke(vm, focus[0], {}, options.out, "fresh-focus", require_job=False)
        private_write(options.out / "binding.json", json.dumps(bound))
        log = device.execute(["python3", "-c", "import os; print(os.path.join(os.environ['HOME'], '.local/state/tilo/session-logs/latest.log'))"]).strip()
        # The first ordinary motion establishes the worker observer and is
        # outside measurement. It never borrows an existing Settings window.
        device.move(options.reset_pane)
        device.settled_hover(options.reset_pane)
        observer = json.loads(device.execute(["python3", "-c", IDENTITY, log, str(startup["nspid"][-1])]))
        if str(uuid.UUID(observer["observer_session"])) != observer["observer_session"]:
            raise ValueError("Noncanonical observer")
        records, reports = {}, []
        session = observer["observer_session"]
        for index in range(options.samples):
            if index:
                device.move(options.reset_pane)
                device.settled_hover(options.reset_pane)
                observer = json.loads(device.execute(["python3", "-c", IDENTITY, log, str(startup["nspid"][-1])]))
                if observer["observer_session"] != session:
                    raise ValueError("Settings observer changed during measurement")
            # Reset live bounded recording only outside the measured input.
            configure_diagnostics(device, dict(FLAGS, presentation=False))
            configure_diagnostics(device, FLAGS)
            device.move(options.pane)
            deadline = time.monotonic() + 10
            cohort_records = {}
            while True:
                device.scope()
                lines = collect_settings_hover_trace(device, log, cohort_records,
                                observer["observer_session"], observer["after_input_id"])
                private_write(options.out / "timeline.log", "\n".join(dict.fromkeys([*records, *lines])))
                try:
                    report = summarize_physical_settings_stages(lines, options.pane, require_capture=True, hover=True)
                    require_gpu_stages(report)
                    if (len(report["frames"]) != 1 or report["process_id"] != startup["nspid"][-1]
                            or report["observer_session"] != observer["observer_session"]
                            or report["main_stages_available"] is not True):
                        raise ValueError("Exact single-input worker/main-stage coverage unavailable")
                    break
                except (ValueError, KeyError) as error:
                    if time.monotonic() >= deadline:
                        raise RuntimeError("Settings hover coverage failed: " + str(error)) from error
                    time.sleep(.1)
            reports.append(report)
            records.update(dict.fromkeys(lines))
            private_write(options.out / "samples.json", json.dumps(reports))
        final = summarize_physical_settings_stages(list(records), options.pane, require_capture=True, hover=True)
        require_gpu_stages(final)
        if final["latency"]["n"] != options.samples:
            raise ValueError("Requested sample count was not preserved")
        device.scope()
        if first_json(vm.run("--json", "slot", "show"))["slot"] != shown["slot"]:
            raise RuntimeError("Selected source cohort changed")
        return final
    finally:
        configure_diagnostics(device, original)
        private_write(options.out / "diagnostics-restored.json", json.dumps(original))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "cohort-receipt", "out"):
        parser.add_argument("--" + name, required=True, type=Path)
    for name in ("vm", "slot", "pane", "reset-pane"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--source-commit", required=True,
                        help="exact source of the activated cohort; never inferred from this controller")
    parser.add_argument("--uid", required=True, type=int)
    parser.add_argument("--samples", type=int, default=100)
    options = parser.parse_args()
    if (not 1 <= options.samples <= 1000 or options.pane == options.reset_pane or options.uid <= 0
            or not re.fullmatch('[0-9a-f]{40}', options.source_commit)):
        parser.error("Need 1..1000 samples, different mounted hover/reset panes and positive owner UID")
    options.out.mkdir(parents=True, exist_ok=False, mode=0o700)
    spec = importlib.util.spec_from_file_location("settings_cohort", ROOT / "scripts/vm-acceptance/app-cohort.py")
    cohort = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(cohort)
    vm = TiloVm(str(options.binary.resolve()), options.vm, options.slot, options.out / "commands.log")
    try:
        receipt, shown, installed = cohort.cohort(vm, options, options.out)
        if receipt["cohort_source"]["source_commit"] != options.source_commit:
            raise RuntimeError("Activated cohort differs from requested exact source")
        report = capture(vm, cohort, options, shown, installed)
        report.update(cohort_source=receipt["cohort_source"], samples_requested=options.samples,
                      input_origin="slot_uinput_kernel", hardware_peripheral_latency=False,
                      palette_latency_available=False)
        private_write(options.out / "report.json", json.dumps(report, indent=2))
    except (RuntimeError, ValueError, KeyError) as error:
        private_write(options.out / "failure.json", json.dumps({"complete": False, "error": str(error)}))
        parser.exit(1, f"Measurement failed: {error}\nEvidence: {options.out}\n")
    print(json.dumps(report["latency"], indent=2))


if __name__ == "__main__":
    main()
