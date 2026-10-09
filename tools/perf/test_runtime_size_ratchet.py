"""Tests of the shared runtime's size budget (tools/perf/runtime_size_ratchet.py)."""

from __future__ import annotations

import io
import json
import pathlib
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
import urllib.error

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import runtime_size_ratchet as ratchet  # noqa: E402

HEADER = """headroom = 0.01
architectures = ["x86_64", "aarch64"]

[targets]
bytes = 150000000
relocation_bytes = 1000000
relocated_bytes = 4000000
symbol_relocations = 5000
needed_absent = ["libgst", "libglib-2.0"]
"""
RUNTIME = """
[runtime.x86_64]
bytes = 131072
relocation_bytes = 65536
relocated_bytes = 65536
symbol_relocations = 64

[runtime.aarch64]
bytes = 131072
relocation_bytes = 65536
relocated_bytes = 65536
symbol_relocations = 64
"""
TEXT = HEADER + RUNTIME
REASON = 'reason = "the toolkit gained a media engine"\nrun = 90210\n'

R_X86_64_RELATIVE, R_X86_64_GLOB_DAT = 8, 6


def budget(text: str = TEXT, **raises: str) -> ratchet.Budget:
    return ratchet.Budget(text, {f"{name}.toml": body for name, body in raises.items()})


def runtime_raise(metric: str = "bytes", added: int = 65536, arch: str = "x86_64", reason: str = REASON) -> str:
    return f"{reason}[runtime.{arch}]\n{metric} = {added}\n"


def elf(rela: list[tuple[int, int]] = (), relr: list[int] = (), needed: list[str] = (),
        machine: int = 62, padding: int = 0) -> bytes:
    """A minimal 64-bit LE ELF file: section headers, .dynamic, .rela.dyn, .relr.dyn and `padding` of .text."""
    dynstr = b"\0" + b"".join(name.encode() + b"\0" for name in needed)
    offsets, cursor = [], 1
    for name in needed:
        offsets.append(cursor)
        cursor += len(name) + 1
    dynamic = b"".join(struct.pack("<qQ", ratchet.DT_NEEDED, offset) for offset in offsets) + struct.pack("<qQ", 0, 0)
    rela_body = b"".join(struct.pack("<QQq", offset, kind, 0) for offset, kind in rela)
    relr_body = b"".join(struct.pack("<Q", entry) for entry in relr)
    text = b"\xcc" * padding
    names = [b"", b".shstrtab", b".dynstr", b".dynamic", b".rela.dyn", b".relr.dyn", b".text"]
    shstrtab = b"\0".join(names) + b"\0"
    name_offsets = [shstrtab.index(name + b"\0") if name else 0 for name in names]
    bodies = [b"", shstrtab, dynstr, dynamic, rela_body, relr_body, text]
    kinds = [0, 3, 3, ratchet.SHT_DYNAMIC, ratchet.SHT_RELA, ratchet.SHT_RELR, 1]
    links = [0, 0, 0, 2, 0, 0, 0]
    data = bytearray(64)
    placed = []
    for body in bodies:
        placed.append(len(data))
        data += body
    shoff = len(data)
    for index, body in enumerate(bodies):
        data += struct.pack("<IIQQQQIIQQ", name_offsets[index], kinds[index], 0, 0, placed[index] if body else 0,
                            len(body), links[index], 0, 8, 0)
    header = b"\x7fELF" + bytes([2, 1, 1]) + bytes(9)
    header += struct.pack("<HHIQQQIHHHHHH", 3, machine, 1, 0, 0, shoff, 0, 64, 0, 0, 64, len(bodies), 1)
    data[:64] = header
    return bytes(data)


class Scratch(unittest.TestCase):
    def path(self, data: bytes, name: str = "libruntime_dylib.so") -> pathlib.Path:
        directory = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(directory)], check=False))
        path = directory / name
        path.write_bytes(data)
        return path


