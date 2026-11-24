import importlib.util
import io
import struct
import sys
import unittest
import zipfile
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "ci/windows/gstreamer_runtime.py"
SPEC = importlib.util.spec_from_file_location("gstreamer_runtime", SCRIPT)
runtime = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = runtime
SPEC.loader.exec_module(runtime)


def executable(imported=(), delayed=(), machine=runtime.AMD64):
    """A PE32+ image whose import and delay-import tables name these DLLs."""
    section_rva, section_raw = 0x1000, 0x200
    imports_at = 0
    delayed_at = imports_at + 20 * (len(imported) + 1)
    strings_at = delayed_at + 32 * (len(delayed) + 1)
    body, strings = bytearray(strings_at), bytearray()
    for index, name in enumerate(imported):
        struct.pack_into("<5I", body, imports_at + 20 * index, 0, 0, 0,
                         section_rva + strings_at + len(strings), 0)
        strings += name.encode() + b"\0"
    for index, name in enumerate(delayed):
        struct.pack_into("<8I", body, delayed_at + 32 * index, 1,
                         section_rva + strings_at + len(strings), 0, 0, 0, 0, 0, 0)
        strings += name.encode() + b"\0"
    body += strings
    header = bytearray(section_raw)
    header[:2] = b"MZ"
    struct.pack_into("<I", header, 0x3C, 0x40)
    header[0x40:0x44] = b"PE\0\0"
    optional_size = 112 + 16 * 8
    struct.pack_into("<HHIIIHH", header, 0x44, machine, 1, 0, 0, 0, optional_size, 0x2022)
    optional = 0x58
    struct.pack_into("<H", header, optional, 0x20B)
    directories = optional + 112
    if imported:
        struct.pack_into("<II", header, directories + 8, section_rva + imports_at, 20)
    if delayed:
        struct.pack_into("<II", header, directories + 13 * 8, section_rva + delayed_at, 32)
    section = optional + optional_size
    header[section:section + 8] = b".idata\0\0"
    struct.pack_into("<IIII", header, section + 8, len(body), section_rva, len(body), section_raw)
    return bytes(header + body)


def reviewed_tree():
    """A release whose plugins load exactly the reviewed libraries, and only Windows beside."""
    libraries = sorted(runtime.LIBRARIES)
    tree = {f"bin/{name}": executable(["KERNEL32.dll"]) for name in libraries}
    for index, name in enumerate(sorted(runtime.PLUGINS)):
        tree[f"lib/gstreamer-1.0/gst{name}.dll"] = executable(
            [libraries[index % len(libraries)].upper(), "kernel32.dll"])
    tree[runtime.SCANNER] = executable(libraries, ["api-ms-win-crt-runtime-l1-1-0.dll"])
    return tree


class ImportsTests(unittest.TestCase):
    def test_both_import_tables_are_read(self):
        machine, names = runtime.imports(executable(["gstreamer-1.0-0.dll"], ["mf.dll"]))
        self.assertEqual(machine, runtime.AMD64)
        self.assertEqual(names, ["gstreamer-1.0-0.dll", "mf.dll"])

    def test_anything_but_a_windows_executable_is_refused(self):
        with self.assertRaisesRegex(ValueError, "not a Windows executable"):
            runtime.imports(b"\x7fELF" + bytes(64))


class BundleAuditTests(unittest.TestCase):
    """A runtime bundle carries exactly the closure staged, byte for byte, and nothing more."""

    def setUp(self):
        import json
        import tempfile
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        tree = reviewed_tree()
        tree["licenses/LGPL-2.1-or-later.txt"] = b"notice"
        self.staged = root / "gstreamer-runtime-x86_64.json"
        self.staged.write_text(json.dumps({"files": {
            path: {"sha256": runtime.digest(data)} for path, data in tree.items()}}))
        self.bundle = root / "bundle"
        for path, data in tree.items():
            kept = path.replace("libexec/", "lib/libexec/", 1).replace("licenses/", "lib/licenses/", 1)
            (self.bundle / kept).parent.mkdir(parents=True, exist_ok=True)
            (self.bundle / kept).write_bytes(data)
        (self.bundle / "bin/tilolang-process.exe").write_bytes(executable(["kernel32.dll"]))
        (self.bundle / "bundle.toml").write_text("version = \"1\"\n")

    def test_the_staged_closure_passes(self):
        self.assertEqual(runtime.audit(self.bundle, self.staged), len(reviewed_tree()) + 1)

    def test_a_file_the_closure_did_not_stage_is_refused(self):
        (self.bundle / "lib/gstreamer-1.0/gstx264.dll").write_bytes(executable(["kernel32.dll"]))
        with self.assertRaisesRegex(ValueError, r"extra \['lib/gstreamer-1.0/gstx264.dll'\]"):
            runtime.audit(self.bundle, self.staged)

    def test_a_changed_library_is_refused(self):
        library = sorted(runtime.LIBRARIES)[0]
        (self.bundle / f"bin/{library}").write_bytes(executable(["user32.dll"]))
        with self.assertRaisesRegex(ValueError, f"bin/{library} is not the file staged"):
            runtime.audit(self.bundle, self.staged)


