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
            self.assertEqual(PROOF.physical_send_release_revision(output, offset), {"revision": 7})
            with log.open("a") as stream:
                stream.write('[app so.tilo.chat stderr] tilo-physical-input ' + json.dumps(release) + "\n")
            with self.assertRaises(RuntimeError):
                PROOF.physical_send_release_revision(output, offset)

    def test_supplement_uses_actual_seat_send_after_unchanged_journeys(self):
        source = inspect.getsource(PROOF.physical_send_after_stop)
        for forbidden in ("SetText", "AddSelection", "DoAction", "GrabFocus", "invoke(socket, message"):
            self.assertNotIn(forbidden, source)
        for required in ('"op": "type"', '"op": "click_at"', "component_global_center",
                         "canonical_physical_message", "owned_agent_identity", "release_exact(4)",
                         '"waiting_for_reply"', '"committed"', "overflow_count() != 0"):
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
                "provider.request_count() != 4"):
            self.assertIn(required, source)
        for forbidden in ("provider.release_exact(5)", "SetText", "AddSelection", "stage_scenario"):
            self.assertNotIn(forbidden, source)
        bootstrap = inspect.getsource(PROOF.bootstrap)
        self.assertLess(bootstrap.index("physical_send_after_stop("),
                        bootstrap.index("native_agent_membership_revocation("))

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
