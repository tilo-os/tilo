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
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from acceptance import core, records, report  # noqa: E402
from acceptance.checks import (agent, apps, basics, clock, daily_apps,  # noqa: E402,F401
                               input_devices, provenance, resources, search, startup, ui, window)
from acceptance.context import Context  # noqa: E402
from management import start_management  # noqa: E402
from layer_artifacts import identities, verify as verify_artifacts  # noqa: E402
from acceptance.vm import TiloVm, VmError, first_json  # noqa: E402

REPO = HERE.parents[1]

BUDGETS = {
    # Idle: a still desktop parks the compositor; the clock may repaint once a minute.
    "idle_shell_cpu_pct": 1.0, "idle_frames": 2, "idle_user_cpu_pct": 3.0,
    "idle_apps_shell_cpu_pct": 1.5, "idle_apps_frames": 2, "idle_apps_user_cpu_pct": 5.0,
    "idle_loop_ticks_per_s": 2.0,
    # On a software-rendered VM the compositor's CPU is judged without its CPU rasteriser's
    # threads, whose work a GPU does on a real desktop. With no frame painted in
    # the window those threads have nothing to do and must stay idle.
    "idle_software_raster_cpu_pct": 0.1,
    # Terminal's shell runtime with its shell running and nothing typed (R02, #4057):
    # every `tilo-ush` process summed, CPU and context switches of all their threads.
    # Measured 0.0% and 0.0/s (run 91757) once the egress DNS stub's threads block
    # on their sockets; before, its 200 ms stop-flag polls were all
    # 10.7/s (run 91741). 1/s is 30 wakeups in the 30 s window.
    "idle_apps_ush_cpu_pct": 1.0, "idle_apps_ush_wakeups_per_s": 1.0,
    # A natively committed workspace with Notes and Files open and nothing edited (R09): the
    # compositor's collaboration broker threads summed, and every app's collaboration shovel.
    # The broker holds each committed read's wait and re-checks it when an identity or
    # membership input announces a change: 0.2/s measured headless with watched identity
    # files, 21-24/s on the 200 ms re-check that runs when they cannot be watched. The
    # shovels carry nothing while nobody edits.
    "idle_shared_broker_wakeups_per_s": 1.0, "idle_shared_shovel_wakeups_per_s": 0.0,
    # Memory. An app is budgeted by its own cgroup (memory.current, and its anon share);
    # processes and the compositor by anonymous RSS, the memory they hold privately.
    "app_memory_mb": 400, "app_anon_mb": 250,
    "shell_anon_mb": 500, "process_anon_mb": 300, "session_memory_mb": 2500,
    # The compositor with no app open (R01): 238.7 MB on main before its freed
    # heap was trimmed after startup and quiet moments and before its renderer stopped
    # initialising GL beside Vulkan; 159-167 MB after (runs 94419, 94637, lavapipe).
    "shell_idle_anon_mb": 200,
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
    # The installed-desktop gates build-image.yml runs on a KVM guest: kernel and userspace
    # to a usable greeter (whole seconds, the boot gate's unit), and the render-path gate's
    # per-frame presentation and main-thread work on a software renderer.
    "boot_to_greeter_s": 20, "render_frame_ms": 1000, "render_frame_work_ms": 100,
    # Userspace start to greetd active (R06), and the greeter's sign-in request to a drawn
    # desktop whose control socket answers (R08): the account's first sign-in, with cold
    # caches and first-login setup, and the next one. The same disk measured 4.4-4.5 s and
    # 6.8-8.8 s to the greeter and 3.7-5.7 s to the desktop on runners of one class with
    # no CPU steal, so these catch a stall of seconds, not drift; the records carry the
    # trend. Software-rendered: the first frame waits on lavapipe compiling the renderer.
    "userspace_to_greeter_s": 10.0, "sign_in_first_usable_s": 10.0, "sign_in_again_usable_s": 10.0,
}

