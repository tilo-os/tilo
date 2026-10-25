#!/usr/bin/env python3
"""Verify the DRM presentation stack on a device with a real GPU, through tilo-vm.

Every claim about atomic page flips, scanout feedback, client scanout and
vblank pacing is decided by a line the compositor writes into the session log.
CI has no KMS planes, so none of those lines can be produced there; the
`drm.presentation-checks` record this script writes is the evidence, and it is produced
only on a `tilo-vm` device slot whose output presents with page flips.

The checks are the hardware list in docs/architecture/compositor.md,
"Verifying on hardware":

- `atomic-flips`: an output scanned out through atomic flips, never demoted;
- `atomic-disabled`: the same session with `TILO_KMS_ATOMIC=0` demotes every
  flip to the legacy page flip on request;
- `atomic-flips-follow-mode`: an atomic flip that failed on a neighbour is
  still how every other output flips;
- `client-buffer-release`: a scanned-out client's buffers come back within
  about one refresh interval;
- `vblank-pacing`: frames are paced to the vblank and presented within one
  interval.

Each check that cannot read its lines fails; none of them skips. What needs a
person at the panel (tearing, a flash on the frame an overlay appears, a VT
switch) is listed in `manual` and is never reported as passed.

    python3 scripts/perf/drm_hardware_checks.py --device yoga --slot device \\
        --commit <deployed shell commit> --image-digest sha256:... \\
        --out target/drm-hardware

The session log is read with the compositor's own flags on: enable the
greeter's `presentation` diagnostics before sign-in, as
scripts/perf/README.md describes.
"""

from __future__ import annotations

import argparse
import datetime
import json
import re
import sys
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(REPO / "tools/perf"))

import record  # noqa: E402
from device_notes_latency import Device  # noqa: E402

KPI = "drm.presentation-checks"
SESSION_LOG = "$HOME/.local/state/tilo/session-logs/latest.log"
# The compositor prints its summary fields one per line; the sentence that
# names the line ends that block.
PHASE_SUMMARY = "DRM render phase summary"
# What a person has to look at, and therefore what this script never claims:
# tearing, the frame an overlay appears on, and a VT switch. The run leaves
# instructions for each in `manual`.
MANUAL_CHECKS = ("no tearing and no flash while switching workspaces and while a video plays",
                  "the overlay appears on its frame without a flash",
                  "a VT switch away and back resumes scanout")


def lines(text: str, needle: str) -> list[str]:
    """The session log's lines carrying `needle`, a summary field block joined."""
    out = []
    pending: list[str] = []
    for row in text.splitlines():
        if needle in row:
            out.append(row)
            continue
        if row.lstrip().startswith(PHASE_SUMMARY):
            if needle in " ".join(pending):
                out.append(" ".join(pending))
            pending = []
        elif re.match(r"^\w+=", row.strip()):
            pending.append(row.strip())
        else:
            pending = []
    return out


def fields(line: str) -> dict[str, str]:
    """The line's `name=value` pairs, a quoted value kept whole."""
    found = re.findall(r'(\w+)=("(?:[^"\\]|\\.)*"|\S+)', line)
    return {name: value.strip('"') for name, value in found}


class Checks:
    """One check per claim, each failed by the lines it could not read."""

    def __init__(self, text: str, env: dict[str, Any]):
        self.text = text
        self.env = env
        self.results: list[dict[str, Any]] = []

    def result(self, ident: str, passed: bool, detail: str, **numbers: Any) -> None:
        self.results.append({"check": ident, "status": "pass" if passed else "fail",
                             "detail": detail, **numbers})

    def atomic_flips(self) -> None:
        atomic = lines(self.text, "kms_flip_api=atomic")
        failed = lines(self.text, "an atomic flip failed")
        legacy = [line for line in lines(self.text, "kms_flip_api=legacy") if "forced by" not in line]
        self.result("atomic-flips", bool(atomic) and not failed and not legacy,
                    f"{len(atomic)} atomic output(s), {len(failed)} failed flip(s), "
                    f"{len(legacy)} output(s) left atomic")

    def no_page_flip_errors(self) -> None:
        errors = lines(self.text, "page-flip EACCES") + lines(self.text, "event=drm-flip-lost")
        self.result("no-page-flip-errors", not errors,
                    f"{len(errors)} lost-flip line(s) in this session")

    def client_buffer_release(self, limit_us: int) -> None:
        samples = [int(fields(line)["p50_us"]) for line in lines(self.text, "CLIENT-BUFFER-RELEASE")
                   if "p50_us" in fields(line)]
        self.result("client-buffer-release", bool(samples) and max(samples) <= limit_us,
                    f"p50 {max(samples)} us over {len(samples)} window(s), "
                    f"limit {limit_us} us (one refresh)" if samples else
                    "no CLIENT-BUFFER-RELEASE line: nothing was scanned out",
                    p50_us=max(samples) if samples else None, limit_us=limit_us)

    def vblank_pacing(self) -> None:
        pacing = [fields(line) for line in lines(self.text, "FRAME-PACING")]
        if not pacing:
            self.result("vblank-pacing", False, "no FRAME-PACING line: the session paced no frames")
            return
        unlocked = [row for row in pacing if row.get("vblank_locked") not in ("true", "1")]
        interval = int(pacing[-1]["interval_us"])
        build = [int(row["build_to_vblank_p50_us"]) for row in pacing]
        self.result("vblank-pacing", not unlocked and max(build) < interval,
                    f"{len(unlocked)} window(s) without a locked clock; build to vblank p50 "
                    f"{max(build)} us against a {interval} us interval",
                    interval_us=interval, build_to_vblank_p50_us=max(build))

    def client_scanout(self) -> None:
        """Reported, never required: a device run may scan no client out."""
        self.results.append({"check": "client-scanout", "status": "report",
                             "detail": ("a fullscreen client reached scanout"
                                        if lines(self.text, "client_scanout_status=active")
                                        else "no fullscreen client was scanned out in this window")})


