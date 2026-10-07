"""The device capture must refuse physical input to an unverified editor."""
import argparse
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from device_notes_latency import capture, require_scratch, observed_capture, diagnostic_options


class DeviceFixture:
    def __init__(self, wrong_focus=False, wrong_page=False, after_input=None):
        self.title = None
        self.inputs = 0
        self.page = "6b70142a-2ef8-4c3c-8e2c-a385611ab642"
        self.wrong_focus = wrong_focus
        self.wrong_page = wrong_page
        self.after_input = after_input or {}

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
            state = {
                "app": "so.tilo.notes", "selected_page": self.page,
                "bound_page": self.page,
                "main_page": "protected-existing-page" if self.wrong_page else self.page,
                "editor_binding_page": self.page, "editor_focused": True,
                "adopted": True, "pending_page_events": 0,
                "body_chars": self.inputs, "body_bytes": self.inputs,
                "rendered_body_chars": self.inputs,
                "hash_algorithm": "blake3", "body_hash": "0" * 64,
                "rendered_body_hash": "0" * 64,
                "caret": {"byte_offset": self.inputs},
                "active_line_present": True, "active_line_focused": True,
                "editor_route_available": True, "active_line_disabled": False,
                "editor_read_only": False, "editor_composing": False,
                "sidebar_renaming": False, "sidebar_menu_open": False,
                "editor_menu_open": False, "sidebar_drawer_open": False,
            }
            if self.inputs:
                state.update(self.after_input)
            return state
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
    def run_capture(self, device, directory, samples=1):
        options = argparse.Namespace(device="test", slot="device", samples=samples,
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

    def test_actual_child_route_fields_are_required_before_physical_input(self):
        device = DeviceFixture()
        state = device.action("inspect-editor", {})
        require_scratch(state, device.page, 0)
        expected = {
            "active_line_present": True, "active_line_focused": True,
            "editor_route_available": True, "active_line_disabled": False,
            "editor_read_only": False, "editor_composing": False,
            "sidebar_renaming": False, "sidebar_menu_open": False,
            "editor_menu_open": False, "sidebar_drawer_open": False,
        }
        for key, value in expected.items():
            for replacement in (None, not value, int(value), "true" if value else "false"):
                with self.subTest(field=key, replacement=replacement):
                    unsafe = dict(state, **{key: replacement})
                    with self.assertRaisesRegex(RuntimeError, "active editor route"):
                        require_scratch(unsafe, device.page, 0)
            absent = dict(state)
            del absent[key]
            with self.subTest(field=key, absent=True):
                with self.assertRaisesRegex(RuntimeError, "active editor route"):
                    require_scratch(absent, device.page, 0)

    def test_stale_bound_page_refuses_even_when_other_bindings_match(self):
        device = DeviceFixture()
        state = device.action("inspect-editor", {})
        state["bound_page"] = "different-page"
        with self.assertRaisesRegex(RuntimeError, "not bound"):
            require_scratch(state, device.page, 0)

    def test_unavailable_active_line_never_reaches_kernel_input(self):
        for field, value in (("active_line_disabled", True), ("active_line_focused", False),
                             ("editor_composing", True), ("editor_route_available", None)):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                device = DeviceFixture()
                action = device.action
                def unavailable(operation, payload, resource=None):
                    state = action(operation, payload, resource)
                    if operation == "inspect-editor":
                        state[field] = value
                    return state
                device.action = unavailable
                with self.assertRaisesRegex(RuntimeError, "active editor route"):
                    self.run_capture(device, directory, samples=3)
                self.assertEqual(device.inputs, 0)

    def test_live_observation_restores_all_options_after_failed_first_key(self):
        device = DeviceFixture(after_input={"body_chars": 0})
        original = dict(presentation=False, gpu_stages=False,
                        gpu_timestamps=True, gpu_dispatch_timestamps=False)
        current = dict(original)
        transitions = []
        control = device.control
        def diagnostics(arguments):
            if arguments[:3] == ["actions", "invoke", "session-diagnostics"]:
                if arguments[3].startswith("configure"):
                    current.update(json.loads(arguments[-1]))
                    transitions.append(dict(current))
                return {"diagnostics": dict(current)}
            return control(arguments)
        device.control = diagnostics
        with tempfile.TemporaryDirectory() as directory:
            options = argparse.Namespace(device="test", slot="device", samples=100,
                                         out=Path(directory), manage_diagnostics=True)
            with patch("device_notes_latency.time.sleep"):
                with self.assertRaises(RuntimeError):
                    observed_capture(device, options)
            self.assertEqual(device.inputs, 1)
            self.assertEqual(transitions, [dict(original, gpu_stages=True),
                             dict(original, presentation=True, gpu_stages=True), original])
            self.assertEqual(current, original)
            self.assertTrue((Path(directory) / "diagnostics-restored.json").exists())

    def test_observation_restores_after_cancellation_without_input(self):
        original = dict(presentation=True, gpu_stages=False,
                        gpu_timestamps=False, gpu_dispatch_timestamps=False)
        transitions = []
        class CancelledDevice:
            def control(self, arguments):
                if arguments[0] == "state-dump":
                    raise KeyboardInterrupt()
                if arguments[3].startswith("configure"):
                    transitions.append(json.loads(arguments[-1]))
                return {"diagnostics": original}
        with tempfile.TemporaryDirectory() as directory:
            options = argparse.Namespace(out=Path(directory), manage_diagnostics=True)
            with self.assertRaises(KeyboardInterrupt):
                observed_capture(CancelledDevice(), options)
            self.assertEqual(transitions, [original])
            self.assertTrue((Path(directory) / "diagnostics-restored.json").exists())

    def test_unverified_diagnostics_never_change_options_or_type(self):
        for field in ("presentation", "gpu_stages", "gpu_timestamps", "gpu_dispatch_timestamps"):
            options = dict(presentation=False, gpu_stages=False,
                           gpu_timestamps=False, gpu_dispatch_timestamps=False)
            options[field] = 0
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                diagnostic_options({"diagnostics": options})

    def test_failed_first_insertion_never_sends_a_second_key(self):
        refusals = [
            {"body_chars": 0, "body_bytes": 0, "rendered_body_chars": 0,
             "caret": {"byte_offset": 0}},
            {"rendered_body_chars": 0},
            {"caret": {"byte_offset": 0}},
            {"rendered_body_hash": "1" * 64},
            {"body_chars": 2, "body_bytes": 2, "rendered_body_chars": 2,
             "caret": {"byte_offset": 2}},
            {"active_line_focused": False},
        ]
        for state in refusals:
            with self.subTest(state=state), tempfile.TemporaryDirectory() as directory:
                device = DeviceFixture(after_input=state)
                with self.assertRaises(RuntimeError):
                    self.run_capture(device, directory, samples=3)
                self.assertEqual(device.inputs, 1)
                report = json.loads((Path(directory) / "capture.json").read_text())
                self.assertEqual(report["samples_adopted"], 0)
                self.assertEqual(report["samples_requested"], 3)


if __name__ == "__main__":
    unittest.main()
