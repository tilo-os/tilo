"""Continuous publication policy tests; no compilation or image builds."""
import hashlib
import importlib.util
import io
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import textwrap
import threading
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools/tests"))
from test_imager_enabled_platforms import section

SPEC = importlib.util.spec_from_file_location("continuous_release", HERE / "continuous-release.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ContinuousRelease(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_identity_is_real_source_version_and_immutable_revision(self):
        manifest = self.root / "Cargo.toml"
        manifest.write_text('[package]\nversion = "1.2.3"\n')
        env = {"GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/main",
               "TILO_REF_PROTECTED": "true", "GITHUB_SHA": "a" * 40}
        self.assertEqual(MODULE.identity(env, manifest), ("1.2.3", "imager-main-" + "a" * 40))
        for key, value in (("GITHUB_EVENT_NAME", "workflow_dispatch"),
                           ("GITHUB_REF", "refs/heads/feature"),
                           ("TILO_REF_PROTECTED", "false"), ("GITHUB_SHA", "main")):
            with self.subTest(key=key), self.assertRaises(ValueError):
                MODULE.identity(dict(env, **{key: value}), manifest)
        manifest.write_text('[package]\nversion = "1.2.3-dev"\n')
        with self.assertRaises(ValueError):
            MODULE.identity(env, manifest)

    def test_protection_uses_immutable_repository_and_boolean_branch_metadata(self):
        repository = self.root / "repository.json"
        branch = self.root / "branch.json"
        expected = {"GITHUB_REPOSITORY_ID": "23", "GITHUB_REPOSITORY": "tilo/tilo"}
        repo = {"id": 23, "full_name": "tilo/tilo", "default_branch": "main"}
        main = {"name": "main", "protected": True}
        repository.write_text(json.dumps(repo))
        branch.write_text(json.dumps(main))
        self.assertEqual(MODULE.protection(repository, branch, expected), "TILO_REF_PROTECTED=true")
        for changed in (dict(repo, id=24), dict(repo, id=True), dict(repo, full_name="other/tilo"),
                        dict(repo, default_branch="develop")):
            repository.write_text(json.dumps(changed))
            with self.subTest(repository=changed), self.assertRaises(ValueError):
                MODULE.protection(repository, branch, expected)
        repository.write_text(json.dumps(repo))
        for changed in (dict(main, name="feature"), dict(main, protected=False),
                        dict(main, protected="true"), {"message": "not found"}):
            branch.write_text(json.dumps(changed))
            with self.subTest(branch=changed), self.assertRaises(ValueError):
                MODULE.protection(repository, branch, expected)
        branch.write_text(json.dumps(main))
        for changed in (dict(expected, GITHUB_REPOSITORY_ID=""), dict(expected, GITHUB_REPOSITORY="")):
            with self.assertRaises(ValueError):
                MODULE.protection(repository, branch, changed)
        branch.write_text("[]")
        with self.assertRaises(ValueError):
            MODULE.protection(repository, branch, expected)

    def test_workflow_protection_preflight_requires_successful_api_observation(self):
        workflow = (ROOT / ".forgejo/workflows/release-imager.yml").read_text()
        step = section(workflow, "plan", 2).split("- name: Check protected main against Forgejo source metadata\n", 1)[1]
        program = textwrap.dedent(step.split("        run: |\n", 1)[1].split("      - name: Bind continuous", 1)[0])
        state = {"status": 200, "protected": True, "id": 23}
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_GET(self):
                value = ({"name": "main", "protected": state["protected"]}
                         if self.path.endswith("/branches/main") else
                         {"id": state["id"], "full_name": "tilo/tilo", "default_branch": "main"})
                body = json.dumps(value).encode()
                self.send_response(state["status"])
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        def cleanup():
            server.shutdown()
            server.server_close()
            thread.join()
        self.addCleanup(cleanup)
        env_file, outputs = self.root / "env", self.root / "outputs"
        env = dict(os.environ, GITHUB_REPOSITORY="tilo/tilo", GITHUB_REPOSITORY_ID="23",
                   GITHUB_API_URL=f"http://127.0.0.1:{server.server_port}", GITHUB_TOKEN="test-job-token",
                   RUNNER_TEMP=str(self.root), GITHUB_ENV=str(env_file), GITHUB_OUTPUT=str(outputs))
        for status, protected, repo_id, accepted in ((200, True, 23, True), (302, True, 23, False),
                                                    (403, True, 23, False), (200, False, 23, False),
                                                    (200, True, 24, False)):
            state.update(status=status, protected=protected, id=repo_id)
            env_file.write_text("")
            outputs.write_text("")
            result = subprocess.run(["bash", "-e", "-c", program], cwd=ROOT, env=env, capture_output=True)
            with self.subTest(status=status, protected=protected, repository=repo_id):
                self.assertEqual(result.returncode == 0, accepted, result.stderr.decode())
                self.assertEqual(env_file.read_text(), "TILO_REF_PROTECTED=true\n" if accepted else "")
                self.assertEqual(outputs.read_text(), "ref-protected=true\n" if accepted else "")

    def package_fixture(self):
        for arch in ("x86_64", "aarch64"):
            names = [f"tilo-imager-linux-{arch}.{suffix}" for suffix in ("AppImage", "tar.gz", "flatpak")]
            names += [f"tilo-imager-windows-{arch}.{suffix}" for suffix in ("exe", "zip")]
            names += [f"tilo-imager-setup-windows-{arch}.exe"]
            names += [f"tilo-imager-macos-{arch}.{suffix}" for suffix in ("zip", "dmg")]
            for name in names:
                (self.root / name).write_bytes(name.encode())
                digest = hashlib.sha256(name.encode()).hexdigest()
                (self.root / (name + ".sha256")).write_text(f"{digest}  {name}\n")
                if "-linux-" in name:
                    (self.root / (name + ".asc")).write_text("signature verified separately by the production GPG verifier")

    def test_all_six_native_packages_are_required(self):
        self.package_fixture()
        self.assertEqual(len(MODULE.assets(self.root)), 16)
        target = self.root / "tilo-imager-macos-aarch64.dmg"
        target.unlink()
        with self.assertRaisesRegex(ValueError, "missing"):
            MODULE.assets(self.root)

    def test_changed_digest_duplicate_and_missing_signature_fail(self):
        self.package_fixture()
        target = self.root / "tilo-imager-linux-x86_64.AppImage"
        signature = target.with_name(target.name + ".asc")
        signature.unlink()
        with self.assertRaisesRegex(ValueError, "signature"):
            MODULE.assets(self.root)
        signature.write_text("signed")
        target.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "digest"):
            MODULE.assets(self.root)
        target.write_bytes(target.name.encode())
        duplicate = self.root / "duplicate"
        duplicate.mkdir()
        (duplicate / target.name).write_bytes(b"duplicate")
        with self.assertRaisesRegex(ValueError, "duplicate"):
            MODULE.assets(self.root)

    def test_catalog_reuses_measured_public_media_and_fails_on_changed_bytes(self):
        entries = []
        downloads = {}
        for arch in ("x86_64", "aarch64"):
            name = f"tilo-installer-linux-{arch}.img.zst"
            downloads[name] = b"measured " + arch.encode()
            downloads[name + ".sha256"] = b"checked later by GPG trust verifier"
            downloads[name + ".asc"] = b"checked later by GPG trust verifier"
            entries.append({"arch": arch, "url": f"https://github.com/tilo-os/releases/releases/download/imager-v1.2.3/{name}",
                            "archive_sha256": hashlib.sha256(downloads[name]).hexdigest(),
                            "archive_size": len(downloads[name]), "image_sha256": "b" * 64,
                            "image_size": 123})
        document = {"schema": 1, "tag": "imager-v1.2.3", "releases": entries}
        source = self.root / "reviewed.json"
        source.write_text(json.dumps(document))
        downloads["tilo-installer-catalog-v1.json"] = source.read_bytes()
        downloads["tilo-installer-catalog-v1.json.sha256"] = b"digest"
        downloads["tilo-installer-catalog-v1.json.asc"] = b"signature"
        def fetch(url, timeout):
            self.assertTrue(url.startswith("https://github.com/tilo-os/releases/releases/download/imager-v1.2.3/"))
            self.assertEqual(timeout, 120)
            return io.BytesIO(downloads[url.rsplit("/", 1)[1]])
        with patch.object(MODULE, "urlopen", fetch), patch.dict(os.environ, {"TILO_RELEASE_SERVER": "https://github.com", "TILO_RELEASE_REPOSITORY": "tilo-os/releases"}):
            MODULE.catalog(source, self.root / "media")
            downloads["tilo-installer-linux-aarch64.img.zst"] = b"changed"
            with self.assertRaises(ValueError):
                MODULE.catalog(source, self.root / "changed")

    def test_placeholder_catalog_fails_before_network(self):
        source = self.root / "catalog.json"
        source.write_text('{"schema":1,"tag":"unreleased","releases":[]}')
        with patch.object(MODULE, "urlopen") as fetch, self.assertRaises(ValueError):
            MODULE.catalog(source, self.root / "media")
        fetch.assert_not_called()

    def test_public_payload_is_prerelease_and_cannot_advance_latest(self):
        script = (HERE / "publish-github-release.sh").read_text()
        creation = script.split('payload="$(python3 - "$release_tag" "$prerelease" "${GITHUB_SHA:-}" <<\'PY\'\n', 1)[1].split('\nPY', 1)[0]
        promotion = script.split('payload="$(python3 - "$release_tag" "$prerelease" <<\'PY\'\n', 1)[1].split('\nPY', 1)[0]
        tag = "imager-main-" + "a" * 40
        for program, arguments in ((creation, [tag, "true", "a" * 40]), (promotion, [tag, "true"])):
            result = subprocess.run([sys.executable, "-", *arguments], input=program, text=True, capture_output=True, check=True)
            payload = json.loads(result.stdout)
            self.assertTrue(payload["prerelease"])
            self.assertEqual(payload["make_latest"], "false")
            self.assertEqual(payload["tag_name"], tag)
            self.assertIn("Tilo Installer", payload["name"])
        result = subprocess.run([sys.executable, "-", "imager-v1.2.3", "false", ""], input=creation, text=True, capture_output=True, check=True)
        self.assertFalse(json.loads(result.stdout)["prerelease"])

    def test_native_guards_reject_wrong_os_and_architecture_before_signing(self):
        workflow = (ROOT / ".forgejo/workflows/release-imager.yml").read_text()
        commands = self.root / "commands"
        commands.mkdir()
        uname = commands / "uname"
        uname.write_text('#!/bin/sh\ncase "$1" in -s) echo "$TEST_SYSTEM";; -m) echo "$TEST_MACHINE";; *) exit 2;; esac\n')
        uname.chmod(0o755)
        for platform, title, system in (("linux", "Linux", "Linux"), ("macos", "macOS", "Darwin")):
            job = section(workflow, platform, 2)
            step = job.split(f"- name: Require native {title} architecture\n", 1)[1].split("      - ", 1)[0]
            program = textwrap.dedent(step.split("run: |\n", 1)[1]).replace("${{ matrix.arch }}", "aarch64")
            for actual_system, machine, expected in ((system, "arm64", 0), (system, "x86_64", 1),
                                                      ("Windows_NT", "arm64", 1)):
                env = dict(os.environ, PATH=str(commands) + os.pathsep + os.environ["PATH"],
                           TEST_SYSTEM=actual_system, TEST_MACHINE=machine)
                result = subprocess.run(["bash", "-e", "-c", program], cwd=ROOT, env=env, capture_output=True)
                with self.subTest(platform=platform, system=actual_system, machine=machine):
                    self.assertEqual(result.returncode, expected, result.stderr.decode())
            self.assertLess(job.index(f"Require native {title} architecture"), job.index("Configure release signing"))

    def test_workflow_keeps_native_signing_and_scoped_scheduling(self):
        workflow = (ROOT / ".forgejo/workflows/release-imager.yml").read_text()
        push = section(workflow, "push", 2)
        self.assertIn('branches: ["main"]', push)
        self.assertIn('tags: ["imager-v*"]', push)
        self.assertIn("    paths:", push)
        self.assertTrue(section(workflow, "workflow_dispatch", 2))
        self.assertIn("github.event_name != 'push'", section(workflow, "medium", 2))
        self.assertIn("needs: [plan, linux, windows, macos]", section(workflow, "publish", 2))
        plan = section(workflow, "plan", 2)
        self.assertIn("continuous-release.py protection", plan)
        self.assertIn("GITHUB_REPOSITORY_ID: ${{ github.repository_id }}", plan)
        self.assertNotIn("${{ github.ref_protected }}", workflow)
        self.assertIn("needs.plan.outputs.ref-protected", section(workflow, "publish", 2))
        for platform in ("linux", "macos"):
            self.assertIn('./os/dev/assert-native-arch.sh "${{ matrix.arch }}"', section(workflow, platform, 2))
        self.assertIn("needs.medium.result == 'success'", section(workflow, "catalog", 2))
        for platform in ("linux", "windows", "macos"):
            job = section(workflow, platform, 2)
            self.assertIn("enable-openid-connect: true", job)
            self.assertIn("needs.catalog.result == 'success'", job)
            self.assertIn("CREDENTIAL_BROKER", job)
            self.assertIn("TILO_REQUIRE_SIGNING=1", job)
            self.assertIn("TILO_REQUIRE_NATIVE_PACKAGES", job)
            self.assertIn("if-no-files-found: error", job)
        for platform in ("windows", "macos"):
            self.assertEqual(set(re.findall(r"- arch: (\w+)", section(workflow, platform, 2))), {"x86_64", "aarch64"})
        for promotion in ("homebrew", "winget", "site-downloads"):
            self.assertIn("    if: startsWith(github.ref, 'refs/tags/imager-v')\n", section(workflow, promotion, 2))


if __name__ == "__main__":
    unittest.main()
