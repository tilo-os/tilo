#!/usr/bin/env python3
"""The nextest filter that leaves out tests whose capability this machine lacks.

    capability_filter.py [--packages NAME...] [--probe-output FILE]   # the filter
    capability_filter.py --skip-patterns [--packages NAME...]         # libtest skips
    capability_filter.py --require-variables                          # KEY=1 lines

Reads .config/nextest-capabilities.toml. Without --probe-output it runs
tilo-test-env's `tilo-capability-probe` program as the tests' runner starts a
test binary (tools/cargo-runner.sh: in the test environment), here, where the
tests run, and reads its report. A run t hands the program
(t.toml's `starts`, NEXTEST_BIN_EXE_tilo-capability-probe) runs that build of
it and compiles nothing; any other builds it with Cargo first. For
each test group whose probe failed it prints a line to stderr naming the
capability, the reason and the t.toml check that runs the group, and the
filter it prints on stdout leaves that group out, so nextest counts those
tests as not run. The filter is empty when nothing is left out.

--packages names the packages a run tests (every package when given none): a
run of none of the manifest's packages prints nothing and probes nothing, so
the ordinary job pays no probe. tools/ci/nextest.sh gives it what the run
selects, which is how a run leaves these tests out wherever it is started.

--skip-patterns answers the same question for libtest, which takes `--skip`
substring patterns instead of nextest's filtersets: it prints a line
`package<TAB>pattern` per test group whose probe failed, the pattern being the
group's filter term as a libtest skip. tools/test-env.sh gives the patterns to
tools/cargo-runner.sh beside the test environment, so a plain `cargo test`
leaves those tests out as a nextest run does, and starts none of them. Only a
filter a libtest skip can express is accepted: a `test(=NAME)` or a `test(/regex/)`
that is a literal, anchored or not at its start. Anything else ends the run
naming the group, rather than leaving a gated test to fail.

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
AS_A_TEST = ["bash", str(ROOT / "tools" / "with-test-environment.sh")]
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


def left_out_groups(capabilities: dict, probes: dict[str, str | None],
                    packages: set[str] | None = None):
    """Each group whose probe failed, with its capability, the group, and the probe's reason."""
    for name, capability in capabilities.items():
        for group in groups({name: capability}, packages):
            reason = probes[group["probe"]]
            if reason is not None:
                yield name, group, reason


def note(name: str, group: dict, reason: str) -> str:
    """The line a run says for a group it left out: what was not run, why, and how to."""
    said = f"not run: tests that need {name} ({group['probe']}: {reason}): {group['filter']}"
    checks = group.get("checks", [])
    if len(checks) == 1:
        said += f"; run by check {checks[0]}"
    elif checks:
        said += f"; run by checks {', '.join(checks)}"
    else:
        said += "; no check names it (a t.toml family runs it)"
    return said


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
    for name, group, reason in left_out_groups(capabilities, probes, packages):
        notes.append(note(name, group, reason))
        left_out.append(f"({group['filter']})")
    return ("not (" + " or ".join(left_out) + ")" if left_out else ""), notes


# A nextest filterset as the manifest writes one: parentheses, `and`, `or`,
# `package(NAME)`, `test(=NAME)` and `test(/regex/)`. Anything else is refused
# rather than read as something it is not.
TOKEN = re.compile(
    r"\s*(?:"
    r"(?P<paren>[()])"
    r"|(?P<operator>and\b|or\b)"
    r"|package\(\s*(?P<package>[^)\s]+)\s*\)"
    r"|test\(\s*=\s*(?P<exact>[^)]+?)\s*\)"
    r"|test\(\s*/(?P<regex>.+?)/\s*\)"
    r")"
)
# What a libtest skip cannot say: the regex constructs a substring cannot
# stand in for, and a trailing anchor, which would skip every test the literal
# begins.
UNTRANSLATABLE = re.compile(r"[\\$.*+?()\[\]{}|]")


