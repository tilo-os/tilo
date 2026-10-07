"""Root-operated, three-key capture on one newly owned native Notes worker.

Import is inert. Supply the existing authenticated owner VM adapter and qualified
app-cohort module. Never closes tabs, unlocks a session, or borrows existing Notes.
"""
import json
import re
import shlex
import time

from device_notes_latency import (Device, diagnostic_options, configure_diagnostics,
                                  observed_capture, private_write)
from physical_notes_stages import summarize_physical_notes_stages

APP = "so.tilo.notes"
FLAGS = {field: True for field in ("presentation", "gpu_stages", "gpu_timestamps",
                                  "gpu_dispatch_timestamps")}
STARTUP = r'''
import json, os, sys
pid, expected, uid = int(sys.argv[1]), sys.argv[2], int(sys.argv[3])
p = '/proc/' + str(pid)
def start():
    stat = open(p + '/stat').read()
    return stat[stat.rindex(')') + 2:].split()[19]
if start() != expected or os.stat(p).st_uid != uid:
    raise SystemExit('Native worker lifetime differs')
with open(p + '/environ', 'rb') as f:
    body = f.read(65537)
if len(body) > 65536:
    raise SystemExit('Native environment exceeds observation bound')
with open(p + '/status') as f:
    nspid = [line for line in f if line.startswith('NSpid:')]
if len(nspid) != 1:
    raise SystemExit('Native PID namespace identity unavailable')
nspid = [int(value) for value in nspid[0].split()[1:]]
namespace = os.stat(p + '/ns/pid').st_ino
if not nspid or nspid[0] != pid or any(value <= 0 for value in nspid):
    raise SystemExit('Native PID namespace identity differs')
keys = ('TILO_PROFILE_PRESENTATION', 'TILO_PROFILE_GPU_STAGES',
        'VELLO_GPU_TIMESTAMPS', 'VELLO_GPU_DISPATCH_TIMESTAMPS')
values = dict(entry.split(b'=', 1) for entry in body.split(b'\0') if b'=' in entry)
flags = {key: values.get(key.encode()) == b'1' for key in keys}
del body, values
if (start() != expected or os.stat(p).st_uid != uid
        or os.stat(p + '/ns/pid').st_ino != namespace or not all(flags.values())):
    raise SystemExit('Exact startup GPU diagnostics unavailable')
print(json.dumps({'pid': pid, 'start_time_ticks': expected, 'flags': flags,
                  'nspid': nspid, 'pid_namespace_inode': namespace}))
'''


def tabs(state):
    result = {}
    tree = state["workspace_tree"]
    for workspace in tree["workspaces"]:
        for strip in workspace["strips"]:
            for group in strip["window_groups"]:
                for tab in group["tabs"]:
                    key = (workspace["room"], str(tab["id"]))
                    if key in result:
                        raise RuntimeError("Duplicate public tab identity")
                    tile = tab.get("tile", {}).get("Foreign", {})
                    result[key] = dict(group=group["id"], application=tile.get("app_id"),
                                       surface=tile.get("surface_id"),
                                       association=tree.get("local_app_records", {}).get(str(tab["id"])))
    return result


