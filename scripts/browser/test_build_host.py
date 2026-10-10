"""Runtime acquisition must fail closed on corrupt/unexpected SDK input."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("build_host", Path(__file__).with_name("build-host.py"))
build_host = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(build_host)


class AcquisitionTests(unittest.TestCase):
    def test_rebuilt_sdk_source_is_bound_to_the_locked_immutable_package(self):
        name = "cef_binary_test+revision_linux64_minimal.tar.bz2"
        source = "a" * 64
        url = "https://code.tilo.so/api/packages/tilo/generic/tilo-cef-sdk/" + source + "/cef_binary_test%2Brevision_linux64_minimal.tar.bz2"
        entry = {"name": name, "url": url, "source_patch_sha256": source}
        self.assertEqual(build_host.archive_url(entry), url)
        self.assertEqual(build_host.archive_url({"name": name}),
                         "https://cef-builds.spotifycdn.com/cef_binary_test%2Brevision_linux64_minimal.tar.bz2")
        with self.assertRaises(ValueError):
            build_host.archive_url({"name": name, "source_patch_sha256": source})
        for candidate in (
            url.replace("https:", "http:"),
            url.replace("code.tilo.so", "foreign.example"),
            url.replace("code.tilo.so", "user@code.tilo.so"),
            url.replace(source, "b" * 64),
            url.replace("tilo-cef-sdk", "another-package"),
            url + "?version=latest",
            url + "#latest",
        ):
            with self.subTest(url=candidate), self.assertRaises(ValueError):
                build_host.archive_url({**entry, "url": candidate})
        for source in (None, "latest", "A" * 64):
            with self.subTest(source=source), self.assertRaises(ValueError):
                build_host.archive_url({**entry, "source_patch_sha256": source})

    def test_blocker_uses_cargos_native_artifact_instead_of_guessing_the_target_directory(self):
        with tempfile.TemporaryDirectory() as tmp:
            artifact = Path(tmp) / "native-target/aarch64/thin-app/libtilo_chromium_blocker.a"
            artifact.parent.mkdir(parents=True)
            artifact.write_bytes(b"static library")
            metadata = {
                "reason": "compiler-artifact",
                "target": {"name": "tilo_chromium_blocker", "crate_types": ["staticlib", "rlib"]},
                "filenames": [str(artifact.with_suffix(".rlib")), str(artifact)],
            }
            with patch.object(build_host.subprocess, "run") as run:
                run.return_value.stdout = json.dumps(metadata) + "\n"
                self.assertEqual(build_host.build_blocker_library(), artifact)
                self.assertIn("--locked", run.call_args.args[0])
                artifact.unlink()
                with self.assertRaisesRegex(ValueError, "did not report"):
                    build_host.build_blocker_library()
                metadata["target"]["name"] = "another_crate"
                artifact.write_bytes(b"another library")
                run.return_value.stdout = json.dumps(metadata) + "\n"
                with self.assertRaisesRegex(ValueError, "did not report"):
                    build_host.build_blocker_library()

    def test_platform_and_range_inputs_are_validated_exactly(self):
        for system, machine, want in (
            ("Darwin", "arm64", "macosarm64"),
            ("Darwin", "x86_64", "macosx64"),
            ("Linux", "aarch64", "linuxarm64"),
            ("Linux", "x86_64", "linux64"),
        ):
            with self.subTest(platform=(system, machine)):
                self.assertEqual(build_host.platform_key(system, machine), want)
        with self.subTest(platform="unsupported riscv64"), self.assertRaises(ValueError):
            build_host.platform_key("Linux", "riscv64")

        # A range response must match the requested bytes exactly.
        for label, header, received, want in (
            ("exact match", "bytes 0-9/20", 10, True),
            ("missing header", None, 10, False),
            ("wider range than requested", "bytes 0-19/20", 20, False),
            ("short body", "bytes 0-9/20", 9, False),
        ):
            with self.subTest(range=label):
                self.assertIs(build_host.valid_range(header, 0, 9, 20, received), want)

    def test_archive_verification_requires_matching_size_and_sha256(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "sdk"
            path.write_bytes(b"sdk-content")
            entry = {
                "size": 11,
                "sha1": hashlib.sha1(b"sdk-content").hexdigest(),
                "sha256": hashlib.sha256(b"sdk-content").hexdigest(),
            }
            with self.subTest(case="sha256 is mandatory"):
                with self.assertRaisesRegex(ValueError, "requires"):
                    build_host.verify_archive(path, {"size": 11})
            with self.subTest(case="sha256 must be well formed"):
                with self.assertRaisesRegex(ValueError, "invalid"):
                    build_host.verify_archive(path, {"size": 11, "sha256": "z" * 64})
            for label, content, want in (
                ("matching content", b"sdk-content", True),
                ("bad checksum", b"bad-content", False),
                ("bad size", b"short", False),
            ):
                with self.subTest(case=label):
                    path.write_bytes(content)
                    self.assertIs(build_host.verify_archive(path, entry), want)


class RuntimeTreeTests(unittest.TestCase):
    def build_output(self, root, skip=()):
        build = Path(root) / "build"
        for name in build_host.RUNTIME_FILES:
            if name not in skip:
                (build / name).parent.mkdir(parents=True, exist_ok=True)
                (build / name).write_bytes(b"payload " + name.encode())
        (build / "locales").mkdir()
        (build / "locales/en-US.pak").write_bytes(b"pak")
        (build / "chrome-sandbox").write_bytes(b"setuid helper")
        (build / "tilo-protocol-test").write_bytes(b"test")
        return build

    def test_the_runtime_tree_holds_exactly_what_the_engine_loads(self):
        with tempfile.TemporaryDirectory() as tmp:
            build = self.build_output(tmp)
            engine = build_host.install_runtime(build, Path(tmp) / "runtime")
            self.assertEqual(engine, Path(tmp) / "runtime/chromium")
            installed = sorted(str(path.relative_to(engine)) for path in engine.rglob("*") if path.is_file())
            self.assertEqual(installed, sorted([*build_host.RUNTIME_FILES, "locales/en-US.pak"]))

    def test_a_missing_engine_file_fails_the_build(self):
        with tempfile.TemporaryDirectory() as tmp:
            build = self.build_output(tmp, skip=("libcef.so",))
            with self.assertRaisesRegex(ValueError, "libcef.so"):
                build_host.install_runtime(build, Path(tmp) / "runtime")

    def test_a_supplied_archive_must_be_the_pinned_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / "sdk.tar.bz2"
            archive.write_bytes(b"not the sdk")
            entry = {"name": "cef_binary_x.tar.bz2", "size": 3, "sha256": hashlib.sha256(b"sdk").hexdigest()}
            with self.assertRaisesRegex(ValueError, "pinned"):
                build_host.acquire_sdk(Path(tmp) / "cache", entry, archive)


if __name__ == "__main__":
    unittest.main()
