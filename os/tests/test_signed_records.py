#!/usr/bin/env python3
"""os/scripts/signed-records.py: a build's own signatures stay out of the layers of what they sign."""
import importlib.util
from pathlib import Path
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("signed_records", ROOT / "os/scripts/signed-records.py")
records = importlib.util.module_from_spec(spec)
spec.loader.exec_module(records)

RUNTIME = "ab" * 32


def write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def signed_root(payload: bytes, key: bytes) -> bytes:
    """A signed root's shape: magic, format, manifest hash, payload hash, flag, key, signature."""
    return b"TLOPKG\0\0" + b"\2\0\0\0" + b"\1" * 32 + payload + b"\1" + key + b"\2" * 64


class SignedRecordsTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)

    def install(self, store: Path, key: bytes) -> None:
        """Two apps signed with KEY, as tilo-install leaves them, and the env-runtime image."""
        registry = ""
        for app, payload in (("notes", b"\3" * 32), ("files", b"\4" * 32)):
            package = bytes(a ^ b for a, b in zip(payload, key)).hex()
            write(store / f"pkgs/{package}/bin/{app}", app.encode() * 100)
            meta = store / f"pkgs/{package}.pkg"
            write(meta / "signed-root", signed_root(payload, key))
            write(meta / "manifest.toml", f'publisher = "{key.hex()}"\n'.encode())
            write(meta / "collection.qcl", app.encode())
            registry += (f'[[entry]]\nid = "so.tilo.{app}"\npackage_id_hex = "{package}"\n'
                         f'payload_root = "{store}/pkgs/{package}"\n'
                         f'manifest_path = "{meta}/manifest.toml"\npinned = false\n\n')
        write(store / "registry.toml", registry.encode())
        write(store / "bin/notes", key)
        write(store / f"pkgs/{RUNTIME}/usr/bin/bash", b"bash")
        write(store / f"pkgs/{RUNTIME}.pkg/signed-root", b"published")
        write(store / "image-publishers", b"cd" * 32 + b"\n")

    def tree(self, root: Path) -> dict[str, bytes]:
        return {p.relative_to(root).as_posix(): p.read_bytes() for p in sorted(root.rglob("*")) if p.is_file()}

    def test_trees_are_named_by_their_payload_whatever_the_key(self):
        contents = []
        for build, key in (("one", b"\5" * 32), ("two", b"\6" * 32)):
            store = self.base / build / "store"
            self.install(store, key)
            records.name_trees(store)
            rows = tomllib.loads((store / "registry.toml").read_text())["entry"]
            self.assertEqual([Path(r["payload_root"]).relative_to(store).as_posix() for r in rows],
                             ["trees/" + "03" * 32, "trees/" + "04" * 32])
            self.assertEqual((Path(rows[0]["payload_root"]) / "bin/notes").read_bytes(), b"notes" * 100)
            self.assertEqual(sorted(p.name for p in (store / "pkgs").iterdir()),
                             sorted([RUNTIME, f"{RUNTIME}.pkg"] + [Path(r["manifest_path"]).parent.name for r in rows]))
            signatures = self.base / build / "signatures"
            records.store(store, signatures)
            contents.append(self.tree(store))
            self.assertEqual(sorted(self.tree(signatures)),
                             sorted(["registry.toml", "bin/notes"]
                                    + [f"pkgs/{Path(r['manifest_path']).parent.name}/{name}" for r in rows
                                       for name in ("signed-root", "manifest.toml", "collection.qcl")]))
        self.assertEqual(contents[0], contents[1], "what stays in the store differs with the key")
        self.assertIn("trees/" + "03" * 32 + "/bin/notes", contents[0])
        self.assertIn(f"pkgs/{RUNTIME}/usr/bin/bash", contents[0])

    def test_a_row_outside_the_store_is_refused(self):
        store = self.base / "store"
        self.install(store, b"\5" * 32)
        registry = store / "registry.toml"
        registry.write_text(registry.read_text().replace(f'"{store}/pkgs/', '"/elsewhere/pkgs/', 1))
        with self.assertRaises(SystemExit):
            records.name_trees(store)

    def test_runtime_manifests_and_publishers_leave_the_image(self):
        image, out = self.base / "image", self.base / "signatures"
        for runtime in ("2026.07+native.0123", "146.0"):
            write(image / f"usr/lib/tilo-runtime/{runtime}/bundle.toml", b"signed")
            write(image / f"usr/lib/tilo-runtime/{runtime}/lib/libc.so.6", b"libc")
        for name in records.RUNTIME_PUBLISHERS:
            write(image / "usr/share/tilo" / name, b"key\n")
        write(image / "usr/share/tilo/language-packs/de/strings", b"Hallo")
        records.runtimes(image, out)
        self.assertEqual(sorted(self.tree(out)), sorted([
            "usr/lib/tilo-runtime/146.0/bundle.toml",
            "usr/lib/tilo-runtime/2026.07+native.0123/bundle.toml",
            "usr/share/tilo/browser-runtime-publisher.hex",
            "usr/share/tilo/runtime-bundle-publisher.hex"]))
        self.assertEqual(sorted(self.tree(image)), sorted([
            "usr/lib/tilo-runtime/146.0/lib/libc.so.6",
            "usr/lib/tilo-runtime/2026.07+native.0123/lib/libc.so.6",
            "usr/share/tilo/language-packs/de/strings"]))


if __name__ == "__main__":
    unittest.main()
