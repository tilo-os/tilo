"""A test shard prepares its environment from the merged tree.

A pull request runs its own head's check.yml against its head merged onto
main. An environment requirement added as a workflow step therefore never
reaches a pull request branched before it, while main's tests already demand
it. The test job instead calls tools/ci/prepare-test-shard.sh after the merge;
these tests hold that line and exercise the production script.
"""
from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/ci/prepare-test-shard.sh"


class PrepareTestShardScript(unittest.TestCase):
    """Run the production script against stub helpers that record their calls."""

    def prepare(self, packages: str = "", scoped: str = "") -> tuple[list[str], str]:
        with tempfile.TemporaryDirectory() as tmp:
            tree = Path(tmp) / "tree"
            bin_dir = Path(tmp) / "bin"
            log = Path(tmp) / "calls"
            env_file = Path(tmp) / "github-env"
            env_file.write_text("")
            (tree / "tools/ci").mkdir(parents=True)
            (tree / "os/dev").mkdir(parents=True)
            bin_dir.mkdir()
            shutil.copy(SCRIPT, tree / "tools/ci/prepare-test-shard.sh")
            for name in ("nested_sandboxes.py", "nested-sandboxes.toml"):
                shutil.copy(ROOT / "tools/ci" / name, tree / "tools/ci" / name)
            recorder = '#!/usr/bin/env bash\nprintf "%s %s\\n" "{name}" "$*" >> "$CALLS"\n{tail}'
            stubs = {
                tree / "tools/ensure-headless-audio.sh": "",
                tree / "tools/ci/ensure-x11-bridge-tools.sh": "",
                tree / "tools/ci/ensure-session-bus-tools.sh": "",
                tree / "tools/ci/install-pdfium.sh": 'echo /usr/lib64/libpdfium.so\n',
                tree / "tools/ci/require-pdf-sandbox.sh": "",
                tree / "tools/ci/measure.sh": "",
                tree / "os/dev/cache-github-assets.sh": "",
                bin_dir / "dnf": "",
                bin_dir / "npm": "",
                bin_dir / "node": "",
                bin_dir / "chromium": "",
                bin_dir / "stat": "echo xfs\n",
                bin_dir / "bwrap": "",
            }
            for path, tail in stubs.items():
                name = path.name if path.parent == bin_dir else path.relative_to(tree).as_posix()
                path.write_text(recorder.format(name=name, tail=tail))
                path.chmod(0o755)
            env = dict(os.environ, PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                       CALLS=str(log), GITHUB_ENV=str(env_file), GITHUB_WORKSPACE=str(tree),
                       TEST_PACKAGES=packages, SCOPED_TESTS=scoped)
            result = subprocess.run(["bash", "tools/ci/prepare-test-shard.sh"], cwd=tree, env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = log.read_text().splitlines() if log.exists() else []
            return calls, env_file.read_text().replace(str(tree), "$TREE")

    @staticmethod
    def tools(calls: list[str]) -> list[str]:
        return [call.split(" ", 1)[0] for call in calls]

    def test_every_shard_gets_headless_audio(self) -> None:
        for packages in ("", "-p collab-types"):
            with self.subTest(packages=packages):
                calls, exported = self.prepare(packages)
                self.assertEqual(self.tools(calls), ["tools/ensure-headless-audio.sh"])
                self.assertEqual(exported, "")

    def test_shell_shards_get_the_x11_bridge(self) -> None:
        calls, _ = self.prepare("-p tilo-ui -p shell")
        self.assertIn("tools/ci/ensure-x11-bridge-tools.sh", self.tools(calls))

    def test_nested_sandbox_shards_probe_user_namespaces(self) -> None:
        calls, _ = self.prepare("-p collab-types -p ush")
        self.assertEqual(self.tools(calls)[0], "bwrap")
        calls, _ = self.prepare("-p collab-types -p ush-extra")
        self.assertNotIn("bwrap", self.tools(calls))

    def test_secret_service_shards_get_the_session_bus_client(self) -> None:
        calls, _ = self.prepare("-p tilo-secret-service")
        self.assertIn("tools/ci/ensure-session-bus-tools.sh", self.tools(calls))
        calls, _ = self.prepare("-p tilo-secret-service-extra")
        self.assertNotIn("tools/ci/ensure-session-bus-tools.sh", self.tools(calls))

    def test_package_names_match_exactly(self) -> None:
        calls, _ = self.prepare("-p shell-extra -p example-imager -p tilo-app-cli")
        self.assertEqual(self.tools(calls), ["tools/ensure-headless-audio.sh"])

    def test_pdf_shards_stage_pdfium_and_require_its_sandbox(self) -> None:
        for package in ("pdf-viewer", "tilo-pdf", "tilo-pdf-helper", "tilo-thumbnail"):
            with self.subTest(package=package):
                calls, exported = self.prepare(f"-p {package}")
                self.assertIn("tools/ci/require-pdf-sandbox.sh", self.tools(calls))
                self.assertIn("TILO_PDFIUM_LIB=/usr/lib64/libpdfium.so\n", exported)

    def test_browser_peer_builds_its_page_library_in_the_builders_chromium(self) -> None:
        scoped = '[{"package":"tilo-browser-peer","test":"voice","filter":"all()"}]'
        for packages, scoped_tests in (("-p tilo-browser-peer", ""), ("", scoped)):
            with self.subTest(packages=packages, scoped=scoped_tests):
                calls, exported = self.prepare(packages, scoped_tests)
                self.assertEqual(self.tools(calls)[-2:], ["npm", "node"])
                self.assertNotIn("dnf", self.tools(calls))
                self.assertNotIn("CHROME=", exported)

    def test_imager_gets_its_qualified_cache_root(self) -> None:
        _, exported = self.prepare("-p imager")
        self.assertEqual(exported, "TILO_IMAGER_TEST_CACHE_ROOT=$TREE/target/ci/imager-test-fs\n")

    def test_installer_consumers_build_the_installer_fixture(self) -> None:
        for package in ("app-cli", "app-host"):
            with self.subTest(package=package):
                calls, _ = self.prepare(f"-p {package}")
                measured = [call for call in calls if call.startswith("tools/ci/measure.sh ")]
                self.assertEqual(len(measured), 1)
                self.assertIn("cargo build --locked -p install --bin tilo-install", measured[0])


if __name__ == "__main__":
    unittest.main()
