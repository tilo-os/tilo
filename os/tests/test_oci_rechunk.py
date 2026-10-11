#!/usr/bin/env python3
"""os/scripts/oci-rechunk.py: the chunked OS image is published only if it is the raw image's tree."""

from __future__ import annotations

import gzip
import hashlib
import importlib.util
import io
import json
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPTS = HERE.parent / "scripts"


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


rechunk = load("oci_rechunk", SCRIPTS / "oci-rechunk.py")
archive_ref = load("oci_archive_ref", SCRIPTS / "oci-archive-ref.py")
runner = load("rechunk_chunkah", SCRIPTS / "rechunk-chunkah.py")

NAME = "registry.tilo.so/tilo/tilo-ci:pr-1-abcdef0-x86_64"
CAP = b"\x01\x00\x00\x02\x00\x10\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00"


def entry(name: str, kind: str = "file", data: bytes = b"", mode: int = 0o644, mtime: int = 1000,
          uid: int = 0, link: str = "", xattrs: dict[str, bytes] | None = None):
    return (name, kind, data, mode, mtime, uid, link, xattrs or {})


def tar_bytes(entries) -> bytes:
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for name, kind, data, mode, mtime, uid, link, xattrs in entries:
            member = tarfile.TarInfo(name)
            member.mode, member.mtime, member.uid, member.gid = mode, mtime, uid, uid
            member.pax_headers = {f"SCHILY.xattr.{key}": value.decode("utf-8", "surrogateescape")
                                  for key, value in xattrs.items()}
            if kind == "dir":
                member.type = tarfile.DIRTYPE
                archive.addfile(member)
            elif kind == "symlink":
                member.type, member.linkname = tarfile.SYMTYPE, link
                archive.addfile(member)
            elif kind == "hardlink":
                member.type, member.linkname = tarfile.LNKTYPE, link
                archive.addfile(member)
            else:
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
    return output.getvalue()


def layout(root: Path, layers, *, labels: dict[str, str] | None = None, created: str = "2026-10-10T00:00:00Z",
           config_body: dict | None = None, history: bool = True) -> str:
    """An OCI layout of gzip layers, the way chunkah and BuildKit write one."""
    blobs = root / "blobs" / "sha256"
    blobs.mkdir(parents=True)
    (root / "oci-layout").write_text('{"imageLayoutVersion":"1.0.0"}')

    def put(data: bytes) -> str:
        digest = hashlib.sha256(data).hexdigest()
        (blobs / digest).write_bytes(data)
        return "sha256:" + digest

    descriptors, diff_ids = [], []
    for entries in layers:
        raw = tar_bytes(entries)
        compressed = gzip.compress(raw, mtime=0)
        descriptors.append({"mediaType": rechunk.LAYER_GZIP, "digest": put(compressed), "size": len(compressed)})
        diff_ids.append("sha256:" + hashlib.sha256(raw).hexdigest())
    config = {"created": created, "architecture": "amd64", "os": "linux",
              "config": config_body if config_body is not None else {"Labels": labels or {}},
              "rootfs": {"type": "layers", "diff_ids": diff_ids},
              "history": [{"created": created, "created_by": f"layer {i}"} for i in range(len(layers))] if history else []}
    config_bytes = json.dumps(config).encode()
    manifest = {"schemaVersion": 2, "mediaType": rechunk.MANIFEST,
                "config": {"mediaType": rechunk.CONFIG, "digest": put(config_bytes), "size": len(config_bytes)},
                "layers": descriptors}
    manifest_bytes = json.dumps(manifest).encode()
    digest = put(manifest_bytes)
    (root / "index.json").write_text(json.dumps({"schemaVersion": 2, "manifests": [
        {"mediaType": rechunk.MANIFEST, "digest": digest, "size": len(manifest_bytes)}]}))
    return digest


