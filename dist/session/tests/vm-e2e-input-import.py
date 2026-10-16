#!/usr/bin/env python3
"""The host-desktop VM test's GNOME input import check fails when the import is broken."""
import importlib.util
import json
from pathlib import Path
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "vm-e2e/run.py"
spec = importlib.util.spec_from_file_location("vm_e2e_input", SCRIPT)
harness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(harness)

IMPORTED = {"input": {"keyboard_layout": "de:nodeadkeys", "keyboard_layouts": ["de:nodeadkeys"],
                      "compose_key": "ralt", "host_input_migration_version": 4,
                      "key_repeat_delay_ms": 500}}


def problems(doc) -> list[str]:
    return harness.input_import_problems(doc if isinstance(doc, str) else json.dumps(doc))[1]


def with_input(**changes):
    section = {**IMPORTED["input"], **changes}
    return {"input": {k: v for k, v in section.items() if v is not None}}


class InputImportTests(unittest.TestCase):
    def test_the_imported_german_layout_and_compose_key_pass(self):
        self.assertEqual(problems(IMPORTED), [])

    def test_the_guest_seeds_what_the_check_expects(self):
        guest = (SCRIPT.parent / "guest.sh").read_text()
        self.assertIn("gnome_sources=\"[('xkb', 'de+nodeadkeys')]\"", guest)
        self.assertIn("gnome_xkb_options=\"['compose:ralt']\"", guest)

    def test_a_wrong_layout_fails(self):
        found = problems(with_input(keyboard_layout="fr", keyboard_layouts=["fr"]))
        self.assertEqual(len(found), 2)
        self.assertIn("input.keyboard_layout is 'fr', want 'de:nodeadkeys'", found)

    def test_the_us_default_left_in_place_fails(self):
        self.assertTrue(problems(with_input(keyboard_layout="us", keyboard_layouts=["us"])))

    def test_a_missing_compose_key_fails(self):
        self.assertEqual(problems(with_input(compose_key=None)),
                         ["input.compose_key is None, want 'ralt'"])

    def test_an_import_that_never_completed_fails(self):
        found = problems(with_input(host_input_migration_version=None))
        self.assertEqual(len(found), 1)
        self.assertIn("did not complete", found[0])

    def test_no_input_section_no_file_or_garbage_fails(self):
        for doc in ({"appearance": {"mode": "dark"}}, "", "receipt: x\n{", "[]"):
            with self.subTest(doc=doc):
                self.assertTrue(problems(doc))


class TypingLayoutTests(unittest.TestCase):
    def test_the_password_and_app_names_on_the_german_layout(self):
        keys = [k for press in harness.key_presses(harness.GUEST_PASSWORD, "de") for k in press]
        self.assertEqual(keys.count("slash"), 2)
        self.assertNotIn("minus", keys)
        self.assertEqual(harness.key_presses("Terminal", "de")[0], ("shift", "t"))

    def test_y_and_z_swap_on_the_german_layout(self):
        self.assertEqual(harness.key_presses("yzY", "de"), [("z",), ("y",), ("shift", "z")])
        self.assertEqual(harness.key_presses("yzY", "us"), [("y",), ("z",), ("shift", "y")])

    def test_the_layout_follows_the_session_settings(self):
        self.assertEqual(harness.typing_layout(IMPORTED), "de")
        self.assertEqual(harness.typing_layout({"input": {"keyboard_layout": "us"}}), "us")
        self.assertEqual(harness.typing_layout({}), "us")


if __name__ == "__main__":
    unittest.main()