# Run in a German session (a second slot), after the main pass.
DE_PASS = ("U03", "U04", "S03")
# A14 runs before anything else opens Files, so it times the session's first launch. R05
# runs last but P03, so the adapter lines of every app the run opened are in the log; R07
# and A16 judge everything the run rendered and launched. R06 reads this boot's
# timestamps, so it may run anywhere. A15 times apps R02 has already opened once. R09
# shares a workspace on a relay it starts for the run, so it comes late, but before R07, A16
# and R05, which judge the apps it opens too. R08 signs the dev account in at seat0's
# greeter, which starts a second compositor: it runs after every check that measures or
# renders the slot's session. K01 sets the VM's clock back and restarts the session, so it
# runs after the checks that judge what the run opened, and before R08.
ORDER = ["P01", "P02", "R06", "R01", "A14", "R02", "R03", "A15", "A17", "R04",
         "W01", "W02", "W03", "W04", "W05", "W06", "W07", "W08", "W09", "W10", "W11", "W12",
         "W13", "W14", "W15", "W16", "W17", "W18", "W19", "W20", "W21",
         "A01", "A02", "A03", "A04", "A05", "A06", "A07",
         "A08", "A09", "A10", "A11", "A12", "A13",
         "S01", "S02", "S04", "S05", "S06", "S07", "S08", "S09",
         "U01", "U02", "B01", "B02", "B03", "B04", "B05", "B06",
         "M01", "M02", "M03", "M04", "I01", "I02", "I03", "I04", "I05", "I06", "I07",
         "R09", "R07", "A16", "R05", "K01", "R08", "P03"]


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
        if not p["booted_digest"]:
            p["why"] = "bootc status has no booted image digest"
            return p
    except (VmError, ValueError) as exc:
        p["why"] = f"could not read bootc status: {exc}"
        return p
    if record is None:
        p["why"] = (f"no provenance record {rec_path} for the VM's disk: build disks with "
                    "scripts/vm-acceptance/build-disk.sh, which writes one")
        return p
    if not isinstance(record.get("commit"), str) or not record["commit"]:
        p["why"] = "the disk provenance record has no commit"
        return p
    if want_commit and not record["commit"].startswith(want_commit[:12]) \
            and not want_commit.startswith(record["commit"][:12]):
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


def renderer(vm: TiloVm, without_3d: bool = False) -> tuple[str, dict]:
    """The renderer as a readable line, and whether frames reach real display hardware.

    `without_3d`: this run created the VM without virgl, so its virtio GPU renders
    nothing and every frame is a CPU rasterizer's. Mesa's dri2 warning is no proof
    of that: a session pinned to lavapipe never asks Mesa's EGL for that GPU."""
    out = vm.exec(
        "echo nodes: $(ls /dev/dri 2>/dev/null | tr '\\n' ' '); "
        "echo drivers: $(for c in /sys/class/drm/card*/device/driver; do basename $(readlink $c); done 2>/dev/null | sort -u | tr '\\n' ' '); "
        "grep -m4 -iE 'adapter|llvmpipe|lavapipe|virgl|softpipe|failed to create dri2' "
        "\"$HOME/.local/state/tilo/session-logs/latest.log\" 2>/dev/null | cut -c1-160", check=False)
    lines = [l for l in out.splitlines() if l.strip() and not l.startswith("receipt:")]
    text = " | ".join(lines)
    drivers = next((l.split(":", 1)[1].split() for l in lines if l.startswith("drivers:")), [])
    soft = without_3d or "renderD" not in text or "dri2" in text
    hardware = sorted(set(drivers) & HARDWARE_DRM_DRIVERS)
    virgl = any(d.startswith("virtio") for d in drivers)
    label = ("software (no GPU render node; wgpu on a CPU rasterizer): " if soft
             else "GPU (virgl): " if virgl else f"GPU ({', '.join(hardware) or 'unknown driver'}): ")
    display = {"hardware": bool(hardware) and not virgl, "drivers": drivers,
               "why": (f"frames reach {', '.join(hardware)} hardware" if hardware and not virgl
                       else "frames are presented through a virtual display (virtio-gpu, a hypervisor "
                            "framebuffer or a CPU rasterizer), whose timing is the host's")}
    return label + text, display


def slot_create(vm: TiloVm, outputs: list[str], locale: str | None) -> None:
    helptext = vm.run("slot", "create", "--help", slot=False)
    args = ["slot", "create", vm.slot, "--headful", "--outputs", ",".join(outputs)]
    # A throwaway test identity where this tilo-vm can mint one (`slot create --identity`).
    if "--identity" in helptext:
        args.append("--identity")
    if locale:
        args += ["--locale", locale]
    vm.run(*args, timeout=300, slot=False)


