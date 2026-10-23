"""OS image build helpers: the image epoch and the OCI archive ref selector."""
from pathlib import Path
import hashlib
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ImageEpochTests(unittest.TestCase):
    def test_the_image_epoch_is_the_newest_input_creation_time(self):
        with tempfile.TemporaryDirectory() as temporary:
            fake = Path(temporary)
            (fake / "skopeo").write_text(
                "#!/bin/sh\n"
                'case "$2" in\n'
                '  docker://base) echo \'{"Created": "2026-10-01T00:00:00Z"}\' ;;\n'
                '  docker://rpms) echo \'{"Created": "2026-10-09T12:00:00.5Z"}\' ;;\n'
                "  *) exit 1 ;;\n"
                "esac\n")
            (fake / "skopeo").chmod(0o755)
            env = {**os.environ, "PATH": f"{fake}:{os.environ['PATH']}"}
            script = str(ROOT / "os/dev/image-epoch.sh")
            for order in (["base", "rpms"], ["rpms", "base"]):
                epoch = subprocess.run(["bash", script, *order], env=env, check=True,
                                       capture_output=True, text=True).stdout
                # 2026-10-09T12:00:00Z, the newer of the two, in whole seconds.
                self.assertEqual(epoch, "1791547200\n", order)
            for missing in (["base", "nothing"], [""], []):
                failed = subprocess.run(["bash", script, *missing], env=env, capture_output=True, text=True)
                self.assertNotEqual(failed.returncode, 0, missing)


class OciArchiveRefTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "posix" and shutil.which("skopeo"), "skopeo on POSIX required")
    def test_two_ref_archive_needs_an_explicit_source_reference(self):
        # Buildx emits one top-level OCI index descriptor for each image tag.
        # The selected ref must work for both inspection and copy/import.
        first = "registry.tilo.so/tilo/tilo:gen1"
        second = "registry.tilo.so/tilo/tilo:main-abcdef0"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            layout = root / "layout"
            layout.mkdir()

            def blob(payload: bytes) -> tuple[str, int]:
                digest = hashlib.sha256(payload).hexdigest()
                target = layout / "blobs" / "sha256" / digest
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(payload)
                return digest, len(payload)

            def encoded(value: dict) -> bytes:
                return json.dumps(value, separators=(",", ":")).encode()

            layer_buffer = io.BytesIO()
            with tarfile.open(fileobj=layer_buffer, mode="w"):
                pass
            layer_digest, layer_size = blob(layer_buffer.getvalue())
            config_digest, config_size = blob(encoded({
                "architecture": "arm64", "os": "linux", "config": {},
                "rootfs": {"type": "layers", "diff_ids": [f"sha256:{layer_digest}"]},
            }))
            manifest_digest, manifest_size = blob(encoded({
                "schemaVersion": 2,
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "config": {
                    "mediaType": "application/vnd.oci.image.config.v1+json",
                    "digest": f"sha256:{config_digest}", "size": config_size,
                },
                "layers": [{
                    "mediaType": "application/vnd.oci.image.layer.v1.tar",
                    "digest": f"sha256:{layer_digest}", "size": layer_size,
                }],
            }))
            (layout / "oci-layout").write_text('{"imageLayoutVersion":"1.0.0"}')
            (layout / "index.json").write_bytes(encoded({
                "schemaVersion": 2,
                "manifests": [{
                    "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    "digest": f"sha256:{manifest_digest}", "size": manifest_size,
                    "annotations": {
                        "io.containerd.image.name": ref,
                        "org.opencontainers.image.ref.name": ref.rsplit(":", 1)[1],
                    },
                } for ref in (first, second)],
            }))
            archive = root / "image.tar"
            with tarfile.open(archive, "w") as tar:
                for path in sorted(layout.rglob("*"), key=lambda p: (len(p.parts), p.as_posix())):
                    if path.is_dir():
                        entry = tarfile.TarInfo(path.relative_to(layout).as_posix() + "/")
                        entry.type = tarfile.DIRTYPE
                        entry.mode = 0o755
                    else:
                        payload = path.read_bytes()
                        entry = tarfile.TarInfo(path.relative_to(layout).as_posix())
                        entry.size = len(payload)
                        entry.mode = 0o644
                    entry.uid = os.getuid()
                    entry.gid = os.getgid()
                    if path.is_dir():
                        tar.addfile(entry)
                    else:
                        tar.addfile(entry, io.BytesIO(payload))

            ambiguous = subprocess.run(
                ["skopeo", "inspect", "--raw", f"oci-archive:{archive}"],
                capture_output=True, text=True,
            )
            self.assertNotEqual(ambiguous.returncode, 0)
            selector = ROOT / "os/scripts/oci-archive-ref.py"
            chosen_ref = subprocess.check_output(
                [sys.executable, str(selector), str(archive), first,
                 f"sha256:{manifest_digest}"], text=True,
            ).strip()
            self.assertEqual(chosen_ref, "gen1")
            wrong_digest = subprocess.run(
                [sys.executable, str(selector), str(archive), first,
                 f"sha256:{'0' * 64}"], capture_output=True, text=True,
            )
            self.assertNotEqual(wrong_digest.returncode, 0)
            chosen = subprocess.run(
                ["skopeo", "inspect", "--raw", f"oci-archive:{archive}:{chosen_ref}"],
                capture_output=True, text=True,
            )
            self.assertEqual(chosen.returncode, 0, chosen.stderr)
            self.assertEqual(hashlib.sha256(chosen.stdout.encode()).hexdigest(), manifest_digest)
            subprocess.run(
                ["skopeo", "copy", f"oci-archive:{archive}:{chosen_ref}",
                 f"dir:{root / 'selected'}"],
                capture_output=True, text=True, check=True,
            )


if __name__ == "__main__":
    unittest.main()
