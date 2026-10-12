#!/usr/bin/env python3
"""Exercise the production firmware mount with tiny, offline Podman builds.

Run on a builder: python3 os/tests/rootfs_firmware_cache.py
No registry image, compiler, real firmware or full OS build is needed.
"""

import json
import re
import shutil
import subprocess
import tempfile
import time
import unittest
import uuid
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]


class RootfsFirmwareCacheTests(unittest.TestCase):
    def test_only_the_selected_profile_invalidates_the_kernel_mount(self):
        if shutil.which("podman") is None:
            raise RuntimeError("this integration test requires Podman")
        source = (REPO / "os/Containerfile").read_text()
        mount = re.search(
            r"--mount=(type=bind,source=os/hardware/firmware[^\s]*,"
            r"target=/run/tilo-hardware-firmware)", source,
        ).group(1)
        directory = re.search(r'--firmware-dir "([^"]+)"', source).group(1)
        selected = mount.split("source=", 1)[1].split(",", 1)[0]
        selected = selected.replace("${TILO_HARDWARE_PROFILE}", "generic")
        self.assertTrue((REPO / selected).is_dir(), "a clean generic checkout must build")
        owner = "rootfs-firmware-cache-" + uuid.uuid4().hex
        observations = []
        with tempfile.TemporaryDirectory(prefix=owner + "-") as temp:
            root = Path(temp)
            runtime = root / "runtime"
            # Use the runner's existing shell, cat and their dynamic libraries.
            # This avoids a registry pull or a static-binary/compiler dependency.
            paths = {Path("/bin/sh"), Path("/usr/bin/cat")}
            for binary in list(paths):
                result = subprocess.run(
                    ["ldd", str(binary)], check=True, capture_output=True, text=True,
                )
                paths.update(Path(p) for p in re.findall(r"(/[^\s]+)", result.stdout))
            self.assertLess(sum(p.stat().st_size for p in paths), 32 * 1024 * 1024)
            for path in paths:
                target = runtime / path.relative_to("/")
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, target)
            firmware = root / "os/hardware/firmware"
            generic = firmware / "generic/probe"
            other = firmware / "fixture-board/probe"
            for path, data in [(generic, "generic-before"), (other, "board-before")]:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(data)
            recipe = root / "Containerfile"
            recipe.write_text(
                "FROM scratch\n"
                f"LABEL io.tilo.test.owner={owner}\n"
                "COPY runtime/ /\n"
                "ARG TILO_HARDWARE_PROFILE=generic\n"
                f'RUN --mount={mount} /usr/bin/cat "{directory}/probe" > /actual\n'
            )
            # Give the owned synthetic build context its normal container-file
            # label on SELinux hosts; keep container confinement enabled.
            if Path("/sys/fs/selinux/enforce").exists():
                subprocess.run(
                    ["chcon", "-R", "-t", "container_file_t", str(root)],
                    check=True, capture_output=True,
                )

            def build(profile="generic"):
                started = time.monotonic()
                result = subprocess.run(
                    ["podman", "build", "--pull=never", "--network=none", "--layers",
                     "--force-rm", "--cpu-period=100000", "--cpu-quota=200000",
                     "--memory=128m", "--iidfile", str(root / "image-id"),
                     "--build-arg", f"TILO_HARDWARE_PROFILE={profile}",
                     "-f", str(recipe), str(root)],
                    capture_output=True, text=True, timeout=45,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                run = re.search(
                    r"STEP \d+/\d+: RUN [^\n]+\n(.*?)(?=\nSTEP |\nCOMMIT|\Z)",
                    result.stdout, re.S,
                )
                self.assertIsNotNone(run, "must observe the actual firmware RUN")
                observation = {"profile": profile, "seconds": time.monotonic() - started,
                               "cached": "Using cache" in run.group(1)}
                observations.append(observation)
                return (root / "image-id").read_text().strip(), observation["cached"]

            def actual(image):
                container = subprocess.check_output(
                    ["podman", "create", image, "/bin/sh"], text=True,
                ).strip()
                try:
                    subprocess.run(
                        ["podman", "cp", f"{container}:/actual", str(root / "actual")],
                        check=True, capture_output=True,
                    )
                    return (root / "actual").read_text()
                finally:
                    subprocess.run(["podman", "rm", container], check=True, capture_output=True)

            try:
                image, cached = build()
                self.assertFalse(cached)
                self.assertEqual(actual(image), "generic-before")
                _, cached = build()
                self.assertTrue(cached, "the unchanged production mount must cache")
                other.write_text("board-imported")
                _, cached = build()
                self.assertTrue(cached, "another profile must not rebuild the generic kernel")
                generic.write_text("generic-changed")
                image, cached = build()
                self.assertFalse(cached, "selected firmware must invalidate the kernel cache")
                self.assertEqual(actual(image), "generic-changed")
                image, _ = build("fixture-board")
                self.assertEqual(actual(image), "board-imported", "board paths keep the same bytes")
            finally:
                images = subprocess.check_output(
                    ["podman", "images", "--filter", f"label=io.tilo.test.owner={owner}",
                     "--format", "{{.ID}}"], text=True,
                ).splitlines()
                if images:
                    subprocess.run(
                        ["podman", "rmi", *sorted(set(images))], check=True, capture_output=True,
                    )
                print(json.dumps({"owner": owner, "builds": observations}))


if __name__ == "__main__":
    unittest.main()