def keep_slot_logs(vm: TiloVm, out: Path) -> Path | None:
    """What the slot's compositor and tilo-vmd said, for a run that could not set up its
    session: `tilo-vm logs <slot>`, which the throwaway VM takes with it when it goes."""
    try:
        text = vm.run("logs", vm.slot, timeout=120, check=False, slot=False)
    except VmError as exc:
        text = f"tilo-vm logs {vm.slot} failed: {exc}"
    if not text.strip():
        return None
    path = out / "data" / f"P00-{vm.slot}-logs.txt"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    return path


def write_records(path: Path, samples: list[dict], meta: dict, runner: str) -> int:
    """The run's performance records (acceptance/records.py). They name the commit whose
    code was measured: the swapped ref's when components were swapped, else the disk's.
    An aborted run measured nothing and writes an empty file."""
    if not samples:
        records.write(path, [])
        return 0
    provenance = meta.get("provenance") or {}
    swap = provenance.get("swap") or {}
    base = (provenance.get("record") or {}).get("commit") or meta.get("commit") or "unknown"
    swapped = [c["component"] for c in swap.get("components", []) if "error" not in c]
    guest = f"{meta['vm_cpus']} vCPU, {meta['vm_memory_mb']} MiB, outputs {','.join(meta['outputs'])}"
    env = records.environment(swap.get("commit") if swapped else base,
                              records.renderer(meta.get("renderer"), meta.get("display")), runner, guest)
    run_meta = {"suite": "scripts/vm-acceptance", "base_commit": base,
                "swapped": ",".join(swapped) or "none",
                "image_digest": provenance.get("booted_digest") or "unknown",
                "renderer_line": (meta.get("renderer") or "")[:200]}
    built = records.build(samples, env, run_meta)
    records.write(path, built)
    return len(built)


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


def engine_pins(source: Path) -> list[str]:
    """The engine versions the source's apps pin (`optional-runtimes`), as the
    apps+runtime assembly ships them (os/dev/assemble-app-runtime.py)."""
    pins = set()
    for manifest in sorted((source / "apps").glob("*/tilo-app.toml")):
        package = tomllib.loads(manifest.read_text()).get("package", {})
        pins.update(package.get("optional-runtimes", []))
    return sorted(pin.split("/", 1)[1] for pin in pins if pin.startswith("tilo-runtime/"))


def stage_base_engines(vm: TiloVm, source: Path, destination: Path) -> list[str]:
    """Copy each engine the source's apps pin from the base disk into `destination`,
    where the apps+runtime assembly takes it as one of the host's bundles.

    The engine is built only in an image's browser stage and the registry may not
    offer it, so a CI host has none; the base the cohort is installed on runs the
    same pinned version whenever the pin did not move. The assembly still verifies
    its signature, and still refuses the cohort when no host bundle has the pin."""
    staged = []
    for version in engine_pins(source):
        if vm.root_exec(f"test -f /usr/lib/tilo-runtime/{shlex.quote(version)}/bundle.toml "
                        "&& echo present || true").strip() != "present":
            continue
        destination.mkdir(parents=True, exist_ok=True)
        tar = subprocess.Popen(
            [vm.binary, "vm", "ssh", "--",
             f"tar -C /usr/lib/tilo-runtime -cf - {shlex.quote(version)}"],
            env=dict(os.environ, TILO_VM=vm.vm), stdout=subprocess.PIPE)
        unpacked = subprocess.run(["tar", "-C", str(destination), "-xf", "-"],
                                  stdin=tar.stdout, check=False)
        if tar.stdout:
            tar.stdout.close()
        if tar.wait() == 0 and unpacked.returncode == 0:
            staged.append(version)
    return staged


