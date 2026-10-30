#!/usr/bin/env python3
"""os/Containerfile is os/image/'s pieces, and each layer action builds what it says."""

from __future__ import annotations

import gzip
import importlib.util
import io
import json
import os
import subprocess
import sys
import tarfile
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


def tar(*entries: tuple) -> bytes:
    """A layer: (name, None) a directory, (name, data) a file, (name, None, target) a hard link."""
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for name, data, *link in entries:
            info = tarfile.TarInfo(name)
            info.mtime = 1
            if link:
                info.type, info.linkname = tarfile.LNKTYPE, link[0]
            elif data is None:
                info.type, info.mode = tarfile.DIRTYPE, 0o755
            else:
                info.size = len(data)
            archive.addfile(info, io.BytesIO(data) if data else None)
    return out.getvalue()


def names(data: bytes) -> list[tuple]:
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        return [(m.name, archive.extractfile(m).read() if m.isfile() else m.linkname or None)
                for m in archive]


MINE = tar(("usr/bin/mine", b"mine"))
MORE = tar(("usr/bin/more", b"more"))


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
            self.layout(root / "built", [b"base", MINE])
            (root / "inputs.json").write_text(json.dumps({"inputs": {}, "inputsDigest": "sha256:" + "0" * 64}))
            layers.cmd_take(str(root / "base"), str(root / "built"), str(root / "fragment"), "mine",
                            str(root / "inputs.json"))
            fragment = json.loads((root / "fragment/fragment.json").read_text())
            self.assertEqual([l["diffID"] for l in fragment["layers"]], [layers.sha256_bytes(MINE)])
            first = layers.compose(str(root / "one"), [str(root / "base"), str(root / "fragment")], "0")
            again = layers.compose(str(root / "two"), [str(root / "base"), str(root / "fragment")], "0")
            self.assertEqual(first["digest"], again["digest"])
            manifest, config = layers.image_of_layout(str(root / "one/oci"))
            self.assertEqual(config["rootfs"]["diff_ids"], [layers.sha256_bytes(d) for d in (b"base", MINE)])
            self.assertEqual(manifest["layers"][1]["annotations"][layers.ACTION], "mine")
            layers.cmd_take(str(root / "base"), str(root / "built"), str(root / "again"), "mine",
                            str(root / "inputs.json"))
            self.assertEqual(json.loads((root / "again/fragment.json").read_text())["layers"], fragment["layers"])

    def test_take_compresses_the_uncompressed_layers_it_keeps(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.layout(root / "base", [b"base"])
            self.layout(root / "built", [b"base", MINE, MORE])
            (root / "inputs.json").write_text(json.dumps({"inputs": {}, "inputsDigest": ""}))
            for out in ("one", "two"):
                layers.cmd_take(str(root / "base"), str(root / "built"), str(root / out), "mine",
                                str(root / "inputs.json"))
            fragment = json.loads((root / "one/fragment.json").read_text())
            self.assertEqual([l["mediaType"] for l in fragment["layers"]], [layers.LAYER_GZIP] * 2)
            for layer, data in zip(fragment["layers"], (MINE, MORE)):
                blob = (root / "one" / "blobs" / "sha256" / layer["digest"][7:]).read_bytes()
                self.assertEqual(layers.sha256_bytes(blob), layer["digest"])
                self.assertEqual(len(blob), layer["size"])
                self.assertEqual(gzip.decompress(blob), data)
            self.assertEqual(json.loads((root / "two/fragment.json").read_text())["layers"], fragment["layers"])

    def test_an_export_given_placeholders_writes_only_the_layers_on_top(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.layout(root / "base", [b"base", b"empty"])
            # As the export leaves it: the parent's layers are the empty placeholders it found,
            # a layer repeating one of them (an instruction that changed nothing) is one too.
            self.layout(root / "built", [b"base", b"empty", MINE, b"empty"])
            layers.cmd_placeholders(str(root / "base"), str(root / "placeholders"))
            for digest in (layers.sha256_bytes(b"base"), layers.sha256_bytes(b"empty")):
                self.assertEqual(Path(layers.blob_path(str(root / "placeholders"), digest)).stat().st_size, 0)
                Path(layers.blob_path(str(root / "built"), digest)).write_bytes(b"")
            (root / "inputs.json").write_text(json.dumps({"inputs": {}, "inputsDigest": ""}))
            layers.cmd_take(str(root / "base"), str(root / "built"), str(root / "fragment"), "mine",
                            str(root / "inputs.json"))
            fragment = json.loads((root / "fragment/fragment.json").read_text())
            self.assertEqual([l["diffID"] for l in fragment["layers"]],
                             [layers.sha256_bytes(MINE), layers.sha256_bytes(b"empty")])
            base = layers.image_of_layout(str(root / "base"))[0]["layers"][1]
            self.assertEqual(fragment["layers"][1]["digest"], base["digest"])
            self.assertEqual(layers.read_blob(str(root / "fragment"), base["digest"]), b"empty")
            composed = layers.compose(str(root / "image"), [str(root / "base"), str(root / "fragment")], "0")
            self.assertTrue(composed["digest"].startswith("sha256:"))

    def test_a_view_links_and_a_composition_copies(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.layout(root / "base", [b"base"])
            first = layers.compose(str(root / "view"), [str(root / "base")], "0", symlink=True)
            again = layers.compose(str(root / "kept"), [str(root / "base")], "0")
            self.assertEqual(first["digest"], again["digest"])
            digest = layers.sha256_bytes(b"base")
            self.assertTrue(Path(layers.blob_path(str(root / "view/oci"), digest)).is_symlink())
            self.assertFalse(Path(layers.blob_path(str(root / "kept/oci"), digest)).is_symlink())
            self.assertEqual(Path(layers.blob_path(str(root / "kept/oci"), digest)).stat().st_nlink, 2)
            copied = layers.compose(str(root / "copied"), [str(root / "base")], "0", copy=True)
            self.assertEqual(copied["digest"], first["digest"])
            blob = Path(layers.blob_path(str(root / "copied/oci"), digest))
            self.assertFalse(blob.is_symlink())
            self.assertEqual(blob.stat().st_nlink, 1)
            self.assertEqual(blob.read_bytes(), b"base")

    def test_take_gives_each_app_tree_a_layer_of_its_own(self) -> None:
        trees = "usr/share/tilo-apps/trees"

        def store(b_app: bytes) -> bytes:
            return tar(("usr", None), ("usr/share/tilo-apps", None), ("usr/share/tilo-apps/registry.toml", b"rows"),
                       (trees, None), (f"{trees}/.wh.gone", b""),
                       (f"{trees}/a", None), (f"{trees}/a/app", b"a"), (f"{trees}/a/same", b"shared"),
                       (f"{trees}/b", None), (f"{trees}/b/app", b_app),
                       (f"{trees}/b/same", None, f"{trees}/a/same"), (f"{trees}/b/again", None, f"{trees}/a/same"))

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.layout(root / "base", [b"base"])
            (root / "inputs.json").write_text(json.dumps({"inputs": {}, "inputsDigest": ""}))
            taken = []
            for build, b_app in (("one", b"b"), ("two", b"b, changed")):
                self.layout(root / build, [b"base", store(b_app)])
                layers.cmd_take(str(root / "base"), str(root / build), str(root / f"{build}-fragment"), "image",
                                str(root / "inputs.json"))
                fragment = json.loads((root / f"{build}-fragment/fragment.json").read_text())
                self.assertEqual(fragment["config"]["rootfs"]["diff_ids"],
                                 [layers.sha256_bytes(b"base")] + [l["diffID"] for l in fragment["layers"]])
                self.assertFalse((root / f"{build}-fragment/.apart").exists())
                taken.append([gzip.decompress(layers.read_blob(str(root / f"{build}-fragment"), l["digest"]))
                              for l in fragment["layers"]])
            rest, a, b = taken[0]
            self.assertEqual(names(rest), [("usr", None), ("usr/share/tilo-apps", None),
                                           ("usr/share/tilo-apps/registry.toml", b"rows"), (trees, None),
                                           (f"{trees}/.wh.gone", b"")])
            self.assertEqual(names(a), [(f"{trees}/a", None), (f"{trees}/a/app", b"a"),
                                        (f"{trees}/a/same", b"shared")])
            # A link to another tree's file is that file here, and its other names link to it.
            self.assertEqual(names(b), [(f"{trees}/b", None), (f"{trees}/b/app", b"b"),
                                        (f"{trees}/b/same", b"shared"), (f"{trees}/b/again", f"{trees}/b/same")])
            # Another build that changed only b: the rest and a are the same layers.
            self.assertEqual(taken[1][:2], taken[0][:2])
            self.assertNotEqual(taken[1][2], b)
            composed = layers.compose(str(root / "image"), [str(root / "base"), str(root / "one-fragment")], "0")
            self.assertEqual(len(layers.image_of_layout(str(root / "image/oci"))[0]["layers"]), 4)
            self.assertTrue(composed["digest"].startswith("sha256:"))

    def test_take_refuses_an_image_not_built_on_its_parent(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.layout(root / "base", [b"base"])
            self.layout(root / "built", [b"other", MINE])
            (root / "inputs.json").write_text(json.dumps({"inputs": {}, "inputsDigest": ""}))
            result = subprocess.run([sys.executable, "-I", str(REPO / "tools/ci/os/oci_layers.py"), "take",
                                     str(root / "base"), str(root / "built"), str(root / "fragment"), "mine",
                                     str(root / "inputs.json")], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("is not built on", result.stderr)


if __name__ == "__main__":
    unittest.main()
