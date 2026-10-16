"""Build / OS dev picks x86_64 for pull requests and adds aarch64 when needed."""
import importlib.util
import fnmatch
import os
import http.server
import json
import threading
import subprocess
import tempfile
import textwrap
import re
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("os_dev_arches", ROOT / "tools/ci/os_dev_arches.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class OsDevArchesTests(unittest.TestCase):
    def test_workflow_excludes_only_windows_host_transport(self):
        workflow = (ROOT / ".forgejo/workflows/os-dev.yml").read_text()
        event = re.search(r"(?ms)^  pull_request:\n(.*?)(?=^[^ ].*|\Z)", workflow)
        self.assertIsNotNone(event, "Cannot observe the pull_request event")
        patterns = re.findall(r'^      - "([^\"]+)"$', event[1], re.MULTILINE)
        self.assertIn("os/**", patterns)

        def selected(path):
            included = False
            for pattern in patterns:
                negative = pattern.startswith("!")
                if fnmatch.fnmatchcase(path, pattern[1:] if negative else pattern):
                    included = not negative
            return included

        host = ["os/dev/vm-windows.ps1", "os/tests/vm-windows-tool-resolver-test.ps1"]
        for path in host:
            self.assertFalse(selected(path), path)
        payload = ["os/Containerfile", "os/Containerfile.dev", "os/Containerfile.installer",
                   "os/usr/libexec/tilo-installer-code-refresh.py",
                   "os/dev/windows_provider.py", "os/dev/uki.sh",
                   "os/dev/prepare-hardware-profile.py", "os/kernel/config/aarch64",
                   "os/hardware/profiles/lenovo-83ed.toml",
                   "os/dev/vm-windows.ps1.example"]
        for path in payload:
            with self.subTest(path=path):
                self.assertTrue(selected(path), path)
                self.assertTrue(any(selected(item) for item in host + [path]))
        self.assertFalse(any(selected(path) for path in host))
        # Full dev images are explicit; ordinary pushes never request one.
        self.assertNotRegex(workflow, r"(?m)^  push:$")
        self.assertEqual(MODULE.arches("push", paths=host), ["x86_64"])

    def test_architecture_choice(self):
        both = ["x86_64", "aarch64"]
        self.assertEqual(MODULE.arches("push"), ["x86_64"])
        self.assertEqual(MODULE.arches("pull_request", paths=["os/etc/chrony.conf"]), ["x86_64"])
        self.assertEqual(MODULE.arches("pull_request", labels=["os-arm64"]), both)
        for path in ("os/kernel/config/aarch64", "os/hardware/profiles/lenovo-83ed.toml",
                     "os/kernel-buildroot-snapshot.env", "os/dev/uki.sh",
                     "os/external-assets/fex-source.env"):
            with self.subTest(path=path):
                self.assertEqual(MODULE.arches("pull_request", paths=[path]), both)
        self.assertEqual(MODULE.arch_specific(["os/Containerfile", "os/kernel/x"]), ["os/kernel/x"])

    def test_image_requests_are_explicit_before_native_admission(self):
        workflow = (ROOT / ".forgejo/workflows/os-dev.yml").read_text()
        self.assertIn("    types: [opened, synchronize, reopened, labeled]", workflow)
        self.assertIn("  workflow_dispatch:", workflow)
        arches = workflow.split("  arches:\n", 1)[1].split("\n  image:", 1)[0]
        self.assertIn("    if: github.event_name == 'workflow_dispatch' || "
                      "contains(github.event.pull_request.labels.*.name, 'os-image') || "
                      "contains(github.event.pull_request.labels.*.name, 'os-arm64')", arches)
        image = workflow.split("\n  image:\n", 1)[1]
        self.assertIn("    needs: arches\n", image)
        self.assertNotIn("always()", image)
        self.assertIn("    uses: ./.forgejo/workflows/build-image.yml", image)
        release = (ROOT / ".forgejo/workflows/build-image.yml").read_text()
        self.assertRegex(release, r"(?m)^  push:$")
        self.assertIn('tags: ["v*"]', release)

    def test_explicit_main_builds_newest_commit_one_at_a_time_per_arch(self):
        for name, arch in (("os-dev-arm.yml", "aarch64"), ("os-dev-x86.yml", "x86_64")):
            with self.subTest(arch=arch):
                workflow = (ROOT / ".forgejo/workflows" / name).read_text()
                self.assertRegex(workflow, r"(?m)^  workflow_dispatch:$")
                self.assertNotRegex(workflow, r"(?m)^  push:$")
                group = "os-dev-x86-native" if arch == "x86_64" else "os-dev-aarch64-main"
                self.assertIn(f"  group: {group}\n  cancel-in-progress: false\n", workflow)
                self.assertIn('if [[ $head == "$GITHUB_SHA" ]]; then', workflow)
                self.assertIn("    if: needs.newest.outputs.build == 'true'\n"
                              "    uses: ./.forgejo/workflows/build-image.yml\n"
                              "    with:\n"
                              f"      arches: '[\"{arch}\"]'\n", workflow)

    def test_pr_and_main_dev_images_share_the_native_queue_without_cancellation(self):
        for name in ("os-dev.yml", "os-dev-x86.yml"):
            with self.subTest(workflow=name):
                workflow = (ROOT / ".forgejo/workflows" / name).read_text()
                concurrency = re.search(r"(?ms)^concurrency:\n(.*?)(?=^[^ ].*|\Z)", workflow)
                self.assertIsNotNone(concurrency, "Cannot observe caller concurrency")
                self.assertEqual(concurrency.group(1).strip(),
                                 "group: os-dev-x86-native\n  cancel-in-progress: false")
                self.assertIn("uses: ./.forgejo/workflows/build-image.yml", workflow)


class OsDevPinnedDispatchTests(unittest.TestCase):
    def run_guard(self, requested, head, ref="refs/heads/main"):
        workflow = (ROOT / ".forgejo/workflows/os-dev-x86.yml").read_text()
        script = textwrap.dedent(workflow.split("        run: |\n", 1)[1].split("\n  image:", 1)[0])
        source = "a" * 40
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            curl = root / "curl"
            curl.write_text('''#!/bin/sh
printf called > "$CALLS"
printf '{"commit":{"id":"%s"}}\\n' "$MAIN_HEAD"
''')
            curl.chmod(0o755)
            output, calls = root / "output", root / "calls"
            env = {**os.environ, "PATH": f"{root}:{os.environ['PATH']}", "SOURCE_REVISION": requested,
                   "MAIN_HEAD": head, "GITHUB_SHA": source, "GITHUB_REF": ref,
                   "GITHUB_OUTPUT": str(output), "CALLS": str(calls), "TOKEN": "fixture",
                   "GITHUB_SERVER_URL": "https://invalid.example", "GITHUB_REPOSITORY": "tilo/tilo"}
            result = subprocess.run(["bash", "-c", script], env=env, text=True, capture_output=True)
            return result, output.read_text() if output.exists() else "", calls.exists()

    def test_pinned_main_dispatch_builds_captured_source_after_main_advances(self):
        result, output, called = self.run_guard("a" * 40, "b" * 40)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, "source_revision=" + "a" * 40 + "\nbuild=true\n")
        self.assertIn("a" * 40, result.stdout)
        self.assertFalse(called)

    def test_latest_only_dispatch_still_requires_current_main(self):
        for head, expected in (("a" * 40, "true"), ("b" * 40, "false")):
            with self.subTest(head=head):
                result, output, called = self.run_guard("", head)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(output, f"build={expected}\n")
                self.assertTrue(called)

    def test_pinning_rejects_another_ref_and_malformed_revision(self):
        for requested, ref in (("a" * 40, "refs/heads/other"), ("yes", "refs/heads/main")):
            with self.subTest(requested=requested, ref=ref):
                result, output, called = self.run_guard(requested, "b" * 40, ref)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(output, "")
                self.assertFalse(called)

    def test_pin_is_opt_in_and_keeps_native_admission_and_image_contract(self):
        workflow = (ROOT / ".forgejo/workflows/os-dev-x86.yml").read_text()
        self.assertIn("      source_revision:\n", workflow)
        self.assertIn("        required: false\n        default: ''", workflow)
        self.assertIn("  group: os-dev-x86-native\n  cancel-in-progress: false", workflow)
        self.assertIn("    needs: newest\n    if: needs.newest.outputs.build == 'true'", workflow)
        self.assertIn("      arches: '[\"x86_64\"]'", workflow)
        self.assertIn("    secrets: inherit", workflow)


