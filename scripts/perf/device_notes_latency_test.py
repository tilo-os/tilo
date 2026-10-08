"""The device capture must refuse physical input to an unverified editor."""
import argparse
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from device_notes_latency import capture


class DeviceFixture:
    def __init__(self, wrong_focus=False, wrong_page=False):
        self.title = None
        self.inputs = 0
        self.page = "6b70142a-2ef8-4c3c-8e2c-a385611ab642"
        self.wrong_focus = wrong_focus
        self.wrong_page = wrong_page

    def control(self, arguments):
        if arguments == ["state-dump"]:
            return {
                "chrome": {"session_locked": False, "seat_focus": {
                    "keyboard": {"app_id": "so.tilo.terminal" if self.wrong_focus
                                 else "so.tilo.notes", "root_surface": 42},
                    "routed_keyboard_surface": 42}},
                "workspace_tree": {"workspaces": [{"strips": [{"window_groups": [{
                    "tabs": [{"tile": {"Foreign": {
                        "app_id": "so.tilo.notes", "surface_id": 42}}}]}]}]}]},
            }
        if len(arguments) == 2:
            return {"pages": [] if self.title is None else [{
                "item": "notes.page:" + self.page, "title": self.title}]}
        return {"title": self.title, "text": ""}

    def action(self, operation, payload, resource=None):
        if operation == "new-page":
            self.title = payload["value"]
        if operation == "inspect-editor":
            return {
                "app": "so.tilo.notes", "selected_page": self.page,
                "main_page": "protected-existing-page" if self.wrong_page else self.page,
                "editor_binding_page": self.page, "editor_focused": True,
                "adopted": True, "pending_page_events": 0,
                "body_chars": self.inputs, "body_bytes": self.inputs,
                "rendered_body_chars": self.inputs,
                "hash_algorithm": "blake3", "body_hash": "0" * 64,
                "rendered_body_hash": "0" * 64,
                "caret": {"byte_offset": self.inputs},
            }
        return None

    def execute(self, arguments):
        if arguments[0].endswith("python3"):
            return "/owned/session/latest.log\n" if len(arguments) == 3 else ""
        if arguments[0].endswith("wc"):
            return "0 /owned/session/latest.log\n"
        return ""

    def run(self, arguments):
        self.inputs += 1
        return "{}"


class DeviceCaptureSafety(unittest.TestCase):
    def run_capture(self, device, directory):
        options = argparse.Namespace(device="test", slot="device", samples=1,
                                     out=Path(directory))
        with patch("device_notes_latency.time.sleep"):
            return capture(device, options)

    def test_wrong_compositor_keyboard_route_never_types(self):
        device = DeviceFixture(wrong_focus=True)
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(RuntimeError, "keyboard is not routed"):
                self.run_capture(device, directory)
            self.assertEqual(device.inputs, 0)

    def test_different_main_page_never_types(self):
        device = DeviceFixture(wrong_page=True)
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(RuntimeError, "not bound"):
                self.run_capture(device, directory)
            self.assertEqual(device.inputs, 0)

    def test_adopted_input_without_presentation_is_a_failed_capture(self):
        device = DeviceFixture()
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "No physical Notes input"):
                self.run_capture(device, directory)
            self.assertEqual(device.inputs, 1)
            self.assertTrue((Path(directory) / "capture.json").is_file())


if __name__ == "__main__":
    unittest.main()
