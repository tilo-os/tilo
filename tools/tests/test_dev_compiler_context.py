"""The real dev Podman argv receives cache secrets and the chosen CI profile."""
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
        source = HELPER.read_text()
        setup = source.split('read -r -a _extra_build_args <<< "${TILO_BUILD_ARGS:-}"\n', 1)[1]
        setup = setup.split('if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then', 1)[0]
        build = source.split('podman build "${TILO_PODMAN_BUILD_FLAGS[@]}"', 1)[1]
        build = 'podman build "${TILO_PODMAN_BUILD_FLAGS[@]}"' + build.split('msg "built $dev_image', 1)[0]
        with tempfile.TemporaryDirectory() as directory:
            argv = Path(directory) / "argv"
            script = """set -euo pipefail
            die() { printf '%s\n' "$*" >&2; exit 1; }
            podman() { printf '%s\n' "$@" >"$ARGV"; }
            cargo_build_jobs() { printf '4\n'; }
            TILO_PODMAN_BUILD_FLAGS=(--jobs 4 --security-opt label=disable)
            _extra_build_args=(--label sh.tilo.dev.base-trust=integration)
            TILO_OS_DIR=/fixture/os
            TILO_REPO=/fixture
            base=registry.tilo.so/tilo/tilo-ci@sha256:fixture
            rpms_ref=registry.tilo.so/tilo/rpms@sha256:fixture
            dev_compile_base=registry.tilo.so/tilo/fedora-bootc@sha256:fixture
            base_digest=sha256:fixture
            base_created=1791750000
            SOURCE_DATE_EPOCH=1791750000
            dev_image=localhost/tilo-vm:ci
            """ + setup + build
            env = {"PATH": os.environ["PATH"], "ARGV": str(argv), **(overrides or {})}
            result = subprocess.run(["bash", "-c", script], env=env, text=True, capture_output=True)
            return result, argv.read_text().splitlines() if argv.exists() else []

    def test_shared_cache_uses_secret_names_and_preserves_clean_source_arguments(self):
        env = {"TILO_DEV_COMPILER_CACHE": "shared", "TILO_DEV_RUST_LTO": "off",
               "TILO_CARGO_FRESHNESS": "clean", "ACTIONS_CACHE_URL": "http://fixture-cache:8088/run/",
               "ACTIONS_RUNTIME_TOKEN": "fixture-runtime-token"}
        result, argv = self.invoke(env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("id=actions_cache_url,env=ACTIONS_CACHE_URL", argv)
        self.assertIn("id=actions_runtime_token,env=ACTIONS_RUNTIME_TOKEN", argv)
        self.assertNotIn(env["ACTIONS_CACHE_URL"], argv)
        self.assertNotIn(env["ACTIONS_RUNTIME_TOKEN"], argv)
        for argument in ("TILO_CARGO_FRESHNESS=clean", "TILO_DEV_RUST_LTO=off",
                         "RPMS_REF=registry.tilo.so/tilo/rpms@sha256:fixture",
                         "TILO_BASE_DIGEST=sha256:fixture", "SOURCE_DATE_EPOCH=1791750000"):
            self.assertIn(argument, argv)
        self.assertIn("sh.tilo.dev.base-trust=integration", argv)
        self.assertNotIn("--network", argv)

    def test_off_never_forwards_secrets_even_if_runner_variables_exist(self):
        result, argv = self.invoke({"TILO_DEV_COMPILER_CACHE": "off", "ACTIONS_CACHE_URL": "fixture",
                                    "ACTIONS_RUNTIME_TOKEN": "fixture-runtime-token"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--secret", argv)
        self.assertFalse(any(a.startswith("TILO_DEV_RUST_LTO=") for a in argv))

    def test_missing_secret_or_invalid_mode_fails_before_build(self):
        for env in ({"TILO_DEV_COMPILER_CACHE": "shared", "ACTIONS_CACHE_URL": "fixture"},
                    {"TILO_DEV_COMPILER_CACHE": "unknown"}, {"TILO_DEV_RUST_LTO": "fat"}):
            with self.subTest(env=env):
                result, argv = self.invoke(env)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(argv, [])

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
