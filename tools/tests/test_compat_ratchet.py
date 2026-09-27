import importlib.util
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_compat_ratchet.py"
SPEC = importlib.util.spec_from_file_location("check_compat_ratchet", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)

OLD_ENTRY = """
[[api]]
name = "old-hook"
owner = "crates/sdk/ui-core"
target = "Widget::new_hook"
reason = "positional hook"
pattern = 'fn\\s+old_hook\\s*\\('
needles = ["old_hook"]
count = {count}
adapters = {adapters}
"""
TRAIT = """
pub trait Widget {
    /// Temporary compatibility adapter; implementations use new_hook.
    fn old_hook(&mut self) { self.new_hook() }
    fn new_hook(&mut self) {}
}
"""
ADAPTER = '["crates/sdk/ui-core/src/widget.rs::old_hook"]'


class CompatRatchetTests(unittest.TestCase):
    def fixture(self, files: dict[str, str], ledger: str) -> tuple[Path, Path]:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        subprocess.run(["git", "init", "-q", str(root)], check=True)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(textwrap.dedent(text), encoding="utf-8")
        ledger_path = root / "compat_ratchet.toml"
        ledger_path.write_text(ledger, encoding="utf-8")
        return root, ledger_path

    def reasons(self, files: dict[str, str], ledger: str) -> list[str]:
        root, ledger_path = self.fixture(files, ledger)
        return [str(violation) for violation in contract.audit(root, ledger_path)]

    def test_counts_are_a_ratchet_that_must_be_lowered_and_finally_deleted(self):
        sites = {
            "crates/sdk/ui-core/src/widget.rs": TRAIT,
            "apps/a/src/lib.rs": "impl Widget for A { fn old_hook(&mut self) {} }\n",
        }
        with self.subTest(phase="matching count passes"):
            self.assertEqual(self.reasons(sites, OLD_ENTRY.format(count=2, adapters=ADAPTER)), [])
        with self.subTest(phase="a new site fails and names the target"):
            grown = {**sites, "apps/b/src/lib.rs": "fn old_hook(&mut self) {}\n"}
            [reason] = self.reasons(grown, OLD_ENTRY.format(count=2, adapters=ADAPTER))
            self.assertIn("3 site(s), ledger allows 2", reason)
            self.assertIn("Widget::new_hook", reason)
        with self.subTest(phase="a removed site fails until the ledger is lowered"):
            [reason] = self.reasons(sites, OLD_ENTRY.format(count=3, adapters=ADAPTER))
            self.assertIn("lower count to 2", reason)
        with self.subTest(phase="zero sites asks for the entry to be deleted"):
            [reason] = self.reasons({"apps/a/src/lib.rs": "fn new_hook() {}\n"}, OLD_ENTRY.format(count=1, adapters="[]"))
            self.assertIn("delete this entry", reason)

    def test_vendor_generated_and_comment_sites_do_not_count(self):
        files = {
            "crates/sdk/ui-core/src/widget.rs": TRAIT + "/// fn old_hook(&mut self) in a doc example\n//   fn old_hook(\n",
            "vendor/dep/src/lib.rs": "fn old_hook() {}\n",
            "crates/x/src/generated/api_capnp.rs": "fn old_hook() {}\n",
            "crates/x/src/wire_capnp.rs": "fn old_hook() {}\n",
            "crates/x/target/debug/build/out.rs": "fn old_hook() {}\n",
            "docs/notes.rs": "fn old_hook() {}\n",
        }
        self.assertEqual(self.reasons(files, OLD_ENTRY.format(count=1, adapters=ADAPTER)), [])

    def test_paths_limit_an_entry_to_its_prefixes(self):
        ledger = OLD_ENTRY.format(count=1, adapters=ADAPTER) + 'paths = ["crates/sdk/"]\n'
        files = {
            "crates/sdk/ui-core/src/widget.rs": TRAIT,
            "apps/a/src/lib.rs": "fn old_hook() {}\n",
        }
        self.assertEqual(self.reasons(files, ledger), [])

    def test_every_adapter_and_deprecated_item_needs_a_ledger_entry(self):
        with self.subTest(phase="an unledgered sdk adapter fails"):
            [reason] = self.reasons({"crates/sdk/ui-core/src/widget.rs": TRAIT}, "")
            self.assertIn("crates/sdk/ui-core/src/widget.rs:3", reason)
            self.assertIn("widget.rs::old_hook", reason)
        with self.subTest(phase="lint reasons and legacy shims are markers too"):
            files = {
                "crates/sdk/a/src/lib.rs": (
                    '#[allow(clippy::too_many_arguments, reason = "temporary compatibility adapter")]\n'
                    "pub fn wide() {}\n"
                    "/// Legacy shim over the new path.\n"
                    "pub(crate) const fn narrow() {}\n"
                ),
            }
            reasons = self.reasons(files, "")
            self.assertEqual(len(reasons), 2)
            self.assertIn("lib.rs::wide", reasons[0])
            self.assertIn("lib.rs::narrow", reasons[1])
        with self.subTest(phase="ordinary uses of the words are not markers"):
            files = {
                "crates/sdk/a/src/lib.rs": (
                    "/// Imports legacy formats into a temporary buffer.\n"
                    "/// Checks compatibility of the adapter's schema.\n"
                    "pub fn import() {}\n"
                ),
                "crates/session/shell/src/lib.rs": "/// Compatibility adapter outside the SDK.\nfn f() {}\n",
            }
            self.assertEqual(self.reasons(files, ""), [])
        with self.subTest(phase="deprecated items anywhere need an entry"):
            files = {
                "crates/session/shell/src/lib.rs": (
                    '#[deprecated(note = "use g")]\n#[derive(Debug)]\npub struct Old;\n'
                ),
            }
            [reason] = self.reasons(files, "")
            self.assertIn("crates/session/shell/src/lib.rs::Old is marked #[deprecated]", reason)
        with self.subTest(phase="a claimed adapter that is gone is stale"):
            files = {"apps/a/src/lib.rs": "fn old_hook() {}\n"}
            [reason] = self.reasons(files, OLD_ENTRY.format(count=1, adapters=ADAPTER))
            self.assertIn("no longer marked or no longer exists", reason)

    def test_malformed_ledger_entries_are_reported(self):
        base = OLD_ENTRY.format(count=1, adapters="[]")
        cases = {
            "missing": ('[[api]]\nname = "x"\npattern = "a"\n', "missing owner"),
            "duplicate": (base + base, "duplicate"),
            "regex": (base.replace("fn\\s+old_hook\\s*\\(", "("), "invalid pattern"),
            "needles": (base.replace('["old_hook"]', "[]"), "needles"),
            "count": (base.replace("count = 1", "count = -1"), "non-negative"),
        }
        for phase, (ledger, expected) in cases.items():
            with self.subTest(phase=phase):
                root, ledger_path = self.fixture({"apps/a/src/lib.rs": "fn old_hook() {}\n"}, ledger)
                _, problems = contract.load_ledger(ledger_path)
                self.assertTrue(any(expected in str(p) for p in problems), problems)

    def test_checked_in_ledger_is_well_formed(self):
        entries, problems = contract.load_ledger(contract.LEDGER)
        self.assertEqual(problems, [])
        self.assertTrue(entries)
        for entry in entries:
            self.assertGreater(entry["count"], 0, entry["name"])
            for needle in entry["needles"]:
                self.assertIn(needle, entry["pattern"].replace("\\s*", "").replace("\\", ""), entry["name"])


if __name__ == "__main__":
    unittest.main()