class ReadElf(Scratch):
    def test_relocations_pages_and_needed_are_read(self):
        data = elf(rela=[(0x1000, R_X86_64_RELATIVE), (0x1008, R_X86_64_GLOB_DAT), (0x5000, R_X86_64_GLOB_DAT)],
                   needed=["libstd-1.so", "libgstreamer-1.0.so.0", "libc.so.6"])
        stats = ratchet.read_elf(self.path(data))
        self.assertEqual(stats.arch, "x86_64")
        self.assertEqual(stats.bytes, len(data))
        self.assertEqual(stats.relocations, 3)
        self.assertEqual(stats.symbol_relocations, 2)
        self.assertEqual(stats.relocation_bytes, 72)
        self.assertEqual(stats.relocated_bytes, 2 * 4096)
        self.assertEqual(stats.needed, ["libstd-1.so", "libgstreamer-1.0.so.0", "libc.so.6"])

    def test_packed_relative_relocations_are_decoded(self):
        # An address, then a bitmap: bit n (n >= 1) relocates base + (n - 1) * 8,
        # base being the word after the address. 0x2000 starts the run;
        # 0b1011 marks 0x2008 and 0x2018; the next bitmap starts 63 words on.
        relr = [0x2000, (0b101 << 1) | 1, (1 << 63) | 1]
        stats = ratchet.read_elf(self.path(elf(relr=relr)))
        self.assertEqual(stats.relocations, 4)
        self.assertEqual(stats.symbol_relocations, 0)
        self.assertEqual(stats.relocation_bytes, 24)
        self.assertEqual(ratchet._relr_addresses(b"".join(struct.pack("<Q", e) for e in relr)),
                         [0x2000, 0x2008, 0x2018, 0x2008 + 63 * 8 + 62 * 8])
        self.assertEqual(stats.relocated_bytes, 4096)

    def test_aarch64_relative_relocations_need_no_lookup(self):
        stats = ratchet.read_elf(self.path(elf(rela=[(0x1000, 1027), (0x1008, 1025)], machine=183)))
        self.assertEqual((stats.arch, stats.symbol_relocations), ("aarch64", 1))

    def test_a_file_that_is_not_elf_fails(self):
        with self.assertRaises(ValueError):
            ratchet.read_elf(self.path(b"#!/bin/sh\n"))

    def test_another_machine_fails(self):
        with self.assertRaises(ValueError):
            ratchet.read_elf(self.path(elf(machine=3)))


class Measure(Scratch):
    def test_a_runtime_within_its_budgets_passes_and_reports_media_libraries(self):
        dylib = elf(needed=["libglib-2.0.so.0", "libc.so.6"], padding=100_000)
        status, lines, stats = ratchet.measure(self.path(dylib), budget(), "x86_64")
        self.assertEqual(status, 0, lines)
        self.assertTrue(any("within its budget of 131072; KPI target 150000000" in line for line in lines), lines)
        self.assertIn("runtime x86_64: every app loads libglib-2.0.so.0; KPI target: none of libgst, "
                      "libglib-2.0 (TIL-604)", lines)
        self.assertEqual(stats.metrics()["symbol_relocations"], 0)

    def test_over_a_budget_fails_and_names_the_raise(self):
        path = self.path(elf(padding=131072))
        status, lines, _ = ratchet.measure(path, budget(), "x86_64")
        self.assertEqual(status, 1)
        size = path.stat().st_size
        self.assertTrue(any(f"error: runtime x86_64 bytes: {size} is over its budget of 131072" in line
                            for line in lines), lines)
        self.assertIn("    [runtime.x86_64]", lines)
        self.assertIn(f"    bytes = {ratchet.budget_for('bytes', size, 0.01) - 131072}", lines)

    def test_a_raise_file_adds_to_the_budget(self):
        path = self.path(elf(padding=131072))
        self.assertEqual(ratchet.measure(path, budget(media=runtime_raise(added=131072)), "x86_64")[0], 0)
        self.assertEqual(ratchet.measure(path, budget(media=runtime_raise(arch="aarch64", added=131072)),
                                         "x86_64")[0], 1)

    def test_a_much_smaller_runtime_prints_the_lower_budget(self):
        roomy = TEXT.replace("bytes = 131072", "bytes = 1048576", 1)
        status, lines, _ = ratchet.measure(self.path(elf()), budget(roomy), "x86_64")
        self.assertEqual(status, 0)
        self.assertTrue(any("lower it: [runtime.x86_64] bytes = 65536" in line for line in lines), lines)

    def test_the_wrong_architecture_fails(self):
        status, lines, _ = ratchet.measure(self.path(elf()), budget(), "aarch64")
        self.assertEqual(status, 1)
        self.assertIn("built for x86_64, not aarch64", lines[0])

    def test_a_metric_without_a_budget_fails(self):
        status, lines, _ = ratchet.measure(self.path(elf()), budget(TEXT.replace("symbol_relocations = 64\n", "", 1)),
                                           "x86_64")
        self.assertEqual(status, 1)
        self.assertTrue(any("symbol_relocations: 0 and no budget" in line for line in lines), lines)


def collection(entries: dict[str, bytes]) -> bytes:
    body = b"TLOCOL\0\0" + struct.pack("<I", len(entries))
    for path, digest in sorted(entries.items()):
        body += struct.pack("<I", len(path)) + path.encode() + digest
    return body