class ClosureTests(unittest.TestCase):
    def test_the_reviewed_closure_stages_with_the_windows_it_imports(self):
        staged, system = runtime.closure(reviewed_tree())
        runtime.review(staged, system)
        self.assertIn(runtime.SCANNER, staged)
        self.assertEqual(system, {"kernel32.dll", "api-ms-win-crt-runtime-l1-1-0.dll"})

    def test_a_library_no_one_reviewed_is_refused(self):
        tree = reviewed_tree()
        tree["bin/xml2-16.dll"] = executable(["kernel32.dll"])
        tree["lib/gstreamer-1.0/gstdav1d.dll"] = executable(["xml2-16.dll"])
        staged, system = runtime.closure(tree)
        with self.assertRaisesRegex(ValueError, r"unreviewed \['xml2-16.dll'\]"):
            runtime.review(staged, system)

    def test_an_encoder_library_is_refused_even_where_a_plugin_needs_it(self):
        # GStreamer's vorbis and theora plugins load the encoder libraries beside the decoders.
        for encoder in ("vorbisenc-2.dll", "theoraenc-1.dll", "x264-164.dll", "avcodec-61.dll",
                        "openh264-7.dll", "SvtAv1Enc.dll", "mp3lame-0.dll"):
            with self.subTest(encoder=encoder):
                tree = reviewed_tree()
                tree[f"bin/{encoder}"] = executable(["kernel32.dll"])
                tree["lib/gstreamer-1.0/gstopus.dll"] = executable([encoder, "opus-0.dll"])
                staged, system = runtime.closure(tree)
                with self.assertRaisesRegex(ValueError, "an encoder library"):
                    runtime.review(staged, system)

    def test_gpl_code_is_refused(self):
        staged, system = runtime.closure(reviewed_tree())
        for licence in ("GPL-2.0-or-later", "GPL-3.0-only", "AGPL-3.0"):
            with self.subTest(licence=licence):
                libraries = dict(runtime.LIBRARIES, **{"z-1.dll": (licence, [])})
                with mock.patch.object(runtime, "LIBRARIES", libraries):
                    with self.assertRaisesRegex(ValueError, f"z-1.dll is under the {licence}"):
                        runtime.review(staged, system)
        plugins = dict(runtime.PLUGINS, matroska="GPL")
        with mock.patch.object(runtime, "PLUGINS", plugins):
            with self.assertRaisesRegex(ValueError, "the matroska plugin is under the GPL"):
                runtime.review(staged, system)

    def test_a_dll_windows_does_not_carry_is_refused(self):
        tree = reviewed_tree()
        tree["lib/gstreamer-1.0/gstvpx.dll"] = executable(["nvcuda.dll"])
        staged, system = runtime.closure(tree)
        with self.assertRaisesRegex(ValueError, r"neither it nor Windows carries: \['nvcuda.dll'\]"):
            runtime.review(staged, system)

    def test_a_32_bit_binary_is_refused(self):
        tree = reviewed_tree()
        tree["lib/gstreamer-1.0/gstogg.dll"] = executable(["kernel32.dll"], machine=0x14C)
        with self.assertRaisesRegex(ValueError, "gstogg.dll is not an x86_64 binary"):
            runtime.closure(tree)

    def test_a_missing_plugin_is_named(self):
        tree = reviewed_tree()
        del tree["lib/gstreamer-1.0/gstlewton.dll"]
        with self.assertRaisesRegex(ValueError, "has no lib/gstreamer-1.0/gstlewton.dll"):
            runtime.closure(tree)


class MergeTests(unittest.TestCase):
    def wheel(self, package, files):
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w") as archive:
            for path, data in files.items():
                archive.writestr(f"{package}-{runtime.VERSION}.data/purelib/{package}/{path}", data)
            archive.writestr(f"{package}-{runtime.VERSION}.dist-info/METADATA", "")
        return buffer.getvalue()

    def test_wheels_merge_and_a_path_they_disagree_on_is_left_out(self):
        tree = runtime.merge({
            "gstreamer_libs": self.wheel("gstreamer_libs", {
                "__init__.py": "a", "bin/dav1d.dll": "same", "bin/tiff-6.dll": "one"}),
            "gstreamer_plugins_libs": self.wheel("gstreamer_plugins_libs", {
                "__init__.py": "b", "bin/dav1d.dll": "same", "bin/tiff-6.dll": "two"}),
        })
        self.assertEqual(tree, {"bin/dav1d.dll": b"same"})


class NoticeTests(unittest.TestCase):
    def test_every_notice_the_closure_names_is_the_pinned_text(self):
        texts = runtime.notices()
        self.assertIn("licenses/LGPL-2.1-or-later.txt", texts)
        self.assertIn("licenses/libvpx-1.14.1-PATENTS", texts)


if __name__ == "__main__":
    unittest.main()
