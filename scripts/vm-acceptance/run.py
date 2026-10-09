#!/usr/bin/env python3
"""Tilo VM acceptance: desktop essentials, checked in a real KVM VM session.

    scripts/vm-acceptance/run.py --disk ~/vm-images/tilo-dev-x86-<sha>.qcow2 --commit <sha>
    scripts/vm-acceptance/run.py --vm tilofix --commit <sha>          # a VM you already have

Creates (or uses) a dev VM and a headful slot on two outputs (with a throwaway
identity where tilo-vm can mint one), drives it only through tilo-vm (control
socket first, real uinput input second), and writes report.json and report.md with shots under
target/vm-acceptance/<run>/. A check that cannot observe its subject fails.
See README.md beside this file.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import os
import shlex
import shutil
import subprocess
import sys
import time
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from acceptance import core, report  # noqa: E402
from acceptance.checks import (apps, basics, daily_apps, provenance, resources, search,  # noqa: E402,F401
                               startup, ui, window)
from acceptance.context import Context  # noqa: E402
from management import start_management  # noqa: E402
from acceptance.vm import TiloVm, VmError, first_json  # noqa: E402

REPO = HERE.parents[1]

BUDGETS = {
    # Idle: a still desktop parks the compositor; the clock may repaint once a minute.
    "idle_shell_cpu_pct": 1.0, "idle_frames": 2, "idle_user_cpu_pct": 3.0,
    "idle_apps_shell_cpu_pct": 1.5, "idle_apps_frames": 2, "idle_apps_user_cpu_pct": 5.0,
    "idle_loop_ticks_per_s": 2.0,
    # Memory. An app is budgeted by its own cgroup (memory.current, and its anon share);
    # processes and the compositor by anonymous RSS, the memory they hold privately.
    "app_memory_mb": 400, "app_anon_mb": 250,
    "shell_anon_mb": 500, "process_anon_mb": 300, "session_memory_mb": 2500,
    # Frames at 60 Hz. Work per frame: CPU everywhere, wall time on real display hardware.
    "work_ms_p95": 16.7, "work_ms_p99": 33.4,
    # Transitions, the compositor's side: frames painted, lateness against its own
    # deadlines, the longest CPU tick, input to first frame less time the display held it.
    "animation_min_frames": 8, "frame_late_ms_p95": 16.7, "frame_cpu_ms_max": 33.4,
    "input_first_frame_ms": 33.4,
    # Transitions, the display's side (painted-to-painted, and first to last frame, which
    # a held transition stretches while the display is slow), judged on real hardware only.
    "paint_period_ms_p95": 33.4, "paint_gap_ms_max": 100.0, "transition_ms_max": 1000.0,
    # Files, process start to first presented frame. On real hardware the GPU draws it
    # within 300 ms. A VM renders on a CPU rasterizer (lavapipe) or through virgl to the
    # host's GPU, both of which compile every render pipeline on the first frame: a cold
    # launch there is judged against a generous ceiling that still catches a stall of seconds.
    "files_first_frame_s": 3.0, "files_first_frame_hw_s": 0.3,
}

# Run in a German session (a second slot), after the main pass.
DE_PASS = ("U03", "U04", "S03")
# A14 runs before anything else opens Files, so it times the session's first launch. R05
# runs last but P03, so the adapter lines of every app the run opened are in the log.
ORDER = ["P01", "P02", "R01", "A14", "R02", "R03", "R04",
         "W01", "W02", "W03", "W04", "W05", "W06", "W07", "W08", "W09", "W10", "W11", "W12",
         "W13", "W14", "W15", "W16", "W17", "W18", "W19", "W20", "W21",
         "A01", "A02", "A03", "A04", "A05", "A06", "A07",
         "A08", "A09", "A10", "A11", "A12", "A13",
         "S01", "S02", "S04", "S05", "S06", "S07", "S08", "S09",
         "U01", "U02", "B01", "B02", "B03", "B04", "B05", "B06", "B07", "R05", "P03"]


def git(*args: str, repo: Path = REPO) -> str:
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True).stdout.strip()


def find_tilo_vm(arg: str | None) -> str:
    for cand in (arg, os.environ.get("TILO_VM_BIN"), shutil.which("tilo-vm"),
                 str(REPO / "target/debug/tilo-vm")):
        if cand and Path(cand).is_file() and os.access(cand, os.X_OK):
            return cand
    sys.exit("no tilo-vm binary: pass --tilo-vm, set TILO_VM_BIN, or build it (cargo build -p tilo-vm)")


def vms_toml() -> dict:
    base = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
    path = base / "tilo-vm" / "vms.toml"
    return tomllib.loads(path.read_text()) if path.exists() else {}


def mem_available_mb() -> int:
    for line in Path("/proc/meminfo").read_text().splitlines():
        if line.startswith("MemAvailable:"):
            return int(line.split()[1]) // 1024
    return 0


def running_vms() -> list[str]:
    proc = subprocess.run(["virsh", "-c", "qemu:///session", "list", "--name"],
                          capture_output=True, text=True)
    return [l for l in proc.stdout.split() if l]


def provenance_of(vm: TiloVm, disk: Path | None, want_commit: str | None) -> dict:
    """Tie the running image to a commit: the disk's provenance record plus bootc."""
    p = {"disk": str(disk) if disk else None}
    rec_path = Path(str(disk) + ".provenance.json") if disk else None
    record = json.loads(rec_path.read_text()) if rec_path and rec_path.exists() else None
    p["record"] = record
    try:
        status = first_json(vm.vm_cmd("ssh", "--", "bootc", "status", "--json", timeout=60))
        booted = (status.get("status") or {}).get("booted") or {}
        p["booted_digest"] = (booted.get("image") or {}).get("imageDigest")
        p["upgraded_since_disk"] = bool((status.get("status") or {}).get("rollback"))
    except (VmError, ValueError) as exc:
        p["why"] = f"could not read bootc status: {exc}"
        return p
    if record is None:
        p["why"] = (f"no provenance record {rec_path} for the VM's disk: build disks with "
                    "scripts/vm-acceptance/build-disk.sh, which writes one")
        return p
    if want_commit and not record.get("commit", "").startswith(want_commit[:12]) \
            and not want_commit.startswith(record.get("commit", "")[:12]):
        p["mismatch"] = f"disk built from {record.get('commit')}, run asked for {want_commit}"
    if p["upgraded_since_disk"]:
        p["mismatch"] = (p.get("mismatch", "") + "; " if p.get("mismatch") else "") + \
            "the VM was moved onto another image since the disk was written (bootc has a rollback)"
    elif record.get("booted_digest") and record["booted_digest"] != p["booted_digest"]:
        p["mismatch"] = f"booted {p['booted_digest']}, disk's image was {record['booted_digest']}"
    elif not record.get("booted_digest") and rec_path:
        record["booted_digest"] = p["booted_digest"]
        rec_path.write_text(json.dumps(record, indent=1))
    return p


