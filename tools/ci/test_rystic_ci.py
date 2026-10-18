#!/usr/bin/env python3
"""Unlicensed, stdlib-only tests: no Cargo, downloads, or paid twin starts."""

import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import select
import tarfile
import tempfile
import unittest
from unittest.mock import MagicMock, patch

import rystic_ci as ci


class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR"))
        self.addCleanup(self.temp.cleanup)
        self.artifacts = Path(self.temp.name)
        self.home = self.artifacts / "home"
        self.home.mkdir(mode=0o700)
        (self.home / "token").write_text("fake-secret")
        (self.home / "token").chmod(0o600)
        self.cli = self.artifacts / "rystic-cli"
        self.cli.write_bytes(b"fake CLI")
        (self.artifacts / "cli.json").write_text(
            json.dumps(
                {
                    "version": ci.CLI_VERSION,
                    "archive_sha256": ci.CLI_SHA256_AMD64,
                    "binary_sha256": ci.digest(self.cli),
                }
            )
        )
        entries = {}
        for target in ci.TARGETS:
            binary = self.artifacts / target
            binary.write_bytes(target.encode())
            entries[target] = {"sha256": ci.digest(binary)}
        self.manifest = {
            "schema": 3,
            "harness_sha256": ci.digest(ci.__file__),
            "metadata": ci.harness_metadata(),
            "binaries": entries,
            "docx_validator": {"path": str(self.cli), "sha256": ci.digest(self.cli)},
        }
        self.save_manifest()
        self.calls = []
        self.behavior = None
        self.environment = patch.dict(
            os.environ,
            {
                "RYSTIC_HOME": str(self.home),
                "RYSTIC_RELEASE_ENDPOINT": "https://evil.invalid",
            },
        )
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.patcher = patch.object(ci, "command", side_effect=self.fake_command)
        self.patcher.start()
        self.addCleanup(self.patcher.stop)
        self.handshake = patch.object(
            ci, "version_handshake", side_effect=self.fake_handshake
        )
        self.handshake.start()
        self.addCleanup(self.handshake.stop)

    def save_manifest(self):
        (self.artifacts / "manifest.json").write_text(json.dumps(self.manifest))

    def fake_handshake(self, _url):
        product = next(
            call[0][2] for call in reversed(self.calls) if call[0][1] == "run"
        )
        spec = next(s for s in ci.PROVIDERS.values() if s["product"] == product)
        return {key: spec[key] for key in ("product", "version", "model_hash")}

    def fake_command(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        if self.behavior:
            result = self.behavior(argv)
            if result is not None:
                return result
        if argv[1] == "version":
            output = "rystic 0.0.28 (fake)\nprivate license info must never escape"
        elif "--list" in argv:
            _, _, supported, diagnostic = ci.TARGETS[Path(argv[0]).name]
            output = "\n".join(name + ": test" for name in supported + diagnostic)
        elif argv[1] == "run":
            output = json.dumps(
                {"data": {"url": "http://127.0.0.1:12345"}, "license": "secret"}
            )
        else:
            output = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out\n"
        return subprocess.CompletedProcess(argv, 0, output)

    def run_ci(self, strict=False):
        return ci.run(self.artifacts, list(ci.PROVIDERS), strict)

    def starts(self):
        return [argv for argv, _ in self.calls if argv[1] == "run"]

    def assert_cleaned(self, count):
        starts = self.starts()
        self.assertEqual(len(starts), count)
        for start in starts:
            name = start[start.index("--name") + 1]
            self.assertRegex(name, r"^tilo-ci-[0-9a-f]{32}$")
            actions = [argv[1] for argv, _ in self.calls if argv[-1] == name]
            self.assertEqual(actions, ["stop", "rm"])

    def test_supported_inventory_and_secret_free_report(self):
        report = self.run_ci()
        self.assertEqual(report["status"], "passed")
        self.assertEqual([len(p["tests"]) for p in report["providers"]], [6, 3])
        self.assert_cleaned(2)
        pulls = [argv for argv, _ in self.calls if argv[1] == "pull"]
        self.assertEqual([a[-1] for a in pulls], ["0.0.5", "0.0.4"])
        first_start = next(i for i, (a, _) in enumerate(self.calls) if a[1] == "run")
        self.assertTrue(
            all(
                self.calls.index(c) < first_start
                for c in self.calls
                if c[0][1] == "pull"
            )
        )
        for argv, kwargs in self.calls:
            self.assertNotIn("RYSTIC_RELEASE_ENDPOINT", kwargs["env"])
            if "--exact" in argv:
                self.assertNotIn("RYSTIC_HOME", kwargs["env"])
                self.assertLessEqual(kwargs["timeout"], 240)
        encoded = json.dumps(report)
        for secret in ("secret", "license", "token", "http://", "private"):
            self.assertNotIn(secret, encoded)

    def test_strict_assertions_are_fatal_not_expected_success(self):
        diagnostics = {t for _, _, _, names in ci.TARGETS.values() for t in names}
        self.behavior = (
            lambda a: subprocess.CompletedProcess(a, 101, "secret assertion")
            if a[1] in diagnostics
            else None
        )
        report = self.run_ci(strict=True)
        self.assertEqual(report["status"], "failed")
        self.assertEqual([len(p["tests"]) for p in report["providers"]], [7, 4])
        self.assertEqual(
            sum(t["testexit"] == 101 for p in report["providers"] for t in p["tests"]),
            2,
        )
        self.assert_cleaned(2)

    def test_cancellation_cleans_and_never_starts_second_provider(self):
        for signum in (ci.signal.SIGINT, ci.signal.SIGTERM):
            for stage in ("run", "test", "stop"):
                with self.subTest(signum=signum, stage=stage):
                    self.calls.clear()

                    def cancel(argv):
                        if (stage == "test" and "--exact" in argv) or argv[1] == stage:
                            os.kill(os.getpid(), signum)
                        if argv[1] in ("stop", "rm"):
                            # Repeated signals during cleanup cannot abort removal.
                            os.kill(os.getpid(), signum)

                    self.behavior = cancel
                    output = io.StringIO()
                    with (
                        patch.object(
                            ci.sys,
                            "argv",
                            ["runner", "run", "--artifacts", str(self.artifacts)],
                        ),
                        patch("sys.stdout", output),
                    ):
                        code = ci.main()
                    self.assertEqual(code, 128 + signum)
                    self.assertEqual(
                        json.loads(output.getvalue()), {"status": "interrupted"}
                    )
                    self.assert_cleaned(1)

    def test_strict_can_pass_when_contracts_are_fixed(self):
        self.assertEqual(self.run_ci(strict=True)["status"], "passed")

    def test_zero_missing_unknown_and_duplicate_inventory_prevent_paid_start(self):
        for inventory in ("", "unexpected: test", "same: test\nsame: test"):
            with self.subTest(inventory=inventory):
                self.calls.clear()
                self.behavior = (
                    lambda a: subprocess.CompletedProcess(a, 0, inventory)
                    if "--list" in a
                    else None
                )
                self.assertEqual(self.run_ci()["error"], "preflight_failed")
                self.assertFalse(self.starts())

    def test_missing_artifact_and_wrong_harness_prevent_paid_start(self):
        self.manifest["harness_sha256"] = "b" * 64
        self.save_manifest()
        self.assertEqual(self.run_ci()["error"], "preflight_failed")
        self.assertFalse(self.starts())
        self.manifest["harness_sha256"] = ci.digest(ci.__file__)
        self.save_manifest()
        (self.artifacts / "rystic").unlink()
        self.assertEqual(self.run_ci()["error"], "preflight_failed")
        self.assertFalse(self.starts())

    def test_changed_metadata_or_binary_prevents_paid_start(self):
        for kind in ("inventory", "pin", "binary", "validator"):
            with self.subTest(kind=kind):
                self.calls.clear()
                original = json.loads(json.dumps(self.manifest))
                if kind == "inventory":
                    self.manifest["metadata"]["targets"]["rystic"]["supported"] = []
                elif kind == "pin":
                    self.manifest["metadata"]["providers"]["gdrive"]["model_hash"] = (
                        "wrong"
                    )
                elif kind == "binary":
                    self.manifest["binaries"]["rystic"]["sha256"] = "0" * 64
                else:
                    self.manifest["docx_validator"]["sha256"] = "0" * 64
                self.save_manifest()
                self.assertEqual(self.run_ci(strict=True)["error"], "preflight_failed")
                self.assertFalse(self.starts())
                self.manifest = original

    def test_failed_build_removes_stale_success_manifest(self):
        with patch.object(ci, "checked", return_value=""):
            with self.assertRaisesRegex(ci.Failure, "compiler_artifact_count"):
                ci.build(self.artifacts)
        self.assertFalse((self.artifacts / "manifest.json").exists())

    def test_pull_failure_prevents_any_paid_start(self):
        self.behavior = (
            lambda a: subprocess.CompletedProcess(a, 1, "secret")
            if a[1] == "pull"
            else None
        )
        self.assertEqual(self.run_ci()["error"], "preflight_failed")
        self.assertFalse(self.starts())

    def test_start_failure_and_timeout_always_clean_owned_instance(self):
        for timeout in (False, True):
            self.calls.clear()

            def fail(argv):
                if argv[1] == "run":
                    if timeout:
                        raise subprocess.TimeoutExpired(argv, 60, output="secret")
                    return subprocess.CompletedProcess(argv, 1, "secret")

            self.behavior = fail
            report = self.run_ci()
            self.assertEqual(report["status"], "failed")
            self.assert_cleaned(2)
            self.assertNotIn("secret", json.dumps(report))

    def test_wrong_model_fails_and_cleans(self):
        with patch.object(
            ci, "version_handshake", return_value={"model_hash": "wrong"}
        ):
            report = self.run_ci()
        self.assertEqual(report["status"], "failed")
        self.assertFalse(any(p["tests"] for p in report["providers"]))
        self.assert_cleaned(2)

    def test_test_timeout_cleanup_and_zero_execution_failure(self):
        for timeout in (False, True):
            self.calls.clear()

            def fail(argv):
                if "--exact" in argv:
                    if timeout:
                        raise subprocess.TimeoutExpired(argv, 240)
                    return subprocess.CompletedProcess(
                        argv, 0, "test result: ok. 0 passed; 0 failed; 1 ignored;"
                    )

            self.behavior = fail
            self.assertEqual(self.run_ci()["status"], "failed")
            self.assert_cleaned(2)

    def test_cleanup_failure_still_removes_and_prevents_next_provider(self):
        for timeout in (False, True):
            self.calls.clear()

            def fail(argv):
                if argv[1] == "stop":
                    if timeout:
                        raise subprocess.TimeoutExpired(argv, 15)
                    return subprocess.CompletedProcess(argv, 1, "private")

            self.behavior = fail
            report = self.run_ci()
            self.assertEqual(report["status"], "failed")
            self.assertEqual(report["providers"][0]["cleanup"], "failed")
            self.assert_cleaned(1)

    def test_unknown_added_test_is_not_silently_filtered(self):
        def extra(argv):
            if "--list" in argv:
                _, _, supported, diagnostic = ci.TARGETS[Path(argv[0]).name]
                names = supported + diagnostic + ("new_contract",)
                return subprocess.CompletedProcess(
                    argv, 0, "\n".join(n + ": test" for n in names)
                )

        self.behavior = extra
        self.assertEqual(self.run_ci()["error"], "preflight_failed")
        self.assertFalse(self.starts())

    def test_token_permissions_fail_closed(self):
        (self.home / "token").chmod(0o644)
        self.assertEqual(self.run_ci()["error"], "preflight_failed")
        self.assertFalse(self.starts())

    def test_cli_archive_pin_and_exact_member(self):
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w:gz") as archive:
            entry = tarfile.TarInfo("bin/rystic")
            entry.size = 3
            archive.addfile(entry, io.BytesIO(b"CLI"))
        data = output.getvalue()
        sha = hashlib.sha256(data).hexdigest()
        meta = {
            "version": ci.CLI_VERSION,
            "product": "rystic",
            "sha256": sha,
            "asset": "rystic_0.0.28_linux_amd64.tar.gz",
            "url": "https://www.rystic.ai/fake",
        }
        with (
            patch.object(ci, "CLI_SHA256_AMD64", sha),
            patch.object(ci, "fetch", side_effect=[json.dumps(meta).encode(), data]),
        ):
            self.assertEqual(ci.install_cli(self.artifacts)["status"], "installed")
        self.assertEqual(self.cli.read_bytes(), b"CLI")
        with patch.object(ci, "fetch", return_value=json.dumps(meta).encode()):
            with self.assertRaises(ci.Failure):
                ci.install_cli(self.artifacts)

    def test_build_uses_compiler_exact_paths_for_all_three_targets(self):
        source_dir = self.artifacts / "source"
        validator = (
            source_dir / "crates/system/cloud-source-gdrive/tests/validate_docx.py"
        )
        validator.parent.mkdir(parents=True)
        validator.write_text("validator")
        compiled = self.artifacts / "compiler-picked-hash"
        compiled.write_bytes(b"native test")

        def compiler(argv, **kwargs):
            self.assertIn("--no-run", argv)
            self.assertIn("--message-format=json", argv)
            target = argv[argv.index("--test") + 1]
            return json.dumps(
                {
                    "reason": "compiler-artifact",
                    "target": {"name": target},
                    "profile": {"test": True},
                    "executable": str(compiled),
                }
            )

        with (
            patch.object(ci, "ROOT", source_dir),
            patch.object(ci, "checked", side_effect=compiler),
        ):
            report = ci.build(self.artifacts)
        self.assertEqual(set(report["binaries"]), set(ci.TARGETS))
        manifest = json.loads((self.artifacts / "manifest.json").read_text())
        self.assertEqual(manifest["harness_sha256"], ci.digest(ci.__file__))
        self.assertEqual(manifest["metadata"], ci.harness_metadata())
        self.assertEqual(manifest["docx_validator"]["sha256"], ci.digest(validator))
        for target in ci.TARGETS:
            self.assertEqual((self.artifacts / target).read_bytes(), b"native test")
            self.assertEqual(report["binaries"][target]["compiler_path"], str(compiled))


class BoundaryTests(unittest.TestCase):
    def test_shell_forwards_term_and_retains_token_until_child_stops(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as directory:
            root = Path(directory)
            fake = root / "python3"
            fake.write_text(
                "#!/usr/bin/python3\n"
                "import json, os, pathlib, signal\n"
                "home = pathlib.Path(os.environ['RYSTIC_HOME'])\n"
                "def stop(sig, frame):\n"
                "    assert (home / 'token').read_text() == 'fake-secret'\n"
                "    (pathlib.Path(os.environ['CARGO_TARGET_DIR']) / 'stopped').write_text(str(home))\n"
                "    print(json.dumps({'status': 'interrupted'}), flush=True)\n"
                "    raise SystemExit(143)\n"
                "signal.signal(signal.SIGTERM, stop)\n"
                "os.write(int(os.environ['READY_FD']), b'R')\n"
                "signal.pause()\n"
            )
            fake.chmod(0o755)
            read_fd, write_fd = os.pipe()
            env = dict(
                os.environ,
                PATH=str(root) + os.pathsep + os.environ["PATH"],
                CARGO_TARGET_DIR=str(root),
                RYSTIC_CI_TOKEN="fake-secret",
                READY_FD=str(write_fd),
            )
            try:
                with subprocess.Popen(
                    ["bash", str(Path(ci.__file__).with_name("run-rystic-ci.sh"))],
                    env=env,
                    pass_fds=(write_fd,),
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                ) as child:
                    try:
                        self.assertTrue(select.select([read_fd], [], [], 5)[0])
                        self.assertEqual(os.read(read_fd, 1), b"R")
                        child.terminate()
                        stdout, stderr = child.communicate(timeout=5)
                        self.assertEqual(child.returncode, 143)
                        self.assertEqual(stderr, b"")
                        self.assertNotIn(b"fake-secret", stdout)
                    finally:
                        if child.poll() is None:
                            child.kill()
                    home = Path((root / "stopped").read_text())
                    self.assertFalse(home.exists())
                    report = root / "rystic-results" / "supported.json"
                    self.assertEqual(
                        json.loads(report.read_text()), {"status": "interrupted"}
                    )
            finally:
                os.close(read_fd)
                os.close(write_fd)

    def test_cached_artifacts_allow_different_commit_provenance(self):
        runner = RunnerTests()
        runner.setUp()
        self.addCleanup(runner.doCleanups)
        runner.manifest["build_commit"] = "a" * 40
        runner.save_manifest()
        with patch.dict(os.environ, {"T_COMMIT": "b" * 40}):
            self.assertEqual(runner.run_ci()["status"], "passed")

    def test_timeout_kills_process_group_and_reaps(self):
        process = MagicMock()
        process.pid = 123
        process.communicate.side_effect = [
            subprocess.TimeoutExpired(["fake"], 1),
            (b"", b""),
        ]
        process.__enter__.return_value = process
        # Exercise the real command wrapper, rather than RunnerTests' fake.
        with (
            patch.object(ci.subprocess, "Popen", return_value=process),
            patch.object(ci.os, "killpg") as kill,
        ):
            with self.assertRaises(subprocess.TimeoutExpired):
                ci.command(["fake"], timeout=1)
        kill.assert_called_once_with(123, ci.signal.SIGKILL)
        self.assertEqual(process.communicate.call_count, 2)

    def test_interrupted_command_kills_group_and_reraises_sentinel(self):
        for signum in (ci.signal.SIGINT, ci.signal.SIGTERM):
            sentinel = ci.Interrupted(signum)
            process = MagicMock()
            process.pid = 456
            process.__enter__.return_value = process
            process.communicate.side_effect = [sentinel, (b"secret", b"secret")]
            with (
                patch.object(ci.subprocess, "Popen", return_value=process),
                patch.object(ci.os, "killpg") as kill,
                self.assertRaises(ci.Interrupted) as caught,
            ):
                ci.command(["fake"])
            self.assertIs(caught.exception, sentinel)
            kill.assert_called_once_with(456, ci.signal.SIGKILL)
            self.assertEqual(process.communicate.call_count, 2)

    def test_untrusted_downloads_and_non_loopback_fail_before_network(self):
        for url in ("http://www.rystic.ai/archive", "https://evil.invalid/archive"):
            with self.subTest(url=url), self.assertRaises(ci.Failure):
                ci.fetch(url)
        for url in (
            "http://example.com:123",
            "http://127.0.0.1:123@evil.invalid/",
            "https://127.0.0.1:123",
        ):
            with self.subTest(url=url), self.assertRaises(ci.Failure):
                ci.version_handshake(url)


if __name__ == "__main__":
    unittest.main()