class OsDevImageSourceTests(unittest.TestCase):
    def source_script(self):
        workflow = (ROOT / ".forgejo/workflows/build-image.yml").read_text()
        block = workflow.split("      - name: Verify and resolve the exact image source\n", 1)[1]
        return textwrap.dedent(block.split("        run: |\n", 1)[1].split("\n      # Both lanes", 1)[0])

    def run_source(self, requested_kind, event="workflow_call", ref="refs/heads/main", arches='["x86_64"]'):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            env = {**os.environ, "GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": "commit.gpgsign",
                   "GIT_CONFIG_VALUE_0": "false"}
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root, env=env, text=True,
                                               stderr=subprocess.DEVNULL).strip()
            git("init", "-q")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@example.invalid")
            (root / "payload").write_text("base")
            git("add", "payload")
            git("commit", "-qm", "base")
            ancestor = git("rev-parse", "HEAD")
            (root / "payload").write_text("main")
            git("commit", "-qam", "main")
            tip = git("rev-parse", "HEAD")
            git("checkout", "-qb", "other", ancestor)
            (root / "payload").write_text("other")
            git("commit", "-qam", "other")
            divergent = git("rev-parse", "HEAD")
            git("checkout", "--detach", tip)
            requested = {"ancestor": ancestor, "divergent": divergent, "empty": "", "malformed": "main", "unavailable": ancestor}[requested_kind]
            output = root / "output"
            records = {}
            for sha in (ancestor, tip, divergent):
                parents = git("show", "-s", "--format=%P", sha).split()
                records[sha] = {"sha": sha, "parents": [{"sha": value} for value in parents]}
            unavailable = requested_kind == "unavailable"
            class Handler(http.server.BaseHTTPRequestHandler):
                def do_GET(self):
                    sha = self.path.rsplit("/", 1)[-1]
                    if unavailable or sha not in records:
                        self.send_error(503)
                        return
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(json.dumps(records[sha]).encode())
                def log_message(self, *args):
                    pass
            server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.02}, daemon=True)
            thread.start()
            self.addCleanup(server.server_close)
            self.addCleanup(server.shutdown)
            policy = root / "source-policy/tools/ci"
            policy.mkdir(parents=True)
            (policy / "verify_main_source.py").write_bytes((ROOT / "tools/ci/verify_main_source.py").read_bytes())
            env.update({"TOKEN": "fixture", "GITHUB_REPOSITORY": "tilo/tilo",
                        "GITHUB_SERVER_URL": f"http://127.0.0.1:{server.server_port}"})
            env.update({"GITHUB_SHA": tip, "TILO_IMAGE_SOURCE": requested or tip,
                        "REQUESTED_SOURCE": requested, "GITHUB_EVENT_NAME": event, "DEV_ARCHES": arches,
                        "GITHUB_REF": ref, "GITHUB_OUTPUT": str(output)})
            result = subprocess.run(["bash", "-c", self.source_script()], cwd=root,
                                    env=env, capture_output=True, text=True)
            return result, output.read_text() if output.exists() else "", git("rev-parse", "HEAD"), ancestor, tip

    def test_exact_ancestor_is_verified_and_names_the_historical_image(self):
        result, output, head, ancestor, tip = self.run_source("ancestor")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(head, tip)
        self.assertEqual(output, f"source-short={ancestor[:7]}\n")

    def test_source_rejects_divergent_refs_and_release_entry_points(self):
        cases = [("divergent", "workflow_call", "refs/heads/main"),
                 ("malformed", "workflow_call", "refs/heads/main"),
                 ("ancestor", "workflow_dispatch", "refs/heads/main"),
                 ("ancestor", "workflow_call", "refs/heads/other")]
        for kind, event, ref in cases:
            with self.subTest(kind=kind, event=event, ref=ref):
                result, output, head, _, tip = self.run_source(kind, event, ref)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(output, "")
                self.assertEqual(head, tip)

    def test_historical_source_cannot_enter_arm_or_board_publication(self):
        result, output, head, _, tip = self.run_source("ancestor", arches='["x86_64","aarch64"]')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(output, "")
        self.assertEqual(head, tip)

    def test_unobservable_ancestry_fails_before_publication(self):
        result, output, head, _, tip = self.run_source("unavailable")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(output, "")
        self.assertEqual(head, tip)

    def test_default_source_keeps_existing_dispatch_commit(self):
        result, output, head, _, tip = self.run_source("empty", "workflow_dispatch")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(head, tip)
        self.assertEqual(output, f"source-short={tip[:7]}\n")

    def test_source_drives_checkout_tags_labels_and_promotion_without_spoofing_run_sha(self):
        workflow = (ROOT / ".forgejo/workflows/build-image.yml").read_text()
        self.assertIn("  TILO_IMAGE_SOURCE: ${{ inputs.source_revision || github.sha }}", workflow)
        self.assertIn("          ref: ${{ env.TILO_IMAGE_SOURCE }}", workflow)
        self.assertIn("org.opencontainers.image.revision=${{ env.TILO_IMAGE_SOURCE }}", workflow)
        self.assertIn("type=raw,value=main-${{ needs.plan.outputs.source-short }}", workflow)
        self.assertIn('series="main-${TILO_IMAGE_SOURCE:0:7}"', workflow)
        self.assertIn('main-${TILO_IMAGE_SOURCE:0:7}-ci', workflow)
        self.assertIn('git -C "$history" merge-base --is-ancestor "$current" "$TILO_IMAGE_SOURCE"', workflow)
        self.assertNotIn("GITHUB_SHA=", workflow)
        jobs = workflow.split("jobs:\n", 1)[1]
        policy = jobs.split("      - name: Checkout current source verification policy\n", 1)[1].split("      - name: Verify and resolve", 1)[0]
        self.assertIn("ref: ${{ github.sha }}", policy)
        self.assertIn("path: source-policy", policy)
        self.assertNotIn("github.sha", jobs.replace(policy, ""))
        self.assertNotIn("fetch-depth: 0", workflow)



if __name__ == "__main__":
    unittest.main()