def fresh_binding(state, preserved, cohort):
    current = tabs(state)
    if any(current.get(key) != row for key, row in preserved.items()):
        raise RuntimeError("Pre-existing tab or record association changed")
    added = [(key, row) for key, row in current.items() if key not in preserved]
    # Refuse simultaneous/ambiguous ownership; never select a cold-published old row.
    if len(added) != 1:
        if added:
            raise RuntimeError("Fresh owned tab is ambiguous")
        return None
    (room, tab), row = added[0]
    if row["application"] != APP or type(row["surface"]) is not int or row["surface"] <= 0:
        return None
    association = row["association"]
    if association is None:
        return None
    if (not isinstance(association, dict) or association.get("app_id") != APP
            or association.get("room") != room
            or type(association.get("record_instance")) is not int):
        raise RuntimeError("Fresh Notes public record association unavailable")
    record = cohort.mapped_record(state, (room, tab), APP)
    old_records = [old["association"].get("record_instance") for old in preserved.values()
                   if isinstance(old["association"], dict)]
    if not record or association["record_instance"] in old_records:
        raise RuntimeError("Fresh Notes would borrow an existing record")
    proof = state.get("chrome", {}).get("native_window_provenance", {})
    if (proof.get("method") != "authenticated-wayland-peer" or proof.get("omitted") != 0
            or not isinstance(proof.get("windows"), list)):
        raise RuntimeError("Authenticated native provenance unavailable")
    found = [r for r in proof["windows"] if r.get("room") == room
             and str(r.get("tab_id")) == tab and r.get("surface_id") == row["surface"]]
    if not found:
        return None
    if len(found) != 1:
        raise RuntimeError("Fresh native worker ambiguous")
    subject = found[0]
    if (subject.get("package_id") != APP
            or subject.get("record_instance") != association["record_instance"]
            or type(subject.get("peer_pid")) is not int or subject["peer_pid"] <= 0
            or not isinstance(subject.get("peer_start_time_ticks"), str)
            or not subject["peer_start_time_ticks"].isdecimal()
            or int(subject["peer_start_time_ticks"]) <= 0
            or not re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}",
                                str(subject.get("adoption_id")))
            or subject["adoption_id"] == "00000000-0000-0000-0000-000000000000"):
        raise RuntimeError("Native worker record/lifetime unavailable")
    return dict(room=room, tab=tab, group=row["group"], surface=row["surface"],
                record=record, subject=subject)


def fine_gpu_ready(stages, pid, namespace=None):
    trace_pid = pid
    if namespace is not None:
        nspid = namespace.get("nspid")
        if (namespace.get("pid") != pid or not isinstance(nspid, list) or not nspid
                or nspid[0] != pid or any(type(value) is not int or value <= 0 for value in nspid)
                or type(namespace.get("pid_namespace_inode")) is not int
                or namespace["pid_namespace_inode"] <= 0
                or not isinstance(namespace.get("start_time_ticks"), str)
                or not namespace["start_time_ticks"].isdecimal()
                or int(namespace["start_time_ticks"]) <= 0):
            return False
        trace_pid = nspid[-1]
    frames = stages.get("native_gpu", {}).get("frames", [])
    if stages.get("process_id") != trace_pid or len(frames) != 3:
        return False
    fine = {20: "fine_area", 21: "fine_msaa8", 22: "fine_msaa16"}
    for frame in frames:
        if frame.get("complete") is not True:
            return False
        def samples(kind):
            evidence = frame["kinds"][kind]
            return [sample["sample"] for sample in evidence["samples"]] if "samples" in evidence else [evidence["sample"]]
        passes = samples("passes")
        dispatches = [stage for sample in samples("dispatches") for stage in sample["stages"]]
        if not any(sample["pass_count"] > 0 for sample in passes) or not any(
                fine.get(stage["stage_id"]) == stage["stage"]
                and stage["dispatch_count"] > 0 and stage["duration_ns"] > 0
                for stage in dispatches):
            return False
    return True


class OwnedDevice(Device):
    def __init__(self, options, vm, cohort, preserved):
        super().__init__(options)
        self.vm, self.cohort, self.preserved = vm, cohort, preserved
        self.binding = None

    def scope(self):
        state = self.vm.state()
        if state["chrome"]["session_locked"] is not False:
            raise RuntimeError("Owner locked; no automatic unlock/input")
        if self.binding != fresh_binding(state, self.preserved, self.cohort):
            raise RuntimeError("Owned Notes tab/worker lifetime changed")

    def control(self, arguments):
        if arguments[:2] == ["read", APP]:
            return self.action("read", {"item": arguments[2]} if len(arguments) == 3 else {})
        if arguments[:2] == ["actions", "list"]:
            arguments = [*arguments, "--programmatic"]
        return self.vm.control_json(*arguments)

    def action(self, operation, payload, resource=None):
        self.scope()
        result = super().action(operation, payload, resource)
        self.scope()
        return result

    def run(self, arguments):
        if arguments[:1] == ["input"]:
            self.scope()
        return super().run(arguments)