# Kernel drivers of real display hardware. Anything else (virtio-gpu, qxl, bochs, a
# firmware framebuffer) presents through a hypervisor or firmware, whose timing is
# not the compositor's.
HARDWARE_DRM_DRIVERS = {"i915", "xe", "amdgpu", "radeon", "nouveau", "nvidia", "nvidia-drm",
                        "msm", "panfrost", "panthor", "lima", "v3d", "vc4", "etnaviv",
                        "rockchip-drm", "mediatek-drm", "imx-drm", "apple-drm", "asahi"}


def renderer(vm: TiloVm) -> tuple[str, dict]:
    """The renderer as a readable line, and whether frames reach real display hardware."""
    out = vm.exec(
        "echo nodes: $(ls /dev/dri 2>/dev/null | tr '\\n' ' '); "
        "echo drivers: $(for c in /sys/class/drm/card*/device/driver; do basename $(readlink $c); done 2>/dev/null | sort -u | tr '\\n' ' '); "
        "grep -m4 -iE 'adapter|llvmpipe|lavapipe|virgl|softpipe|failed to create dri2' "
        "\"$HOME/.local/state/tilo/session-logs/latest.log\" 2>/dev/null | cut -c1-160", check=False)
    lines = [l for l in out.splitlines() if l.strip() and not l.startswith("receipt:")]
    text = " | ".join(lines)
    drivers = next((l.split(":", 1)[1].split() for l in lines if l.startswith("drivers:")), [])
    soft = "renderD" not in text or "dri2" in text
    hardware = sorted(set(drivers) & HARDWARE_DRM_DRIVERS)
    virgl = any(d.startswith("virtio") for d in drivers)
    label = ("software (no GPU render node; wgpu on a CPU rasterizer): " if soft
             else "GPU (virgl): " if virgl else f"GPU ({', '.join(hardware) or 'unknown driver'}): ")
    display = {"hardware": bool(hardware) and not virgl, "drivers": drivers,
               "why": (f"frames reach {', '.join(hardware)} hardware" if hardware and not virgl
                       else "frames are presented through a virtual display (virtio-gpu, a hypervisor "
                            "framebuffer or a CPU rasterizer), whose timing is the host's")}
    return label + text, display


