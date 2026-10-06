import importlib.util
import inspect
import pathlib
import sys
import tempfile
import unittest


REPO = pathlib.Path(__file__).resolve().parents[2]
sys.path[:0] = [str(REPO / "scripts/headless-examples"), str(REPO / "tools")]
SPEC = importlib.util.spec_from_file_location(
    "packaged_agent_lifecycle",
    REPO / "scripts/headless-examples/packaged-agent-lifecycle.py",
)
PROOF = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROOF)


class PresentedEditorTargetTests(unittest.TestCase):
    def test_float_return_uses_only_physical_editor_and_surface_input(self):
        source = inspect.getsource(PROOF.native_float_return)
        for forbidden in ("SetText", "AddSelection", "DoAction", "GrabFocus"):
            self.assertNotIn(forbidden, source)
        for required in (
            '"op": "type"',
            '"op": "key"',
            '"op": "drag"',
            '"op": "click_at"',
            "physical_chord",
        ):
            self.assertIn(required, source)

    def test_presented_content_geometry_accepts_only_authoritative_finite_fields(self):
        expected = {
            "rect": {"x": 700.0, "y": 100.0, "w": 600.0, "h": 500.0},
            "content_rect": {"x": 720.0, "y": 110.0, "w": 560.0, "h": 470.0},
            "content_origin": {"x": 710.0, "y": 100.0},
            "keyboard_focused": False,
        }
        self.assertEqual(
            PROOF.presented_content_geometry(expected),
            {
                "placement": expected["rect"],
                "content": expected["content_rect"],
                "origin": expected["content_origin"],
            },
        )

        for rect in [
            [700.0, 100.0, 600.0, 500.0],
            {"x": 700.0, "y": 100.0, "w": 600.0},
            {"x": 700.0, "y": 100.0, "w": 0.0, "h": 500.0},
            {"x": float("nan"), "y": 100.0, "w": 600.0, "h": 500.0},
        ]:
            with self.subTest(rect=rect), self.assertRaises(RuntimeError):
                PROOF.presented_content_geometry({**expected, "content_rect": rect})
        with self.assertRaises(RuntimeError):
            PROOF.presented_content_geometry({**expected, "keyboard_focused": None})

    def test_editor_window_extent_is_translated_through_exact_presented_frame(self):
        sent = []

        class Client:
            def __init__(self, *_args):
                pass

            def call(self, *_args):
                return "((10, 20, 100, 40),)"

        original_client = PROOF.atspi.AtspiClient
        original_address = PROOF.atspi.atspi_address
        original_call = PROOF.call
        original_require_ok = PROOF.require_ok
        try:
            PROOF.atspi.AtspiClient = Client
            PROOF.atspi.atspi_address = lambda: "private"
            PROOF.call = lambda socket, payload: sent.append((socket, payload)) or {"ok": True}
            PROOF.require_ok = lambda response: response
            PROOF.physically_click_editor(
                "socket",
                {"bus": "bus", "composer": "path"},
                {
                    "placement": {"x": 700.0, "y": 100.0, "w": 600.0, "h": 500.0},
                    "content": {"x": 720.0, "y": 110.0, "w": 560.0, "h": 470.0},
                    "origin": {"x": 710.0, "y": 100.0},
                },
            )
            Client.call = lambda _self, *_args: "((590, 20, 20, 40),)"
            with self.assertRaises(RuntimeError):
                PROOF.physically_click_editor(
                    "socket",
                    {"bus": "bus", "composer": "path"},
                    {
                        "placement": {"x": 700.0, "y": 100.0, "w": 600.0, "h": 500.0},
                        "content": {"x": 720.0, "y": 110.0, "w": 560.0, "h": 470.0},
                        "origin": {"x": 710.0, "y": 100.0},
                    },
                )
        finally:
            PROOF.atspi.AtspiClient = original_client
            PROOF.atspi.atspi_address = original_address
            PROOF.call = original_call
            PROOF.require_ok = original_require_ok

        self.assertEqual(
            sent,
            [("socket", {"op": "click_at", "x": 770.0, "y": 140.0})],
        )

    def test_unfocused_chat_is_captured_and_refused_before_any_key(self):
        calls = []
        frame = {
            "app_id": "so.tilo.chat",
            "surface_id": 7,
            "frame_seq": 1,
            "last_error": None,
            "presented": True,
            "rect": {"x": 0, "y": 0, "w": 800, "h": 600},
            "content_rect": {"x": 0, "y": 0, "w": 800, "h": 600},
            "content_origin": {"x": 0, "y": 0},
            "keyboard_focused": False,
        }
        original_call = PROOF.call
        original_require_ok = PROOF.require_ok
        with tempfile.TemporaryDirectory() as directory:
            try:
                PROOF.call = lambda _socket, payload: calls.append(payload) or (
                    {"answer": {"frames": [frame]}}
                    if payload["op"] == "foreign_frames" else {"ok": True}
                )
                PROOF.require_ok = lambda response: response
                with self.assertRaises(RuntimeError):
                    PROOF.require_chat_keyboard_focus_after_click(
                        "socket", pathlib.Path(directory)
                    )
            finally:
                PROOF.call = original_call
                PROOF.require_ok = original_require_ok

        self.assertEqual([row["op"] for row in calls], ["foreign_frames", "snapshot"])

    def test_observer_enabled_row_must_belong_to_chat(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            (output / "shell.log").write_text(
                '[app so.tilo.store stderr] tilo-physical-input-observer '
                '{"app_id":"so.tilo.chat","enabled":true}\n'
                'prefix [app so.tilo.chat stderr] tilo-physical-input-observer '
                '{"app_id":"so.tilo.chat","enabled":true}\n'
                '[app so.tilo.chat stderr] tilo-physical-input-observer '
                '{"app_id":"so.tilo.chat","enabled":false}\n'
            )
            self.assertIs(PROOF.chat_input_observer_enabled(output), False)

    def test_shell_bound_chat_observer_accepts_missing_window_app_id(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            (output / "shell.log").write_text(
                'tilo-physical-input-observer {"app_id":null,"enabled":false}\n'
                '[app so.tilo.store stderr] tilo-physical-input-observer '
                '{"app_id":null,"enabled":false}\n'
                '[app so.tilo.chat stderr] tilo-physical-input-observer '
                '{"app_id":null,"enabled":true}\n'
            )
            self.assertIs(PROOF.chat_input_observer_enabled(output), True)

    def test_task_phase_requires_one_exact_public_status_label(self):
        class Client:
            def __init__(self, *_args):
                pass

            def named(self, tree, label):
                return [path for path, name in tree.items() if name == label]

        original_client = PROOF.atspi.AtspiClient
        original_address = PROOF.atspi.atspi_address
        original_subtree = PROOF.accessible_subtree
        try:
            PROOF.atspi.AtspiClient = Client
            PROOF.atspi.atspi_address = lambda: "private"
            PROOF.accessible_subtree = lambda *_args: {
                "/status": "Planning: Lifecycle task",
                "/other": "Planning another task",
            }
            access = {"bus": "bus", "window": "/window"}
            self.assertTrue(PROOF.task_phase_accessibility(
                access, "Planning: Lifecycle task"
            ))
            self.assertFalse(PROOF.task_phase_accessibility(
                access, "Planning another"
            ))
        finally:
            PROOF.atspi.AtspiClient = original_client
            PROOF.atspi.atspi_address = original_address
            PROOF.accessible_subtree = original_subtree


if __name__ == "__main__":
    unittest.main()
