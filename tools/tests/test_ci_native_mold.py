import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from hang import HANG_BOUND


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "ci"))
import t_matrix  # noqa: E402
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
            timeout=HANG_BOUND,
        )

    def test_native_mold_wrapper_links_only_explicit_native_targets_with_hardening(self) -> None:
        with self.subTest(phase='preserves hardening and spaced arguments'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='preserves x86 shadow stack flag'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='requires an explicit native gnu target'):
            self.doCleanups()
            self.setUp()
            self.environment.pop("TILO_CI_MOLD_TARGET")
            self.assertEqual(self.run_wrapper().returncode, 2)
            for target in (
                "x86_64-unknown-linux-gnu",
                "aarch64-unknown-linux-musl",
            ):
                self.environment["TILO_CI_MOLD_TARGET"] = target
                self.assertEqual(self.run_wrapper().returncode, 2)
        with self.subTest(phase='rejects a cross compiler'):
            self.doCleanups()
            self.setUp()
            self.tool("gcc", "echo x86_64-redhat-linux-gnu")
            self.assertEqual(self.run_wrapper().returncode, 2)
        with self.subTest(phase='accepts fedora driver tuple'):
            self.doCleanups()
            self.setUp()
            self.tool(
                "gcc",
                'if [ "$1" = -dumpmachine ]; then '
                'echo aarch64-redhat-linux; else printf "%s\\n" "$@"; fi',
            )
            self.assertEqual(self.run_wrapper().returncode, 0)
        with self.subTest(phase='bounds linker threads'):
            self.doCleanups()
            self.setUp()
            for value in ("0", "33", "-1", "two", "2;echo bad"):
                self.environment["TILO_CI_MOLD_THREADS"] = value
                self.assertEqual(self.run_wrapper().returncode, 2)
            self.environment["TILO_CI_MOLD_THREADS"] = "4"
            result = self.run_wrapper()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("-Wl,--threads=4", result.stdout.splitlines())


class NativeMoldBuilderPolicyTests(unittest.TestCase):
    def test_builder_installs_mold_as_the_native_default_linker(self) -> None:
        with self.subTest(phase='builder records mold and rebuilds when wrapper changes'):
            recipe = (ROOT / "os" / "Containerfile.builder").read_text(encoding="utf-8")
            assets = (ROOT / "os" / "external-assets" / "github-assets.tsv").read_text(
                encoding="utf-8"
            )
            checks = {check["name"]: check for check in t_matrix.checks(ROOT)}
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
                for architecture in ("x86_64", "aarch64"):
                    self.assertIn(path, checks[f"image-ci-builder-{architecture}"]["inputs"])
            self.assertIn("bash /tmp/verify-native-mold.sh", recipe)
        with self.subTest(phase='builder makes mold the native default linker'):
            recipe = (ROOT / "os" / "Containerfile.builder").read_text(encoding="utf-8")
            verify = (ROOT / "tools" / "ci" / "verify-native-mold.sh").read_text(
                encoding="utf-8"
            )
            # CI overrides CARGO_HOME, so the default lives in the root config,
            # keyed on the native triple only, with the wrapper's explicit target.
            self.assertIn('target="$(uname -m)-unknown-linux-gnu"', recipe)
            self.assertIn("> /.cargo/config.toml", recipe)
            self.assertIn(
                "'[target.%s]\\nlinker = \"/usr/local/bin/tilo-ci-mold\"\\n\\n[env]\\n"
                "TILO_CI_MOLD_TARGET = \"%s\"\\n'",
                recipe,
            )
            self.assertNotIn("rustflags", recipe.split("> /.cargo/config.toml")[0].rsplit("RUN", 1)[1])
            # The image build proves Cargo links with mold without job settings.
            self.assertIn('CARGO_HOME="$work/cargo-home"', verify)
            self.assertIn("env -u TILO_CI_MOLD_TARGET -u RUSTC_WRAPPER", verify)
            self.assertIn('readelf -p .comment "$default_binary" | grep -F \'mold \'', verify)


if __name__ == "__main__":
    unittest.main()
