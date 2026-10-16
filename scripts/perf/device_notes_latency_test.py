"""The device capture measures in a Notes notebook of its own and refuses unverified input.

`FakeTiloVm` answers `tilo-vm` the way a person's device does: a workspace
of theirs with their Notes notebook open, a `workspace.create` that mints a new
room, and an application launch that opens Notes in the active workspace. The
real `Device` drives it, so these tests cover the exact commands the script
sends.
"""
import argparse
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from device_notes_latency import Device, capture

USER_ROOM = "0f1e2d3c-4b5a-4968-8776-655443322110"
SCRATCH_ROOM = "6b70142a-2ef8-4c3c-8e2c-a385611ab642"
OPERATIONS = ("new-page", "read", "inspect-editor")


def notes_rows(prefix, room, record, pages=()):
    """A notebook's rows: its operations and each page's Open."""
    return [dict(provider="app-actions", id=f"{prefix}-{operation}", application="so.tilo.notes",
                 room=room, record=record, operation=operation, resource="notebook")
            for operation in OPERATIONS] + [
        dict(provider="app-actions", id=f"{prefix}-open", application="so.tilo.notes",
             room=room, record=record, operation="open", resource=page) for page in pages]


def workspace(id, name, room, surfaces):
    return {"id": id, "name": name, "room": room, "strips": [{"window_groups": [{"tabs": [
        {"tile": {"Foreign": {"app_id": "so.tilo.notes", "surface_id": surface}}}
        for surface in surfaces]}]}]}


class FakeTiloVm:
    """`tilo-vm` on a device whose person has Notes open in their own workspace."""

    def __init__(self, wrong_focus=False, wrong_page=False, launch_lands_home=False,
                 two_records=False):
        self.wrong_focus = wrong_focus
        self.wrong_page = wrong_page
        self.launch_lands_home = launch_lands_home
        self.two_records = two_records
        self.workspaces = [workspace(1, "Home", USER_ROOM, [7])]
        self.active = 1
        self.keyboard = 7
        # The person's notebook, open, with a page of theirs.
        self.rows = notes_rows("user", USER_ROOM, "package-primary", ["notes.page:user"])
        self.pages = {}
        self.page = None
        self.inputs = 0
        self.invoked = []
        self.jobs = {}

    def __call__(self, arguments):
        if arguments[:1] == ["input"]:
            self.inputs += 1
            return "{}"
        if arguments[:2] == ["slot", "exec"]:
            command = arguments[arguments.index("--") + 1:]
            return "/owned/session/latest.log\n" if len(command) == 3 else ""
        assert arguments[:3] == ["control", "--slot", "device"], arguments
        return json.dumps(self.control(arguments[3:]))

    def control(self, arguments):
        if arguments == ["state-dump"]:
            return {"chrome": {"session_locked": False, "seat_focus": {
                        "keyboard": {"app_id": "so.tilo.terminal" if self.wrong_focus
                                     else "so.tilo.notes", "root_surface": self.keyboard},
                        "routed_keyboard_surface": self.keyboard}},
                    "workspace_tree": {"workspaces": self.workspaces, "active": self.active}}
        if arguments[:2] == ["actions", "list"]:
            return {"actions": self.rows}
        if arguments[:2] == ["actions", "status"]:
            return {"receipt": {"update": {"state": "succeeded",
                                           "output": {"value": self.jobs[int(arguments[2])]}}}}
        assert arguments[:2] == ["actions", "invoke"], arguments
        provider, action, payload = arguments[2], arguments[3], json.loads(arguments[5])
        self.invoked.append((provider, action, payload))
        if (provider, action) == ("actions", "workspace.create"):
            self.workspaces.append(workspace(2, payload["name"], SCRATCH_ROOM, []))
            self.active = 2
            return {"receipt": {"update": {"state": "succeeded"}}}
        if (provider, action) == ("applications", "so.tilo.notes"):
            room = USER_ROOM if self.launch_lands_home else SCRATCH_ROOM
            target = next(ws for ws in self.workspaces if ws["room"] == room)
            target["strips"][0]["window_groups"][0]["tabs"].append(
                {"tile": {"Foreign": {"app_id": "so.tilo.notes", "surface_id": 42}}})
            self.keyboard = 42
            if not self.launch_lands_home:
                self.rows = self.rows + notes_rows("scratch", SCRATCH_ROOM, "package-primary")
            if self.two_records:
                self.rows = self.rows + notes_rows("other", SCRATCH_ROOM, "derived:other")
            return {"receipt": {"update": {"state": "succeeded"}}}
        job = len(self.jobs) + 1
        self.jobs[job] = self.answer(action, payload)
        return {"receipt": {"broker_invocation_id": job, "update": {"state": "accepted"}}}

    def answer(self, action, payload):
        if action == "scratch-new-page":
            self.page = "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d"
            self.pages["notes.page:" + self.page] = payload["value"]
            self.rows = self.rows + notes_rows("scratch", SCRATCH_ROOM, "package-primary",
                                               ["notes.page:" + self.page])[-1:]
            return None
        if action == "scratch-read" and "item" in payload:
            return {"title": self.pages[payload["item"]], "text": ""}
        if action == "scratch-read":
            return {"pages": [{"item": item, "title": title} for item, title in self.pages.items()]}
        if action == "scratch-inspect-editor":
            count = self.inputs
            return {
                "app": "so.tilo.notes", "selected_page": self.page,
                "main_page": "protected-existing-page" if self.wrong_page else self.page,
                "editor_binding_page": self.page, "editor_focused": True,
                "adopted": True, "pending_page_events": 0,
                "body_chars": count, "body_bytes": count, "rendered_body_chars": count,
                "hash_algorithm": "blake3", "body_hash": "0" * 64,
                "rendered_body_hash": "0" * 64, "caret": {"byte_offset": count},
            }
        return None

    def notes_actions(self):
        return [action for provider, action, _ in self.invoked if provider == "app-actions"]