# The raw image: Containerfile-shaped layers, with a whiteout, an opaque
# directory, a hard link, an ostree repository under /sysroot and directory
# times that a later layer changes.
RAW = [
    [entry("usr", "dir", mtime=100), entry("usr/bin", "dir", mtime=100), entry("usr/lib", "dir", mtime=100),
     entry("usr/share", "dir", mtime=100), entry("usr/share/fonts", "dir", mtime=100),
     entry("usr/bin/tool", data=b"tool v1", mode=0o755, mtime=900, xattrs={"security.capability": CAP}),
     entry("usr/bin/tool-alias", "hardlink", link="usr/bin/tool"),
     entry("usr/lib/old.so", data=b"old", mtime=900), entry("usr/lib/libz.so", "symlink", link="libz.so.1"),
     entry("usr/lib/libz.so.1", data=b"zlib", mtime=800),
     entry("etc", "dir", mtime=100), entry("etc/conf.d", "dir", mtime=100),
     entry("etc/conf.d/stale", data=b"stale", mtime=900),
     entry("sysroot", "dir", mtime=100), entry("sysroot/ostree", "dir", mtime=100),
     entry("sysroot/ostree/object", data=b"x" * 64, mtime=1)],
    [entry("usr/lib/.wh.old.so", data=b""), entry("etc/conf.d", "dir", mtime=1500),
     entry("etc/conf.d/.wh..wh..opq", data=b""), entry("etc/conf.d/fresh", data=b"fresh", mtime=1500),
     entry("usr/share/fonts/sans.ttf", data=b"glyphs", mtime=1500, uid=0)],
    [entry("usr/share/fonts", "dir", mtime=2000, xattrs={"user.pinned": b"1"}),
     entry("usr/bin", "dir", mtime=2000)],
]


def chunked_layers(*, tool: bytes = b"tool v1", tool_mode: int = 0o755, tool_uid: int = 0,
                   tool_time: int = 700, cap: bool = True, fresh: bool = True, extra: bool = False,
                   link: bool = True, fonts_mode: int = 0o644):
    """What chunkah writes for RAW: two components, times capped, directories at their clamps."""
    tool_xattrs = {"security.capability": CAP} if cap else {}
    first = [entry("usr", "dir", mtime=50), entry("usr/bin", "dir", mtime=50),
             entry("usr/bin/tool", data=tool, mode=tool_mode, mtime=tool_time, uid=tool_uid, xattrs=tool_xattrs)]
    if link:
        first.append(entry("usr/bin/tool-alias", "hardlink", link="usr/bin/tool"))
    else:
        first.append(entry("usr/bin/tool-alias", data=tool, mode=tool_mode, mtime=tool_time, xattrs=tool_xattrs))
    first += [entry("usr/lib", "dir", mtime=50), entry("usr/lib/libz.so", "symlink", link="libz.so.1", mtime=50),
              entry("usr/lib/libz.so.1", data=b"zlib", mtime=800)]
    second = [entry("usr", "dir", mtime=60), entry("usr/share", "dir", mtime=60),
              entry("usr/share/fonts", "dir", mtime=60, mode=fonts_mode, xattrs={"user.pinned": b"1"}),
              entry("usr/share/fonts/sans.ttf", data=b"glyphs", mtime=1200),
              entry("etc", "dir", mtime=60), entry("etc/conf.d", "dir", mtime=60),
              entry("sysroot", "dir", mtime=60)]
    if fresh:
        second.append(entry("etc/conf.d/fresh", data=b"fresh", mtime=1200))
    if extra:
        second.append(entry("etc/conf.d/stale", data=b"stale", mtime=900))
    return [first, second]