def tokens(text: str):
    """The filterset's tokens: ('(',), (')',), ('and',), ('package', name),
    ('exact', name) and ('regex', source)."""
    at = 0
    while at < len(text):
        if text[at:].strip() == "":
            return
        found = TOKEN.match(text, at)
        if found is None:
            sys.exit(f"capability groups are filtersets of package() and test() terms; "
                     f"this one is not: {text[at:][:60]!r}")
        if found.group("paren") is not None:
            yield (found.group("paren"),)
        elif found.group("operator") is not None:
            yield (found.group("operator"),)
        elif found.group("package") is not None:
            yield ("package", found.group("package"))
        elif found.group("exact") is not None:
            yield ("exact", found.group("exact"))
        else:
            yield ("regex", found.group("regex"))
        at = found.end()


def alternatives(text: str) -> list[list[tuple[str, str]]]:
    """The filterset as the alternatives of terms it matches: a list of conjunctions, each
    naming one package (or none) and its tests."""
    stream = list(tokens(text))
    at = 0

    def peek() -> tuple[str, ...]:
        return stream[at] if at < len(stream) else ()

    def parse_or() -> list[list[tuple[str, str]]]:
        nonlocal at
        found = parse_and()
        while peek() == ("or",):
            at += 1
            found += parse_and()
        return found

    def parse_and() -> list[list[tuple[str, str]]]:
        nonlocal at
        found = parse_atom()
        while peek() == ("and",):
            at += 1
            right = parse_atom()
            found = [left + other for left in found for other in right]
        return found

    def parse_atom() -> list[list[tuple[str, str]]]:
        nonlocal at
        token = peek()
        if token == ("(",):
            at += 1
            found = parse_or()
            if peek() != (")",):
                sys.exit(f"capability group has an unclosed parenthesis: {text!r}")
            at += 1
            return found
        if not token or token[0] in ("package", "exact", "regex"):
            if not token:
                sys.exit(f"capability group ends where a term is expected: {text!r}")
            at += 1
            return [[token]]
        sys.exit(f"capability group is not a filterset of package() and test() terms: {text!r}")

    found = parse_or()
    if at != len(stream):
        sys.exit(f"capability group has words after its filter: {text!r}")
    return found


def skip_pattern(term: tuple[str, str]) -> str:
    """A test term as a libtest `--skip` substring."""
    kind, value = term
    if kind == "exact":
        return value
    pattern = value.removeprefix("^")
    if not pattern or UNTRANSLATABLE.search(pattern):
        sys.exit(
            f"capability group names test(/{value}/), which a libtest skip cannot express; "
            f"write its tests as test(=NAME) or as a literal prefix"
        )
    return pattern


def skip_patterns(text: str, packages: set[str] | None = None) -> tuple[dict[str, list[str]], list[str]]:
    """From a probe report: the libtest skips per package that leave out each gated group's
    tests ({} when nothing), and a line per left-out group naming its capability and why."""
    capabilities = manifest()
    probes, variables = probe_report(text)
    if not probes:
        sys.exit("the capability probe reported nothing")
    check(capabilities, probes, variables, packages)
    found: dict[str, list[str]] = {}
    notes = []
    for name, group, reason in left_out_groups(capabilities, probes, packages):
        notes.append(note(name, group, reason))
        for terms in alternatives(group["filter"]):
            named = [value for kind, value in terms if kind == "package"]
            if len(named) != 1:
                sys.exit(
                    f"capability group {group['filter']!r} must name one package per alternative "
                    f"to be a libtest skip"
                )
            if packages is not None and named[0] not in packages:
                continue
            skips = found.setdefault(named[0], [])
            for term in terms:
                if term[0] == "package":
                    continue
                pattern = skip_pattern(term)
                if pattern not in skips:
                    skips.append(pattern)
    return found, notes


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
        "--skip-patterns",
        action="store_true",
        help="print libtest skips, as `package<TAB>pattern`, instead of a nextest filter",
    )
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
    if args.skip_patterns:
        patterns, notes = skip_patterns(text, packages)
        for said in notes:
            print(said, file=sys.stderr)
        for package, skips in patterns.items():
            for pattern in skips:
                print(f"{package}\t{pattern}")
        return
    left_out, notes = exclusions(text, packages)
    for said in notes:
        print(said, file=sys.stderr)
    if left_out:
        print(left_out)


if __name__ == "__main__":
    main()