class DeviceCaptureSafety(unittest.TestCase):
    def run_capture(self, tilo_vm, directory):
        options = argparse.Namespace(device="test", slot="device", tool="tilo-vm", samples=1,
                                     out=Path(directory))
        device = Device(options)
        device.run = tilo_vm
        with patch("device_notes_latency.time.sleep"), patch("scratch_notes.time.sleep"):
            return capture(device, options)

    def assert_user_notebook_untouched(self, tilo_vm):
        self.assertFalse([action for action in tilo_vm.notes_actions()
                          if not action.startswith("scratch-")])

    def test_the_capture_measures_in_its_own_room_never_the_users_notebook(self):
        tilo_vm = FakeTiloVm()
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "No physical Notes input"):
                self.run_capture(tilo_vm, directory)
            capture_record = json.loads((Path(directory) / "capture.json").read_text())
        # Its own workspace first, then Notes in it, then only that notebook.
        self.assertEqual(tilo_vm.invoked[0][:2], ("actions", "workspace.create"))
        self.assertTrue(tilo_vm.invoked[0][2]["name"].startswith("Notes latency scratch "))
        self.assertEqual(tilo_vm.invoked[1][:2], ("applications", "so.tilo.notes"))
        self.assertIn("scratch-new-page", tilo_vm.notes_actions())
        self.assert_user_notebook_untouched(tilo_vm)
        self.assertEqual(tilo_vm.inputs, 1)
        self.assertEqual(capture_record["scratch_room"], SCRATCH_ROOM)
        self.assertNotEqual(capture_record["scratch_room"], USER_ROOM)

    def test_notes_that_opened_in_the_users_workspace_is_refused_before_any_action(self):
        tilo_vm = FakeTiloVm(launch_lands_home=True)
        clock = iter(range(0, 1000))
        with tempfile.TemporaryDirectory() as directory, \
                patch("scratch_notes.time.monotonic", lambda: next(clock)):
            with self.assertRaisesRegex(RuntimeError, "never opened in the scratch room"):
                self.run_capture(tilo_vm, directory)
        self.assertEqual(tilo_vm.notes_actions(), [])
        self.assertEqual(tilo_vm.inputs, 0)

    def test_two_notebooks_in_the_scratch_room_are_refused(self):
        tilo_vm = FakeTiloVm(two_records=True)
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(RuntimeError, "more than one record"):
                self.run_capture(tilo_vm, directory)
        self.assertEqual(tilo_vm.notes_actions(), [])

    def test_no_notes_action_is_addressed_before_a_scratch_room_is_proven(self):
        device = Device(argparse.Namespace(device="test", slot="device", tool="tilo-vm"))
        with patch.object(device, "run") as run:
            with self.assertRaisesRegex(RuntimeError, "No scratch Notes room"):
                device.action("new-page", {"value": "scratch"})
            with self.assertRaisesRegex(RuntimeError, "No scratch Notes room"):
                device.read()
            run.assert_not_called()

    def test_wrong_compositor_keyboard_route_never_types(self):
        tilo_vm = FakeTiloVm(wrong_focus=True)
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(RuntimeError, "keyboard is not routed"):
                self.run_capture(tilo_vm, directory)
        self.assertEqual(tilo_vm.inputs, 0)
        self.assert_user_notebook_untouched(tilo_vm)

    def test_different_main_page_never_types(self):
        tilo_vm = FakeTiloVm(wrong_page=True)
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(RuntimeError, "not bound"):
                self.run_capture(tilo_vm, directory)
        self.assertEqual(tilo_vm.inputs, 0)


if __name__ == "__main__":
    unittest.main()