def package(files: dict[str, bytes]) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        for name, body in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(body)
            archive.addfile(info, io.BytesIO(body))
    return buffer.getvalue()


def version(arch: str, released: int, url: str) -> dict:
    return {"version": f"0.1.{released}", "tapp_url": url, "released_at_unix": released,
            "target": {"platform": "tilo-os", "architecture": arch, "runtime": f"tilo-runtime/{released}"}}


class Published(unittest.TestCase):
    def registry(self, chunked: bool) -> tuple[dict, dict[str, bytes]]:
        objects: dict[str, bytes] = {}
        versions = []
        for arch, machine in (("x86_64", 62), ("aarch64", 183)):
            dylib = elf(machine=machine, needed=["libc.so.6"])
            digest = ("a" if arch == "x86_64" else "b") * 64
            if chunked:
                pieces = [dylib[:100], dylib[100:]]
                chunks = []
                for index, piece in enumerate(pieces):
                    chunk = f"{index}{digest[1:]}"
                    objects[f"https://r.invalid/blobs/{chunk}"] = piece
                    chunks.append({"hash": chunk, "len": len(piece)})
                objects[f"https://r.invalid/blobs/{digest}.chunks"] = json.dumps(
                    {"format": 1, "blob": digest, "len": len(dylib), "chunk_bytes": 100, "chunks": chunks}).encode()
            else:
                objects[f"https://r.invalid/blobs/{digest}"] = dylib
            url = f"https://r.invalid/{arch}.tapp"
            objects[url] = package({"manifest.toml": b"", "collection.qcl": collection(
                {ratchet.DYLIB: bytes.fromhex(digest), "lib/libc.so.6": bytes(32)})})
            versions += [version(arch, 2, url), version(arch, 1, "https://r.invalid/old.tapp")]
        entries = {"sequence": 7, "entries": [{"package_id": ratchet.RUNTIME_PACKAGE, "versions": versions},
                                              {"package_id": "so.tilo.notes", "versions": []}]}
        return entries, objects

    @staticmethod
    def getter(objects: dict[str, bytes]):
        def get(url: str) -> bytes:
            if url not in objects:
                raise urllib.error.HTTPError(url, 404, "not found", {}, io.BytesIO())
            return objects[url]
        return get

    def test_the_newest_runtime_of_each_architecture_is_measured(self):
        for chunked in (False, True):
            with self.subTest(chunked=chunked):
                entries, objects = self.registry(chunked)
                status, lines, measured = ratchet.published(entries, budget(), "https://r.invalid",
                                                            self.getter(objects))
                self.assertEqual(status, 0, lines)
                self.assertEqual(sorted(measured), ["aarch64", "x86_64"])
                self.assertEqual(measured["x86_64"]["version"], "0.1.2")
                self.assertEqual(measured["aarch64"]["bytes"], len(elf(machine=183, needed=["libc.so.6"])))

    def test_a_missing_architecture_fails(self):
        entries, objects = self.registry(False)
        entries["entries"][0]["versions"] = [v for v in entries["entries"][0]["versions"]
                                             if v["target"]["architecture"] == "x86_64"]
        status, lines, measured = ratchet.published(entries, budget(), "https://r.invalid", self.getter(objects))
        self.assertEqual(status, 1)
        self.assertIn("error: aarch64: the registry lists no so.tilo.runtime", lines)
        self.assertEqual(list(measured), ["x86_64"])

    def test_a_registry_without_the_runtime_fails(self):
        with self.assertRaises(ValueError):
            ratchet.newest_runtimes({"entries": [{"package_id": "so.tilo.notes", "versions": []}]})

    def test_a_short_chunk_fails(self):
        entries, objects = self.registry(True)
        key = next(url for url in objects if url.endswith("/blobs/0" + "a" * 63))
        objects[key] = objects[key][:-1]
        with self.assertRaises(ValueError):
            ratchet.published(entries, budget(), "https://r.invalid", self.getter(objects))

    def test_a_collection_without_the_dylib_fails(self):
        with self.assertRaises(ValueError):
            ratchet.collection_hash(collection({"lib/libc.so.6": bytes(32)}), ratchet.DYLIB)


