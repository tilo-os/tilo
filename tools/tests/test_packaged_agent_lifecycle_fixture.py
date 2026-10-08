import importlib.util
import inspect
import io
import pathlib
import sys
import tempfile
import unittest
import threading
import concurrent.futures
import json
import urllib.request
import urllib.error
from unittest.mock import patch


REPO = pathlib.Path(__file__).resolve().parents[2]
sys.path[:0] = [str(REPO / "scripts/headless-examples"), str(REPO / "tools")]
SPEC = importlib.util.spec_from_file_location(
    "packaged_agent_lifecycle",
    REPO / "scripts/headless-examples/packaged-agent-lifecycle.py",
)
PROOF = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROOF)


class PresentedEditorTargetTests(unittest.TestCase):
    @staticmethod
    def permission_action():
        return {"provider": "broker:1", "id": "offered-message", "application": "so.tilo.chat",
                "room": "room", "record": "", "resource": "agent-resource", "operation": "message"}

    def test_hidden_offer_observation_requires_current_independent_read_evidence(self):
        original = self.permission_action()
        opposite = {**original, "provider": "broker:2", "id": "offered-queue",
                    "application": "so.tilo.desktop", "record": "tasks",
                    "resource": "desktop:tasks", "operation": "queue"}
        read = {**original, "id": "offered-open", "operation": "open-agent-conversation"}
        access = {"bus": "bus", "process": 7, "window": "/window"}
        with patch.object(PROOF, "actions", return_value=[{}, opposite, read]), \
                patch.object(PROOF, "chat_process_identity", return_value=(7, "start")), \
                patch.object(PROOF.time, "monotonic", return_value=100):
            observed = PROOF.observe_hidden_permission_offer(
                "socket", original, opposite, "open-agent-conversation",
                access, access, (7, "start"),
            )
        self.assertEqual(observed.original, PROOF.permission_offer_identity(original))
        self.assertEqual(observed.observed_at, 100)
        for catalogue in ([original, opposite, read], [read], [opposite],
                          [opposite, {**read, "room": "foreign"}],
                          [opposite, {**read, "resource": "other"}],
                          [opposite, {**read, "provider": "foreign"}],
                          [opposite, read, read]):
            with self.subTest(catalogue=catalogue), \
                    patch.object(PROOF, "actions", return_value=catalogue), \
                    patch.object(PROOF, "chat_process_identity", return_value=(7, "start")), \
                    self.assertRaises(RuntimeError):
                PROOF.observe_hidden_permission_offer(
                    "socket", original, opposite, "open-agent-conversation",
                    access, access, (7, "start"),
                )
        with patch.object(PROOF, "actions") as catalogue, self.assertRaises(RuntimeError):
            PROOF.observe_hidden_permission_offer(
                "socket", original, opposite, "open-agent-conversation",
                access, {**access, "window": "/other"}, (7, "start"),
            )
        catalogue.assert_not_called()
        with patch.object(PROOF, "actions") as catalogue, \
                patch.object(PROOF, "chat_process_identity", return_value=(7, "replaced")), \
                self.assertRaises(RuntimeError):
            PROOF.observe_hidden_permission_offer(
                "socket", original, opposite, "open-agent-conversation",
                access, access, (7, "start"),
            )
        catalogue.assert_not_called()
        private = "private-catalogue-error"
        with patch.object(PROOF, "actions", side_effect=RuntimeError(private)), \
                patch.object(PROOF, "chat_process_identity", return_value=(7, "start")), \
                self.assertRaises(RuntimeError) as raised:
            PROOF.observe_hidden_permission_offer(
                "socket", original, opposite, "open-agent-conversation",
                access, access, (7, "start"),
            )
        self.assertNotIn(private, str(raised.exception))

    def test_stale_immediate_errors_are_not_permission_refusals(self):
        action = self.permission_action()
        observed = PROOF.HiddenOfferObservation(PROOF.permission_offer_identity(action), 100)
        private = "private-action-error"
        for error in ({"code": "unavailable", "message": private},
                      {"code": "invalid_arguments", "message": private},
                      {"code": "action_failed", "message": private},
                      {"code": "permission_denied", "message": private},
                      {"code": "other", "message": private}, private, None):
            with self.subTest(error=error), patch.object(PROOF.time, "monotonic", return_value=100), \
                    patch.object(PROOF, "call", return_value={"err": error}), \
                    self.assertRaises(RuntimeError) as raised:
                PROOF.require_stale_action_refused(
                    "socket", action, {"value": private}, None, observation=observed
                )
            self.assertNotIn(private, str(raised.exception))

    def test_stale_unknown_protocol_shapes_are_not_refusals(self):
        action = self.permission_action()
        observed = PROOF.HiddenOfferObservation(PROOF.permission_offer_identity(action), 100)
        for response in (None, [], "private-response", {}, {"answer": []},
                         {"err": {"code": "unknown_action"}, "answer": {"receipt": {}}}):
            with self.subTest(response=response), \
                    patch.object(PROOF.time, "monotonic", return_value=100), \
                    patch.object(PROOF, "call", return_value=response), \
                    self.assertRaises(RuntimeError) as raised:
                PROOF.require_stale_action_refused(
                    "socket", action, {}, None, observation=observed,
                )
            self.assertNotIn("private-response", str(raised.exception))

    def test_unknown_action_requires_exact_fresh_hidden_offer_context(self):
        action = self.permission_action()
        identity = PROOF.permission_offer_identity(action)
        for observed in (None, PROOF.HiddenOfferObservation(("foreign",), 100),
                         PROOF.HiddenOfferObservation(identity, 97),
                         PROOF.HiddenOfferObservation(identity, 101)):
            with self.subTest(observed=observed), \
                    patch.object(PROOF.time, "monotonic", return_value=100), \
                    patch.object(PROOF, "call") as invoke, self.assertRaises(RuntimeError):
                PROOF.require_stale_action_refused(
                    "socket", action, {}, None, observation=observed
                )
            invoke.assert_not_called()
        with patch.object(PROOF.time, "monotonic", return_value=100), \
                patch.object(PROOF, "call", return_value={"err": {"code": "unknown_action"}}):
            self.assertEqual(PROOF.require_stale_action_refused(
                "socket", action, {}, None,
                observation=PROOF.HiddenOfferObservation(identity, 100),
            ), "hidden_action_refused")

    def test_permission_selection_waits_without_replaying_physical_keys(self):
        access = {"bus": "bus"}
        output = pathlib.Path("/proof")
        selected = {"selection": [2, 6], "caret": 2, "draft_matches": True}
        with patch.object(PROOF, "wait_for", side_effect=[{}, True]), \
                patch.object(PROOF, "presented_content_geometry", return_value={}), \
                patch.object(PROOF, "physically_click_editor"), \
                patch.object(PROOF, "require_chat_keyboard_focus_after_click"), \
                patch.object(PROOF, "physical_chord"), \
                patch.object(PROOF, "set_physical_modifiers") as modifiers, \
                patch.object(PROOF, "call", return_value={"ok": {}}) as input_call, \
                patch.object(PROOF, "wait_for_private_selection", return_value=selected) as observe:
            self.assertEqual(PROOF.prepare_private_permission_draft(
                "socket", access, "abcdef", 4, output, None,
            ), selected)
        self.assertEqual(sum(row.args[1] == {"op": "key", "key": "ArrowLeft"}
                             for row in input_call.call_args_list), 4)
        observe.assert_called_once_with("socket", access, "abcdef", [2, 6], None, output)
        self.assertEqual(modifiers.call_args_list[-1].kwargs, {})

    def test_native_person_inspector_is_private_and_fail_closed(self):
        class Result:
            returncode = 0
            stdout = json.dumps({"person": "a" * 64})
            stderr = ""

        with tempfile.TemporaryDirectory() as directory, patch.object(
                PROOF.subprocess, "run", return_value=Result()):
            person = PROOF.inspect_native_person(
                pathlib.Path(directory), {"TILO_STATE_PATH": "/state"}, "room"
            )
        self.assertEqual(person, "a" * 64)

        private = "private-person-and-action-payload"
        Result.stdout = private
        with tempfile.TemporaryDirectory() as directory, patch.object(
                PROOF.subprocess, "run", return_value=Result()), self.assertRaises(
                    RuntimeError
                ) as raised:
            PROOF.inspect_native_person(
                pathlib.Path(directory), {"TILO_STATE_PATH": "/state"}, "room"
            )
        self.assertNotIn(private, str(raised.exception))

    def test_expiring_app_restriction_uses_exact_person_and_scope(self):
        save = {"id": "save"}
        assign = {"id": "assign"}
        calls = []

        def invoke(_socket, action, arguments):
            calls.append((action, arguments))
            return {"receipt": {"action": action["id"]}}

        with patch.object(PROOF, "wait_for", side_effect=[save, assign]), \
                patch.object(PROOF, "invoke", side_effect=invoke), \
                patch.object(PROOF, "wait_terminal", side_effect=lambda _s, row, _p: row), \
                patch.object(PROOF, "terminal_invocation", side_effect=["saved", "assigned"]), \
                patch.object(PROOF.time, "time", return_value=100):
            result = PROOF.assign_expiring_app_write_restriction(
                "socket", "room", "b" * 64, "so.tilo.chat", "profile", None
            )
        self.assertEqual(calls[0][1]["rules"], [{
            "scope": {"App": "so.tilo.chat"},
            "effect": "Restrict",
            "verb": "Write",
        }])
        self.assertEqual(calls[1][1], {
            "profile": "profile", "subject_kind": "person", "subject": "b" * 64,
            "expires_unix": 112, "confirm": True,
        })
        self.assertEqual(result, {
            "save_invocation": "saved", "assign_invocation": "assigned",
            "expires_unix": 112,
        })
        self.assertNotIn("b" * 64, json.dumps(result))

    def test_permission_evidence_never_serializes_private_person(self):
        class Provider:
            def request_count(self):
                return 4

            def overflow_count(self):
                return 0

        private_person = "c" * 64
        chat = {"chat_write_restricted": True}
        task = {"task_write_restricted": True}
        order = []

        def chat_step(*_args):
            order.append("chat")
            return chat

        def task_step(*_args):
            order.append("task")
            return task

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "inspect_native_person", return_value=private_person), \
                patch.object(PROOF, "owned_agent_identity", return_value=(8, "start")), \
                patch.object(PROOF, "native_chat_write_downgrade", side_effect=chat_step), \
                patch.object(PROOF, "native_task_write_downgrade", side_effect=task_step):
            output = pathlib.Path(directory)
            evidence = PROOF.native_person_permission_downgrades(
                "socket", "room", "record", Provider(), output, "1600x900",
                type("Process", (), {"pid": 7})(), pathlib.Path("/binaries"), {},
            )
            serialized = (output / "person-permission-downgrade.json").read_text()
        self.assertTrue(evidence["typed_person_verified_in_memory"])
        self.assertNotIn(private_person, serialized)
        self.assertEqual(order, ["chat", "task"])

    def test_admitted_lifecycle_never_counts_as_hidden_permission_refusal(self):
        for state in ("accepted", "progress", "item", "failed", "cancelled", "succeeded"):
            receipt = {"broker_invocation_id": 9, "update": {
                "invocation": "original", "state": state, "output": "private-result",
            }}
            with self.subTest(state=state), \
                    patch.object(PROOF, "call", return_value={"answer": {"receipt": receipt}}) as invoke, \
                    patch.object(PROOF.time, "monotonic", return_value=100), \
                    self.assertRaises(RuntimeError) as raised:
                PROOF.require_stale_action_refused(
                    "socket", self.permission_action(), {"value": "private"}, None,
                    observation=PROOF.HiddenOfferObservation(
                        PROOF.permission_offer_identity(self.permission_action()), 100,
                    ),
                )
            self.assertIn("admitted", str(raised.exception))
            self.assertNotIn("private-result", str(raised.exception))
            invoke.assert_called_once()

    def test_component_target_retains_exact_client_and_global_geometry(self):
        class Client:
            def call(self, path, interface, coord_type):
                self.assertions = (path, interface, coord_type)
                return "((40, 20, 30, 10),)"

        client = Client()
        geometry = {
            "origin": {"x": 100.0, "y": 200.0},
            "content": {"x": 100.0, "y": 200.0, "w": 400.0, "h": 300.0},
            "placement": {"x": 90.0, "y": 190.0, "w": 420.0, "h": 320.0},
        }
        with patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF.atspi, "AtspiClient", return_value=client):
            target = PROOF.component_global_target(
                {"bus": "bus"}, "/drag", geometry, "drag handle"
            )
        self.assertEqual(client.assertions, (
            "/drag", "org.a11y.atspi.Component.GetExtents", "1"
        ))
        self.assertEqual(target, {
            "client_rect": {"x": 40, "y": 20, "w": 30, "h": 10},
            "global_center": {"x": 155.0, "y": 225.0},
        })

    def test_ime_drag_refusal_exports_only_closed_public_geometry(self):
        frame = {
            "app_id": "so.tilo.chat", "floating_id": None,
            "presented": True, "last_error": None, "frame_seq": 8,
            "rect": {"x": 10, "y": 20, "w": 500, "h": 400},
            "content_rect": {"x": 12, "y": 42, "w": 496, "h": 376},
            "content_origin": {"x": 12, "y": 42},
            "keyboard_focused": True,
            "private_model_data": "must-not-leave-the-frame",
        }
        drag = {
            "handle_client_rect": {"x": 400, "y": 8, "w": 30, "h": 10},
            "drag_from": {"x": 427, "y": 55},
            "drag_to": {"x": 2, "y": 103},
            "primary_placement": frame["rect"],
            "primary_content": frame["content_rect"],
            "primary_content_origin": frame["content_origin"],
            "primary_frame_seq": 7,
        }
        with tempfile.TemporaryDirectory() as directory, patch.object(
                PROOF, "call", side_effect=[
                    {"answer": {"frames": [frame]}}, {"ok": {}},
                ]):
            output = pathlib.Path(directory)
            PROOF.write_ime_drag_refusal("socket", output, "1600x900", drag)
            evidence = (output / "ime-drag-refused.json").read_text()
        self.assertNotIn("must-not-leave-the-frame", evidence)
        self.assertNotIn("private_model_data", evidence)
        self.assertTrue(json.loads(evidence)["primary_frame_advanced"])
        self.assertEqual(json.loads(evidence)["floating_observed_count"], 0)

    def test_ime_drag_refusal_distinguishes_unpainted_native_surface_without_exporting_errors(self):
        frame = {
            "app_id": "so.tilo.chat", "floating_id": "inert-fixture-surface",
            "presented": False, "pixels_ready": False, "frame_seq": 0,
            "last_error": "private transport details must not leave the observation",
            "suspended": True, "transferring": True,
        }
        with tempfile.TemporaryDirectory() as directory, patch.object(
                PROOF, "call", side_effect=[
                    {"answer": {"frames": [frame]}}, {"ok": {}},
                ]):
            output = pathlib.Path(directory)
            PROOF.write_ime_drag_refusal("socket", output, "1600x900", {})
            evidence = json.loads((output / "ime-drag-refused.json").read_text())
        self.assertFalse(evidence["floating_presented"])
        self.assertEqual(evidence["floating_observed_count"], 1)
        self.assertEqual(evidence["floating_pixels_ready_count"], 0)
        self.assertEqual(evidence["floating_error_count"], 1)
        self.assertEqual(evidence["floating_suspended_count"], 1)
        self.assertEqual(evidence["floating_transferring_count"], 1)
        self.assertNotIn(frame["last_error"], json.dumps(evidence))
        self.assertNotIn(frame["floating_id"], json.dumps(evidence))

    def test_broken_ime_drag_observer_cannot_replace_original_failure(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(
                PROOF, "write_ime_drag_refusal", side_effect=RuntimeError("observer failed")
        ):
            output = pathlib.Path(directory)
            PROOF.preserve_ime_drag_refusal("socket", output, "1600x900", {})
            evidence = json.loads((output / "ime-drag-refused.json").read_text())
        self.assertEqual(evidence, {
            "phase": "after_physical_drag_without_floating_presentation",
            "diagnostic_unavailable": True,
            "private_content_recorded": False,
        })

    def test_agent_control_keyboard_focus_never_invokes_the_accessibility_action(self):
        class Client:
            def __init__(self):
                self.calls = []

            def call(self, path, interface, *args, **kwargs):
                self.calls.append((path, interface, args, kwargs))
                if interface.endswith("GrabFocus"):
                    return "(true,)"
                if interface.endswith("GetActions"):
                    return "([('click', '', '')],)"
                raise AssertionError(interface)

        client = Client()
        with patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF.atspi, "AtspiClient", return_value=client):
            self.assertEqual(PROOF.focus_agent_conversation_control(
                {"bus": "bus"}, "/control", "Float"
            ), {"keyboard_focus": "Float", "accepted": True})
        self.assertEqual(
            [call[1] for call in client.calls],
            ["org.a11y.atspi.Component.GrabFocus", "org.a11y.atspi.Action.GetActions"],
        )

    def test_physical_selection_observes_delivery_without_replaying_keys(self):
        before = {"selection": None, "caret": 9, "draft_matches": True,
                  "accessibility_focused": True}
        after = dict(before, selection=[4, 9], caret=4)

        def observe(_label, predicate, _process):
            self.assertIsNone(predicate())
            return predicate()

        with patch.object(PROOF, "private_editor_observation", side_effect=[before, after]), \
                patch.object(PROOF, "wait_for", side_effect=observe), \
                patch.object(PROOF, "call") as calls:
            result = PROOF.wait_for_private_selection(
                "socket", {}, "test text", [4, 9], None, pathlib.Path("unused"))
        self.assertEqual(result, after)
        calls.assert_not_called()

    def test_physical_selection_timeout_keeps_exact_assertion_and_closed_evidence(self):
        timeout = PROOF.ProofTimeout("original selection timeout")
        observed = {"selection": [5, 9], "caret": 5, "draft_matches": True,
                    "accessibility_focused": True, "private": "secret model text"}

        def expire(_label, predicate, _process):
            self.assertIsNone(predicate())
            raise timeout

        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            with patch.object(PROOF, "private_editor_observation", return_value=observed), \
                    patch.object(PROOF, "wait_for", side_effect=expire), \
                    patch.object(PROOF, "call", return_value={"ok": True}) as calls, \
                    patch.object(PROOF, "require_ok"), \
                    self.assertRaises(PROOF.ProofTimeout) as raised:
                PROOF.wait_for_private_selection("socket", {}, "test text", [4, 9], None, output)
            self.assertIs(raised.exception, timeout)
            evidence = json.loads((output / "physical-selection-refused.json").read_text())
            self.assertEqual(evidence["expected_selection"], [4, 9])
            self.assertEqual(evidence["observed_selection"], [5, 9])
            self.assertNotIn("secret model text", json.dumps(evidence))
            calls.assert_called_once_with("socket", {"op": "snapshot", "path": str(
                output / "physical-selection-refused.png")})

    def test_terminal_timeout_diagnostic_is_closed_and_preserves_original_failure(self):
        original = "00000000-0000-0000-0000-000000000021"
        private = "private prompt result error and output"
        for identity in [original, private]:
            receipt = {"broker_invocation_id": 37, "update": {
                "invocation": identity, "state": "accepted", "result": private,
                "error": private, "text": private}, "private": private}
            timeout = PROOF.ProofTimeout("bounded observation timeout")

            def expire(_label, completed, _process):
                self.assertIsNone(completed())
                self.assertIsNone(completed())
                raise timeout

            with tempfile.TemporaryDirectory() as directory:
                path = pathlib.Path(directory) / "closed.json"
                with patch.object(PROOF, "wait_for", side_effect=expire), \
                        patch.object(PROOF, "call", return_value={"answer": {"receipt": receipt}}), \
                        self.assertRaises(PROOF.ProofTimeout) as raised:
                    PROOF.wait_terminal("socket", {"receipt": receipt}, None, diagnostic_path=path)
                self.assertIs(raised.exception, timeout)
                self.assertEqual(json.loads(path.read_text()), {
                    "original_invocation": original if identity == original else None,
                    "broker_invocation_id": 37,
                    "states_seen": {"accepted": 2},
                    "original_unchanged": True,
                })
                self.assertNotIn(private, path.read_text())

    def test_terminal_refusal_diagnostic_whitelists_codes_not_error_payloads(self):
        original = "00000000-0000-0000-0000-000000000021"
        private = "private prompt result error and output"
        receipt = {"broker_invocation_id": 37, "update": {
            "invocation": original, "state": "accepted"}}
        for code in ["unknown_action", "invalid_arguments", "action_failed", private, None]:
            with tempfile.TemporaryDirectory() as directory:
                path = pathlib.Path(directory) / "closed.json"
                with patch.object(PROOF, "wait_for", side_effect=lambda _l, fn, _p: fn()), \
                        patch.object(PROOF, "call", return_value={"err": {
                            "code": code, "message": private}}), \
                        self.assertRaisesRegex(RuntimeError, "terminal observation was refused"):
                    PROOF.wait_terminal("socket", {"receipt": receipt}, None, diagnostic_path=path)
                self.assertEqual(json.loads(path.read_text()), {
                    "original_invocation": original,
                    "broker_invocation_id": 37,
                    "states_seen": {"accepted": 1},
                    "original_unchanged": True,
                    "status_refusal": code if code in {
                        "unknown_action", "invalid_arguments", "action_failed"
                    } else "other",
                })
                self.assertNotIn(private, path.read_text())

    def test_reply_timeout_captures_fresh_scene_without_masking_failure(self):
        timeout = PROOF.ProofTimeout("original timeout")
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            with patch.object(PROOF, "wait_for", side_effect=timeout), \
                    patch.object(PROOF, "call", return_value={"ok": True}) as calls, \
                    patch.object(PROOF, "require_ok"), \
                    self.assertRaises(PROOF.ProofTimeout) as raised:
                PROOF.wait_for_agent_reply("socket", "room", "", "parent", "agent", None, output)
            self.assertIs(raised.exception, timeout)
            calls.assert_called_once_with("socket", {"op": "snapshot", "path": str(
                output / "managed-reply-timeout.png")})
            with patch.object(PROOF, "wait_for", side_effect=timeout), \
                    patch.object(PROOF, "call", side_effect=RuntimeError("private error")), \
                    self.assertRaises(PROOF.ProofTimeout) as raised:
                PROOF.wait_for_agent_reply("socket", "room", "", "parent", "agent", None, output)
            self.assertIs(raised.exception, timeout)
            self.assertNotIn("private", " ".join(raised.exception.__notes__))

    def test_message_terminal_timeout_saves_only_closed_evidence_and_failure_scene(self):
        original = "00000000-0000-0000-0000-000000000063"
        private = "private direct Message text and model output"
        pending = {"broker_invocation_id": 41, "update": {
            "invocation": original, "state": "accepted", "result": private,
            "error": private, "text": private}, "private": private}
        timeout = PROOF.ProofTimeout("original Message timeout")

        def expire(_label, completed, _process):
            self.assertIsNone(completed())
            raise timeout

        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)

            def respond(_socket, request):
                if request.get("op") == "snapshot":
                    return {"ok": True}
                return {"answer": {"receipt": pending}}

            with patch.object(PROOF, "wait_for", side_effect=expire), \
                    patch.object(PROOF, "call", side_effect=respond) as calls, \
                    self.assertRaises(PROOF.ProofTimeout) as raised:
                PROOF.wait_for_direct_message_terminal(
                    "socket", {"receipt": pending}, None, output
                )
            self.assertIs(raised.exception, timeout)
            diagnostic = output / "managed-message-terminal-timeout.json"
            self.assertEqual(json.loads(diagnostic.read_text()), {
                "original_invocation": original,
                "broker_invocation_id": 41,
                "states_seen": {"accepted": 1},
                "original_unchanged": True,
            })
            self.assertNotIn(private, diagnostic.read_text())
            self.assertEqual(calls.call_args_list[-1].args, ("socket", {
                "op": "snapshot",
                "path": str(output / "managed-message-terminal-timeout.png"),
            }))

            succeeded = {"update": {"invocation": original, "state": "succeeded"}}
            with patch.object(PROOF, "wait_terminal", return_value=succeeded), \
                    patch.object(PROOF, "call") as calls:
                self.assertIs(PROOF.wait_for_direct_message_terminal(
                    "socket", {"receipt": pending}, None, output
                ), succeeded)
            calls.assert_not_called()

    def test_native_delivery_diagnostic_is_bounded_closed_and_content_free(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            log = output / "shell.log"
            private = "private prompt and model output must not escape"
            log.write_text(
                private + "\n"
                + "[app so.tilo.chat stdout] DEBUG tilo_agent_delivery: "
                + "native-delivery:sign:in_flight\n"
                + "WARN unrelated raw error details\n"
                + "[app so.tilo.chat stderr] \x1b[2m2026-10-06T12:00:00Z\x1b[0m "
                + "\x1b[34mDEBUG\x1b[0m \x1b[35mtilo_agent_delivery\x1b[0m: "
                + "native-delivery:sign:definitive_refusal\x1b[0m\n"
            )
            result = PROOF.native_delivery_diagnostic(output)
            self.assertEqual(result, {
                "log_count": 1,
                "marker_count": 2,
                "markers": [
                    {"source": "chat_app", "stage": "sign", "outcome": "in_flight"},
                    {"source": "chat_app", "stage": "sign",
                     "outcome": "definitive_refusal"},
                ],
                "source": "chat_app",
                "stage": "sign",
                "outcome": "definitive_refusal",
                "log_truncated": False,
            })
            self.assertNotIn(private, json.dumps(result))

    def test_native_delivery_enqueue_classification_is_closed(self):
        markers = [
            ("document_enqueue_busy", "disabled"),
            ("document_enqueue_not_ready", "in_flight"),
            ("document_enqueue_unavailable", "disabled"),
            ("document_enqueue_too_large", "disabled"),
            ("document_custody", "disabled"),
            ("document_primary", "disabled"),
            ("document_route", "disabled"),
            ("document_transport", "disabled"),
        ]
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            private = "private prompt output receipt and error"
            (output / "shell.log").write_text("\n".join(
                "[app so.tilo.chat stderr] DEBUG tilo_agent_delivery: "
                f"native-delivery:{stage}:{outcome}" for stage, outcome in markers
            ) + "\n[app so.tilo.chat stderr] DEBUG tilo_agent_delivery: "
                f"native-delivery:{private}:disabled\n{private}\n")
            result = PROOF.native_delivery_diagnostic(output)
            self.assertEqual(result["marker_count"], len(markers))
            self.assertEqual(
                [(entry["stage"], entry["outcome"]) for entry in result["markers"]],
                markers,
            )
            self.assertNotIn(private, json.dumps(result))

    def test_native_delivery_diagnostic_preserves_ambiguity(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            log = output / "shell.log"
            log.write_text(
                "[app so.tilo.chat stdout] DEBUG tilo_agent_delivery: "
                "native-delivery:owner_send:in_flight\n"
                "[app so.tilo.agent stdout] DEBUG tilo_agent_delivery: "
                "native-delivery:owner_send:saved\n"
                "[app so.tilo.chat stderr] private tilo_agent_delivery: "
                "native-delivery:owner_send:saved\n"
                "[app so.tilo.chat stdout] native-delivery:owner_send:saved\n"
            )
            self.assertEqual(PROOF.native_delivery_diagnostic(output), {
                "log_count": 1,
                "marker_count": 1,
                "markers": [{"source": "chat_app", "stage": "owner_send",
                             "outcome": "in_flight"}],
                "source": "chat_app",
                "stage": "owner_send",
                "outcome": "in_flight",
                "log_truncated": False,
            })
            log.unlink()
            self.assertEqual(PROOF.native_delivery_diagnostic(output), {
                "log_count": 0,
                "marker_count": 0,
                "markers": [],
                "source": None,
                "stage": None,
                "outcome": "unobserved",
                "log_truncated": False,
            })

    def test_native_delivery_diagnostic_accepts_only_unprefixed_shell_target(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            (output / "shell.log").write_text(
                "DEBUG tilo_agent_delivery: native-delivery:source_signer:enabled\n"
                "DEBUG tilo_agent_delivery: native-delivery:broker_original:in_flight\n"
                "[app so.tilo.agent stderr] DEBUG tilo_agent_delivery: "
                "native-delivery:broker_original:complete\n"
                "shell payload DEBUG tilo_agent_delivery: "
                "native-delivery:broker_original:definitive_refusal\n"
            )
            self.assertEqual(PROOF.native_delivery_diagnostic(output), {
                "log_count": 1,
                "marker_count": 2,
                "markers": [
                    {"source": "shell_broker", "stage": "source_signer",
                     "outcome": "enabled"},
                    {"source": "shell_broker", "stage": "broker_original",
                     "outcome": "in_flight"},
                ],
                "source": "shell_broker",
                "stage": "broker_original",
                "outcome": "in_flight",
                "log_truncated": False,
            })

    def test_native_delivery_diagnostic_retains_observed_middle_of_bounded_log(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            marker = (
                "DEBUG tilo_agent_delivery: "
                "native-delivery:broker_stream_replaced:definitive_refusal\n"
            )
            spoof = (
                "[app so.tilo.agent stderr] DEBUG tilo_agent_delivery: "
                "native-delivery:broker_worker_missing:definitive_refusal\n"
            )
            (output / "shell.log").write_bytes(
                b"x" * (171 * 1024) + b"\n" + marker.encode()
                + b"y" * (509 * 1024) + b"\n" + spoof.encode()
            )
            result = PROOF.native_delivery_diagnostic(output)
            self.assertEqual(result["marker_count"], 1)
            self.assertEqual(result["markers"], [{
                "source": "shell_broker",
                "stage": "broker_stream_replaced",
                "outcome": "definitive_refusal",
            }])
            self.assertFalse(result["log_truncated"])

    def test_native_delivery_diagnostic_reports_bounded_head_and_tail_gap(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            head = (
                "DEBUG tilo_agent_delivery: native-delivery:source_signer:enabled\n"
            )
            tail = (
                "[app so.tilo.chat stderr] DEBUG tilo_agent_delivery: "
                "native-delivery:broker_worker_missing:definitive_refusal\n"
            )
            (output / "shell.log").write_bytes(
                head.encode() + b"x" * (1100 * 1024) + b"\n" + tail.encode()
            )
            result = PROOF.native_delivery_diagnostic(output)
            self.assertEqual(result["marker_count"], 2)
            self.assertEqual(result["markers"][0]["stage"], "source_signer")
            self.assertEqual(result["markers"][1]["stage"], "broker_worker_missing")
            self.assertTrue(result["log_truncated"])

    def test_native_delivery_diagnostic_reports_bounded_line_gap(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            marker = (
                "DEBUG tilo_agent_delivery: native-delivery:broker_lease_missing:"
                "definitive_refusal\n"
            )
            (output / "shell.log").write_text("filler\n" * 4097 + marker)
            result = PROOF.native_delivery_diagnostic(output)
            self.assertEqual(result["marker_count"], 1)
            self.assertEqual(result["stage"], "broker_lease_missing")
            self.assertTrue(result["log_truncated"])

    def test_native_delivery_diagnostic_failure_still_cleans_owned_state(self):
        class Closed:
            def __init__(self):
                self.closed = False

            def close(self):
                self.closed = True

            def cleanup(self):
                self.closed = True

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            profile = root / "profile"
            output = root / "output"
            profile.mkdir()
            output.mkdir()
            log = io.StringIO()
            provider = Closed()
            runtime_owner = Closed()
            with patch.object(pathlib.Path, "write_text", side_effect=OSError("refused")):
                with self.assertRaisesRegex(
                        RuntimeError, "^native delivery diagnostic unavailable$"):
                    PROOF.finalize_fixture(
                        profile, output, [], [log], provider, runtime_owner
                    )
            self.assertTrue(log.closed)
            self.assertTrue(provider.closed)
            self.assertTrue(runtime_owner.closed)
            self.assertFalse(profile.exists())

    def test_finalization_retains_only_closed_managed_startup_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            profile = root / "profile"
            output = root / "output"
            profile.mkdir()
            output.mkdir()
            private_content = "private instruction and model output must not escape"
            (profile / "launcher-debug-output.log").write_text(
                "managed Agent enrolled subscription acknowledged\n"
                "managed Agent enrolled task execution entered\n"
                + private_content + "\n"
            )
            PROOF.finalize_fixture(profile, output, [], [], None, None)
            retained = (output / "managed-child-final.json").read_text()
            evidence = json.loads(retained)
            self.assertEqual(evidence["log_count"], 1)
            self.assertEqual(evidence["line_count"], 3)
            self.assertTrue(evidence["subscription_acknowledged"])
            self.assertTrue(evidence["execution_entered"])
            self.assertFalse(evidence["model_turn_entered"])
            self.assertIsNone(evidence["first_error_kind"])
            self.assertNotIn(private_content, retained)
            self.assertFalse(profile.exists())

    def test_normal_descriptor_transport_is_not_a_managed_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = pathlib.Path(directory)
            log = profile / "launcher-debug-output.log"
            log.write_text(
                'INFO transport="TILO_SVC_COLLAB_FD" joined the room; waiting for the document\n'
                'INFO managed Agent task catalogue unavailable\n'
                'INFO managed Agent model turn entered\n'
            )
            diagnostic = PROOF.managed_child_diagnostic(profile)
            self.assertIsNone(diagnostic["first_error_kind"])
            self.assertTrue(diagnostic["catalogue_unavailable"])
            self.assertTrue(diagnostic["model_turn_entered"])
            log.write_text('ERROR TILO_SVC_COLLAB_FD descriptor invalid\n')
            self.assertEqual(
                PROOF.managed_child_diagnostic(profile)["first_error_kind"],
                "managed_collaboration_descriptor_failed",
            )

    def test_exact_new_pointer_release_retains_revision_after_later_ack_frame(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            log = output / "shell.log"
            old = {"kind": "pointer_released", "before": {"app": "so.tilo.chat"},
                   "after": {"app": "so.tilo.chat", "agent_message_submission": {
                       "draft_revision": 99, "current_revision": 99, "draft_empty": False}}}
            log.write_text('[app so.tilo.chat stderr] tilo-physical-input ' + json.dumps(old) + "\n")
            offset = log.stat().st_size
            release = {"kind": "pointer_released", "before": {"app": "so.tilo.chat"},
                       "after": {"app": "so.tilo.chat", "agent_message_submission": {
                           "draft_revision": 7, "current_revision": 7, "draft_empty": False}}}
            with log.open("a") as stream:
                stream.write('[app so.tilo.chat stderr] tilo-physical-input ' + json.dumps(release) + "\n")
                stream.write('[app so.tilo.chat stderr] tilo-physical-frame ' + json.dumps({
                    "state": {"app": "so.tilo.chat", "agent_message_submission": None}}) + "\n")
                stream.write('[app so.tilo.store stderr] tilo-physical-input ' + json.dumps(release) + "\n")
            self.assertEqual(PROOF.physical_send_activation_revision(
                output, offset, "pointer_released"), {"revision": 7})
            with log.open("a") as stream:
                stream.write('[app so.tilo.chat stderr] tilo-physical-input ' + json.dumps(release) + "\n")
            with self.assertRaises(RuntimeError):
                PROOF.physical_send_activation_revision(output, offset, "pointer_released")

    def test_keyboard_send_activation_retains_exact_revision(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            log = output / "shell.log"
            offset = 0
            activation = {
                "kind": "key_pressed",
                "before": {"app": "so.tilo.chat", "agent_message_submission": None},
                "after": {"app": "so.tilo.chat", "agent_message_submission": {
                    "draft_revision": 11, "current_revision": 11, "draft_empty": False,
                }},
            }
            log.write_text(
                "[app so.tilo.chat stderr] tilo-physical-input "
                + json.dumps(activation) + "\n"
            )
            self.assertEqual(PROOF.physical_send_activation_revision(
                output, offset, "key_pressed"), {"revision": 11})
            with self.assertRaises(RuntimeError):
                PROOF.physical_send_activation_revision(output, offset, "ime_commit")

    def test_supplement_uses_actual_seat_send_after_unchanged_journeys(self):
        source = inspect.getsource(PROOF.physical_send_after_stop)
        for forbidden in ("SetText", "AddSelection", "DoAction", "invoke(socket, message"):
            self.assertNotIn(forbidden, source)
        for required in ('"op": "type"', '"op": "key"', "focus_agent_conversation_control",
                         "canonical_physical_message", "owned_agent_identity", "release_exact(4)",
                         '"waiting_for_reply"', '"committed"', "overflow_count() != 0",
                         'physical_send_activation_revision(\n                            output, send_log_offset, "key_pressed")'):
            self.assertIn(required, source)
        bootstrap = inspect.getsource(PROOF.bootstrap)
        self.assertLess(bootstrap.index("managed_task_and_message_lifecycle("),
                        bootstrap.index("native_float_return("))
        self.assertLess(bootstrap.index("native_float_return("),
                        bootstrap.index("physical_send_after_stop("))

    def test_membership_revocation_uses_owner_actions_and_no_fifth_model_turn(self):
        rows = [
            {"application": "so.tilo.desktop", "room": "room-a", "record": "membership",
             "resource": "desktop:agent:tide", "operation": "remove", "id": "remove"},
            {"application": "so.tilo.desktop", "room": "room-b", "record": "membership",
             "resource": "desktop:agent:tide", "operation": "remove", "id": "foreign"},
        ]
        with patch.object(PROOF, "actions", return_value=rows):
            self.assertEqual(PROOF.offered_membership_action(
                "socket", "room-a", "desktop:agent:tide", "remove"
            )["id"], "remove")
        source = inspect.getsource(PROOF.native_agent_membership_revocation)
        for required in (
                "offered_membership_action", "wait_terminal", "owned_agent_identities",
                "agent_read_only_accessibility", "start_managed_agent",
                "agent_conversation_accessibility", "private_editor_observation",
                "provider.request_count() != 4", "provider.overflow_count() != 0",
                "before-agent-membership-revocation-", "fresh_floating_presentation",
                "before_restored_return", "restored_primary_repainted"):
            self.assertIn(required, source)
        for forbidden in ("provider.release_exact(5)", "SetText", "AddSelection", "stage_scenario"):
            self.assertNotIn(forbidden, source)
        bootstrap = inspect.getsource(PROOF.bootstrap)
        self.assertLess(bootstrap.index("physical_send_after_stop("),
                        bootstrap.index("native_agent_membership_revocation("))

    def test_fresh_floating_presentation_uses_the_native_surface_identity(self):
        previous = {"floating_id": "float-a", "surface_id": 41, "frame_seq": 8}
        replacement = {"floating_id": "float-a", "surface_id": 42, "frame_seq": 1}
        newer_reuse = {"floating_id": "float-a", "surface_id": 41, "frame_seq": 9}
        self.assertIs(PROOF.fresh_floating_presentation(replacement, previous), replacement)
        self.assertIs(PROOF.fresh_floating_presentation(newer_reuse, previous), newer_reuse)
        self.assertIsNone(PROOF.fresh_floating_presentation(
            {"floating_id": "float-a", "surface_id": 41, "frame_seq": 8}, previous
        ))
        self.assertIsNone(PROOF.fresh_floating_presentation(
            {"floating_id": "float-a", "surface_id": 41, "frame_seq": 7}, previous
        ))
        self.assertIsNone(PROOF.fresh_floating_presentation(
            {"floating_id": "float-b", "surface_id": 42, "frame_seq": 10}, previous
        ))
        for malformed in (None, True, 0, -1, "42"):
            with self.subTest(surface_id=malformed), self.assertRaises(RuntimeError):
                PROOF.fresh_floating_presentation(
                    {"floating_id": "float-a", "surface_id": malformed, "frame_seq": 1},
                    previous,
                )

    def test_owned_agent_observer_matches_the_packaged_binary_and_shell_ancestry(self):
        def process(root, pid, parent, started, program):
            directory = root / str(pid)
            directory.mkdir()
            fields = ["S", str(parent), *("0" for _ in range(17)), str(started)]
            (directory / "stat").write_text(f"{pid} ({program}) " + " ".join(fields))
            (directory / "cmdline").write_bytes(
                f"/runtime/payload/bin/{program}\0--managed\0".encode()
            )

        with tempfile.TemporaryDirectory() as directory:
            proc = pathlib.Path(directory)
            process(proc, 100, 1, 10, "shell")
            process(proc, 200, 100, 20, "tilo-launcher")
            process(proc, 300, 200, 777, "tilo-agent")
            process(proc, 301, 200, 778, "agent")
            process(proc, 302, 999, 779, "tilo-agent")
            self.assertEqual(PROOF.owned_agent_identities(100, proc), [(300, "777")])

    def test_final_lifecycle_requires_structured_float_return_success(self):
        self.assertTrue(PROOF.float_return_evidence_proven({"proven": True}))
        for evidence in (True, {"proven": 1}, {"proven": False}, {}, {"frame_seq": 9}):
            with self.subTest(evidence=evidence):
                self.assertFalse(PROOF.float_return_evidence_proven(evidence))
        source = inspect.getsource(PROOF.bootstrap)
        self.assertIn("float_return_evidence_proven(float_return_proven)", source)

    def test_public_progress_requires_current_exact_original_and_canonical_reply(self):
        original = {"invocation": "00000000-0000-0000-0000-000000000031",
                    "request_row": "m_00000000000000000000000000000031"}
        entry = {"original_invocation": original["invocation"], "request_row": original["request_row"],
                 "generation": 9, "phase": "waiting_for_reply", "unavailable": False}
        state = {"agent_message_progress": [entry]}
        self.assertEqual(PROOF.chat_public_message_progress(state, original, "waiting_for_reply"), entry)
        self.assertIsNone(PROOF.chat_public_message_progress({"agent_message_progress": [
            dict(entry, phase=None, generation=None, unavailable=True)]}, original, "waiting_for_reply"))
        for changed in [{"unavailable": True}, {"generation": None}, {"generation": True},
                        {"request_row": "foreign-row"}, {"prepared_text": "private"},
                        {"phase": "needs_attention"}, {"reply_row": "uncommitted-row"}]:
            with self.subTest(changed=changed), self.assertRaises(RuntimeError):
                PROOF.chat_public_message_progress(
                    {"agent_message_progress": [dict(entry, **changed)]}, original, "waiting_for_reply")
        with self.assertRaises(RuntimeError):
            PROOF.chat_public_message_progress({"agent_message_progress": [entry, entry]},
                                               original, "waiting_for_reply")
        committed = dict(entry, phase="committed", reply_row="m_00000000000000000000000000000032")
        self.assertEqual(PROOF.chat_public_message_progress(
            {"agent_message_progress": [committed]}, original, "committed"), committed)
        with self.assertRaises(RuntimeError):
            PROOF.chat_public_message_progress(
                {"agent_message_progress": [dict(committed, reply_row="foreign")]}, original, "committed")

    def test_pending_capture_requires_exact_revision_without_private_payload(self):
        state = {"agent_message_submission": {"draft_revision": 7, "current_revision": 7,
                                               "draft_empty": False}}
        self.assertEqual(PROOF.pending_draft_revision(state), 7)
        for changed in [{"current_revision": 8}, {"draft_empty": True}, {"text": "private"},
                        {"draft_revision": True}]:
            with self.subTest(changed=changed), self.assertRaises(RuntimeError):
                PROOF.pending_draft_revision({"agent_message_submission": dict(
                    state["agent_message_submission"], **changed)})
        self.assertIsNone(PROOF.pending_draft_revision({}))

    def test_latest_mounted_snapshot_does_not_reuse_old_fresh_progress(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            newest = {"app": "so.tilo.chat", "agent_message_progress": [], "unavailable": True}
            (output / "shell.log").write_text(
                '[app so.tilo.chat stderr] tilo-physical-input ' + json.dumps({
                    "after": {"app": "so.tilo.chat", "old": True}}) + "\n"
                + '[app so.tilo.chat stderr] tilo-physical-frame ' + json.dumps({"state": newest}) + "\n"
                + '[app so.tilo.store stderr] tilo-physical-input ' + json.dumps({
                    "after": {"app": "so.tilo.chat", "foreign": True}}) + "\n")
            self.assertEqual(PROOF.latest_chat_input_state(output), newest)

    def test_physical_message_original_is_derived_from_saved_canonical_person_row(self):
        request = {"id": "m_00000000000000000000000000000021", "author": "person:fixture",
                   "parent": "", "agentIntent": "message", "text": "fixture",
                   "complete": True}
        messages = [request]
        action = {"application": "so.tilo.chat", "room": "room", "record": "",
                  "operation": "read-messages"}
        with patch.object(PROOF, "actions", return_value=[action]), \
                patch.object(PROOF, "invoke", return_value={}), \
                patch.object(PROOF, "wait_terminal", return_value={}), \
                patch.object(PROOF, "terminal_value", return_value={
                    "content": {"channel": "actual-direct-channel", "messages": messages}}):
            self.assertEqual(PROOF.canonical_physical_message("socket", "room", "", "fixture", None),
                             {"invocation": "00000000-0000-0000-0000-000000000021",
                              "request_row": request["id"], "channel": "actual-direct-channel"})
            for changed in [{"id": "caller-asserted-row"}, {"parent": "another-parent"},
                            {"author": "agent:tide"}, {"agentIntent": "queue"},
                            {"complete": False}]:
                original = request.copy()
                request.update(changed)
                with self.subTest(changed=changed), self.assertRaises(RuntimeError):
                    PROOF.canonical_physical_message("socket", "room", "", "fixture", None)
                request.clear()
                request.update(original)
            messages.append(dict(request))
            with self.assertRaises(RuntimeError):
                PROOF.canonical_physical_message("socket", "room", "", "fixture", None)

    def test_real_provider_fourth_post_settles_while_original_third_stays_held(self):
        provider = PROOF.HeldProvider()
        provider.require_owner_admission(lambda: {"phase": "accepted"})

        def post():
            request = urllib.request.Request(
                provider.endpoint + "/chat/completions",
                json.dumps({"model": "bounded-fixture"}).encode(),
                {"Authorization": "Bearer isolated-fixture-not-a-real-provider-key"},
            )
            with urllib.request.urlopen(request, timeout=5) as response:
                return response.status

        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            try:
                original = [pool.submit(post) for _ in range(3)]
                PROOF.wait_for("three actual bounded POSTs",
                               lambda: provider.request_count() == 3, timeout=5)
                provider.release_through(2)
                # Two completed requests do not release the third.
                PROOF.wait_for("the two released original responses",
                               lambda: sum(future.done() for future in original) == 2, timeout=5)
                provider.require_fourth_message_admission(lambda: {"phase": "WaitingForReply"})
                fourth = pool.submit(post)
                PROOF.wait_for("actual fourth POST",
                               lambda: provider.request_count() == 4, timeout=5)
                provider.release_exact(4)
                self.assertEqual(fourth.result(timeout=5), 200)
                self.assertEqual(sum(future.done() for future in original), 2)
                self.assertEqual(provider.allowed, 2)
                self.assertEqual(provider.overflow_count(), 0)
                with self.assertRaises(urllib.error.HTTPError) as refused:
                    post()
                self.assertEqual(refused.exception.code, 429)
                refused.exception.close()
                self.assertEqual(provider.request_count(), 4)
                self.assertEqual(provider.overflow_count(), 1)
                provider.release_through(3)
                self.assertEqual([future.result(timeout=5) for future in original], [200] * 3)
            finally:
                provider.close()

    def test_marker_provider_invokes_one_discovered_exact_chat_action(self):
        provider = PROOF.HeldProvider(marker_agent="tide", marker_resource="chat.person:tide")
        provider.require_owner_admission(
            lambda: {"phase": "planning", "sequence": 1, "run_epoch": 7}
        )
        operation = "broker:42:chat.person:tide#open-agent-conversation"

        def post(payload):
            request = urllib.request.Request(
                provider.endpoint + "/chat/completions",
                json.dumps(payload).encode(),
                {"Authorization": "Bearer isolated-fixture-not-a-real-provider-key"},
            )
            with urllib.request.urlopen(request, timeout=5) as response:
                return json.loads(response.read())

        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            try:
                response = pool.submit(post, {
                    "model": "bounded-fixture",
                    "messages": [{"role": "user", "content":
                                  "Operations published right now:\n\n"
                                  "In so.tilo.chat: Tide (chat.agent) — resource chat.person:tide\n"
                                  f"  {operation} — Open conversation\n"
                                  "The request:\n\nOpen that conversation."}],
                })
                PROOF.wait_for("one marker provider request",
                               lambda: provider.request_count() == 1, timeout=5)
                provider.release_through(1)
                tool = response.result(timeout=5)["choices"][0]["message"]["tool_calls"][0]
                self.assertEqual(tool["function"]["name"], "invoke")
                arguments = json.loads(tool["function"]["arguments"])
                self.assertEqual(arguments["operation"], operation)
                self.assertEqual(arguments["parameters"], {})
                self.assertEqual(provider.request_count(), 1)
                self.assertEqual(provider.overflow_count(), 0)
            finally:
                provider.close()

    def test_marker_provider_finishes_third_turn_without_releasing_superseded_second(self):
        provider = PROOF.HeldProvider(marker_agent="tide", marker_resource="chat.person:tide")
        provider.require_owner_admission(
            lambda: {"phase": "planning", "sequence": 1, "run_epoch": 7}
        )
        operation = "broker:42:chat.person:tide#open-agent-conversation"
        payload = {
            "model": "bounded-fixture",
            "messages": [{"role": "user", "content":
                          "Operations published right now:\n\n"
                          "In so.tilo.chat: Tide (chat.agent) — resource chat.person:tide\n"
                          f"  {operation} — Open conversation\n"
                          "The request:\n\nOpen that conversation."}],
        }

        def post():
            request = urllib.request.Request(
                provider.endpoint + "/chat/completions", json.dumps(payload).encode(),
                {"Authorization": "Bearer isolated-fixture-not-a-real-provider-key"},
            )
            with urllib.request.urlopen(request, timeout=5) as response:
                return json.loads(response.read())

        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            try:
                first = pool.submit(post)
                PROOF.wait_for("first marker turn", lambda: provider.request_count() == 1,
                               timeout=5)
                provider.release_through(1)
                self.assertEqual(
                    first.result(timeout=5)["choices"][0]["message"]["tool_calls"][0]
                    ["function"]["name"], "invoke",
                )

                second = pool.submit(post)
                PROOF.wait_for("held marker continuation", provider.marker_second_held,
                               timeout=5)
                third = pool.submit(post)
                PROOF.wait_for("superseding marker turn", provider.marker_second_superseded,
                               timeout=5)
                provider.release_marker_terminal()
                self.assertEqual(
                    third.result(timeout=5)["choices"][0]["message"]["tool_calls"][0]
                    ["function"]["name"], "plan",
                )
                self.assertFalse(second.done())
                self.assertEqual(provider.allowed, 1)
                self.assertEqual(provider.exact_releases, {3})
                with self.assertRaises(urllib.error.HTTPError) as refused:
                    post()
                self.assertEqual(refused.exception.code, 429)
                refused.exception.close()
                provider.release_through(2)
                self.assertEqual(
                    second.result(timeout=5)["choices"][0]["message"]["tool_calls"][0]
                    ["function"]["name"], "plan",
                )
            finally:
                provider.close()

    def test_marker_operation_refuses_absent_ambiguous_or_noncanonical_targets(self):
        expected = "broker:9:chat.person:tide#open-agent-conversation"
        self.assertEqual(PROOF.marker_operation_from_request(
            {"messages": [{"role": "user", "content":
                            "Operations published right now:\n\n"
                            "In so.tilo.chat: Tide (chat.agent) — resource chat.person:tide\n"
                            "  " + expected + " — Open conversation\n"
                            "  " + expected + " — Open conversation\n" +
                            "\nThe request:\n\nOpen it."}]}, "tide", "chat.person:tide"
        ), expected)
        for request in (
                {"messages": [{"role": "user", "content":
                                "Operations published right now:\n\nno operation\nThe request:\n\n"}]},
                {"messages": [{"role": "user", "content":
                                "Operations published right now:\n\n"
                                "  " + expected + " — Open conversation\n"
                                "  broker:8:chat.person:tide#open-agent-conversation — Open conversation\n"
                                "The request:\n\n"}]},
                {"messages": [{"role": "user", "content":
                                "Operations published right now:\n\n"
                                "  broker:9:chat.person:reed#open-agent-conversation — Open conversation\n"
                                "The request:\n\n"}]},
                {"messages": [{"role": "user", "content":
                                "Operations published right now:\n\n"
                                "In so.tilo.chat: " + expected + " (chat.agent) — resource chat.person:tide\n"
                                "  broker:9:somewhere#else — label " + expected + "\n"
                                "The request:\n\n" + expected}]}):
            with self.subTest(request=request), self.assertRaises(RuntimeError):
                PROOF.marker_operation_from_request(request, "tide", "chat.person:tide")
        with self.assertRaises(RuntimeError):
            PROOF.marker_operation_from_request({"messages": []}, "Tide with spaces", "chat.person:tide")

    def test_marker_operation_keeps_discovered_bound_resource_opaque_and_exact(self):
        resource = "tilo-ui-bound:v1:host:current:636861742e706572736f6e3a74696465"
        expected = f"broker:9:{resource}#open-agent-conversation"
        request = {"messages": [{"role": "user", "content":
            "Operations published right now:\n\n"
            f"In so.tilo.chat: Tide (chat.agent) — resource {resource}\n"
            f"  {expected} — Open conversation\nThe request:\n\nOpen it."}]}
        self.assertEqual(PROOF.marker_operation_from_request(request, "tide", resource), expected)
        for stale in [None, "chat.person:tide", resource.replace("current", "retired")]:
            with self.subTest(resource=stale), self.assertRaises(RuntimeError):
                PROOF.marker_operation_from_request(request, "tide", stale)

    def test_marker_stage_uses_real_governance_and_accessibility_doors(self):
        rows = [{
            "application": "tilo.workspace", "room": "room-a",
            "resource": "workspace.permissions:room-a", "operation": "profile.save",
            "id": "save",
        }, {
            "application": "tilo.workspace", "room": "room-b",
            "resource": "workspace.permissions:room-b", "operation": "profile.save",
            "id": "foreign",
        }]
        with patch.object(PROOF, "actions", return_value=rows):
            self.assertEqual(PROOF.offered_governance_action(
                "socket", "room-a", "profile.save"
            )["id"], "save")
        grant = inspect.getsource(PROOF.grant_exact_marker_action)
        self.assertIn('"profile.save"', grant)
        self.assertIn('"profile.assign"', grant)
        self.assertIn('"Action"', grant)
        self.assertIn('"open-agent-conversation"', grant)
        self.assertIn('"broad_chat_grant": False', grant)
        stage = inspect.getsource(PROOF.managed_agent_marker_stage)
        for required in (
                "start_managed_agent", "agent_marker_accessibility",
                "activate_shell_accessibility_control", "Agent quick composer",
                "exercise_agent_quick_composer", "provider.request_count() != 3",
                "provider.marker_second_held", "provider.marker_second_superseded",
                "provider.release_marker_terminal", 'interaction["receipt_revision"]',
                'third_admission["applied_revision"] != interaction["receipt_revision"]',
                'admission["sequence"] < second_admission["sequence"]',
                "full_lifecycle_proven"):
            self.assertIn(required, stage)
        for forbidden in ("open_agent_quick_composer_for_shots", "stage_scenario"):
            self.assertNotIn(forbidden, stage)

    def test_marker_composer_uses_physical_private_drafts_and_exact_owner_frontier(self):
        source = inspect.getsource(PROOF.exercise_agent_quick_composer)
        for required in (
                '"op": "click_named"', '"op": "type"',
                '"agent-quick-editor"', '"agent-quick-send"', '"agent-quick-close"',
                'for intent in ("message", "queue", "steer")',
                'activate_agent_quick_composer_control(access, intent)',
                'activate_agent_quick_composer_control(access, "steer")',
                'activate_agent_quick_composer_control(access, "message")',
                '"status") != "in_progress"', 'task.get("run_epoch") != run_epoch',
                'revision > before_revision', '"ui_invocation_uuid_observable": False',
                'task.get("applied_revision")', 'applied_revision == receipt_revision',
                '"owner_direction_applied": True',
                '"message_submission_proven": False', '"queue_submission_proven": False',
                '"independent_permission_gates_proven": False'):
            self.assertIn(required, source)
        for forbidden in ("SetText", "open_agent_quick_composer_for_shots", "invoke(socket"):
            self.assertNotIn(forbidden, source)

    def test_fourth_response_does_not_release_the_original_third_turn(self):
        provider = object.__new__(PROOF.HeldProvider)
        provider.request_lock = threading.Condition()
        provider.requests = 3
        provider.allowed = 2
        provider.exact_releases = set()
        provider.owner_admission_probe = lambda: {"original_task": True}
        probe = lambda: {"original_message": True}
        provider.require_fourth_message_admission(probe)
        self.assertIs(provider.owner_admission_probe, probe)
        with self.assertRaises(RuntimeError):
            provider.release_exact(4)
        provider.requests = 4
        provider.release_exact(4)
        self.assertEqual(provider.allowed, 2)
        self.assertEqual(provider.exact_releases, {4})
        self.assertFalse(provider.allowed >= 3 or 3 in provider.exact_releases)
        provider.release_through(3)
        self.assertEqual(provider.allowed, 3)
        self.assertEqual(provider.exact_releases, {4})

    def test_fourth_probe_cannot_replace_any_original_request_admission(self):
        for requests, allowed in [(0, 0), (1, 0), (2, 1), (3, 3), (4, 2)]:
            provider = object.__new__(PROOF.HeldProvider)
            provider.request_lock = threading.Condition()
            provider.requests = requests
            provider.allowed = allowed
            provider.owner_admission_probe = lambda: {"original_task": True}
            with self.subTest(requests=requests, allowed=allowed):
                with self.assertRaises(RuntimeError):
                    provider.require_fourth_message_admission(lambda: {})

    def test_float_return_uses_only_physical_editor_and_surface_input(self):
        source = inspect.getsource(PROOF.native_float_return)
        for forbidden in ("SetText", "AddSelection", "DoAction"):
            self.assertNotIn(forbidden, source)
        for required in (
            '"op": "type"',
            '"op": "key"',
            '"op": "drag"',
            '"op": "click_at"',
            "physical_chord",
            "focus_agent_conversation_control",
            "undo_history_crossed_float",
            "undo_history_crossed_return",
        ):
            self.assertIn(required, source)
        self.assertLess(source.index("pointer_drag_detach"), source.index("keyboard_float"))
        self.assertLess(source.index("pre-detach undo history in the detached editor"),
                        source.index("detached edit retained for attached undo"))
        self.assertLess(source.index("detached edit retained for attached undo"),
                        source.index("detached undo history in the returned editor"))
        self.assertGreaterEqual(source.count("before = wait_for_private_selection("), 2)
        self.assertIn("before_return_editor = wait_for_private_selection(", source)

    def test_semantic_float_return_requires_native_pixels_and_retained_editor(self):
        source = inspect.getsource(PROOF.native_semantic_float_return)
        for required in ("offered_agent_action", "float-agent-conversation",
                         "return-agent-conversation", "fresh_floating_presentation",
                         "floating_surface_presenting", "fresh_primary_presentation"):
            self.assertIn(required, source)
        self.assertEqual(source.count("private_editor_observation(access, draft) != before"), 2)
        self.assertEqual(source.count("wait_terminal(socket, invoke("), 2)
        for forbidden in ("SetText", "AddSelection", "DoAction", '"op": "type"'):
            self.assertNotIn(forbidden, source)
        self.assertIn("native_semantic_float_return(", inspect.getsource(PROOF.native_float_return))

    def test_semantic_return_requires_a_new_primary_frame(self):
        previous = {"frame_seq": 7}
        for current, accepted in ((None, False), ({"frame_seq": 7}, False),
                                  ({"frame_seq": 6}, False), ({"frame_seq": 8}, True)):
            with self.subTest(current=current), patch.object(
                    PROOF, "presented_primary_frame", return_value=current):
                result = PROOF.fresh_primary_presentation({}, previous)
                self.assertEqual(result is not None, accepted)

    def test_native_ime_retains_pixels_before_accessibility_or_focus_refusal(self):
        source = inspect.getsource(PROOF.native_ime_float_return)
        self.assertLess(source.index("ime-detached-first-frame-"),
                        source.index('"detached Tide editor during Rime preedit"'))
        for required in ("ime-detached-native.json", "ime-detached-accessibility.json",
                         '"keyboard_focused"', '"same_input_method_process"'):
            self.assertIn(required, source)
        self.assertNotIn('"last_error"', source)

    def test_return_seat_refresh_requires_exact_native_source_without_frame_regression(self):
        previous = {"app_id": "so.tilo.chat", "surface_id": 7, "frame_seq": 10,
                    "keyboard_focused": False}
        focused = {**previous, "keyboard_focused": True}
        for current, accepted in ((None, False), ({**focused, "surface_id": 8}, False),
                                  ({**focused, "frame_seq": 9}, False),
                                  ({**focused, "frame_seq": 11}, True),
                                  (focused, True), (previous, True)):
            with self.subTest(current=current), patch.object(
                    PROOF, "presented_primary_frame", return_value=current) as observe:
                result = PROOF.current_same_primary_presentation({}, previous)
            observe.assert_called_once_with({}, "so.tilo.chat")
            if accepted:
                self.assertIs(result, current)
                self.assertEqual(result["keyboard_focused"], current["keyboard_focused"])
            else:
                self.assertIsNone(result)
        self.assertFalse(previous["keyboard_focused"])

    def test_detached_ime_accepts_only_exact_child_or_authenticated_source_continuation(self):
        source = {
            "app_id": "so.tilo.chat", "surface_id": 7, "frame_seq": 10,
            "presented": True, "last_error": None, "keyboard_focused": True,
        }
        child = {
            "app_id": "so.tilo.chat", "floating_id": "float-current",
            "surface_id": 11, "frame_seq": 20, "presented": True,
            "pixels_ready": True, "last_error": None, "keyboard_focused": False,
            "suspended": False, "transferring": False,
            "input_route": "floating-preedit-source", "input_source_surface_id": 7,
        }
        response = lambda rows: {"answer": {"frames": rows}}

        observed, evidence = PROOF.detached_ime_input_route(
            response([source, child]), child, source
        )
        self.assertIs(observed, child)
        self.assertEqual(evidence, {
            "child_keyboard_focused": False,
            "source_preedit_continuation": True,
        })

        focused_child = {
            **child, "keyboard_focused": True,
            "input_route": None, "input_source_surface_id": None,
        }
        observed, evidence = PROOF.detached_ime_input_route(
            response([{**source, "keyboard_focused": False}, focused_child]),
            focused_child,
            source,
        )
        self.assertIs(observed, focused_child)
        self.assertEqual(evidence, {
            "child_keyboard_focused": True,
            "source_preedit_continuation": False,
        })

    def test_detached_ime_refuses_malformed_foreign_stale_and_ended_continuations(self):
        source = {
            "app_id": "so.tilo.chat", "surface_id": 7, "frame_seq": 10,
            "presented": True, "last_error": None, "keyboard_focused": True,
        }
        child = {
            "app_id": "so.tilo.chat", "floating_id": "float-current",
            "surface_id": 11, "frame_seq": 20, "presented": True,
            "pixels_ready": True, "last_error": None, "keyboard_focused": False,
            "suspended": False, "transferring": False,
            "input_route": "floating-preedit-source", "input_source_surface_id": 7,
        }
        response = lambda rows: {"answer": {"frames": rows}}
        invalid = [
            ({**child, "input_route": None}, source),
            ({**child, "input_source_surface_id": "7"}, source),
            ({**child, "keyboard_focused": None}, source),
            ({**child, "input_source_surface_id": 8}, source),
            (child, {**source, "surface_id": 8}),
            (child, {**source, "keyboard_focused": None}),
            (child, {**source, "keyboard_focused": False}),
            ({**child, "floating_id": "float-foreign"}, source),
            ({**child, "presented": False}, source),
            ({**child, "suspended": True}, source),
            ({**child, "transferring": True}, source),
        ]
        for current_child, current_source in invalid:
            with self.subTest(child=current_child, source=current_source), self.assertRaises(RuntimeError):
                PROOF.detached_ime_input_route(
                    response([current_source, current_child]), child, source
                )

        ended = {key: value for key, value in child.items()
                 if key not in {"input_route", "input_source_surface_id"}}
        with self.assertRaises(RuntimeError):
            PROOF.detached_ime_input_route(response([source, ended]), child, source)

        contradictory = {**child, "keyboard_focused": True}
        with self.assertRaises(RuntimeError):
            PROOF.detached_ime_input_route(response([source, contradictory]), child, source)

    def test_returned_ime_accepts_only_exact_primary_or_authenticated_child_continuation(self):
        primary = {
            "app_id": "so.tilo.chat", "surface_id": 7, "frame_seq": 30,
            "presented": True, "last_error": None, "keyboard_focused": False,
            "input_route": "floating-preedit-child", "input_source_surface_id": 11,
        }
        child = {
            "app_id": "so.tilo.chat", "floating_id": "float-current",
            "surface_id": 11, "frame_seq": 22, "presented": False,
            "pixels_ready": True, "last_error": None, "keyboard_focused": True,
            "suspended": True, "transferring": False,
        }
        response = lambda rows: {"answer": {"frames": rows}}
        observed, evidence = PROOF.returned_ime_input_route(
            response([primary, child]), primary, child
        )
        self.assertIs(observed, primary)
        self.assertEqual(evidence, {
            "primary_keyboard_focused": False,
            "child_preedit_continuation": True,
        })

        focused = {
            **primary, "keyboard_focused": True,
            "input_route": None, "input_source_surface_id": None,
        }
        observed, evidence = PROOF.returned_ime_input_route(
            response([focused, {**child, "keyboard_focused": False}]), focused, child
        )
        self.assertIs(observed, focused)
        self.assertEqual(evidence, {
            "primary_keyboard_focused": True,
            "child_preedit_continuation": False,
        })

    def test_returned_ime_refuses_foreign_stale_malformed_and_ended_continuations(self):
        primary = {
            "app_id": "so.tilo.chat", "surface_id": 7, "frame_seq": 30,
            "presented": True, "last_error": None, "keyboard_focused": False,
            "input_route": "floating-preedit-child", "input_source_surface_id": 11,
        }
        child = {
            "app_id": "so.tilo.chat", "floating_id": "float-current",
            "surface_id": 11, "frame_seq": 22, "presented": False,
            "pixels_ready": True, "last_error": None, "keyboard_focused": True,
            "suspended": True, "transferring": False,
        }
        response = lambda rows: {"answer": {"frames": rows}}
        invalid = [
            ({**primary, "input_route": None}, child),
            ({**primary, "input_source_surface_id": "11"}, child),
            ({**primary, "keyboard_focused": None}, child),
            ({**primary, "input_source_surface_id": 12}, child),
            (primary, {**child, "surface_id": 12}),
            (primary, {**child, "floating_id": "float-foreign"}),
            (primary, {**child, "frame_seq": 21}),
            (primary, {**child, "keyboard_focused": False}),
            (primary, {**child, "suspended": False}),
            (primary, {**child, "transferring": True}),
        ]
        for destination, source in invalid:
            with self.subTest(destination=destination, source=source), self.assertRaises(RuntimeError):
                PROOF.returned_ime_input_route(
                    response([destination, source]), primary, child
                )
        ended = {key: value for key, value in primary.items()
                 if key not in {"input_route", "input_source_surface_id"}}
        with self.assertRaises(RuntimeError):
            PROOF.returned_ime_input_route(response([ended, child]), primary, child)
        contradictory = {**primary, "keyboard_focused": True}
        with self.assertRaises(RuntimeError):
            PROOF.returned_ime_input_route(response([contradictory, child]), primary, child)

    def test_return_diagnostics_are_only_closed_counts_and_actual_bools(self):
        private = "private-editor-and-native-identity"
        first = {"frame_seq": 10, "keyboard_focused": False, "surface_id": 7,
                 "title": private, "output": private}
        current = {**first, "frame_seq": 11, "keyboard_focused": True}
        evidence = PROOF.returned_ime_observation(first, current, True, False, True)
        self.assertTrue(evidence["keyboard_focused"])
        self.assertFalse(evidence["initial_keyboard_focused"])
        self.assertFalse(evidence["chat_text_input_active"])
        self.assertNotIn(private, json.dumps(evidence))
        self.assertNotIn("surface_id", evidence)
        self.assertTrue(all(type(value) in (bool, int) or value is None
                            for value in evidence.values()))
        missing = PROOF.returned_ime_observation(first, None, False, False, False)
        self.assertFalse(missing["keyboard_focused"])
        self.assertFalse(missing["same_source_presented"])
        self.assertIsNone(missing["current_frame_seq"])

    def test_return_reobserves_after_accessibility_before_strict_focus_assertion(self):
        source = inspect.getsource(PROOF.native_ime_float_return)
        ready = source.index('"returned Tide editor during Rime preedit"')
        refresh = source.index("returned, returned_input = returned_ime_input_route(", ready)
        diagnostic = source.index('"ime-returned-native.json"', refresh)
        assertion = source.index('returned_access["process"] != chat_process', diagnostic)
        self.assertLess(ready, refresh)
        self.assertLess(refresh, diagnostic)
        self.assertLess(diagnostic, assertion)
        self.assertIn('"ime-returned-first-frame-', source)

    def test_native_ime_stage_uses_real_rime_and_preserves_each_handoff_boundary(self):
        source = inspect.getsource(PROOF.native_ime_float_return)
        for forbidden in ("Ime::Preedit", "TrustedInputMethod", "SetText", "AddSelection"):
            self.assertNotIn(forbidden, source)
        for required in (
                '"text": "nihao"', '"key": "Space"', 'committed_after = "u你好"',
                "same_fcitx_process", "chat_text_input_active",
                "detached_ime_input_route",
                "ime-preedit-attached-", "ime-preedit-detached-",
                "ime-preedit-returned-", "drag_agent_conversation",
                "fresh_floating_presentation", "pre-detach edit undo after both handoffs",
                'focus_agent_conversation_control(returned_access, returned_access["drag"]',
                'focus_agent_conversation_control(keyboard_access, keyboard_access["control"]'):
            self.assertIn(required, source)
        self.assertIn("preserve_ime_drag_refusal", source)
        self.assertLess(source.index("committed_value_unchanged_before_float"),
                        source.index("committed_value_unchanged_after_float"))
        self.assertLess(source.index("committed_value_unchanged_after_float"),
                        source.index("committed_value_unchanged_after_return"))

    def test_native_ime_commits_and_undoes_in_float_before_returning_a_new_preedit(self):
        source = inspect.getsource(PROOF.native_ime_float_return)
        detached_ready = source.index('"detached Tide editor during Rime preedit"')
        floating_commit = source.index('"Rime\'s exact detached commit and child seat"')
        floating_select = source.index("wait_for_private_selection(", floating_commit)
        floating_undo = source.index('"detached Rime commit undo"', floating_select)
        second_type = source.index('{"op": "type", "text": "nihao"}', floating_undo)
        second_preedit = source.index('"genuine Rime preedit in the detached Chat frame"', second_type)
        returned = source.index('"Rime preedit\'s returned primary frame"', second_preedit)
        returned_commit = source.index('"Rime\'s exact post-Return commit and primary seat"', returned)
        self.assertLess(detached_ready, floating_commit)
        self.assertLess(floating_commit, floating_select)
        self.assertLess(floating_select, floating_undo)
        self.assertLess(floating_undo, second_type)
        self.assertLess(second_type, second_preedit)
        self.assertLess(second_preedit, returned)
        self.assertLess(returned, returned_commit)
        for proof in (
                '"floating_rime_commit": True', '"floating_rime_selection": True',
                '"floating_rime_commit_undo": True', '"returned_rime_commit": True',
                '"returned_primary_keyboard_focused_after_commit": True'):
            self.assertIn(proof, source)
        self.assertEqual(source.count('{"op": "key", "key": "Space"}'), 2)
        self.assertEqual(source.count('{"op": "type", "text": "nihao"}'), 2)

    def test_rime_settings_merge_preserves_existing_scratch_settings(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = pathlib.Path(directory)
            path = profile / "config/tilo/settings.json"
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps({
                "version": 1,
                "appearance": {"theme": "high-contrast"},
                "input": {"repeat_delay": 270},
            }))
            PROOF.configure_rime_input(profile)
            settings = json.loads(path.read_text())
        self.assertEqual(settings["appearance"], {"theme": "high-contrast"})
        self.assertEqual(settings["input"], {
            "repeat_delay": 270,
            "keyboard_layout": "ime/rime",
            "keyboard_layouts": ["us", "ime/rime"],
        })

    def test_chat_text_input_requires_exact_keyboard_and_active_text_input(self):
        focused = {"seat_focus": {
            "keyboard": {"app_id": "so.tilo.chat"},
            "text_input": {"app_id": "so.tilo.chat"},
            "text_input_active": True,
        }}
        with patch.object(PROOF, "chrome_state", return_value=focused):
            self.assertTrue(PROOF.chat_text_input_active("socket"))
        for changed in (
                {"text_input_active": False},
                {"keyboard": {"app_id": "so.tilo.store"}},
                {"text_input": {"app_id": "so.tilo.store"}}):
            state = {"seat_focus": {**focused["seat_focus"], **changed}}
            with self.subTest(changed=changed), \
                    patch.object(PROOF, "chrome_state", return_value=state):
                self.assertFalse(PROOF.chat_text_input_active("socket"))

    def test_owned_fcitx_observer_and_cleanup_are_exact(self):
        def process(root, pid, parent, group, started, program, executable):
            directory = root / str(pid)
            directory.mkdir()
            fields = ["S", str(parent), str(group), *("0" for _ in range(16)), str(started)]
            (directory / "stat").write_text(f"{pid} ({program}) " + " ".join(fields))
            (directory / "cmdline").write_bytes(f"{executable}\0".encode())
            (directory / "exe").symlink_to(executable)

        with tempfile.TemporaryDirectory() as directory:
            proc = pathlib.Path(directory)
            process(proc, 100, 1, 100, 10, "shell", "/usr/bin/shell")
            process(proc, 200, 100, 200, 20, "fcitx5", "/usr/bin/fcitx5")
            process(proc, 201, 100, 201, 21, "fcitx5", "/usr/bin/not-fcitx")
            process(proc, 202, 999, 202, 22, "fcitx5", "/usr/bin/fcitx5")
            executables = {
                200: pathlib.Path("/usr/bin/fcitx5"),
                201: pathlib.Path("/usr/bin/not-fcitx"),
                202: pathlib.Path("/usr/bin/fcitx5"),
            }
            with patch.object(
                    PROOF, "process_executable",
                    side_effect=lambda pid, _root=proc: executables.get(pid)):
                identity = PROOF.owned_fcitx_identity(100, proc)
                self.assertEqual(identity, {"pid": 200, "group": 200, "started": "20"})
                self.assertTrue(PROOF.same_fcitx_process(identity, proc))
                self.assertFalse(PROOF.same_fcitx_process(
                    {**identity, "started": "foreign"}, proc
                ))

        with patch.object(PROOF, "same_fcitx_process", side_effect=[True, False]), \
                patch.object(PROOF.os, "killpg") as killpg:
            PROOF.cleanup_owned_fcitx(identity)
        killpg.assert_called_once_with(200, PROOF.signal.SIGTERM)

    def test_native_ime_stage_is_explicit_and_configured_before_shell_launch(self):
        bootstrap = inspect.getsource(PROOF.bootstrap)
        self.assertLess(bootstrap.index('run(binaries / "examples/proof_provider"'),
                        bootstrap.index("configure_rime_input(profile)"))
        self.assertLess(bootstrap.index("configure_rime_input(profile)"),
                        bootstrap.index("subprocess.Popen(shell_args"))
        self.assertIn('"stage": "native-ime"', bootstrap)
        self.assertIn("owned_fcitx_identity(shell.pid)", bootstrap)
        self.assertIn("native_ime_float_return(", bootstrap)
        main = inspect.getsource(PROOF.main)
        self.assertIn('"native-ime"', main)
        self.assertIn('ime_only=args.stage == "native-ime"', main)

    def test_floating_focus_refresh_keeps_exact_native_identity_and_fresh_state(self):
        first = {
            "app_id": "so.tilo.chat", "floating_id": "inert-fixture-surface",
            "surface_id": 7, "frame_seq": 1, "presented": True,
            "pixels_ready": True, "last_error": None, "keyboard_focused": False,
        }
        current = {**first, "frame_seq": 2, "keyboard_focused": True}
        response = lambda row: {"answer": {"frames": [row]}}
        self.assertEqual(
            PROOF.current_same_floating_presentation(response(current), first), current
        )
        self.assertFalse(first["keyboard_focused"], "the first observation stays immutable")
        self.assertEqual(
            PROOF.current_same_floating_presentation(response(first), first), first,
            "refreshing does not fabricate focus for an unfocused destination",
        )
        for changed in [
            {"surface_id": 8}, {"floating_id": "another-surface"},
            {"frame_seq": 0}, {"presented": False},
        ]:
            self.assertIsNone(PROOF.current_same_floating_presentation(
                response({**current, **changed}), first
            ))

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
