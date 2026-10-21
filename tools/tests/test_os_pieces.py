#!/usr/bin/env python3
"""os/Containerfile is os/image/'s pieces, and each layer action builds what it says."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


pieces = load("os_pieces_under_test", REPO / "tools/ci/os/pieces.py")
layers = load("oci_layers_under_test", REPO / "tools/ci/os/oci_layers.py")


class PiecesTests(unittest.TestCase):
    def test_the_containerfile_is_the_pieces(self) -> None:
        self.assertEqual((REPO / "os/Containerfile").read_text(encoding="utf-8"), pieces.whole())

    def test_no_piece_ends_inside_an_instruction(self) -> None:
        # Pieces are joined into one file: a trailing continuation would swallow the next piece.
        for path in sorted((REPO / "os/image").rglob("*.Containerfile")):
            lines = [line for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
            self.assertFalse(lines[-1].rstrip().endswith("\\"), path)

    def test_every_layer_builds_over_a_layer_below(self) -> None:
        defined = {"${BASE_REF}"}
        for layer in pieces.plan()["layer"]:
            parent = pieces.parent(layer["name"])
            if layer.get("scratch"):
                self.assertEqual(parent, "scratch", layer["name"])
            else:
                self.assertIn(parent, defined, layer["name"])
            defined |= set(pieces.stages(pieces.action_text(layer["name"])))

    def test_no_piece_compiles_the_workspace(self) -> None:
        # Only bootc, a pinned upstream release, compiles inside an image build;
        # the workspace compiles outside (os/image/compile.sh).
        compiling = sorted(path.name for path in (REPO / "os/image").rglob("*.Containerfile")
                           if "cargo build" in path.read_text(encoding="utf-8"))
        self.assertEqual(compiling, ["bootc-builder.Containerfile"])

    def test_the_image_stages_end_in_the_seal(self) -> None:
        stages = pieces.image_stages()
        self.assertEqual(stages[0], "packages")
        self.assertEqual(stages[-1], "final")


class OciLayersTests(unittest.TestCase):
    def blob(self, root: Path, data: bytes) -> dict:
        digest, size = layers.put_blob(str(root), data)
        return {"digest": digest, "size": size}

    def layout(self, root: Path, blobs: list[bytes]) -> None:
        root.mkdir(parents=True)
        descriptors = [dict(self.blob(root, b), mediaType="application/vnd.oci.image.layer.v1.tar") for b in blobs]
        diff_ids = [d["digest"] for d in descriptors]
        config = {"architecture": layers.oci_arch(os.uname().machine), "os": "linux", "config": {},
                  "rootfs": {"type": "layers", "diff_ids": diff_ids}}
        config_digest, config_size = layers.put_blob(str(root), layers.canonical(config))
        manifest = {"schemaVersion": 2, "mediaType": layers.MANIFEST,
                    "config": {"mediaType": layers.CONFIG, "digest": config_digest, "size": config_size},
                    "layers": descriptors}
        digest, size = layers.put_blob(str(root), layers.canonical(manifest))
        layers.write_layout(str(root), {"mediaType": layers.MANIFEST, "digest": digest, "size": size})

    def test_take_then_compose_gives_back_the_image(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.layout(root / "base", [b"base"])
            self.layout(root / "built", [b"base", b"mine"])
            (root / "inputs.json").write_text(json.dumps({"inputs": {}, "inputsDigest": "sha256:" + "0" * 64}))
            layers.cmd_take(str(root / "base"), str(root / "built"), str(root / "fragment"), "mine",
                            str(root / "inputs.json"))
            fragment = json.loads((root / "fragment/fragment.json").read_text())
            self.assertEqual([l["diffID"] for l in fragment["layers"]], [layers.sha256_bytes(b"mine")])
            first = layers.compose(str(root / "one"), [str(root / "base"), str(root / "fragment")], "0")
            again = layers.compose(str(root / "two"), [str(root / "base"), str(root / "fragment")], "0")
            self.assertEqual(first["digest"], again["digest"])
            manifest, config = layers.image_of_layout(str(root / "one/oci"))
            self.assertEqual(config["rootfs"]["diff_ids"], [layers.sha256_bytes(d) for d in (b"base", b"mine")])
            self.assertEqual(manifest["layers"][1]["annotations"][layers.ACTION], "mine")
            layers.cmd_take(str(root / "base"), str(root / "built"), str(root / "again"), "mine",
                            str(root / "inputs.json"))
            self.assertEqual(json.loads((root / "again/fragment.json").read_text())["layers"], fragment["layers"])

    def test_take_refuses_an_image_not_built_on_its_parent(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.layout(root / "base", [b"base"])
            self.layout(root / "built", [b"other", b"mine"])
            (root / "inputs.json").write_text(json.dumps({"inputs": {}, "inputsDigest": ""}))
            result = subprocess.run([sys.executable, "-I", str(REPO / "tools/ci/os/oci_layers.py"), "take",
                                     str(root / "base"), str(root / "built"), str(root / "fragment"), "mine",
                                     str(root / "inputs.json")], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("is not built on", result.stderr)

if __name__ == "__main__":
    unittest.main()