class Check(unittest.TestCase):
    def test_an_unchanged_budget_passes(self):
        self.assertEqual(ratchet.check(budget(), budget()), [])

    def test_a_lower_budget_passes(self):
        self.assertEqual(ratchet.check(budget(), budget(TEXT.replace("bytes = 131072", "bytes = 65536", 1))), [])

    def test_a_raised_budget_needs_a_raise_file(self):
        after = budget(TEXT.replace("bytes = 131072", "bytes = 196608", 1))
        self.assertEqual(ratchet.check(budget(), after),
                         ["[runtime.x86_64] bytes: raised from 131072 to 196608 beyond the 0 its new raise files add"])

    def test_a_new_raise_file_allows_its_growth(self):
        self.assertEqual(ratchet.check(budget(), budget(media=runtime_raise())), [])

    def test_a_raise_file_already_on_the_base_allows_nothing_more(self):
        before = budget(media=runtime_raise())
        after = budget(TEXT.replace("bytes = 131072", "bytes = 196608", 1), media=runtime_raise())
        self.assertEqual(len(ratchet.check(before, after)), 1)

    def test_a_raise_needs_a_reason_and_a_run(self):
        problems = ratchet.check(budget(), budget(bad=runtime_raise(reason='reason = "short"\n')))
        self.assertTrue(any("needs a reason" in problem for problem in problems), problems)

    def test_a_raise_may_not_more_than_double_a_budget(self):
        problems = ratchet.check(budget(), budget(huge=runtime_raise(added=131073)))
        self.assertTrue(any("more than doubles" in problem for problem in problems), problems)

    def test_a_raise_names_a_budgeted_metric(self):
        problems = ratchet.check(budget(), budget(odd=runtime_raise(metric="pages")))
        self.assertTrue(any("names nothing with a budget" in problem for problem in problems), problems)

    def test_targets_and_headroom_only_tighten(self):
        after = budget(TEXT.replace("bytes = 150000000", "bytes = 160000000")
                       .replace("headroom = 0.01", "headroom = 0.05")
                       .replace('needed_absent = ["libgst", "libglib-2.0"]', 'needed_absent = ["libgst"]'))
        problems = ratchet.check(budget(), after)
        self.assertIn("headroom raised", problems)
        self.assertIn("KPI target bytes raised; targets only decrease", problems)
        self.assertIn("[targets] needed_absent lost an entry; targets only tighten", problems)

    def test_every_architecture_has_every_metric(self):
        problems = ratchet.check(None, budget(HEADER + RUNTIME.split("[runtime.aarch64]")[0]))
        self.assertIn("[runtime.<arch>] tables must be exactly x86_64, aarch64", problems)
        problems = ratchet.check(None, budget(TEXT.replace("symbol_relocations = 64\n", "", 1)))
        self.assertEqual(problems, ["[runtime.x86_64] needs a positive budget for each of bytes, relocation_bytes, "
                                    "relocated_bytes, symbol_relocations"])


class Revisions(unittest.TestCase):
    def test_the_budget_and_its_raises_are_read_at_a_revision(self):
        root = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(root)], check=False))

        def git(*args: str) -> None:
            subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@invalid", *args],
                           cwd=root, check=True, capture_output=True)

        git("init", "-q")
        (root / "README").write_text("before the budget\n")
        git("add", "README")
        git("commit", "-q", "-m", "first")
        directory = root / ratchet.DIRECTORY
        (directory / "raises").mkdir(parents=True)
        (directory / "budget.toml").write_text(TEXT)
        (directory / "raises" / "media.toml").write_text(runtime_raise())
        git("add", ".")
        git("commit", "-q", "-m", "second")
        at = ratchet.load_at("HEAD", root)
        self.assertEqual(at.totals()[("x86_64", "bytes")], 196608)
        self.assertEqual(ratchet.load(root).totals(), at.totals())
        self.assertIsNone(ratchet.load_at("HEAD~1", root))
        with self.assertRaises(ValueError):
            ratchet.load_at("0" * 40, root)


class CommittedBudget(unittest.TestCase):
    def test_the_committed_budget_is_well_formed(self):
        self.assertEqual(ratchet.check(None, ratchet.load()), [])

    def test_its_raise_files_are_valid(self):
        current = ratchet.load()
        for name, raised in sorted(current.raises.items()):
            with self.subTest(raise_file=name):
                self.assertEqual(ratchet.raise_problems(name, raised, current), [])

    def test_the_kpi_records_carry_the_committed_targets(self):
        import tomllib
        kpis = tomllib.loads((HERE / "kpi-budgets.toml").read_text())["kpi"]
        current = ratchet.load()
        for arch in current.architectures:
            for metric, target in current.targets.items():
                with self.subTest(arch=arch, metric=metric):
                    self.assertEqual(kpis[f"footprint.runtime.{metric}.{arch}"]["max"], float(target))


if __name__ == "__main__":
    unittest.main()
