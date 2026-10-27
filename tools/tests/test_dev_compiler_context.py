"""The dev image's Podman argv: its components come compiled, as the `compiled-dev` context."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "os/dev/build-dev-image.sh"


class DevCompilerContextTests(unittest.TestCase):
    def invoke(self, overrides=None):
        source = HELPER.read_text()
        start = source.index("# The dev components, compiled outside the image build")
        build = source[start:].split('msg "built $dev_image', 1)[0]
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "podman.log"
            compiled = Path(directory) / "compiled-dev"
            compiled.mkdir()
            script = """set -euo pipefail
            die() { printf '%s\n' "$*" >&2; exit 1; }
            msg() { :; }
            podman() { printf '%s\n' "$*" >>"$LOG"; }
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
            """ + build
            output = Path(directory) / "output"
            env = {"PATH": os.environ["PATH"], "LOG": str(log), "TILO_COMPILED_DEV": str(compiled),
                   "TILO_OUTPUT": str(output), **(overrides or {})}
            result = subprocess.run(["bash", "-c", script], env=env, text=True, capture_output=True)
            return result, log.read_text().splitlines() if log.exists() else [], compiled, output

    def test_a_compiled_tree_is_given_as_the_build_context_and_nothing_compiles(self):
        result, calls, compiled, output = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(calls), 1, calls)
        build = calls[0]
        # No runtime given: an empty one, so the build fails unless the base ships one.
        self.assertIn(f"--build-context dev-env-runtime={output}/no-env-runtime", build)
        self.assertIn("-f /fixture/os/Containerfile.dev", build)
        self.assertIn(f"--build-context compiled-dev={compiled.resolve()}", build)
        for argument in ("RPMS_REF=registry.tilo.so/tilo/rpms@sha256:fixture",
                         "TILO_BASE_DIGEST=sha256:fixture", "SOURCE_DATE_EPOCH=1791750000",
                         "sh.tilo.dev.base-trust=integration"):
            self.assertIn(argument, build)
        self.assertNotIn("--secret", build)
        self.assertNotIn("TILO_CARGO_FRESHNESS", build)

    def test_a_given_env_runtime_is_the_dev_env_runtime_context(self):
        with tempfile.TemporaryDirectory() as directory:
            runtime = Path(directory)
            result, calls, _, _ = self.invoke({"TILO_DEV_ENV_RUNTIME": str(runtime)})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(f"--build-context dev-env-runtime={runtime.resolve()}", calls[0])

    def test_the_dev_image_recipe_ensures_an_env_runtime(self):
        recipe = (ROOT / "os/Containerfile.dev").read_text()
        self.assertIn("from=dev-env-runtime", recipe)
        self.assertIn("ensure-dev-env-runtime.sh", recipe)

    def test_the_dev_image_recipe_compiles_nothing(self):
        recipe = (ROOT / "os/Containerfile.dev").read_text()
        self.assertIn("COPY --from=compiled-dev / /out/", recipe)
        self.assertNotIn("cargo build", recipe)


if __name__ == "__main__":
    unittest.main()