def swap_components(vm: TiloVm, components: list[str], ref: str, source: Path,
                    artifact: dict | None = None) -> dict:
    """Build each component from `source` and swap it into the VM:
    optional artifacts enter through verified import; the default path builds on the host.
    session components and apps into the run's slot, VM-wide ones into `vm`. Then read back the
    layer each slot runs, so P01 states what was tested: the base image plus these, from `ref`."""
    scopes = component_scopes(source)
    head = git("rev-parse", "HEAD", repo=source)
    if artifact:
        if head != artifact["expected"]["source_commit"]:
            raise ValueError("artifact consumer source checkout does not match producer")
        components = verify_artifacts(artifact["receipt"], artifact["expected"],
                                      artifact["sha256"], components)
    # The artifact consumer verifies the cohort; it may be a newer CLI than the ref's
    # own (the workflow's checkout), so a branch older than a consumer fix stays testable.
    consumer = (artifact or {}).get("consumer") or vm
    swapped, slots = [], set()
    for component in components:
        slot = "vm" if scopes.get(component) == "vm" else vm.slot
        entry = {"component": component, "slot": slot}
        if artifact:
            entry["consumer"] = consumer.binary
        started = time.monotonic()
        try:
            if artifact:
                expected = artifact["expected"]
                out = consumer.run("--json", "swap-artifact", component, "--slot", slot,
                             "--receipt", str(artifact["receipt"]),
                             "--source", expected["source_commit"],
                             "--builder-image", expected["builder_image"],
                             "--run-id", str(expected["run_id"]),
                             "--run-attempt", str(expected["run_attempt"]),
                             "--receipt-sha256", artifact["sha256"],
                             timeout=5400, slot=False, cwd=source)
            else:
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
            shown = whole_json(vm.run("--json", "slot", "show", slot, timeout=120, slot=False))
            layers[slot] = (shown.get("slot") or {}).get("components") or {}
        except VmError as exc:
            layers[slot] = {"error": exc.reason()}
    for entry in swapped:
        entry["layer"] = layers.get(entry["slot"], {}).get(entry["component"])
    return {"ref": ref, "commit": head, "components": swapped, "slot_layers": layers}