def capture_three(vm, cohort, options, installed, uid, executable):
    """All effects require explicit Root invocation. options.out must be new/private."""
    if options.samples != 3:
        raise RuntimeError("This fine GPU capture requires exactly three keys")
    if (not isinstance(installed, dict) or not any(
            path.endswith("/lib/libruntime_dylib.so") and re.fullmatch("[0-9a-f]{64}", digest)
            for path, digest in installed.items())
            or not re.fullmatch("[0-9a-f]{64}", str(executable))):
        raise RuntimeError("Verified selected SDK/executable inventory required")
    before = vm.state()
    if before["chrome"]["session_locked"] is not False:
        raise RuntimeError("Unlocked owner required; no automatic unlock")
    device = OwnedDevice(options, vm, cohort, tabs(before))
    original = diagnostic_options(device.control([
        "actions", "invoke", "session-diagnostics", "inspect@session-diagnostics",
        "--args-json", "{}"]))
    private_write(options.out / "prelaunch-diagnostics-original.json", json.dumps(original))
    try:
        configure_diagnostics(device, FLAGS)
        rows = device.control(["actions", "list", "tab.open-beside", "--json"])["actions"]
        opens = [r for r in rows if r.get("provider") == "actions" and r.get("id") == "tab.open-beside"]
        if len(opens) != 1:
            raise RuntimeError("Unique declared new-tab operation unavailable")
        from acceptance.action_jobs import invoke
        invoke(vm, opens[0], {"app": APP, "background": True}, options.out, "fresh-open", require_job=False)
        deadline = time.monotonic() + 60
        while True:
            device.binding = fresh_binding(vm.state(), device.preserved, cohort)
            if device.binding is not None:
                break
            if time.monotonic() >= deadline:
                raise RuntimeError("Fresh worker unavailable; no launch replay/input")
            time.sleep(.25)
        bound = device.binding
        options.notes_room, options.notes_record = bound["room"], bound["record"]
        device.keyboard_surface = bound["surface"]
        cohort.mapped(vm, "notes", installed, uid, options.out,
                      executable=executable, selected=bound["subject"])
        command = shlex.join(["python3", "-c", STARTUP, str(bound["subject"]["peer_pid"]),
                              bound["subject"]["peer_start_time_ticks"], str(uid)])
        from acceptance.vm import first_json
        startup = first_json(vm.root_exec(command))
        if (startup.get("pid") != bound["subject"]["peer_pid"]
                or startup.get("start_time_ticks") != bound["subject"]["peer_start_time_ticks"]
                or len(startup.get("flags", {})) != 4
                or any(value is not True for value in startup["flags"].values())):
            raise RuntimeError("Startup observation differs from pinned worker")
        private_write(options.out / "startup-flags.json", json.dumps(startup))
        device.scope()
        focus_id = f"tab.focus:{bound['group']}:{bound['tab']}"
        focus = [r for r in device.control(["actions", "list", focus_id, "--json"])["actions"]
                 if r.get("provider") == "desktop-windows" and r.get("id") == focus_id]
        if len(focus) != 1:
            raise RuntimeError("Exact newly owned focus operation unavailable")
        invoke(vm, focus[0], {}, options.out, "fresh-focus", require_job=False)
        device.scope()
        private_write(options.out / "fresh-worker-binding.json", json.dumps(bound))
        options.manage_diagnostics = True
        report = observed_capture(device, options)
        capture = json.loads((options.out / "capture.json").read_text())
        from device_notes_trace import collect_trace
        records = dict.fromkeys((options.out / "timeline.log").read_text().splitlines())
        deadline = time.monotonic() + 10
        while True:
            device.scope()
            lines = collect_trace(device, capture["log_path"], capture["scratch_page"], records)
            stages = summarize_physical_notes_stages(lines, capture["scratch_page"], require_capture=True)
            private_write(options.out / "timeline.log", "\n".join(lines))
            if len(report["samples"]) == 3 and fine_gpu_ready(stages, bound["subject"]["peer_pid"], startup):
                break
            if time.monotonic() >= deadline:
                raise RuntimeError("Three exact physical/KMS/GPU frame families were not observed")
            time.sleep(.25)
        private_write(options.out / "report.json", json.dumps(report, indent=2))
        private_write(options.out / "stages.json", json.dumps(stages, indent=2))
        return report, stages, device
    finally:
        configure_diagnostics(device, original)
        private_write(options.out / "prelaunch-diagnostics-restored.json", json.dumps(original))
