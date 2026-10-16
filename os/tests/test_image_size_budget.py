"""os/tests/image_size_budget.py and the shipped os/image-size-budget.toml."""

import contextlib
import io
import json
import os
import struct
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import image_size_budget as budget  # noqa: E402

REPO = HERE.parents[1]


def elf_with_symtab(path: Path, symtab_size: int) -> None:
    """A minimal ELF64 LE file whose section table names one `.symtab`."""
    shstrtab = b"\0.symtab\0.shstrtab\0"
    body = b"\0" * symtab_size
    data_off = 64
    str_off = data_off + len(body)
    shoff = str_off + len(shstrtab)
    header = bytearray(64)
    header[:4] = b"\x7fELF"
    header[4], header[5], header[6] = 2, 1, 1
    struct.pack_into("<Q", header, 0x28, shoff)
    struct.pack_into("<HHH", header, 0x3A, 64, 3, 2)
    null = bytes(64)
    symtab = struct.pack("<IIQQQQIIQQ", 1, 2, 0, 0, data_off, symtab_size, 0, 0, 1, 0)
    strtab = struct.pack("<IIQQQQIIQQ", 9, 3, 0, 0, str_off, len(shstrtab), 0, 0, 1, 0)
    path.write_bytes(bytes(header) + body + shstrtab + null + symtab + strtab)


class Tree(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)

    def put(self, relative, size):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"x" * size)
        return path

    def run_cli(self, *argv):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = budget.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def budgets(self, usr_bytes, extra=""):
        path = self.root.parent / f"{self.root.name}.toml"
        path.write_text(f'[moved_to_sysext]\npaths = ["usr/lib/tilo/windows-runners", "usr/bin/FEX"]\n'
                        f'[flavour.t]\nusr_bytes = {usr_bytes}\n{extra}')
        self.addCleanup(path.unlink)
        return path


class Measure(Tree):
    def test_counts_each_inode_once_and_skips_symlinks_and_other_trees(self):
        first = self.put("usr/lib/a", 1000)
        os.link(first, self.root / "usr/lib/a-hardlink")
        os.symlink("a", self.root / "usr/lib/a-symlink")
        self.put("var/lib/big", 5000)
        total, largest = budget.measure_usr(self.root)
        self.assertEqual(total, 1000)
        self.assertEqual(largest[0][0], 1000)

    def test_a_missing_usr_is_an_error_not_a_pass(self):
        code, _, err = self.run_cli("check", "--flavour", "t", "--root", str(self.root),
                                    "--budgets", str(self.budgets(10)))
        self.assertEqual(code, 2)
        self.assertIn("not a directory", err)


class RootCheck(Tree):
    def test_within_budget_passes(self):
        self.put("usr/lib/a", 900)
        code, out, _ = self.run_cli("check", "--flavour", "t", "--root", str(self.root),
                                    "--budgets", str(self.budgets(1000)))
        self.assertEqual(code, 0)
        self.assertIn("ok for t", out)

    def test_over_budget_fails_and_names_the_largest_files(self):
        self.put("usr/lib/huge.so", 1500)
        self.put("usr/lib/small", 10)
        code, _, err = self.run_cli("check", "--flavour", "t", "--root", str(self.root),
                                    "--budgets", str(self.budgets(1000)))
        self.assertEqual(code, 1)
        self.assertIn("over the", err)
        self.assertIn("/usr/lib/huge.so", err)

    def test_moved_files_must_not_return_to_the_image(self):
        self.put("usr/lib/tilo/windows-runners/proton/proton", 10)
        self.put("usr/bin/FEX", 10)
        self.put("usr/lib/ok", 10)
        code, _, err = self.run_cli("check", "--flavour", "t", "--root", str(self.root),
                                    "--budgets", str(self.budgets(10_000)))
        self.assertEqual(code, 1)
        self.assertIn("/usr/lib/tilo/windows-runners", err)
        self.assertIn("/usr/bin/FEX", err)
        self.assertIn("windows-compat system extension", err)

    def test_an_unknown_flavour_is_an_input_error(self):
        self.put("usr/lib/a", 1)
        code, _, err = self.run_cli("check", "--flavour", "nope", "--root", str(self.root),
                                    "--budgets", str(self.budgets(10)))
        self.assertEqual(code, 2)
        self.assertIn("no budget for flavour", err)

    def test_symbol_table_limit_catches_an_unstripped_binary(self):
        elf_with_symtab(self.put("usr/bin/shell", 0), 5000)
        elf_with_symtab(self.put("usr/bin/clean", 0), 100)
        self.assertEqual(budget.symtab_bytes(self.root / "usr/bin/shell"), 5000)
        code, _, err = self.run_cli("check", "--flavour", "t", "--root", str(self.root),
                                    "--budgets", str(self.budgets(1_000_000, "max_elf_symtab_bytes = 1000\n")))
        self.assertEqual(code, 1)
        self.assertIn("/usr/bin/shell", err)
        self.assertNotIn("/usr/bin/clean", err)

    def test_non_elf_files_have_no_symtab(self):
        self.assertEqual(budget.symtab_bytes(self.put("usr/bin/script", 100)), 0)


class ReportCheck(Tree):
    def report(self, usr, files=()):
        path = self.root / "report.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"totals": {"usr_content": usr}, "files": [list(f) for f in files]}))
        return path

    def test_report_over_budget_fails(self):
        code, _, err = self.run_cli("check", "--flavour", "t", "--report", str(self.report(2000, [["/usr/x", 2000]])),
                                    "--budgets", str(self.budgets(1000)))
        self.assertEqual(code, 1)
        self.assertIn("/usr/x", err)

    def test_report_listing_a_moved_file_fails(self):
        report = self.report(10, [["/usr/lib/tilo/windows-runners/proton/files/bin/wine64", 9]])
        code, _, err = self.run_cli("check", "--flavour", "t", "--report", str(report),
                                    "--budgets", str(self.budgets(1000)))
        self.assertEqual(code, 1)
        self.assertIn("windows-runners", err)

    def test_report_without_the_measurement_is_not_a_pass(self):
        path = self.root / "r.json"
        path.write_text(json.dumps({"totals": {"content": 1}}))
        code, _, err = self.run_cli("check", "--flavour", "t", "--report", str(path),
                                    "--budgets", str(self.budgets(1000)))
        self.assertEqual(code, 2)
        self.assertIn("not observed", err)


class ShippedBudgets(unittest.TestCase):
    def test_every_ci_flavour_has_a_budget(self):
        data = budget.load_budgets()
        for flavour in ("production-x86_64", "production-aarch64", "board-lenovo-83ed",
                        "dev-x86_64", "dev-aarch64"):
            self.assertIn(flavour, data["flavour"])

    def test_budgets_stay_inside_the_users_ten_gigabytes_for_production(self):
        data = budget.load_budgets()
        for name, flavour in data["flavour"].items():
            if name.startswith(("production-", "board-")):
                self.assertLessEqual(flavour["usr_bytes"], 10_000_000_000, name)

    def test_the_image_workflow_runs_the_check_for_every_flavour(self):
        workflow = (REPO / ".forgejo/workflows/build-image.yml").read_text()
        self.assertIn("os/tests/image_size_budget.py check", workflow)
        self.assertIn("tools/perf/image_size.py", workflow)

    def test_the_policy_that_hands_the_check_its_input_exists(self):
        self.assertIn("usr_content", (REPO / "tools/perf/image_size.py").read_text())


if __name__ == "__main__":
    unittest.main()
