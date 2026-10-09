#!/usr/bin/env python3
"""Input and workspace-switch latency on reference hardware, through tilo-vm.

The weekly, report-only half of the latency KPIs (TIL-587;
docs/benchmarks/reference-hardware.md names the machines). It drives a
device slot the way a person does, through the kernel input path
(`tilo-vm input`, uinput into libinput), and writes one record per KPI:

- `hw.key_to_present.notes.<hz>hz`: a key typed into a scratch Notes page,
  from the original kernel/libinput event to the KMS presentation of the
  frame that shows it (`device_notes_latency.py`, which owns the scratch page
  and every refusal);
- `hw.workspace_switch.{first_frame,settled,dropped_frames}.<hz>hz`:
  Super+Page Down / Page Up, from the compositor's own `WORKSPACE-SWITCH`
  lines, dated at the page flip's vblank.

`<hz>` is the refresh the compositor paced to, read off the switches. Both
clocks stop at hardware presentation: they exclude peripheral scan time and
panel pixel response, which only a photodiode sees.

    python3 scripts/perf/device_latency_bench.py --device yoga --slot device \\
        --commit <deployed shell commit> --image-digest sha256:... \\
        --out target/device-latency

It never publishes: the records are compared with the store read-only and
written to `<out>/records.jsonl` for the weekly report.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(REPO / "tools/perf"))

import adapters  # noqa: E402
import record  # noqa: E402
from device_notes_latency import Device, capture  # noqa: E402

SESSION_LOG = "$HOME/.local/state/tilo/session-logs/latest.log"
# A switch's frame run is 400 ms; it is read once it has settled, so the
# reading does not compete with the frames it measures.
SWITCH_SETTLE_S = 1.0


def refresh_hz(switches: list[dict[str, int]]) -> int:
    """The refresh the compositor paced every switch to; mixed rates are refused."""
    rates = {round(1_000_000 / sample["interval_us"]) for sample in switches if sample["interval_us"] > 0}
    if len(rates) != 1:
        raise ValueError(f"the switches ran at refresh rates {sorted(rates)}; measure one display mode at a time")
    return rates.pop()


def device_records(switches: list[dict[str, int]], notes_ms: list[float],
                   env: dict[str, Any]) -> list[record.Record]:
    hz = refresh_hz(switches)
    meta = {**env.get("meta", {}), "panel_hz": str(hz), "input": "kernel (tilo-vm input, uinput)"}
    env = {**env, "meta": meta}
    items = adapters.workspace_switch(switches, env, f"hw.workspace_switch.{{}}.{hz}hz")
    if notes_ms:
        base = {name: value for name, value in env.items() if name != "meta"}
        items.insert(0, record.from_samples(f"hw.key_to_present.notes.{hz}hz", "ms", notes_ms, meta=meta, **base))
    return items


class Switcher:
    """Workspace switches on the device's own seat, timed by its compositor."""

    def __init__(self, device: Device, slot: str):
        self.device = device
        self.slot = slot

    def lines(self) -> list[dict[str, int]]:
        text = self.device.execute(["sh", "-c", f'grep -a "WORKSPACE-SWITCH" "{SESSION_LOG}"; true'])
        return adapters.switch_samples(text)

    def key(self, combo: str) -> None:
        self.device.run(["input", "--slot", self.slot, "key", combo])

    def ensure_two_workspaces(self) -> None:
        count = len(self.device.control(["state-dump"])["workspace_tree"]["workspaces"])
        if count >= 2:
            return
        self.device.run(["control", "--slot", self.slot, "actions", "invoke", "actions", "overview.toggle"])
        time.sleep(1.0)
        self.device.run(["control", "--slot", self.slot, "click", "add_workspace"])
        deadline = time.monotonic() + 10
        while len(self.device.control(["state-dump"])["workspace_tree"]["workspaces"]) <= count:
            if time.monotonic() >= deadline:
                raise RuntimeError("the overview's add_workspace control made no workspace")
            time.sleep(0.5)
        if self.device.control(["state-dump"])["chrome"]["overlays"].get("overview"):
            self.key("Escape")
            time.sleep(1.0)

    def run(self, count: int) -> list[dict[str, int]]:
        self.ensure_two_workspaces()
        before = len(self.lines())
        tree = self.device.control(["state-dump"])["workspace_tree"]
        ids = [workspace["id"] for workspace in tree["workspaces"]]
        combos = ["super+pagedown", "super+pageup"]
        if ids.index(tree["active"]) == len(ids) - 1:
            combos.reverse()
        for index in range(count):
            self.key(combos[index % 2])
            time.sleep(SWITCH_SETTLE_S)
            deadline = time.monotonic() + 10
            while len(self.lines()) < before + index + 1:
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"switch {index + 1} of {count}: no WORKSPACE-SWITCH line "
                                       "(is the deployed compositor new enough?)")
                time.sleep(0.5)
            print(f"Workspace switch {index + 1}/{count} timed", flush=True)
        lines = self.lines()[before:]
        if len(lines) != count:
            raise RuntimeError(f"{count} chords produced {len(lines)} WORKSPACE-SWITCH lines")
        return lines