def slot_create(vm: TiloVm, outputs: list[str], locale: str | None,
                source: Path | None = None) -> dict:
    helptext = vm.run("slot", "create", "--help", slot=False, cwd=source)
    args = ["--json", "slot", "create", vm.slot, "--headful", "--outputs", ",".join(outputs)]
    # A throwaway test identity where this tilo-vm can mint one (`slot create --identity`).
    if "--identity" in helptext:
        args.append("--identity")
    if locale:
        args += ["--locale", locale]
    result = first_json(vm.run(*args, timeout=300, slot=False, cwd=source))
    if not isinstance(result, dict):
        raise VmError("slot create returned no structured result")
    return result


def whole_json(text: str) -> dict:
    """`tilo-vm --json`'s one object, which it may print across lines; {} if there is none."""
    start = text.find("{")
    try:
        return json.JSONDecoder().raw_decode(text[start:])[0] if start >= 0 else {}
    except ValueError:
        return {}


def component_scopes(source: Path) -> dict[str, str]:
    manifest = tomllib.loads((source / "os/components.toml").read_text())
    return {c["name"]: c.get("scope", "session") for c in manifest.get("component", [])}


def swap_components(vm: TiloVm, components: list[str], ref: str, source: Path,
                    initial: dict | None = None) -> dict:
    """Build each component from the worktree `source` on the host and swap it into the VM:
    session components and apps into the run's slot, VM-wide ones into `vm`. Then read back the
    layer each slot runs, so P01 states what was tested: the base image plus these, from `ref`."""
    scopes = component_scopes(source)
    head = git("rev-parse", "HEAD", repo=source)
    if initial and (initial.get("ref") != ref or initial.get("commit") != head):
        raise VmError("the source changed after the bootstrap Shell swap")
    swapped = [dict(entry) for entry in (initial or {}).get("components", [])]
    slots = {entry["slot"] for entry in swapped if not entry.get("error")}
    for component in components:
        slot = "vm" if scopes.get(component) == "vm" else vm.slot
        entry = {"component": component, "slot": slot}
        started = time.monotonic()
        try:
            out = vm.run("--json", "swap", component, "--slot", slot, "--builder", "host",
                         timeout=5400, slot=False, cwd=source)
            entry["seconds"] = round(time.monotonic() - started)
            entry["result"] = whole_json(out)
            slots.add(slot)
        except VmError as exc:
            entry["error"] = exc.reason()
        print(f"swap {component} into {slot}: " + entry.get("error", f"done in {entry.get('seconds')}s"),
              flush=True)
        swapped.append(entry)
    layers = {}
    for slot in sorted(slots):
        try:
            shown = whole_json(vm.run("--json", "slot", "show", slot, timeout=120, slot=False, cwd=source))
            layers[slot] = (shown.get("slot") or {}).get("components") or {}
        except VmError as exc:
            layers[slot] = {"error": exc.reason()}
    for entry in swapped:
        observed_layer = layers.get(entry["slot"], {}).get(entry["component"])
        if initial and entry["component"] == "shell" and observed_layer != entry.get("layer"):
            raise VmError("bootstrap Shell layer changed during final component readback")
        entry["layer"] = observed_layer
    return {"ref": ref, "commit": head, "components": swapped, "slot_layers": layers}


