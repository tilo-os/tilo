#!/usr/bin/env python3
"""Exercise the actual RPM filters before an expensive native package build."""

import importlib.util
import re
import runpy
import unittest
from pathlib import Path
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[2]
PATTERNS = []
runpy.run_path(str(ROOT / "dist/rpm/tilo-session.rpmlintrc"),
               init_globals={"addFilter": PATTERNS.append})
FILTERS = [re.compile(pattern) for pattern in PATTERNS]
RUNTIME_FILTERS = [pattern for pattern in FILTERS if "/usr/lib/tilo-runtime/" in pattern.pattern]
DIGEST = "0123456789abcdef" * 2
NATIVE = "2026.07+native." + DIGEST


def messages(version, package="tilo-session-apps.x86_64"):
    root = "/usr/lib/tilo-runtime/" + version
    return [
        f"{package}: E: invalid-soname {root}/lib/libruntime_dylib.so libruntime_dylib.so",
        f"{package}: E: call-to-mktemp {root}/lib/lib64/libc.so.6",
    ]


class RuntimeFilterContract(unittest.TestCase):
    def test_release_and_native_use_both_existing_exceptions(self):
        # One filter per exception covers both spellings. Separate release and
        # native filters would leave half unused on each valid package set.
        self.assertEqual(len(RUNTIME_FILTERS), 2)
        for version in ("2026.07", NATIVE):
            for package in ("tilo-session-apps", "tilo-session-apps.x86_64",
                            "tilo-session-apps.aarch64"):
                cohort = messages(version, package)
                with self.subTest(version=version, package=package):
                    self.assertTrue(all(any(p.search(line) for p in RUNTIME_FILTERS)
                                        for line in cohort))
                    self.assertTrue(all(any(p.search(line) for line in cohort)
                                        for p in RUNTIME_FILTERS), "unused runtime filter")

    def test_noncanonical_versions_stay_visible(self):
        for version in ("2026.08", "2026x07", "2026.07+native.",
                        "2026.07+native." + DIGEST[:-1],
                        "2026.07+native." + DIGEST + "0",
                        "2026.07+native." + DIGEST.upper(),
                        "2026.07+native." + DIGEST + "/../2026.07"):
            for line in messages(version):
                with self.subTest(line=line):
                    self.assertFalse(any(p.search(line) for p in FILTERS))

    def test_other_packages_errors_and_paths_stay_visible(self):
        cohort = messages(NATIVE)
        negatives = messages(NATIVE, "tilo-session") + messages(NATIVE, "tilo-session-devel")
        negatives += messages(NATIVE, "tilo-session-apps-extra")
        negatives += [cohort[0].replace("invalid-soname", "invalid-rpath"),
                      cohort[1].replace("call-to-mktemp", "executable-stack"),
                      cohort[0].replace("/lib/libruntime_dylib.so", "/lib/libother.so"),
                      cohort[0].replace("libruntime_dylib.so", "libruntime_dylibXso"),
                      cohort[1].replace("/lib/lib64/libc.so.6", "/lib/libc.so.6"),
                      cohort[1].replace("/lib/lib64/libc.so.6", "/lib/lib64/libother.so.6"),
                      cohort[1].replace("/usr/lib/tilo-runtime/", "/usr/lib/other-runtime/"),
                      cohort[1] + "/extra", "other-prefix " + cohort[1]]
        for line in negatives:
            with self.subTest(line=line):
                self.assertFalse(any(p.search(line) for p in FILTERS))

    def test_unused_filter_diagnostics_are_never_suppressed(self):
        for package in ("tilo-session-apps", "tilo-session-devel"):
            for pattern in PATTERNS:
                line = f'{package}.x86_64: E: unused-rpmlintrc-filter "{pattern}"'
                with self.subTest(package=package, pattern=pattern):
                    self.assertFalse(any(p.search(line) for p in FILTERS))

    @unittest.skipUnless(importlib.util.find_spec("rpmlint"),
                         "real filter-engine check runs in the Fedora packaging userspace")
    def test_real_rpmlint_used_and_unused_filter_behavior(self):
        from rpmlint.filter import Filter

        config = SimpleNamespace(configuration={
            "BadnessThreshold": 1000, "Scoring": {}, "Filters": PATTERNS,
            "FilterErrorTitles": [], "BlockedFilters": [],
        }, strict=False, info=False, rpmlintrc_filters=PATTERNS)
        package = SimpleNamespace(name="tilo-session-apps", arch="x86_64", current_linenum=None)
        for version in ("2026.07", NATIVE):
            with self.subTest(version=version):
                engine = Filter(config)
                root = "/usr/lib/tilo-runtime/" + version
                engine.add_info("E", package, "invalid-soname",
                                root + "/lib/libruntime_dylib.so", "libruntime_dylib.so")
                engine.add_info("E", package, "call-to-mktemp", root + "/lib/lib64/libc.so.6")
                engine.add_info("W", package, "non-executable-script",
                                "/usr/lib/tilo-apps/pkgs/" + "ab" * 32 + "/usr/bin/fixture")
                engine.validate_filters(package)
                self.assertEqual(engine.filtered_out, 3)
                self.assertEqual(engine.printed_messages["E"], 0)
                self.assertEqual(engine.used_filters, set(PATTERNS))
                # Validate a genuinely unused filter through rpmlint itself;
                # no regex exception may hide that configuration diagnostic.
                unused = Filter(config)
                unused.validate_filters(package)
                self.assertEqual(unused.printed_messages["E"], len(PATTERNS))
                self.assertTrue(all("unused-rpmlintrc-filter" in x for x in unused.results))

    def test_preflight_runs_before_native_compilation(self):
        builder = (ROOT / "dist/rpm/build-rpm.sh").read_text()
        self.assertLess(builder.index("test-rpmlint-config.py"),
                        builder.index("cargo build --release"))
        workflow = (ROOT / ".forgejo/workflows/host-session-e2e.yml").read_text()
        self.assertLess(workflow.index("test-rpmlint-config.py"),
                        workflow.index("Fetch the env-runtime environment image"))


if __name__ == "__main__":
    unittest.main()
