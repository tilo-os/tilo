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
        provider = PROOF.HeldProvider(marker_agent="tide")
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

    def test_marker_operation_refuses_absent_ambiguous_or_noncanonical_targets(self):
        expected = "broker:9:chat.person:tide#open-agent-conversation"
        self.assertEqual(PROOF.marker_operation_from_request(
            {"messages": [{"role": "user", "content":
                            "Operations published right now:\n\n"
                            "In so.tilo.chat: Tide (chat.agent) — resource chat.person:tide\n"
                            "  " + expected + " — Open conversation\n"
                            "  " + expected + " — Open conversation\n" +
                            "\nThe request:\n\nOpen it."}]}, "tide"
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
                PROOF.marker_operation_from_request(request, "tide")
        with self.assertRaises(RuntimeError):
            PROOF.marker_operation_from_request({"messages": []}, "Tide with spaces")

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
                "provider.request_count() != 1", "full_lifecycle_proven"):
            self.assertIn(required, stage)
        for forbidden in ("open_agent_quick_composer_for_shots", "stage_scenario"):
            self.assertNotIn(forbidden, stage)

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