def bootstrap_session(vm: TiloVm, outputs: list[str], plan: dict, provenance: dict) -> None:
    source = plan["source"]
    components = plan["components"]
    try:
        slot_create(vm, outputs, None, source)
    except VmError as exc:
        expected_error = {
            "error": f"slot '{vm.slot}' outputs could not be set: exit status: 1: "
                     "the compositor did not pick up the display change",
            "hint": f"rerun tilo-vm slot create {vm.slot} --headful --outputs <same list> "
                    f"to apply it again; tilo-vm logs {vm.slot} shows what the compositor did",
            "see": "tilo-vm guide driving",
        }
        expected_command = ("--json", "slot", "create", vm.slot, "--headful", "--outputs",
                            ",".join(outputs), "--identity")
        if (components.count("shell") != 1 or exc.returncode != 1
                or exc.command != expected_command or not isinstance(exc.data, dict)
                or any(exc.data.get(key) != value for key, value in expected_error.items())):
            raise
        shown = first_json(vm.run("--json", "slot", "show", vm.slot,
                                 timeout=120, slot=False, cwd=source))
        observed = shown.get("slot") if isinstance(shown, dict) else None
        expected = source_owner(source)
        verify_slot(observed, vm.slot, outputs, expected)
        if (type(observed.get("generation")) is not int or observed["generation"] != 0
                or observed.get("generations") != [0] or observed.get("components") != {}):
            raise VmError("display bootstrap requires an observed base slot with no prior layers")
        bootstrap = {"base_create_error": exc.data, "base_slot": observed}
        early = swap_components(vm, ["shell"], plan["ref"], source)
        provenance["swap"] = early
        provenance["bootstrap"] = bootstrap
        entry = early["components"][0]
        if entry.get("error") or not entry.get("layer"):
            raise VmError("bootstrap Shell was not applied: " + entry.get("error", "no observed layer"))
        result = entry.get("result") or {}
        if (result.get("slot") != vm.slot or type(result.get("generation")) is not int
                or result["generation"] <= observed["generation"]):
            raise VmError("bootstrap Shell swap did not return the owned candidate generation")
        applied = slot_create(vm, outputs, None, source)
        final_slot = applied.get("slot")
        verify_slot(final_slot, vm.slot, outputs, expected)
        if (type(final_slot.get("generation")) is not int
                or final_slot["generation"] != result["generation"]
                or final_slot.get("user") != observed["user"] or final_slot.get("seat") != observed["seat"]
                or not isinstance(final_slot.get("components"), dict)
                or final_slot["components"].get("shell") != entry["layer"]):
            raise VmError("bootstrap slot or Shell layer changed before display readback")
        identity = applied.get("identity")
        actions = applied.get("actions")
        if (not isinstance(identity, dict) or not isinstance(actions, list)
                or any(not isinstance(action, dict) for action in actions)):
            raise VmError("bootstrap create returned no structured identity or output actions")
        confirmed = [action.get("outputs") for action in actions
                     if action.get("action") == "set_outputs"]
        if (identity.get("throwaway") is not True or not identity.get("display_name")
                or len(confirmed) != 1 or not isinstance(confirmed[0], str)
                or output_specs(confirmed[0].split(",")) != output_specs(outputs)):
            raise VmError("bootstrap create did not verify the original outputs and throwaway identity")
        bootstrap["candidate_create"] = applied
        remaining = [component for component in components if component != "shell"]
        provenance["swap"] = swap_components(vm, remaining, plan["ref"], source, initial=early)
        return
    if components:
        provenance["swap"] = swap_components(vm, components, plan["ref"], source)


