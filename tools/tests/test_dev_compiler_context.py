"""The real dev Podman argv receives cache variables by name and the chosen CI profile.

The dev components compile outside the image build (os/image/compile.sh --dev, run with
`podman run` in the OS builder image); the image build then takes the compiled tree.
"""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "os/dev/build-dev-image.sh"
WORKFLOW = ROOT / ".forgejo/workflows/build-image.yml"


class DevCompilerContextTests(unittest.TestCase):
    def invoke(self, overrides=None):
        """The helper from its compile through the image build, with podman recorded: the
        argv of each podman call, in order."""
        source = HELPER.read_text()
        body = source.split("# The dev components, compiled outside the image build", 1)[1]
        body = body.split('msg "built $dev_image', 1)[0]
        with tempfile.TemporaryDirectory() as directory:
            calls = Path(directory) / "calls"
            os_dir = Path(directory) / "os"
            (os_dir / "dev").mkdir(parents=True)
            assets = os_dir / "dev/cache-github-assets.sh"
            assets.write_text("#!/bin/sh\n")
            assets.chmod(0o755)
            script = """set -euo pipefail
            die() { printf '%s\\n' "$*" >&2; exit 1; }
            msg() { :; }
            podman() { { printf '%s\\n' "$@"; printf -- '--end--\\n'; } >>"$CALLS"; }
            cargo_build_jobs() { printf '4\\n'; }
            TILO_PODMAN_BUILD_FLAGS=(--jobs 4 --security-opt label=disable)
            _extra_build_args=(--label sh.tilo.dev.base-trust=integration)
            TILO_REPO=/fixture
            base=registry.tilo.so/tilo/tilo-ci@sha256:fixture
            rpms_ref=registry.tilo.so/tilo/rpms@sha256:fixture
            dev_compile_base=registry.tilo.so/tilo/fedora-bootc@sha256:fixture
            base_digest=sha256:fixture
            base_created=1791750000
            SOURCE_DATE_EPOCH=1791750000
            dev_image=localhost/tilo-vm:ci
            #""" + body
            env = {"PATH": os.environ["PATH"], "CALLS": str(calls), "TILO_OS_DIR": str(os_dir),
                   "TILO_OUTPUT": str(Path(directory) / "out"), **(overrides or {})}
            result = subprocess.run(["bash", "-c", script], env=env, text=True, capture_output=True)
            recorded = calls.read_text().split("--end--\n") if calls.exists() else []
            return result, [c.splitlines() for c in recorded if c]

    def compile_argv(self, calls):
        runs = [c for c in calls if c and c[0] == "run"]
        self.assertEqual(len(runs), 1, calls)
        return runs[0]

    def test_shared_cache_passes_the_runner_variables_by_name_only(self):
        env = {"TILO_DEV_COMPILER_CACHE": "shared", "TILO_DEV_RUST_LTO": "off",
               "ACTIONS_CACHE_URL": "http://fixture-cache:8088/run/",
               "ACTIONS_RUNTIME_TOKEN": "fixture-runtime-token"}
        result, calls = self.invoke(env)
        self.assertEqual(result.returncode, 0, result.stderr)
        argv = self.compile_argv(calls)
        self.assertIn("ACTIONS_CACHE_URL", argv)
        self.assertIn("ACTIONS_RUNTIME_TOKEN", argv)
        self.assertIn("RUSTC_WRAPPER=sccache", argv)
        self.assertIn("TILO_DEV_RUST_LTO=off", argv)
        for call in calls:
            self.assertNotIn(env["ACTIONS_CACHE_URL"], "\n".join(call))
            self.assertNotIn(env["ACTIONS_RUNTIME_TOKEN"], "\n".join(call))
        image = calls[-1]
        self.assertEqual(image[0], "build")
        for argument in ("RPMS_REF=registry.tilo.so/tilo/rpms@sha256:fixture",
                         "TILO_BASE_DIGEST=sha256:fixture", "SOURCE_DATE_EPOCH=1791750000"):
            self.assertIn(argument, image)
        self.assertIn("sh.tilo.dev.base-trust=integration", image)
        self.assertTrue(any(a.startswith("compiled-dev=") for a in image), image)
        self.assertNotIn("--network", image)

    def test_off_never_forwards_cache_variables_even_if_runner_variables_exist(self):
        result, calls = self.invoke({"TILO_DEV_COMPILER_CACHE": "off", "ACTIONS_CACHE_URL": "fixture",
                                     "ACTIONS_RUNTIME_TOKEN": "fixture-runtime-token"})
        self.assertEqual(result.returncode, 0, result.stderr)
        argv = self.compile_argv(calls)
        self.assertNotIn("ACTIONS_CACHE_URL", argv)
        self.assertNotIn("ACTIONS_RUNTIME_TOKEN", argv)
        self.assertFalse(any(a.startswith("RUSTC_WRAPPER=") for a in argv), argv)
        self.assertFalse(any("--secret" in a for c in calls for a in c))

    def test_a_tree_compiled_already_is_not_compiled_again(self):
        with tempfile.TemporaryDirectory() as compiled:
            result, calls = self.invoke({"TILO_COMPILED_DEV": compiled})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual([c[0] for c in calls], ["build"], calls)
            self.assertIn(f"compiled-dev={os.path.realpath(compiled)}", calls[0])

    def test_missing_secret_or_invalid_mode_fails_before_build(self):
        for env in ({"TILO_DEV_COMPILER_CACHE": "shared", "ACTIONS_CACHE_URL": "fixture"},
                    {"TILO_DEV_COMPILER_CACHE": "unknown"}, {"TILO_DEV_RUST_LTO": "fat"}):
            with self.subTest(env=env):
                result, calls = self.invoke(env)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(calls, [])

    def test_workflow_forwarding_is_inside_non_publication_dev_gate(self):
        source = WORKFLOW.read_text()
        step = source.split("      - name: Reseal, install, boot, and stage the verified image\n", 1)[1]
        step = step.split("\n      - name:", 1)[0]
        self.assertIn("DEV_COMPILER_CACHE: ${{ steps.compiler-cache.outputs.mode }}", step)
        before, dev = step.split("          if [[ $DEV_GATE == true ]]; then", 1)
        dev, publication = dev.split("          else\n", 1)
        self.assertIn('TILO_DEV_COMPILER_CACHE="$DEV_COMPILER_CACHE" TILO_DEV_RUST_LTO=off', dev)
        self.assertIn("TILO_CARGO_FRESHNESS=clean", dev)
        self.assertNotIn("TILO_DEV_COMPILER_CACHE=", before + publication)


if __name__ == "__main__":
    unittest.main()
