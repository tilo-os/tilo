import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
WRAPPER = ROOT / "tools" / "ci" / "native-mold-linker.sh"


class NativeMoldLinkerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.binary_directory = Path(self.temporary.name)
        self.environment = dict(
            os.environ,
            PATH=f"{self.binary_directory}:/usr/bin:/bin",
            TILO_CI_MOLD_TARGET="aarch64-unknown-linux-gnu",
        )
        self.environment.pop("TILO_CI_MOLD_THREADS", None)
        self.tool("uname", '[ "$1" = -s ] && echo Linux || echo aarch64')
        self.tool("mold", "exit 0")
        self.tool(
            "gcc",
            'if [ "$1" = -dumpmachine ]; then '
            'echo aarch64-redhat-linux-gnu; else printf "%s\\n" "$@"; fi',
        )

    def tool(self, name: str, body: str) -> None:
        path = self.binary_directory / name
        path.write_text("#!/bin/sh\n" + body + "\n", encoding="utf-8")
        path.chmod(0o755)

    def run_wrapper(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(WRAPPER), *arguments],
            env=self.environment,
            text=True,
            capture_output=True,
            timeout=5,
        )

    def test_preserves_hardening_and_spaced_arguments(self) -> None:
        arguments = [
            "object with spaces.o",
            "-Wl,-z,relro,-z,now",
            "-Wl,-z,noexecstack",
        ]
        result = self.run_wrapper(*arguments)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout.splitlines(),
            arguments + ["-fuse-ld=mold", "-Wl,--threads=2"],
        )

    def test_preserves_x86_shadow_stack_flag(self) -> None:
        self.tool("uname", '[ "$1" = -s ] && echo Linux || echo x86_64')
        self.tool(
            "gcc",
            'if [ "$1" = -dumpmachine ]; then '
            'echo x86_64-redhat-linux-gnu; else printf "%s\\n" "$@"; fi',
        )
        self.environment["TILO_CI_MOLD_TARGET"] = "x86_64-unknown-linux-gnu"
        result = self.run_wrapper("-Wl,-z,shstk")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("-Wl,-z,shstk", result.stdout.splitlines())

    def test_requires_an_explicit_native_gnu_target(self) -> None:
        self.environment.pop("TILO_CI_MOLD_TARGET")
        self.assertEqual(self.run_wrapper().returncode, 2)
        for target in (
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-musl",
        ):
            self.environment["TILO_CI_MOLD_TARGET"] = target
            self.assertEqual(self.run_wrapper().returncode, 2)

    def test_rejects_a_cross_compiler(self) -> None:
        self.tool("gcc", "echo x86_64-redhat-linux-gnu")
        self.assertEqual(self.run_wrapper().returncode, 2)

    def test_accepts_fedora_driver_tuple(self) -> None:
        self.tool(
            "gcc",
            'if [ "$1" = -dumpmachine ]; then '
            'echo aarch64-redhat-linux; else printf "%s\\n" "$@"; fi',
        )
        self.assertEqual(self.run_wrapper().returncode, 0)

    def test_bounds_linker_threads(self) -> None:
        for value in ("0", "33", "-1", "two", "2;echo bad"):
            self.environment["TILO_CI_MOLD_THREADS"] = value
            self.assertEqual(self.run_wrapper().returncode, 2)
        self.environment["TILO_CI_MOLD_THREADS"] = "4"
        result = self.run_wrapper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("-Wl,--threads=4", result.stdout.splitlines())


class NativeMoldBuilderPolicyTests(unittest.TestCase):
    def test_builder_records_mold_and_rebuilds_when_wrapper_changes(self) -> None:
        recipe = (ROOT / "os" / "Containerfile.builder").read_text(encoding="utf-8")
        assets = (ROOT / "os" / "external-assets" / "github-assets.tsv").read_text(
            encoding="utf-8"
        )
        workflow = (
            ROOT / ".forgejo" / "workflows" / "build-builder-image.yml"
        ).read_text(encoding="utf-8")
        self.assertNotIn("dnf install -y --setopt=install_weak_deps=False mold", recipe)
        self.assertIn("mold-source.txt", recipe)
        for architecture, checksum in (
            (
                "x86_64",
                "4c999e19ffa31afa5aa429c679b665d5e2ca5a6b6832ad4b79668e8dcf3d8ec1",
            ),
            (
                "aarch64",
                "c799b9ccae8728793da2186718fbe53b76400a9da396184fac0c64aa3298ec37",
            ),
        ):
            archive = f"mold-2.40.4-{architecture}-linux.tar.gz"
            self.assertIn(f"{checksum}\t{archive}\t", assets)
            self.assertIn(checksum, recipe)
        self.assertIn("sha256sum -c -", recipe)
        self.assertIn(
            "COPY tools/ci/native-mold-linker.sh /usr/local/bin/tilo-ci-mold",
            recipe,
        )
        for path in (
            "tools/ci/native-mold-linker.sh",
            "tools/ci/verify-native-mold.sh",
        ):
            self.assertIn(f'- "{path}"', workflow)
        self.assertIn("bash /tmp/verify-native-mold.sh", recipe)


if __name__ == "__main__":
    unittest.main()