def output_specs(outputs: list[str]) -> list[dict]:
    specs = []
    for output in outputs:
        try:
            mode, separator, scale = output.strip().lower().partition("@")
            if separator and not scale:
                raise ValueError("missing scale after @")
            width, height = (int(value) for value in mode.split("x"))
            scale = float(scale or 1)
            # The canonical protocol serializes OutputSpec as WIDTHxHEIGHT@SCALE strings.
            # Apply its bounds before crediting an observed mode or scale.
            if (not 1 <= width <= 8192 or not 1 <= height <= 8192
                    or not math.isfinite(scale) or not 0.5 <= scale <= 4):
                raise ValueError("outside the protocol's extent or scale bounds")
            specs.append({"width": width, "height": height, "scale": scale})
        except (AttributeError, ValueError) as exc:
            raise VmError("display bootstrap received an invalid output specification") from exc
    return specs


def source_owner(source: Path) -> dict:
    host = os.environ.get("HOSTNAME", os.environ.get("COMPUTERNAME"))
    if host is None:
        host = subprocess.run(["hostname"], capture_output=True, text=True, check=True).stdout.strip()
    root = git("rev-parse", "--show-toplevel", repo=source)
    if not host or not root or Path(root).resolve() != source.resolve():
        raise VmError("cannot observe the bootstrap source owner")
    return {"host": host, "worktree": root,
            "branch": git("symbolic-ref", "--quiet", "--short", "HEAD", repo=source) or None}


def verify_slot(slot: dict | None, name: str, outputs: list[str], owner: dict) -> None:
    if (not isinstance(slot, dict) or slot.get("name") != name or slot.get("kind") != "headful"
            or slot.get("session") != "active" or not slot.get("user") or not slot.get("seat")
            or slot.get("owner") != owner or not isinstance(slot.get("outputs"), list)
            or output_specs(slot["outputs"]) != output_specs(outputs)):
        raise VmError("display bootstrap could not observe the exact owned active headful slot")