def switch_locale(vm: TiloVm, lang: str) -> None:
    """Set the system locale as the installer seeds it (/etc/locale.conf, which the
    session's PAM login inherits from systemd) and restart the slot's session in it."""
    vm.vm_cmd("ssh", "--", f"localectl set-locale LANG={lang} && systemctl daemon-reload", timeout=60)
    vm.restart_session()
    got =vm.root_exec(f"tr '\\0' '\\n' < /proc/$(pgrep -u $(id -u tds-{vm.slot}) -x shell | head -1)/environ "
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
    ap.add_argument("--artifact-receipt", type=Path, help="same-run verified prebuilt layer receipt")
    ap.add_argument("--artifact-source", help="exact producing source commit")
    ap.add_argument("--artifact-builder-image", help="digest-pinned producing CI builder")
    ap.add_argument("--artifact-run-id", type=int, help="producing workflow API run ID")
    ap.add_argument("--artifact-run-attempt", type=int)
    ap.add_argument("--artifact-receipt-sha256", help="receipt hash from authenticated producer output")
    ap.add_argument("--artifact-tilo-vm", help="the tilo-vm that verifies and imports the artifacts "
                    "(default: --tilo-vm); it still runs in, and checks, the --swap-from checkout")
    ap.add_argument("--owners", help="area=owner,... to name who each area routes to")
    ap.add_argument("--records", type=Path, help="write the run's performance records here "
                    "(JSON lines, tools/perf/record.py)")
    ap.add_argument("--runner", default="local", help="the runner class the records name")
    ap.add_argument("--out", type=Path)
    args = ap.parse_args()

    if bool(args.bootstrap_tilo_vm) != bool(args.bootstrap_from):
        ap.error("--bootstrap-tilo-vm and --bootstrap-from must be supplied together")
    binary = find_tilo_vm(args.tilo_vm)
    swaps = [c.strip() for c in (args.swap or "").split(",") if c.strip()]
    swap_from = (args.swap_from or REPO).resolve()
    swap_ref = args.swap_ref or git("rev-parse", "HEAD", repo=swap_from)
    artifact = None
    handoff = (args.artifact_receipt, args.artifact_source, args.artifact_builder_image,
               args.artifact_run_id, args.artifact_run_attempt, args.artifact_receipt_sha256)
    if args.artifact_tilo_vm and args.artifact_receipt is None:
        ap.error("--artifact-tilo-vm needs artifact mode")
    if any(value is not None for value in handoff):
        if not swaps or any(value is None for value in handoff):
            ap.error("artifact mode requires swaps and all six --artifact-* identity arguments")
        expected = identities(args.artifact_source, args.artifact_builder_image,
                              args.artifact_run_id, args.artifact_run_attempt)
        if git("rev-parse", "HEAD", repo=swap_from) != expected["source_commit"]:
            ap.error("artifact source does not match acceptance checkout")
        artifact = dict(receipt=args.artifact_receipt.resolve(), expected=expected,
                        sha256=args.artifact_receipt_sha256)
        verify_artifacts(artifact["receipt"], expected, artifact["sha256"], swaps)
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
    if artifact and args.artifact_tilo_vm:
        # Exactly the named consumer: no fallback to another tilo-vm on PATH.
        if not os.access(args.artifact_tilo_vm, os.X_OK) or not Path(args.artifact_tilo_vm).is_file():
            sys.exit(f"--artifact-tilo-vm {args.artifact_tilo_vm} is not an executable file")
        artifact["consumer"] = TiloVm(args.artifact_tilo_vm, vm_name, args.slot, out / "tilo-vm.log")

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
    ctx = None
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
        try:
            slot_create(vm, outputs, None)
        except VmError:
            logs = keep_slot_logs(vm, out)
            if logs:
                print(f"the slot's logs: {logs}", file=sys.stderr)
            raise
        if swaps:
            # Outside the report: an engine is hundreds of megabytes.
            engines = Path(tempfile.mkdtemp(prefix="tilo-base-engines-"))
            bundles = os.environ.get("TILO_RUNTIME_BUNDLE_DIR")
            try:
                if "apps+runtime" in swaps and not artifact:
                    staged = stage_base_engines(vm, swap_from, engines)
                    if staged:
                        print(f"engines from the base for apps+runtime: {', '.join(staged)}",
                              flush=True)
                        os.environ["TILO_RUNTIME_BUNDLE_DIR"] = str(engines)
                swapped = swap_components(vm, swaps, swap_ref, swap_from, artifact)
            finally:
                if bundles is None:
                    os.environ.pop("TILO_RUNTIME_BUNDLE_DIR", None)
                else:
                    os.environ["TILO_RUNTIME_BUNDLE_DIR"] = bundles
                shutil.rmtree(engines, ignore_errors=True)
            if management:
                if swapped["commit"] != management["commit"]:
                    raise VmError("management and session swaps have different source commits")
                swapped["components"] = management["components"] + swapped["components"]
                swapped["slot_layers"].update(management["slot_layers"])
            facts["provenance"]["swap"] = swapped
        time.sleep(5)
        facts["renderer"], facts["display"] = renderer(vm, without_3d=created and not args.gl)
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
            core.keep_awake(ctx)
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
                core.keep_awake(ctx)
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
        if ctx is not None and ctx.facts.get("session_recovered_after"):
            # The checks after which the harness restarted a session left locked.
            meta["session_recovered_after"] = ctx.facts["session_recovered_after"]
        if ctx is not None and ctx.facts.get("session_restarted_during"):
            # Where the slot's session unit restarted on its own: each lost every window.
            meta["session_restarted_during"] = ctx.facts["session_restarted_during"]
        if ctx is not None and ctx.facts.get("dialog_recovered_after"):
            # The checks that left a dialog open, what it was, and how the harness closed it.
            meta["dialog_recovered_after"] = ctx.facts["dialog_recovered_after"]
        if not args.keep:
            try:
                vm.vm_cmd("stop", vm_name, timeout=300, check=False)
                if created:
                    vm.vm_cmd("rm", vm_name, "--force", timeout=300, check=False)
            except VmError as exc:
                meta["cleanup_error"] = str(exc)
        doc = report.write(out, meta, results, owners)
        if args.records:
            try:
                written = write_records(args.records, ctx.samples if ctx else [], meta, args.runner)
                print(f"records: {written} written to {args.records}")
            except ValueError as exc:
                # No file: the comparison then reports every KPI of the run as missing.
                print(f"records: none written: {exc}", file=sys.stderr)
    expected = sum(r.status == "xfail" for r in results)
    print(f"\n{doc['overall'].upper()}: {sum(r.status == 'pass' for r in results)}/{len(results)} checks pass"
          + (f", {expected} expected failure(s)" if expected else ""))
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
