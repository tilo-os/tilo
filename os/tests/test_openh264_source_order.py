#!/usr/bin/env python3
"""Compile the actual OpenH264 source enumerator and test native archive order.

This needs Cargo and a native C compiler, so run it in native CI, not lightweight
Python policy discovery. Only tiny fixture sources are compiled, never OpenH264.
"""
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]


class OpenH264SourceOrderTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        for tool in ("cargo", "cc", "ar"):
            if shutil.which(tool) is None:
                raise RuntimeError(f"{tool} is required for the native source-order test")
        cls.temporary = tempfile.TemporaryDirectory(prefix="openh264-order-")
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.root = Path(cls.temporary.name)
        cls.env = dict(os.environ)
        for name in ("CARGO_TARGET_DIR", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS",
                     "CARGO_BUILD_TARGET", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"):
            cls.env.pop(name, None)
        source = (ROOT / "vendor/openh264-sys2/build.rs").read_text()
        start = source.index("    fn glob_import<")
        opening = source.index("{", start)
        depth = 1
        end = opening + 1
        while depth:
            depth += (source[end] == "{") - (source[end] == "}")
            end += 1
        # Compile the production function verbatim, not a Python equivalent or
        # a copied implementation. The codec build entrypoint is never called.
        (cls.root / "src").mkdir()
        (cls.root / "src/main.rs").write_text(
            "use std::path::Path;\nuse walkdir::WalkDir;\n" + source[start:end] + "\n"
            'fn main() { let a: Vec<String> = std::env::args().collect(); '
            'for path in glob_import(&a[1], &a[2], &a[3]) { println!("{}", path); } }\n')
        lock_text = (ROOT / "Cargo.lock").read_text()
        blocks = re.findall(r"(?ms)^\[\[package\]\]\n.*?(?=^\[\[package\]\]|\Z)", lock_text)
        records = [(tomllib.loads(block)["package"][0], block) for block in blocks]
        walkdir = [record for record, _ in records if record["name"] == "walkdir"]
        if len(walkdir) != 1:
            raise RuntimeError("expected one locked walkdir dependency")
        (cls.root / "Cargo.toml").write_text(
            '[package]\nname="openh264-order-fixture"\nversion="0.0.0"\nedition="2024"\n'
            f'[dependencies]\nwalkdir="={walkdir[0]["version"]}"\n')
        # Reuse the exact existing dependency closure/checksums without resolving
        # new versions or touching the workspace lockfile.
        selected = {}
        pending = [f'walkdir {walkdir[0]["version"]}']
        while pending:
            parts = pending.pop().split()
            matches = [(r, b) for r, b in records if r["name"] == parts[0]
                       and (len(parts) == 1 or r["version"] == parts[1])]
            if len(matches) != 1:
                raise RuntimeError(f"ambiguous locked fixture dependency: {parts}")
            record, block = matches[0]
            key = (record["name"], record["version"])
            if key not in selected:
                selected[key] = block
                pending.extend(record.get("dependencies", []))
        (cls.root / "Cargo.lock").write_text(
            'version = 4\n\n[[package]]\nname = "openh264-order-fixture"\n'
            'version = "0.0.0"\ndependencies = ["walkdir"]\n\n'
            + "\n".join(selected[key] for key in sorted(selected)))
        shutil.copyfile(ROOT / "rust-toolchain.toml", cls.root / "rust-toolchain.toml")
        # Cache restoration is best-effort. A cold runner may fetch these exact
        # locked dependencies; a cache miss must not make verification fail.
        cls.run_command(["cargo", "build", "--locked", "--quiet"])
        cls.driver = cls.root / "target/debug/openh264-order-fixture"

    @classmethod
    def run_command(cls, args, *, cwd=None):
        result = subprocess.run(args, cwd=cwd or cls.root, env=cls.env,
                                text=True, capture_output=True, timeout=60)
        if result.returncode:
            raise AssertionError(result.stdout + result.stderr)
        return result.stdout

    def trees(self, prefix, files):
        roots = []
        for side, names in (("forward", list(files)), ("reverse", list(reversed(files)))):
            root = self.root / f"{prefix}-{side}"
            root.mkdir()
            for name in names:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(files[name])
            roots.append(root)
        return roots

    def enumerate(self, root, extension, exclude):
        output = self.run_command([str(self.driver), str(root), extension, exclude])
        return [Path(path).relative_to(root).as_posix() for path in output.splitlines()]

    def test_creation_order_does_not_change_cpp_or_assembly_inputs(self):
        files = dict.fromkeys(("z/z.cpp", "b.cpp", "a/a.cpp", "a/z.cpp", "a/DllEntry.cpp",
                              "ignored.hpp", "z.asm", "a.asm", "arm64/z.S", "arm64/a.S",
                              "arm64/arm_arch_common_macro.S"), "fixture\n")
        roots = self.trees("filters", files)
        for extension, exclude, expected in (
            (".cpp", "DllEntry.cpp", ["a/a.cpp", "a/z.cpp", "b.cpp", "z/z.cpp"]),
            (".asm", "NEVER_MATCH", ["a.asm", "z.asm"]),
            (".S", "arm_arch_common_macro.S", ["arm64/a.S", "arm64/z.S"]),
        ):
            for root in roots:
                with self.subTest(extension=extension, root=root.name):
                    self.assertEqual(self.enumerate(root, extension, exclude), expected)

    def test_order_preserves_the_real_common_cpp_inventory(self):
        source = ROOT / "vendor/openh264-sys2/upstream/codec/common"
        expected = sorted(path.relative_to(source).as_posix() for path in source.rglob("*.cpp")
                          if "DllEntry.cpp" not in str(path))
        self.assertTrue(expected)
        roots = self.trees("real-names", dict.fromkeys(expected, "fixture\n"))
        for root in (source, *roots):
            self.assertEqual(self.enumerate(root, ".cpp", "DllEntry.cpp"), expected)

    def test_native_archive_bytes_are_stable_across_creation_orders(self):
        roots = self.trees("archive", {
            "z.cpp": 'extern "C" int source_z(void) { return 7; }\n',
            "a.cpp": 'extern "C" int source_a(void) { return 3; }\n',
        })
        hashes = []
        for root in roots:
            objects = []
            for source in self.enumerate(root, ".cpp", "DllEntry.cpp"):
                target = str(Path(source).with_suffix(".o"))
                self.run_command(["cc", "-c", source, "-o", target], cwd=root)
                objects.append(target)
            self.run_command(["ar", "rcsD", "libfixture.a", *objects], cwd=root)
            self.assertEqual(self.run_command(["ar", "t", "libfixture.a"], cwd=root).splitlines(),
                             ["a.o", "z.o"])
            hashes.append(hashlib.sha256((root / "libfixture.a").read_bytes()).hexdigest())
        self.assertEqual(hashes[0], hashes[1])


if __name__ == "__main__":
    unittest.main()