def switch_locale(vm: TiloVm, lang: str) -> None:
    """Set the system locale as the installer seeds it (/etc/locale.conf, which the
    session's PAM login inherits from systemd) and restart the slot's session in it."""
    vm.vm_cmd("ssh", "--", f"localectl set-locale LANG={lang} && systemctl daemon-reload", timeout=60)
    vm.run("slot", "restart", vm.slot, timeout=300, slot=False)
    got = vm.root_exec(f"tr '\\0' '\\n' < /proc/$(pgrep -u $(id -u tds-{vm.slot}) -x shell | head -1)/environ "
                       "| grep -E '^(LANG|LC_ALL|LC_MESSAGES)='", timeout=60)
    if lang not in got:
        raise VmError(f"the restarted compositor runs with {got.strip() or 'no LANG'}, not LANG={lang}")
    time.sleep(5)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--disk", type=Path, help="dev disk (qcow2) to create a throwaway VM from")
    src.add_argument("--vm", help="an existing VM from vms.toml")
    ap.add_argument("--commit", help="the commit the disk must be built from (sha or ref)")
    ap.add_argument("--slot", default="acceptance")
    # The second output is fractionally scaled, so apps are checked at a fractional scale too.
    ap.add_argument("--outputs", default="1920x1080@1,1280x800@1.25")
    ap.add_argument("--locales", default="en,de", help="en and optionally de (a second, German slot)")
    ap.add_argument("--only", help="comma-separated check ids or areas")
    ap.add_argument("--idle-seconds", type=int, default=30)
    ap.add_argument("--memory-mb", type=int, default=4608)
    ap.add_argument("--cpus", type=int, default=6)
    ap.add_argument("--gl", action="store_true", help="create the VM with virgl (--disk only)")
    ap.add_argument("--keep", action="store_true", help="leave the VM running (and a created VM registered)")
    ap.add_argument("--tilo-vm")
    ap.add_argument("--bootstrap-tilo-vm", help="fixture-matching CLI for a canonical management upgrade")
    ap.add_argument("--bootstrap-from", type=Path, help="exact target source for the management upgrade")
    ap.add_argument("--swap", help="comma-separated components (os/components.toml) or app:<dir> to build "
                    "from this worktree on the host and swap in before the checks; the disk is then the base")
    ap.add_argument("--swap-from", type=Path, help="the worktree --swap builds from (default: this one)")
    ap.add_argument("--swap-ref", help="the ref the swapped components are built from, for the report "
                    "(default: the --swap-from worktree's HEAD)")
    ap.add_argument("--owners", help="area=owner,... to name who each area routes to")
    ap.add_argument("--out", type=Path)
    args = ap.parse_args()

    if bool(args.bootstrap_tilo_vm) != bool(args.bootstrap_from):
        ap.error("--bootstrap-tilo-vm and --bootstrap-from must be supplied together")
    binary = find_tilo_vm(args.tilo_vm)
    swaps = [c.strip() for c in (args.swap or "").split(",") if c.strip()]
    swap_from = (args.swap_from or REPO).resolve()
    swap_ref = args.swap_ref or git("rev-parse", "HEAD", repo=swap_from)
    want_commit = git("rev-parse", args.commit) if args.commit else None
    if args.commit and not want_commit:
        want_commit = args.commit
    stamp = dt.datetime.now(dt.UTC).strftime("%Y%m%dT%H%M%SZ")
    out = args.out or REPO / "target" / "vm-acceptance" / f"{stamp}-{(want_commit or 'unknown')[:9]}"
    out.mkdir(parents=True, exist_ok=True)
    outputs = args.outputs.split(",")

    created = False
    if args.disk:
        vm_name = f"acc-{(want_commit or 'x')[:7]}"
        disk = args.disk.expanduser().resolve()
    else:
        vm_name = args.vm
        entry = (vms_toml().get("vms") or {}).get(vm_name)
        if entry is None:
            sys.exit(f"VM {vm_name} is not in vms.toml")
        disk = Path(entry["image"]).expanduser() if entry.get("image") else None
    vm = TiloVm(binary, vm_name, args.slot, out / "tilo-vm.log")

    startup_vm = (TiloVm(find_tilo_vm(args.bootstrap_tilo_vm), vm_name, args.slot, out / "tilo-vm.log")
                  if args.bootstrap_tilo_vm else vm)

    others = [n for n in running_vms() if n != vm_name]
    if others:
        sys.exit(f"other VMs are running ({others}); this laptop runs one VM at a time")
    need = args.memory_mb + 1024
    if vm_name not in running_vms() and mem_available_mb() < need:
        sys.exit(f"only {mem_available_mb()} MiB available, the VM needs {need} MiB with headroom")

    meta = {
        "suite": "scripts/vm-acceptance", "suite_commit": git("rev-parse", "HEAD"),
        "command": shlex.join(sys.argv), "started": stamp,
        "commit": want_commit, "commit_short": (want_commit or "unknown")[:9],
        "commit_subject": git("log", "-1", "--format=%s", want_commit) if want_commit else None,
        "vm": vm_name, "vm_memory_mb": args.memory_mb, "vm_cpus": args.cpus, "gl": args.gl,
        "slot": args.slot, "outputs": outputs, "budgets": BUDGETS,
        "host_mem_available_mb_at_start": mem_available_mb(),
    }
    results: list[core.Result] = []
    owners = report.owners_from(args.owners)
    try:
        cmd = None
        if args.disk:
            existing = (vms_toml().get("vms") or {}).get(vm_name)
            if existing is None:
                cmd = ["create", vm_name, "--image", str(disk), "--heads", str(len(outputs)),
                       "--resolution", outputs[0].split("@")[0], "--memory-mb", str(args.memory_mb),
                       "--cpus", str(args.cpus)] + (["--gl"] if args.gl else [])
                created = True
        management = start_management(vm, startup_vm,
                                      args.bootstrap_from.resolve() if args.bootstrap_from else None,
                                      swap_ref, cmd)
        meta["vm_up"] = dt.datetime.now(dt.UTC).isoformat(timespec="seconds")
        facts = {"provenance": provenance_of(vm, disk, want_commit)}
        if management:
            facts["provenance"]["swap"] = management
        meta["provenance"] = facts["provenance"]
        bootstrap_session(vm, outputs,
                          {"components": swaps, "ref": swap_ref, "source": swap_from},
                          facts["provenance"])
        if management and swaps:
            swapped = facts["provenance"]["swap"]
            if swapped["commit"] != management["commit"]:
                raise VmError("management and session swaps have different source commits")
            swapped["components"] = management["components"] + swapped["components"]
            swapped["slot_layers"].update(management["slot_layers"])
        time.sleep(5)
        facts["renderer"], facts["display"] = renderer(vm)
        meta["renderer"], meta["display"] = facts["renderer"], facts["display"]
        config = {"budgets": BUDGETS, "outputs": outputs, "idle_seconds": args.idle_seconds, "locale": "en"}
        ctx = Context(vm, out, config)
        ctx.facts.update(facts)
        de = "de" in args.locales.split(",")
        if not de:
            ctx.facts["locale_de:why"] = "the German pass was not requested (--locales)"
        selected = select(args.only)
        by_id = {c.id: c for c in core.REGISTRY}
        german = de
        main_ids = [i for i in ORDER + list(DE_PASS) if i in selected and (i not in DE_PASS or not german)]
        for cid in main_ids:
            r = core.run_check(ctx, by_id[cid])
            results.append(r)
            print(f"{r.id} {r.status.upper():4} {r.seconds:6.1f}s  {r.title}" + (f"\n      {r.detail}" if r.detail else ""),
                  flush=True)
        de_ids = [i for i in DE_PASS if i in selected and german]
        if de_ids:
            try:
                switch_locale(vm, "de_DE.UTF-8")
                ctx.facts["locale_de"] = True
                ctx.config["locale"] = "de"
            except VmError as exc:
                ctx.facts["locale_de:why"] = f"the session could not be restarted in German: {exc.reason()}"
            for cid in de_ids:
                r = core.run_check(ctx, by_id[cid])
                results.append(r)
                print(f"{r.id} {r.status.upper():4} {r.seconds:6.1f}s  {r.title}" + (f"\n      {r.detail}" if r.detail else ""),
                      flush=True)
    except (VmError, core.CheckFailed) as exc:
        meta["aborted"] = str(exc)
        print(f"ABORTED: {exc}", file=sys.stderr)
        results.append(core.Result("P00", "provenance", "The acceptance run could reach a working session",
                                   status="fail", detail=str(exc), unobservable=True))
    finally:
        meta["finished"] = dt.datetime.now(dt.UTC).strftime("%Y%m%dT%H%M%SZ")
        if not args.keep:
            try:
                vm.vm_cmd("stop", vm_name, timeout=300, check=False)
                if created:
                    vm.vm_cmd("rm", vm_name, "--force", timeout=300, check=False)
            except VmError as exc:
                meta["cleanup_error"] = str(exc)
        doc = report.write(out, meta, results, owners)
    print(f"\n{doc['overall'].upper()}: {sum(r.status == 'pass' for r in results)}/{len(results)} checks pass")
    print(f"report: {out / 'report.md'}")
    return 0 if doc["overall"] == "pass" else 1


PROVIDERS = {"two_windows": "W01", "workspaces": "W10"}


def select(only: str | None) -> set[str]:
    ids = {c.id for c in core.REGISTRY}
    if not only:
        return ids
    want = {w.strip() for w in only.split(",")}
    chosen = {c.id for c in core.REGISTRY if c.id in want or c.area in want}
    # A check that needs a fact runs after the check that establishes it.
    for c in core.REGISTRY:
        if c.id in chosen:
            chosen |= {PROVIDERS[n] for n in c.needs if n in PROVIDERS}
    return chosen | ({"P01", "P02"} & ids)


if __name__ == "__main__":
    sys.exit(main())
