#!/usr/bin/env python3
"""Pinned-source extraction and fail-closed proxy adaptation."""
import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("provider_preparation", ROOT / "os/scripts/prepare-signal-provider.py")
preparation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preparation)


class PreparationTests(unittest.TestCase):
    def archive(self, root, code, duplicate=False):
        source = root / "source.tar.gz"
        member = "signal-cli-fixture/lib/src/main/java/org/asamk/signal/manager/config/LiveConfig.java"
        with tarfile.open(source, "w:gz") as archive:
            for _ in range(2 if duplicate else 1):
                entry = tarfile.TarInfo(member)
                entry.size = len(code)
                archive.addfile(entry, io.BytesIO(code))
        pin = {"signal_source_commit": "fixture", "assets": {"source": {"sha256": hashlib.sha256(source.read_bytes()).hexdigest()}}}
        return source, pin

    def test_only_the_pinned_proxy_declaration_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old = b"private static final Optional<HttpProxy> systemProxy = Optional.empty();"
            source, pin = self.archive(root, b"prefix\n" + old + b"\nsuffix\n")
            output = root / "prepared"
            preparation.prepare(source, output, pin)
            code = (output / "org/asamk/signal/manager/config/LiveConfig.java").read_text()
            self.assertEqual(code, "prefix\nprivate static final Optional<HttpProxy> systemProxy = TiloHttpProxy.nativeGate();\nsuffix\n")
            with self.assertRaisesRegex(ValueError, "already exists"):
                preparation.prepare(source, output, pin)

    def test_changed_source_refuses_before_creating_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, pin = self.archive(root, b"fixture")
            source.write_bytes(source.read_bytes() + b"changed")
            with self.assertRaisesRegex(ValueError, "digest differs"):
                preparation.prepare(source, root / "prepared", pin)
            self.assertFalse((root / "prepared").exists())

    def test_ambiguous_source_and_changed_declaration_refuse(self):
        for duplicate, message in [(True, "ambiguous"), (False, "declaration differs")]:
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source, pin = self.archive(root, b"fixture", duplicate)
                with self.assertRaisesRegex(ValueError, message):
                    preparation.prepare(source, root / "prepared", pin)
                self.assertFalse((root / "prepared").exists())


if __name__ == "__main__":
    unittest.main()