class Rechunk(unittest.TestCase):
    def setUp(self) -> None:
        self.work = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.work)
        self.raw = self.work / "raw"
        layout(self.raw, RAW, labels={"containers.bootc": "1", "ostree.commit": "abc",
                                      "org.opencontainers.image.revision": "f" * 40},
               config_body={"Labels": {"containers.bootc": "1", "ostree.commit": "abc",
                                       "ostree.final-diffid": "sha256:00",
                                       "org.opencontainers.image.revision": "f" * 40},
                            "Env": ["PATH=/usr/bin"], "Cmd": ["/sbin/init"]},
               created="2026-10-10T08:00:00Z")

    def chunked(self, **changes) -> Path:
        root = self.work / "chunked"
        shutil.rmtree(root, ignore_errors=True)
        layout(root, chunked_layers(**changes), config_body={"Labels": {"org.example.chunkah": "1"}},
               created="2026-01-01T00:00:00Z")
        return root

    def finish(self, root: Path, archive: Path | None = None) -> tuple[str, dict]:
        report = self.work / "report.json"
        digest = rechunk.finish(f"oci:{self.raw}", root, NAME, archive, report)
        return digest, json.loads(report.read_text())

    def test_equal_tree_is_finished_with_the_raw_config_and_exact_directory_times(self) -> None:
        root = self.chunked()
        archive = self.work / "image.tar"
        digest, report = self.finish(root, archive)
        self.assertEqual(report["problems"], 0)
        self.assertEqual(report["pruned_paths"], 2)  # sysroot/ostree and its object
        self.assertEqual(report["pruned_bytes"], 64)
        self.assertGreater(report["clamped_times"], 0)
        self.assertEqual(report["chunked_layers"], 3)
        self.assertEqual(report["directory_times_restored"], 8)  # every directory chunkah wrote

        image = rechunk.image_size.layout_image(root, None)
        self.assertEqual(image.manifest_digest, digest)
        labels = image.config["config"]["Labels"]
        self.assertEqual(labels["containers.bootc"], "1")
        self.assertEqual(labels["org.opencontainers.image.revision"], "f" * 40)
        self.assertNotIn("ostree.commit", labels)
        self.assertNotIn("ostree.final-diffid", labels)
        self.assertNotIn("org.example.chunkah", labels)
        self.assertEqual(labels[rechunk.LAYERING_LABEL], "chunkah 3 layers")
        self.assertEqual(image.config["config"]["Cmd"], ["/sbin/init"])
        self.assertEqual(image.config["created"], "2026-10-10T08:00:00Z")
        self.assertEqual(len(image.config["rootfs"]["diff_ids"]), 3)
        self.assertEqual(image.config["history"][-1]["created_by"], rechunk.DIRECTORY_LAYER)

        # The finished root has the raw image's directory times, not chunkah's clamps.
        final = rechunk.replay(image)
        self.assertEqual(final.nodes["usr/share/fonts"].mtime, 2000)
        self.assertEqual(final.nodes["usr/share/fonts"].xattrs, (("user.pinned", b"1"),))
        self.assertEqual(final.nodes["etc/conf.d"].mtime, 1500)
        self.assertEqual(final.nodes["usr"].mtime, 100)
        self.assertNotIn("sysroot/ostree", final.nodes)
        self.assertIn("sysroot", final.nodes)
        # chunkah's own manifest and config are gone; every blob is referenced.
        referenced = {digest[7:], image.manifest["config"]["digest"][7:],
                      *(item["digest"][7:] for item in image.manifest["layers"])}
        self.assertEqual({blob.name for blob in (root / "blobs" / "sha256").iterdir()}, referenced)

        # buildx's naming, which the workflow's archive steps select by.
        self.assertEqual(archive_ref.select_ref(str(archive), NAME, digest), NAME.rsplit(":", 1)[1])

    def test_finishing_is_reproducible(self) -> None:
        first, _ = self.finish(self.chunked())
        second, _ = self.finish(self.chunked())
        self.assertEqual(first, second)

    def test_a_split_hard_link_is_allowed_and_counted(self) -> None:
        _, report = self.finish(self.chunked(link=False))
        self.assertEqual(report["split_hard_link_groups"], 1)

    def assert_refused(self, expected: str, **changes) -> None:
        root = self.chunked(**changes)
        archive = self.work / "refused.tar"
        with self.assertRaisesRegex(ValueError, "differs from the raw image"):
            rechunk.finish(f"oci:{self.raw}", root, NAME, archive, None)
        self.assertFalse(archive.exists(), "a refused image must not be written")
        result = rechunk.compare(rechunk.replay_source(f"oci:{self.raw}"), rechunk.replay_source(f"oci:{root}"))
        self.assertTrue(any(expected in problem for problem in result.problems), result.problems)

    def test_changed_bytes_are_refused(self) -> None:
        self.assert_refused("/usr/bin/tool: sha", tool=b"tool v2")

    def test_changed_owner_is_refused(self) -> None:
        self.assert_refused("/usr/bin/tool: uid", tool_uid=1000)

    def test_changed_mode_is_refused(self) -> None:
        self.assert_refused("/usr/bin/tool: mode", tool_mode=0o4755)

    def test_a_capability_chunkah_could_not_read_comes_from_the_raw_layers(self) -> None:
        # BuildKit's snapshot of the image lacks the base binaries' file
        # capabilities; the image's own layers carry them, and so must the
        # rechunked image's.
        root = self.chunked(cap=False)
        untouched = rechunk.image_size.layout_image(root, None).manifest["layers"][1]["digest"]
        _, report = self.finish(root)
        self.assertEqual(report["problems"], 0)
        self.assertEqual(report["xattrs_restored"], {
            "paths": 2, "entries": 2, "layers": 1, "first": ["/usr/bin/tool", "/usr/bin/tool-alias"]})
        image = rechunk.image_size.layout_image(root, None)
        final = rechunk.replay(image)
        self.assertEqual(final.nodes["usr/bin/tool"].xattrs, (("security.capability", CAP),))
        self.assertEqual(final.nodes["usr/bin/tool"].inode, final.nodes["usr/bin/tool-alias"].inode)
        # Only the layer that holds them was written again.
        self.assertEqual(image.manifest["layers"][1]["digest"], untouched)
        self.assertEqual(len(list((root / "blobs" / "sha256").iterdir())), 2 + len(image.manifest["layers"]))

    def test_a_raised_time_is_refused(self) -> None:
        self.assert_refused("/usr/bin/tool: time 900 rose to 950", tool_time=950)

    def test_a_missing_file_is_refused(self) -> None:
        self.assert_refused("missing: /etc/conf.d/fresh", fresh=False)

    def test_a_directory_the_time_layer_would_mask_is_refused(self) -> None:
        # The directory layer rewrites the mode too; the proof must not rely on it.
        root = self.chunked(fonts_mode=0o700)
        drift, _ = rechunk.directory_drift(rechunk.replay_source(f"oci:{self.raw}"),
                                           rechunk.replay_source(f"oci:{root}"))
        self.assertEqual(drift, ["/usr/share/fonts: chunkah wrote the directory's mode 0o700, the raw image has 0o644"])
        with self.assertRaisesRegex(ValueError, "differs from the raw image in 1 ways"):
            rechunk.finish(f"oci:{self.raw}", root, NAME, None, None)

    def test_a_whited_out_file_coming_back_is_refused(self) -> None:
        self.assert_refused("not in the raw image: /etc/conf.d/stale", extra=True)

    def test_architecture_must_match(self) -> None:
        raw = {"architecture": "arm64", "os": "linux"}
        with self.assertRaisesRegex(ValueError, "linux/amd64, the raw image is linux/arm64"):
            rechunk.adopt_config({"architecture": "amd64", "os": "linux", "rootfs": {"diff_ids": []}},
                                 raw, "sha256:" + "0" * 64, 1)

    def test_directory_layer_skips_pruned_and_implicit_directories(self) -> None:
        tree = rechunk.Tree()
        with tarfile.open(fileobj=io.BytesIO(tar_bytes([
                entry("opt/app/bin/run", data=b"run"), entry("sysroot", "dir"), entry("sysroot/ostree", "dir"),
                entry("var", "dir", mtime=5)])), mode="r") as archive:
            tree.apply(0, archive)
        with tarfile.open(fileobj=io.BytesIO(rechunk.directory_layer(tree)), mode="r") as archive:
            names = [member.name for member in archive]
        self.assertEqual(names, ["sysroot", "var"])

    def test_a_tree_chunked_on_its_own_joins_the_main_layout(self) -> None:
        # The main run pruned the fonts; their own run kept only them and the
        # directories above them.
        layers = chunked_layers()
        fonts = [item for item in layers[1] if item[0] == "usr/share/fonts/sans.ttf"]
        layers[1] = [item for item in layers[1] if item[0] != "usr/share/fonts/sans.ttf"]
        root = self.work / "chunked"
        layout(root, layers, config_body={"Labels": {}})
        tree = self.work / "tree"
        layout(tree, [[entry("usr", "dir", mtime=70), entry("usr/share", "dir", mtime=70),
                       entry("usr/share/fonts", "dir", mtime=70, xattrs={"user.pinned": b"1"}), *fonts]],
               config_body={"Labels": {}})
        self.assertEqual(rechunk.merge_layouts(root, [tree]), 1)
        _, report = self.finish(root)
        self.assertEqual(report["problems"], 0)
        self.assertEqual(report["chunked_layers"], 4)
        image = rechunk.image_size.layout_image(root, None)
        self.assertEqual(len(image.config["history"]), 4)
        self.assertEqual(rechunk.replay(image).nodes["usr/share/fonts/sans.ttf"].sha,
                         hashlib.sha256(b"glyphs").hexdigest())

    def test_a_tree_run_prunes_everything_beside_its_ancestors(self) -> None:
        rootfs = self.work / "rootfs"
        for path in ("usr/lib/tilo/windows-runners/proton", "usr/lib/tilo/fonts", "usr/lib/sysimage/rpm",
                     "usr/bin", "etc", "sysroot/ostree"):
            (rootfs / path).mkdir(parents=True)
        (rootfs / "usr/lib/libz.so.1").write_text("z")
        self.assertEqual(runner.outside(rootfs, ["/usr/lib/tilo/windows-runners"]), [
            "/etc", "/sysroot", "/usr/bin", "/usr/lib/libz.so.1", "/usr/lib/sysimage", "/usr/lib/tilo/fonts"])

    def test_a_file_set_run_keeps_only_its_files_and_their_parents(self) -> None:
        rootfs = self.work / "rootfs"
        for path in ("usr/lib/tilo-runtime/2026.07+native.ab/lib", "usr/lib/tilo-runtime/2026.10.9-chromium",
                     "usr/share/tilo", "etc/tilo", "usr/bin"):
            (rootfs / path).mkdir(parents=True)
        for path in ("usr/lib/tilo-runtime/2026.07+native.ab/bundle.toml",
                     "usr/lib/tilo-runtime/2026.07+native.ab/lib/libruntime_dylib.so",
                     "usr/lib/tilo-runtime/2026.10.9-chromium/bundle.toml",
                     "usr/share/tilo/runtime-bundle-publisher.hex", "usr/share/tilo/catalog.toml",
                     "etc/tilo/runtime-bundle-trusted-publishers.txt", "etc/tilo/other.conf", "etc/hostname"):
            (rootfs / path).write_text(path)
        # A link is no file of the set: chunkah would follow nothing, and the
        # main run keeps it.
        (rootfs / "usr/share/tilo/app-publisher.hex").symlink_to("runtime-bundle-publisher.hex")
        files = runner.matches(rootfs, runner.FILE_SETS["build-signatures"])
        self.assertEqual(files, [
            "/etc/tilo/runtime-bundle-trusted-publishers.txt",
            "/usr/lib/tilo-runtime/2026.07+native.ab/bundle.toml",
            "/usr/lib/tilo-runtime/2026.10.9-chromium/bundle.toml",
            "/usr/share/tilo/runtime-bundle-publisher.hex",
        ])
        self.assertEqual(runner.outside(rootfs, files), [
            "/etc/hostname", "/etc/tilo/other.conf", "/usr/bin",
            "/usr/lib/tilo-runtime/2026.07+native.ab/lib",
            "/usr/share/tilo/app-publisher.hex", "/usr/share/tilo/catalog.toml"])

    def test_the_runner_gives_present_trees_their_own_run(self) -> None:
        rootfs = self.work / "rootfs"
        (rootfs / "usr/lib/tilo/windows-runners").mkdir(parents=True)
        (rootfs / "etc").mkdir()
        calls = self.work / "calls"
        fake = self.work / "bin"
        fake.mkdir()
        (fake / "chunkah").write_text(
            "#!/bin/sh\n"
            f"printf '%s\\n' \"$*\" >> {calls}\n"
            "while [ $# -gt 0 ]; do\n"
            "  case $1 in --output) mkdir -p \"${2#oci:}\";; --write-manifest-to) echo {} > \"$2\";; esac\n"
            "  shift\n"
            "done\n")
        (fake / "chunkah").chmod(0o755)
        environment = {"PATH": f"{fake}:/usr/bin:/bin", "SOURCE_DATE_EPOCH": "1700000000"}
        out = self.work / "out"
        (rootfs / "etc/tilo").mkdir()
        (rootfs / "etc/tilo/runtime-bundle-trusted-publishers.txt").write_text("ab\n")
        subprocess.run([sys.executable, str(SCRIPTS / "rechunk-chunkah.py"), str(rootfs), str(out), "128"],
                       check=True, env=environment)
        main, tree, signatures = calls.read_text().splitlines()
        self.assertIn("--max-layers 128", main)
        self.assertIn("--prune /sysroot/ --prune /usr/lib/tilo/windows-runners/"
                      " --prune /etc/tilo/runtime-bundle-trusted-publishers.txt", main)
        self.assertIn("--max-layers 16", tree)
        self.assertIn("--prune /etc", tree)
        self.assertIn("--max-layers 1 ", signatures)
        self.assertIn("--prune /usr", signatures)
        self.assertNotIn("--prune /etc/tilo", signatures)
        self.assertTrue((out / "image").is_dir() and (out / "components.json").is_file())
        self.assertTrue((out / "trees" / "windows-runners" / "image").is_dir())
        self.assertTrue((out / "trees" / "build-signatures" / "image").is_dir())
        # No epoch, no run: two builds must share it to share layers.
        failed = subprocess.run([sys.executable, str(SCRIPTS / "rechunk-chunkah.py"), str(rootfs),
                                 str(self.work / "out2"), "128"], env={"PATH": environment["PATH"]},
                                capture_output=True, text=True)
        self.assertNotEqual(failed.returncode, 0)
        self.assertIn("SOURCE_DATE_EPOCH", failed.stderr)

    def test_the_app_store_index_leaves_the_app_store_tree_for_a_layer_of_its_own(self) -> None:
        rootfs = self.work / "rootfs"
        store = rootfs / "usr/share/tilo-apps"
        for path in ("pkgs/ab/bin", "pkgs/ab.pkg", "cohort", "../../bin"):
            (store / path).mkdir(parents=True)
        for path in ("registry.toml", "pkgs/ab/bin/calculator", "pkgs/ab.pkg/manifest.toml",
                     "pkgs/ab.pkg/signed-root", "cohort/so.tilo.calculator.tapp"):
            (store / path).write_text(path)
        calls = self.work / "calls"
        fake = self.work / "bin"
        fake.mkdir()
        (fake / "chunkah").write_text(
            "#!/bin/sh\n"
            f"printf '%s\\n' \"$*\" >> {calls}\n"
            "while [ $# -gt 0 ]; do\n"
            "  case $1 in --output) mkdir -p \"${2#oci:}\";; --write-manifest-to) echo {} > \"$2\";; esac\n"
            "  shift\n"
            "done\n")
        (fake / "chunkah").chmod(0o755)
        environment = {"PATH": f"{fake}:/usr/bin:/bin", "SOURCE_DATE_EPOCH": "1700000000"}
        subprocess.run([sys.executable, str(SCRIPTS / "rechunk-chunkah.py"), str(rootfs), str(self.work / "out"),
                        "128"], check=True, env=environment)
        main, store_run, index = calls.read_text().splitlines()
        # The main run leaves the whole store to the tree's run and the set's.
        self.assertIn("--prune /usr/share/tilo-apps/ ", main + " ")
        self.assertNotIn("registry.toml", main)
        self.assertIn("--max-layers 32", store_run)
        self.assertIn("--prune /usr/bin", store_run)
        for path in ("registry.toml", "pkgs/ab.pkg/manifest.toml", "pkgs/ab.pkg/signed-root"):
            self.assertIn(f"--prune /usr/share/tilo-apps/{path}", store_run)
        self.assertNotIn("calculator", store_run)
        self.assertIn("--max-layers 1 ", index)
        self.assertIn("--prune /usr/share/tilo-apps/cohort", index)
        self.assertIn("--prune /usr/share/tilo-apps/pkgs/ab ", index + " ")
        self.assertNotIn("registry.toml", index)
        self.assertNotIn("ab.pkg", index)

    def test_opaque_root_and_replaced_directory(self) -> None:
        tree = rechunk.Tree()
        for index, entries in enumerate([
                [entry("a", "dir"), entry("a/b", "dir"), entry("a/b/c", data=b"c"), entry("d", data=b"d")],
                [entry("a/b", data=b"now a file")]]):
            with tarfile.open(fileobj=io.BytesIO(tar_bytes(entries)), mode="r") as archive:
                tree.apply(index, archive)
        self.assertEqual(tree.nodes["a/b"].kind, "file")
        self.assertNotIn("a/b/c", tree.nodes)
        self.assertIn("d", tree.nodes)


if __name__ == "__main__":
    unittest.main()
