#!/usr/bin/env python3
"""The nextest filter that leaves out tests whose capability this machine lacks.

    capability_filter.py [--packages NAME...] [--probe-output FILE]   # the filter
    capability_filter.py --require-variables                          # KEY=1 lines

Reads .config/nextest-capabilities.toml. Without --probe-output it runs
tilo-test-env's `tilo-capability-probe` program as the tests' runner starts a
test binary (tools/cargo-runner.sh: in the test environment, as the test user),
here, where the tests run, and reads its report. A run t hands the program
(t.toml's `starts`, NEXTEST_BIN_EXE_tilo-capability-probe) runs that build of
it and compiles nothing; any other builds it with Cargo first. For
each test group whose probe failed it prints a line to stderr naming the
capability and the reason, and the filter it prints on stdout leaves that
group out, so nextest counts those tests as not run. The filter is empty when
nothing is left out.

--packages names the packages a run tests (every package when given none): a
run of none of the manifest's packages prints nothing and probes nothing, so
the ordinary job pays no probe. tools/ci/nextest.sh gives it what the run
selects, which is how a run leaves these tests out wherever it is started.

--require-variables prints the TILO_REQUIRE_* variable of every mapped
capability: the ci profile's setup sets them, so a test of a mapped
capability fails where the capability is missing instead of skipping.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / ".config" / "nextest-capabilities.toml"
PROBE = "tilo-capability-probe"
# Where t's `starts` hands the family's build of it to a run.
HANDED = f"NEXTEST_BIN_EXE_{PROBE}"
BUILD = [
    "build", "--locked", "-p", "tilo-test-env", "--bin", PROBE,
    "--message-format", "json-render-diagnostics",
]
# How tools/cargo-runner.sh starts a test binary, so the probe sees what the tests will.
AS_A_TEST = [
    "bash", str(ROOT / "tools" / "with-test-environment.sh"),
    "bash", str(ROOT / "tools" / "as-test-user.sh"),
]
# The packages a group's filter names (`.config/nextest-capabilities.toml`).
PACKAGE = re.compile(r"package\(\s*([^)\s]+)\s*\)")


def manifest() -> dict:
    with MANIFEST.open("rb") as handle:
        return tomllib.load(handle).get("capabilities", {})


def groups(capabilities: dict, packages: set[str] | None = None) -> list[dict]:
    """Every test group, or only those naming one of `packages`."""
    found = [
        group for capability in capabilities.values() for group in capability.get("tests", [])
    ]
    if packages is None:
        return found
    return [group for group in found if packages & set(PACKAGE.findall(group["filter"]))]


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


def check(capabilities: dict, probes: dict, variables: dict, packages: set[str] | None) -> None:
    for name, capability in capabilities.items():
        if variables and variables.get(name) != capability.get("variable"):
            sys.exit(f"{MANIFEST}: {name}'s variable is {variables.get(name)!r} in tilo-test-env")
        for group in groups({name: capability}, packages):
            if group["probe"] not in probes:
                sys.exit(f"{MANIFEST}: {name} names probe {group['probe']!r}, which tilo-test-env does not have")


def exclusions(text: str, packages: set[str] | None = None) -> tuple[str, list[str]]:
    """From a probe report: the filter leaving out what may not run here ('' when nothing),
    and a line per left-out group naming its capability and why."""
    capabilities = manifest()
    probes, variables = probe_report(text)
    if not probes:
        sys.exit("the capability probe reported nothing")
    check(capabilities, probes, variables, packages)
    left_out, notes = [], []
    for name, capability in capabilities.items():
        for group in groups({name: capability}, packages):
            reason = probes[group["probe"]]
            if reason is not None:
                notes.append(f"not run: tests that need {name} ({group['probe']}: {reason}): {group['filter']}")
                left_out.append(f"({group['filter']})")
    return ("not (" + " or ".join(left_out) + ")" if left_out else ""), notes


def probe_program() -> str:
    """The probe's executable: the one t handed the run, else Cargo's build of it. A handed one
    that can't be run is the run's fault, said as such: building it here instead would hide that
    and compile ~765 units in every shard again."""
    handed = os.environ.get(HANDED)
    if handed is not None:
        if not os.access(handed, os.X_OK):
            sys.exit(f"{HANDED} names {handed!r}, which is not an executable")
        return handed
    build = subprocess.run(
        ["cargo", *BUILD], cwd=ROOT, capture_output=True, text=True
    )
    if build.returncode != 0:
        sys.stderr.write(build.stderr)
        sys.exit("the capability probe did not build")
    for line in build.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message["target"]["name"] == PROBE:
            if message.get("executable"):
                return message["executable"]
    sys.exit(f"cargo named no executable for {PROBE}")


def run_probe() -> str:
    run = subprocess.run([*AS_A_TEST, probe_program()], cwd=ROOT, capture_output=True, text=True)
    if run.returncode != 0:
        sys.stderr.write(run.stdout + run.stderr)
        sys.exit("the capability probe did not run")
    return run.stdout


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe-output", type=Path)
    parser.add_argument("--require-variables", action="store_true")
    parser.add_argument(
        "--packages",
        nargs="*",
        default=None,
        help="the packages the run tests; the manifest's packages when none are named",
    )
    args = parser.parse_args()
    if args.require_variables:
        for capability in manifest().values():
            if capability.get("mapped"):
                print(f"{capability['variable']}=1")
        return
    packages = set(args.packages) if args.packages else None
    if packages is not None and not groups(manifest(), packages):
        # None of this run's packages has a test group here: nothing to probe,
        # nothing to leave out.
        return
    text = args.probe_output.read_text() if args.probe_output else run_probe()
    left_out, notes = exclusions(text, packages)
    for note in notes:
        print(note, file=sys.stderr)
    if left_out:
        print(left_out)


if __name__ == "__main__":
    main()
