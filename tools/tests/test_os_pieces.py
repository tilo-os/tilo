#!/usr/bin/env python3
"""os/Containerfile is os/image/'s pieces, and each layer action builds what it says."""

from __future__ import annotations

import gzip
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
os_toml = load("os_toml_under_test", REPO / "tools/ci/os/os_toml.py")


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

    def test_take_compresses_the_uncompressed_layers_it_keeps(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.layout(root / "base", [b"base"])
            self.layout(root / "built", [b"base", b"mine", b"more"])
            (root / "inputs.json").write_text(json.dumps({"inputs": {}, "inputsDigest": ""}))
            for out in ("one", "two"):
                layers.cmd_take(str(root / "base"), str(root / "built"), str(root / out), "mine",
                                str(root / "inputs.json"))
            fragment = json.loads((root / "one/fragment.json").read_text())
            self.assertEqual([l["mediaType"] for l in fragment["layers"]], [layers.LAYER_GZIP] * 2)
            for layer, data in zip(fragment["layers"], (b"mine", b"more")):
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
            self.layout(root / "built", [b"base", b"empty", b"mine", b"empty"])
            layers.cmd_placeholders(str(root / "base"), str(root / "placeholders"))
            for digest in (layers.sha256_bytes(b"base"), layers.sha256_bytes(b"empty")):
                self.assertEqual(Path(layers.blob_path(str(root / "placeholders"), digest)).stat().st_size, 0)
                Path(layers.blob_path(str(root / "built"), digest)).write_bytes(b"")
            (root / "inputs.json").write_text(json.dumps({"inputs": {}, "inputsDigest": ""}))
            layers.cmd_take(str(root / "base"), str(root / "built"), str(root / "fragment"), "mine",
                            str(root / "inputs.json"))
            fragment = json.loads((root / "fragment/fragment.json").read_text())
            self.assertEqual([l["diffID"] for l in fragment["layers"]],
                             [layers.sha256_bytes(b"mine"), layers.sha256_bytes(b"empty")])
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


class ScriptKeyTests(unittest.TestCase):
    """A layer is keyed by the scripts its tools run, not by those their comments mention."""

    def scripts(self, files: dict[str, str], seed: str) -> list[str]:
        with tempfile.TemporaryDirectory() as temporary:
            for name, text in files.items():
                (Path(temporary) / name).parent.mkdir(parents=True, exist_ok=True)
                (Path(temporary) / name).write_text(text)
            repo, os_toml.REPO = os_toml.REPO, temporary
            try:
                return os_toml.scripts_of([seed])
            finally:
                os_toml.REPO = repo

    def test_code_names_a_script_and_a_comment_does_not(self) -> None:
        files = {name: "" for name in ("os/dev/run.sh", "os/dev/sourced.sh", "os/dev/arg.sh", "os/dev/vm.sh",
                                       "os/tests/gate.sh", "os/dev/helper.py", "os/dev/doc.sh")}
        files["os/dev/build.sh"] = (
            "# boots with os/dev/vm.sh\n"
            'source "$(dirname "${BASH_SOURCE[0]}")/sourced.sh"\n'
            'bash "$TILO_OS_DIR/dev/run.sh" "#x"  # then os/tests/gate.sh\n'
            'check "os/dev/arg.sh" ${#items[@]}\n'
            "python3 os/dev/tool.py\n")
        files["os/dev/tool.py"] = ('"""Like os/dev/doc.sh."""\n'
                                   'HELPER = Path(__file__).with_name("helper.py")  # not os/dev/doc.sh\n')
        self.assertEqual(self.scripts(files, "os/dev/build.sh"),
                         ["os/dev/arg.sh", "os/dev/helper.py", "os/dev/run.sh", "os/dev/sourced.sh", "os/dev/tool.py"])

    def test_no_image_layer_is_keyed_by_the_vm_tools(self) -> None:
        keyed = set(os_toml.scripts_of(os_toml.TOOLS))
        self.assertIn("os/dev/_common.sh", keyed)
        self.assertFalse(keyed & {"os/dev/vm.sh", "os/tests/boot_performance_gate.sh"})


if __name__ == "__main__":
    unittest.main()