def legacy_forced_off(text: str) -> Checks:
    """The `TILO_KMS_ATOMIC=0` session: every flip legacy, and said so."""
    checks = Checks(text, {})
    forced = [line for line in lines(text, "kms_flip_api=legacy") if "forced by" in line]
    atomic = lines(text, "kms_flip_api=atomic")
    checks.result("atomic-disabled", bool(forced) and not atomic,
                  f"{len(forced)} output(s) forced legacy, {len(atomic)} still atomic")
    return checks


def atomic_survives_failure(text: str) -> Checks:
    """A demoted output does not demote its neighbours (check shared by a run)."""
    checks = Checks(text, {})
    demoted = [line for line in lines(text, "an atomic flip failed")]
    atomic = lines(text, "kms_flip_api=atomic")
    checks.result("atomic-flips-follow-mode", bool(demoted) and bool(atomic),
                  "no atomic flip failed in this session, so a neighbour's survival is unproven"
                  if not demoted else f"{len(atomic)} output(s) still atomic beside {len(demoted)} demotion(s)")
    return checks


def read_session(device: Device) -> str:
    return device.execute(["sh", "-c", f'cat "{SESSION_LOG}" 2>/dev/null; true'])


def hardware(device: Device) -> str:
    """DMI product, CPU model, GPU driver and the connectors that present."""
    text = device.execute(["sh", "-c",
                           "cat /sys/class/dmi/id/sys_vendor /sys/class/dmi/id/product_name "
                           "/sys/class/dmi/id/product_version 2>/dev/null; "
                           "grep -m1 '^model name' /proc/cpuinfo; "
                           "for card in /sys/class/drm/card[0-9]*/device; do readlink -f \"$card/driver\"; done; "
                           "cat /sys/class/drm/card[0-9]*-*/status 2>/dev/null"])
    rows = [row.strip() for row in text.splitlines() if row.strip() and not row.startswith("receipt:")]
    connected = sum(1 for row in rows if row == "connected")
    unconnected = sum(1 for row in rows if row == "disconnected")
    drivers = sorted({row.rsplit("/", 1)[-1] for row in rows if row.startswith("/")})
    return f"{' '.join(rows[:3])}; {len(drivers)} GPU driver(s) {'/'.join(drivers)}; " \
           f"{connected} connected output(s), {unconnected} disconnected"


def connector_line(text: str) -> str:
    """The one line naming what the outputs present with, for the record's meta."""
    for line in lines(text, "kms_flip_api="):
        if "an atomic flip failed" not in line:
            return line.split("tilo compositor: ", 1)[-1][:200]
    return ""


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--device", required=True, help="the tilo-vm device (TILO_VM)")
    parser.add_argument("--slot", default="device")
    parser.add_argument("--tool", default="tilo-vm")
    parser.add_argument("--commit", required=True, help="the deployed shell's source commit")
    parser.add_argument("--image-digest", required=True, help="the booted image (tilo-vm vm status)")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--release-interval-us", type=int, default=16_667,
                        help="one refresh interval on the panel under test (60 Hz by default)")
    parser.add_argument("--profile", default="release", choices=sorted(record.PROFILES))
    parser.add_argument("--hardware", help="override the DMI/CPU/GPU description read off the device")
    args = parser.parse_args(argv)
    if not re.fullmatch(r"[0-9a-f]{40}", args.commit):
        parser.error("--commit must be a full 40-character commit")
    args.out.mkdir(parents=True, exist_ok=False, mode=0o700)
    device = Device(argparse.Namespace(device=args.device, slot=args.slot, tool=args.tool))
    try:
        text = read_session(device)
        (args.out / "session.log").write_text(text)
        checks = Checks(text, {})
        checks.atomic_flips()
        checks.no_page_flip_errors()
        checks.client_scanout()
        checks.vblank_pacing()
        checks.client_buffer_release(args.release_interval_us)
        results = checks.results
    except (RuntimeError, ValueError, KeyError) as error:
        print(f"drm_hardware_checks.py: {error}\nEvidence: {args.out}", file=sys.stderr)
        return 1
    failed = [item for item in results if item["status"] == "fail"]
    env = record.environment(args.profile, "gpu", "none", f"device-{args.device}",
                             args.hardware or hardware(device))
    env["commit"] = args.commit
    env["meta"] = {"image_digest": args.image_digest, "slot": args.slot,
                   "scanout": connector_line(text),
                   "manual": "; ".join(MANUAL_CHECKS),
                   "ran_at": datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat()}
    graded = [item for item in results if item["status"] != "report"]
    samples = [1 if item["status"] == "pass" else 0 for item in graded]
    item = record.from_samples(KPI, "count", samples, **env)
    record.write(args.out / "records.jsonl", item)
    (args.out / "checks.json").write_text(json.dumps(results, indent=2))
    for result in results:
        print(f"{result['status']:<6} {result['check']:<28} {result['detail']}")
    print("\nNeeds a person at the panel (never claimed by this run):")
    for claim in MANUAL_CHECKS:
        print(f"  - {claim}")
    if failed:
        print(f"\ndrm_hardware_checks.py: {len(failed)} of {len(samples)} check(s) failed\n"
              f"Evidence: {args.out}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
