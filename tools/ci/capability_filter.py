#!/usr/bin/env python3
"""The nextest filter that leaves out tests whose capability this machine lacks.

    capability_filter.py [--probe-output FILE]        # prints the filter
    capability_filter.py --require-variables          # prints KEY=1 lines

Reads .config/nextest-capabilities.toml. Without --probe-output it runs the
`capability_probe` test of tilo-test-env through `cargo nextest` (so it runs
where the tests run, a remote builder included) and reads its report. For
each test group whose probe failed it prints a line to stderr naming the
capability and the reason, and the filter it prints on stdout leaves that
group out, so nextest counts those tests as not run. The filter is empty when
nothing is left out.

--require-variables prints the TILO_REQUIRE_* variable of every mapped
capability: the ci profile's setup sets them, so a test of a mapped
capability fails where the capability is missing instead of skipping.
"""

import argparse
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / ".config" / "nextest-capabilities.toml"
PROBE = [
    "cargo", "nextest", "run", "--locked", "--profile", "ci",
    "-p", "tilo-test-env", "--test", "capability_probe", "--no-capture",
]


def manifest() -> dict:
    with MANIFEST.open("rb") as handle:
        return tomllib.load(handle).get("capabilities", {})


def probe_report(text: str) -> tuple[dict[str, str | None], dict[str, str]]:
    """Probe name -> None when available, else why not; capability -> variable."""
    probes: dict[str, str | None] = {}
    variables: dict[str, str] = {}
    for line in text.splitlines():
        words = line.strip().split(" ", 3)
        if words[0] == "tilo-capability-variable" and len(words) >= 3:
            variables[words[1]] = words[2]
        elif words[0] == "tilo-capability-probe" and len(words) >= 3:
            status = " ".join(words[2:])
            probes[words[1]] = None if status == "available" else status.removeprefix("unavailable: ")
    return probes, variables


def check(capabilities: dict, probes: dict, variables: dict) -> None:
    for name, capability in capabilities.items():
        if variables and variables.get(name) != capability.get("variable"):
            sys.exit(f"{MANIFEST}: {name}'s variable is {variables.get(name)!r} in tilo-test-env")
        for group in capability.get("tests", []):
            if group["probe"] not in probes:
                sys.exit(f"{MANIFEST}: {name} names probe {group['probe']!r}, which tilo-test-env does not have")


def exclusions(text: str) -> tuple[str, list[str]]:
    """From a probe report: the filter leaving out what may not run here ('' when nothing),
    and a line per left-out group naming its capability and why."""
    capabilities = manifest()
    probes, variables = probe_report(text)
    if not probes:
        sys.exit("the capability probe reported nothing")
    check(capabilities, probes, variables)
    left_out, notes = [], []
    for name, capability in capabilities.items():
        for group in capability.get("tests", []):
            reason = probes[group["probe"]]
            if reason is not None:
                notes.append(f"not run: tests that need {name} ({group['probe']}: {reason}): {group['filter']}")
                left_out.append(f"({group['filter']})")
    return ("not (" + " or ".join(left_out) + ")" if left_out else ""), notes


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe-output", type=Path)
    parser.add_argument("--require-variables", action="store_true")
    args = parser.parse_args()
    if args.require_variables:
        for capability in manifest().values():
            if capability.get("mapped"):
                print(f"{capability['variable']}=1")
        return
    if args.probe_output:
        text = args.probe_output.read_text()
    else:
        run = subprocess.run(PROBE, cwd=ROOT, capture_output=True, text=True)
        text = run.stdout + run.stderr
        if run.returncode != 0:
            sys.stderr.write(text)
            sys.exit("the capability probe did not run")
    left_out, notes = exclusions(text)
    for note in notes:
        print(note, file=sys.stderr)
    if left_out:
        print(left_out)


if __name__ == "__main__":
    main()
