"""tools/ci/build-os.sh runs every step of the OS image, the development image and a board's
image, with the commands those steps' scripts take, in an order where each step comes after what
it reads; tools/ci/signin-e2e-builder.sh takes its image and medium from it."""

import json
import platform
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ARCH = platform.machine()


def dry_run(script, *args):
    with tempfile.TemporaryDirectory() as target:
        result = subprocess.run(
            ["bash", f"tools/ci/{script}", "--target", target, *args, "--dry-run"],
            cwd=ROOT, capture_output=True, text=True, check=True,
        )
        return result.stderr.replace(target, "TARGET")


def board_for(arch):
    for profile in sorted((ROOT / "os/hardware/profiles").glob("*.json")):
        if json.loads(profile.read_text()).get("architecture") == arch:
            return profile.stem
    return None


@unittest.skipUnless(ARCH in ("x86_64", "aarch64"), "the OS builds on x86_64 or aarch64")
class BuildOsTests(unittest.TestCase):
    def assert_in_order(self, output, commands):
        at = 0
        for command in commands:
            found = output.find(command, at)
            self.assertNotEqual(found, -1, f"{command!r} missing or out of order")
            at = found + len(command)

    def os_image_commands(self):
        plan = tomllib.loads((ROOT / "os/image/layers.toml").read_text())
        commands = [
            "bash os/dev/cache-github-assets.sh",
            f"bash tools/ci/publish/rpms-build.sh {ARCH} /target/publish/rpms/{ARCH}",
            f"bash os/image/assembler.sh {ARCH} /target/os/{ARCH}/assembler",
            f"bash os/image/build-layer.sh base {ARCH} /target/os/{ARCH}/base",
        ]
        commands += [
            f"bash os/image/build-layer.sh image {image['name']} {ARCH} /target/os/{ARCH}/{image['name']}"
            for image in plan["image"]
        ]
        commands += [
            f"bash os/image/compile.sh /target/os/{ARCH}/compiled",
            f"bash os/image/compile.sh --chromium /target/os/{ARCH}/chromium",
        ]
        commands += [
            f"bash os/image/build-layer.sh layer {layer['name']} {ARCH} /target/os/{ARCH}/layers/{layer['name']}"
            for layer in plan["layer"]
        ]
        commands.append(f"bash os/image/build-layer.sh compose {ARCH} /target/os/{ARCH}/image")
        return commands

    def test_board_runs_the_image_dev_image_and_board_steps(self):
        board = board_for(ARCH)
        if board is None:
            self.skipTest(f"no board on {ARCH}")
        output = dry_run("build-os.sh", "board", board)
        runtimes = subprocess.run(
            ["bash", "tools/release/local-model-runtime.sh", "ids"],
            cwd=ROOT, capture_output=True, text=True, check=True,
        ).stdout.split()
        self.assert_in_order(output, self.os_image_commands() + [
            f"bash os/image/compile.sh --dev /target/os/{ARCH}/compiled-dev",
            "bash tools/ci/publish/rpm-snapshot-ref.sh /target/publish/rpm-snapshot",
            f"bash tools/ci/publish/env-runtime.sh tree {ARCH} /target/publish/rpm-snapshot "
            f"/target/publish/rpms/{ARCH} /target/publish/env-runtime/dev-trees",
            f"bash tools/ci/publish/env-runtime.sh dev {ARCH} /target/publish/env-runtime/dev-trees "
            f"/target/os/{ARCH}/compiled /target/publish/env-runtime/dev/{ARCH}",
            "bash tools/release/local-model-catalog.sh compiler /target/model-catalog/bin",
            *[
                f"bash tools/release/local-model-runtime.sh build {id} /target/local-model-runtimes/{id}"
                for id in runtimes if id.endswith(f"-{ARCH}")
            ],
            f"bash tools/release/local-model-catalog.sh dev {ARCH} "
            f"/target/model-catalog/bin/tilo-model-catalog /target/model-catalog/dev/{ARCH}",
            f"bash tools/ci/publish/os-image.sh dev {ARCH} /target/os/{ARCH}/image "
            f"/target/publish/os-dev/{ARCH} /target/os/{ARCH}/compiled-dev",
            f"bash tools/ci/publish/os-image.sh board {board} /target/publish/os-dev/{ARCH} "
            f"/target/publish/os-board/{board}",
        ])
        for id in runtimes:
            if not id.endswith(f"-{ARCH}"):
                self.assertNotIn(f"local-model-runtime.sh build {id} ", output)
        # Each step once, though the board needs the image twice over.
        self.assertEqual(output.count(f"compose {ARCH} "), 1)

    def test_a_board_of_another_architecture_is_refused(self):
        other = board_for("x86_64" if ARCH == "aarch64" else "aarch64")
        if other is None:
            self.skipTest("no board of the other architecture")
        with tempfile.TemporaryDirectory() as target:
            result = subprocess.run(
                ["bash", "tools/ci/build-os.sh", "--target", target, "--dry-run", "board", other],
                cwd=ROOT, capture_output=True, text=True,
            )
        self.assertEqual(result.returncode, 2)
        self.assertIn(f"the {other} image builds on", result.stderr)

    def test_signin_e2e_takes_the_image_and_medium_from_build_os(self):
        with tempfile.TemporaryDirectory() as out:
            output = dry_run("signin-e2e-builder.sh", "--out", out)
        self.assert_in_order(output, self.os_image_commands() + [
            "build-os: tilo-vm",
            "bash tools/ci/medium-in-worker-vm.sh",
            "bash tools/ci/signin-e2e.sh /target/publish/dev-installer-medium/",
        ])
        self.assertNotIn("os-image.sh dev", output)
        # One definition of each step: the sign-in script builds nothing itself.
        script = (ROOT / "tools/ci/signin-e2e-builder.sh").read_text()
        for step in ("build-layer.sh", "compile.sh", "rpms-build.sh", "medium-in-worker-vm.sh"):
            self.assertNotIn(step, script.split("set -euo pipefail", 1)[1])


if __name__ == "__main__":
    unittest.main()