def hardware(device: Device) -> str:
    """DMI product and CPU model of the device, both public."""
    text = device.execute(["sh", "-c",
                           "cat /sys/class/dmi/id/sys_vendor /sys/class/dmi/id/product_name "
                           "/sys/class/dmi/id/product_version 2>/dev/null; "
                           "grep -m1 '^model name' /proc/cpuinfo; nproc"])
    lines = [line.strip() for line in text.splitlines() if line.strip() and not line.startswith("receipt:")]
    cpu = next((line.split(":", 1)[1].strip() for line in lines if line.startswith("model name")), "")
    rest = [line for line in lines if not line.startswith("model name")]
    cores = rest.pop() if rest and rest[-1].isdigit() else "?"
    return f"{' '.join(rest)}; {cpu}; {cores} cpus"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--device", required=True, help="the tilo-vm device (TILO_VM)")
    parser.add_argument("--slot", default="device")
    parser.add_argument("--tool", default="tilo-vm")
    parser.add_argument("--notes-samples", type=int, default=500,
                        help="keys typed into the scratch Notes page; 0 skips typing")
    parser.add_argument("--switches", type=int, default=100)
    parser.add_argument("--commit", required=True, help="the deployed shell's source commit")
    parser.add_argument("--image-digest", required=True, help="the booted image (tilo-vm vm status)")
    parser.add_argument("--profile", default="release", choices=sorted(record.PROFILES))
    parser.add_argument("--hardware", help="override the DMI/CPU description read off the device")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--no-compare", action="store_true", help="skip the read-only store comparison")
    args = parser.parse_args(argv)
    if not re.fullmatch(r"[0-9a-f]{40}", args.commit):
        parser.error("--commit must be a full 40-character commit")
    if args.switches < 1 or not 0 <= args.notes_samples <= 1000:
        parser.error("--switches must be positive and --notes-samples within 0..1000")
    args.out.mkdir(parents=True, exist_ok=False, mode=0o700)
    device = Device(argparse.Namespace(device=args.device, slot=args.slot, tool=args.tool))
    try:
        notes_ms: list[float] = []
        if args.notes_samples:
            notes_out = args.out / "notes"
            notes_out.mkdir(mode=0o700)
            report = capture(device, argparse.Namespace(samples=args.notes_samples, out=notes_out,
                                                        slot=args.slot, device=args.device))
            (notes_out / "report.json").write_text(json.dumps(report, indent=2))
            notes_ms = [sample["latency_ms"] for sample in report["samples"]]
            if len(notes_ms) != args.notes_samples:
                raise ValueError(f"{args.notes_samples} keys produced {len(notes_ms)} samples")
        switches = Switcher(device, args.slot).run(args.switches)
        (args.out / "switches.json").write_text(json.dumps(switches, indent=2))
        env = record.environment(args.profile, "gpu", "none", f"device-{args.device}",
                                 args.hardware or hardware(device))
        env["commit"] = args.commit
        env["meta"] = {"image_digest": args.image_digest, "slot": args.slot}
        items = device_records(switches, notes_ms, env)
    except (RuntimeError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        print(f"device_latency_bench.py: {error}\nEvidence: {args.out}", file=sys.stderr)
        return 1
    records_path = args.out / "records.jsonl"
    for item in items:
        record.write(records_path, item)
        print(item.to_json())
    if args.no_compare:
        return 0
    # Read-only: the store is consulted, nothing is uploaded. No --groups: one
    # run measures one refresh rate, so the other rate's budgets are not missing.
    return subprocess.run([sys.executable, str(REPO / "tools/perf/compare.py"), "--results", str(records_path),
                           "--store", "https://code.tilo.so", "--summary", str(args.out / "summary.md")],
                          check=False).returncode


if __name__ == "__main__":
    sys.exit(main())
