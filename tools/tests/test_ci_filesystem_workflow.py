"""Contract of .forgejo/workflows/ci-filesystem.yml."""

from __future__ import annotations

import pathlib
import re
import tomllib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".forgejo/workflows"


def cargo_env(block: str, indent: int) -> dict[str, str]:
    """CARGO_* entries of an `env:` mapping at `indent` spaces."""
    pad = " " * indent
    return {
        name: value.strip().strip("'\"")
        for name, value in re.findall(rf"^{pad}(CARGO_[A-Z0-9_]+): (.+)$", block, re.M)
    }


def env_block(source: str, indent: int) -> str:
    pad = " " * indent
    match = re.search(rf"^{pad}env:\n((?:{pad}  .*\n|\s*\n)*)", source, re.M)
    assert match
    return match.group(1)


class FilesystemWorkflowCargoEnv(unittest.TestCase):
    def test_matches_the_x86_test_shards(self) -> None:
        check = (WORKFLOWS / "check.yml").read_text()
        expected = cargo_env(env_block(check, 0), 2)
        test_job = check.split("\n  test:\n", 1)[1].split("\n  check:\n", 1)[0]
        jobs = re.search(r"^      CARGO_BUILD_JOBS: .*'(\d+)' \}\}$", test_job, re.M)
        self.assertIsNotNone(jobs, "test job's x86 CARGO_BUILD_JOBS")
        expected["CARGO_BUILD_JOBS"] = jobs.group(1)

        filesystem = (WORKFLOWS / "ci-filesystem.yml").read_text()
        self.assertEqual(cargo_env(env_block(filesystem, 0), 2), expected)
        jobs_section = filesystem.split("\njobs:\n", 1)[1]
        self.assertNotRegex(jobs_section, r"(?m)^\s+CARGO_[A-Z0-9_]+:")


class FilesystemWorkflowTriggers(unittest.TestCase):
    def test_pushes_to_main_run_the_pull_request_selection(self) -> None:
        source = (WORKFLOWS / "ci-filesystem.yml").read_text()
        triggers = source.split("\non:\n", 1)[1].split("\nconcurrency:", 1)[0]

        def paths(event: str) -> list[str]:
            body = re.search(rf"^  {event}:\n((?:    .*\n|      .*\n)*)", triggers, re.M)
            self.assertIsNotNone(body, event)
            return re.findall(r'^      - "(.+)"$', body.group(1), re.M)

        self.assertIn("crates/system/fs/**", paths("push"))
        self.assertEqual(paths("push"), paths("pull_request"))


class FilesystemWorkflowNamedShareWire(unittest.TestCase):
    def test_filter_selects_the_production_share_control_test(self) -> None:
        source = (WORKFLOWS / "ci-filesystem.yml").read_text()
        command = next(line for line in source.splitlines()
                       if '"named-share control wire"' in line)
        match = re.search(r"cargo test --locked -p (\S+) (\S+) --lib", command)
        self.assertIsNotNone(match, "wire gate must run its native library test")
        manifest = ROOT / "crates/system/fs-protocol/Cargo.toml"
        package = tomllib.loads(manifest.read_text())["package"]["name"]
        self.assertEqual(match.group(1), package,
                         "wire protocol moved out of collab-types; its filter otherwise runs zero tests")
        self.assertIn("pub mod share_control;", (manifest.parent / "src/lib.rs").read_text())
        module = (manifest.parent / "src/share_control.rs").read_text()
        tests = re.findall(r"#\[test\]\s+fn (\w+)\(", module)
        self.assertTrue(any(match.group(2) in name for name in tests),
                        "wire filter must match an existing production framing test")


if __name__ == "__main__":
    unittest.main()
