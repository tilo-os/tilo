"""Tests of the tapp symbol measurement (tools/perf/tapp_symbol_sizes.py)."""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import tapp_symbol_sizes as symbols  # noqa: E402


class CrateOfTest(unittest.TestCase):
    def test_names_the_crate_of_a_plain_path(self):
        self.assertEqual(symbols.crate_of("tilo_ui::widget::Row::new"), "tilo_ui")

    def test_names_the_crate_an_impl_belongs_to(self):
        self.assertEqual(symbols.crate_of("<tilo_actions::Action as core::fmt::Debug>::fmt"), "tilo_actions")

    def test_a_generic_instance_belongs_to_the_crate_that_compiled_it(self):
        self.assertEqual(symbols.crate_of("core::ptr::drop_in_place<tilo_resource::Record>"), "core")

    def test_a_symbol_without_a_path_is_other(self):
        self.assertEqual(symbols.crate_of("main"), "other")
        self.assertEqual(symbols.crate_of("sqlite3VdbeExec"), "other")

    def test_a_mangled_name_is_other(self):
        self.assertEqual(symbols.crate_of("_ZN4core3fmt17h0123456789abcdefE"), "other")


class ParseSymbolsTest(unittest.TestCase):
    def test_sums_defined_symbols_by_crate(self):
        listing = (
            "0000000000000100 0000000000000020 T tilo_ui::a\n"
            "0000000000000200 0000000000000008 T tilo_ui::b\n"
            "0000000000000300 0000000000000040 T core::c\n"
            "0000000000000400 0000000000000080 T tilo_ui::d\n"
            "                 U vendored::undefined\n"
        )
        self.assertEqual(symbols.parse_symbols(listing), {"tilo_ui": 0xA8, "core": 0x40})


class BiggestTest(unittest.TestCase):
    def test_names_the_largest_symbols_of_one_crate(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = pathlib.Path(directory, "app")
            binary.write_text("", encoding="utf-8")
            fake = pathlib.Path(directory, "nm")
            fake.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' '0000000000000100 0000000000000020 T tilo_ui::small'\n"
                "printf '%s\\n' '0000000000000200 0000000000000400 T tilo_ui::large'\n"
                "printf '%s\\n' '0000000000000300 0000000000000800 T core::larger'\n",
                encoding="utf-8")
            fake.chmod(0o755)
            environment = {"PATH": f"{directory}:/usr/bin:/bin"}
            saved = subprocess.run
            subprocess.run = lambda *args, **kwargs: saved(*args, **{**kwargs, "env": environment})
            try:
                rows = symbols.biggest(binary, "tilo_ui", 5)
            finally:
                subprocess.run = saved
            self.assertEqual(rows, ["    1024 tilo_ui::large", "      32 tilo_ui::small"])


class DiffTest(unittest.TestCase):
    def test_names_what_an_app_gained_by_crate(self):
        base = {"so.tilo.notes": {"notes": 1000, "tilo_ui": 18000, "vello": 9000}}
        head = {"so.tilo.notes": {"notes": 19500, "tilo_ui": 10000, "own_crate": 3000}}
        lines = symbols.diff(base, head, None, None)
        self.assertEqual(lines[0], "so.tilo.notes: 28000 -> 32500 (+4500)")
        self.assertIn("  +   18500 notes", lines)
        self.assertIn("  +    3000 own_crate", lines)
        self.assertIn("  -    8000 tilo_ui", lines)
        self.assertIn("  -    9000 vello", lines)

    def test_names_only_the_app_it_is_asked_for(self):
        base = {"so.tilo.notes": {"notes": 1}, "so.tilo.mail": {"mail": 1}}
        head = {"so.tilo.notes": {"notes": 9}, "so.tilo.mail": {"mail": 9}}
        lines = symbols.diff(base, head, ["so.tilo.mail"], None)
        self.assertEqual([line.split(":")[0] for line in lines if not line.startswith(" ")], ["so.tilo.mail"])


if __name__ == "__main__":
    unittest.main()
