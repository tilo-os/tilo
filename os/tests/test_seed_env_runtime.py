#!/usr/bin/env python3
"""os/scripts/seed-env-runtime.py: the env-runtime ships without the host's files twice."""
import importlib.util
from pathlib import Path
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("seed_env_runtime", ROOT / "os/scripts/seed-env-runtime.py")
seed = importlib.util.module_from_spec(spec)
spec.loader.exec_module(seed)

RUNTIME = "ab" * 32
APP = "cd" * 32


def write(path: Path, data: bytes, mode: int = 0o444) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    path.chmod(mode)


class SeedEnvRuntimeTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        base = Path(temporary.name)
        self.host, self.store = base / "host", base / "store"
        packages = self.store / "pkgs"
        for package, kind in ((RUNTIME, "env-runtime"), (APP, "app")):
            write(packages / f"{package}.pkg/manifest.toml", f'[package]\nkind = "{kind}"\n'.encode(), 0o644)
        tree = packages / RUNTIME
        write(tree / "usr/lib64/libc.so.6", b"libc" * 1000, 0o555)
        write(self.host / "usr/lib64/libc.so.6", b"libc" * 1000, 0o755)
        write(tree / "usr/lib/locale/C.utf8/LC_CTYPE", b"ctype" * 100)
        write(self.host / "usr/lib/locale/C.utf8/LC_CTYPE", b"ctype" * 100)
        write(tree / "usr/bin/runtime-only", b"not on the host")
        write(tree / "usr/lib64/libcurl.so.4", b"runtime build")
        write(self.host / "usr/lib64/libcurl.so.4", b"host's build!")
        write(tree / "etc/os-release", b"NAME=Fedora\n")
        write(self.host / "etc/os-release", b"NAME=Fedora\n")
        # Identical, but the host keeps it from users: a session could not import it.
        write(tree / "usr/libexec/private-helper", b"helper")
        write(self.host / "usr/libexec/private-helper", b"helper", 0o700)
        write(packages / APP / "usr/lib64/libc.so.6", b"libc" * 1000)
        self.whole = base / "whole"
        shutil.copytree(self.store, self.whole, symlinks=True)

    def listing(self) -> list[list[str]]:
        text = (self.store / f"pkgs/{RUNTIME}.pkg/host-sources").read_text()
        return [line.split("\t") for line in text.splitlines()]

    def test_prune_leaves_out_exactly_the_files_the_host_has_byte_for_byte(self):
        saved = seed.prune(self.store, self.host)
        tree = self.store / "pkgs" / RUNTIME
        self.assertEqual(saved, 4000 + 500)
        self.assertEqual(
            [(relative, host) for relative, host, _ in self.listing()],
            [("usr/lib/locale/C.utf8/LC_CTYPE", "/usr/lib/locale/C.utf8/LC_CTYPE"),
             ("usr/lib64/libc.so.6", "/usr/lib64/libc.so.6")])
        self.assertEqual(self.listing()[1][2], seed.sha256(self.host / "usr/lib64/libc.so.6"))
        self.assertFalse((tree / "usr/lib64/libc.so.6").exists())
        # Different bytes, no host twin, and /etc (mutable on a device) stay.
        for kept in ("usr/lib64/libcurl.so.4", "usr/bin/runtime-only", "etc/os-release",
                     "usr/libexec/private-helper"):
            self.assertTrue((tree / kept).is_file(), kept)
        # Only env-runtimes are seeded; an app keeps its whole payload.
        self.assertTrue((self.store / "pkgs" / APP / "usr/lib64/libc.so.6").is_file())
        self.assertFalse((self.store / f"pkgs/{APP}.pkg/host-sources").exists())

    def test_heal_puts_back_what_the_host_no_longer_carries(self):
        seed.prune(self.store, self.host)
        (self.host / "usr/lib64/libc.so.6").chmod(0o644)
        (self.host / "usr/lib64/libc.so.6").write_bytes(b"updated libc")
        restored = seed.heal(self.store, self.whole, self.host)
        tree = self.store / "pkgs" / RUNTIME
        self.assertEqual(restored, 4000)
        self.assertEqual((tree / "usr/lib64/libc.so.6").read_bytes(), b"libc" * 1000)
        self.assertEqual((tree / "usr/lib64/libc.so.6").stat().st_mode & 0o777, 0o555)
        self.assertEqual([entry[0] for entry in self.listing()], ["usr/lib/locale/C.utf8/LC_CTYPE"])
        # Unchanged since: nothing more to put back.
        self.assertEqual(seed.heal(self.store, self.whole, self.host), 0)

    def test_heal_refuses_a_seed_that_no_longer_matches_its_host(self):
        seed.prune(self.store, self.host)
        (self.host / "usr/lib64/libc.so.6").unlink()
        limit = seed.HEAL_LIMIT
        seed.HEAL_LIMIT = 100
        self.addCleanup(setattr, seed, "HEAL_LIMIT", limit)
        with self.assertRaisesRegex(SystemExit, "no longer match the host"):
            seed.heal(self.store, self.whole, self.host)

    def test_heal_fails_when_a_file_is_nowhere(self):
        seed.prune(self.store, self.host)
        (self.host / "usr/lib64/libc.so.6").unlink()
        (self.whole / "pkgs" / RUNTIME / "usr/lib64/libc.so.6").chmod(0o644)
        (self.whole / "pkgs" / RUNTIME / "usr/lib64/libc.so.6").unlink()
        with self.assertRaisesRegex(SystemExit, "neither on the host nor in"):
            seed.heal(self.store, self.whole, self.host)


if __name__ == "__main__":
    unittest.main()
