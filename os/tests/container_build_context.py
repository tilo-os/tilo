#!/usr/bin/env python3
"""Build real external-asset stages offline with rootless Podman.

Run explicitly with `python3 os/tests/container_build_context.py`. No registry,
base image, credentials, or full external-asset download is required.
"""

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]


class ExternalAssetContextTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if shutil.which("podman") is None:
            raise RuntimeError("this integration test requires Podman")

    def test_asset_stages_use_the_named_directory_without_registry_resolution(self):
        for name in ("Containerfile", "Containerfile.builder", "Containerfile.rpms"):
            with self.subTest(containerfile=name), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                assets = root / "assets"
                assets.mkdir()
                payload = b"checksum-verified asset fixture\n"
                (assets / "probe").write_bytes(payload)
                iid = root / "image-id"
                image = None
                container = None
                try:
                    subprocess.run(
                        ["podman", "build", "--pull=never", "--network=none",
                         "--target", "external-assets", "--iidfile", str(iid),
                         "--build-context", f"github-assets={assets}",
                         "--build-arg", "BASE_REF=scratch",
                         "--build-arg", "RPMS_REF=scratch",
                         "--build-arg", "KERNEL_RPMS_REF=scratch",
                         "-f", str(REPO / "os" / name), str(root)],
                        check=True, capture_output=True, text=True, timeout=60,
                    )
                    image = iid.read_text().strip()
                    container = subprocess.check_output(
                        ["podman", "create", image, "/bin/true"], text=True,
                    ).strip()
                    subprocess.run(
                        ["podman", "cp", f"{container}:/probe", str(root / "actual")],
                        check=True, capture_output=True,
                    )
                    self.assertEqual((root / "actual").read_bytes(), payload)
                finally:
                    if container:
                        subprocess.run(["podman", "rm", container], check=True, capture_output=True)
                    if image:
                        subprocess.run(["podman", "rmi", image], check=True, capture_output=True)


if __name__ == "__main__":
    unittest.main()
