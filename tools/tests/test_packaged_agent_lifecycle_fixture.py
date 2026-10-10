import importlib.util
import inspect
import io
import pathlib
import re
import sys
import tempfile
import unittest
import threading
import concurrent.futures
import json
import urllib.request
import urllib.error
from contextlib import ExitStack
from types import SimpleNamespace
from unittest.mock import call, patch


REPO = pathlib.Path(__file__).resolve().parents[2]
sys.path[:0] = [str(REPO / "scripts/headless-examples"), str(REPO / "tools")]
SPEC = importlib.util.spec_from_file_location(
    "packaged_agent_lifecycle",
    REPO / "scripts/headless-examples/packaged-agent-lifecycle.py",
)
PROOF = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROOF)


class CurrentChatControlTests(unittest.TestCase):
    def observe(self, *, reply="(<'Show conversation navigation'>,)", error=None,
                tree=None, trees=None, roles=None, clickable=None, owned_route=None,
                process=42, expect_root_read=True):
        client = PROOF.atspi.AtspiClient("owned-address", "owned-chat")
        tree = tree or {"/reveal": "(<'Show conversation navigation'>,)"}
        trees = trees or {"/window": tree}
        paths = {path for nodes in trees.values() for path in nodes}
        roles = roles or {path: PROOF.AT_SPI_ROLE_BUTTON for path in paths}
        clickable = paths if clickable is None else set(clickable)

        def call_atspi(path, interface, *args, **_kwargs):
            if path == PROOF.atspi.ROOT_PATH \
                    and interface == "org.a11y.atspi.Accessible.GetChildren":
                roots = ", ".join(f"objectpath '{window}'" for window in trees)
                return f"([{roots}],)"
            if interface == "org.a11y.atspi.Action.GetActions":
                return "([('click',)],)" if path in clickable else "(@a(ssb) [],)"
            raise AssertionError((path, interface, args))

        with ExitStack() as context:
            context.enter_context(patch.object(PROOF.atspi, "atspi_address", return_value="owned-address"))
            context.enter_context(patch.object(PROOF.atspi, "bus_pids", return_value={"owned-chat": 41}))
            context.enter_context(patch.object(PROOF.atspi, "app_process", return_value=process))
            context.enter_context(patch.object(PROOF, "process_program", return_value="chat"))
            context.enter_context(patch.object(PROOF.atspi, "AtspiClient", return_value=client))
            reads = context.enter_context(patch.object(client, "call", side_effect=call_atspi))
            context.enter_context(patch.object(client, "role", side_effect=lambda path: roles[path]))
            context.enter_context(patch.object(
                PROOF, "accessible_subtree",
                side_effect=lambda _client, window, _limit: trees[window]
            ))
            context.enter_context(patch.object(
                client, "property", return_value=reply, side_effect=error
            ))
            result = PROOF.chat_accessibility_control(
                "Show conversation navigation", owned_route=owned_route
            )
            root_read = call(
                PROOF.atspi.ROOT_PATH, "org.a11y.atspi.Accessible.GetChildren"
            )
            if expect_root_read:
                self.assertIn(root_read, reads.call_args_list)
            else:
                self.assertNotIn(root_read, reads.call_args_list)
                reads.assert_not_called()
            return result

    def test_exact_current_named_control_is_returned_without_effect_calls(self):
        self.assertEqual(self.observe(), {"bus": "owned-chat", "window": "/window",
                                         "control": "/reveal", "label": "Show conversation navigation"})

    def test_same_named_non_controls_do_not_make_the_actionable_button_ambiguous(self):
        name = "(<'Show conversation navigation'>,)"
        self.assertEqual(self.observe(
            tree={"/tooltip": name, "/reveal": name, "/static": name},
            roles={"/tooltip": 66, "/reveal": PROOF.AT_SPI_ROLE_BUTTON,
                   "/static": 29},
            clickable={"/tooltip", "/reveal"},
        )["control"], "/reveal")
        self.assertIsNone(self.observe(
            tree={"/label": name},
            roles={"/label": PROOF.AT_SPI_ROLE_BUTTON},
            clickable=set(),
        ))

    def test_two_actionable_exact_buttons_remain_a_hard_ambiguity(self):
        name = "(<'Show conversation navigation'>,)"
        with self.assertRaisesRegex(RuntimeError, "duplicate Show conversation navigation"):
            self.observe(
                tree={"/first": name, "/second": name},
                roles={"/first": PROOF.AT_SPI_ROLE_BUTTON,
                       "/second": PROOF.AT_SPI_ROLE_BUTTON},
                clickable={"/first", "/second"},
                owned_route={"bus": "owned-chat", "process": 42, "window": "/window"},
            )

    def test_bound_route_ignores_other_windows_but_never_substitutes_one(self):
        name = "(<'Show conversation navigation'>,)"
        route = {"bus": "owned-chat", "process": 42, "window": "/owned"}
        trees = {
            "/owned": {"/owned-reveal": name},
            "/other": {"/other-reveal": name},
        }
        self.assertEqual(self.observe(
            trees=trees,
            roles={path: PROOF.AT_SPI_ROLE_BUTTON
                   for path in ("/owned-reveal", "/other-reveal")},
            clickable={"/owned-reveal", "/other-reveal"},
            owned_route=route,
        )["control"], "/owned-reveal")
        self.assertIsNone(self.observe(
            trees={"/other": {"/other-reveal": name}},
            roles={"/other-reveal": PROOF.AT_SPI_ROLE_BUTTON},
            clickable={"/other-reveal"},
            owned_route=route,
        ))
        self.assertIsNone(self.observe(
            trees=trees,
            roles={path: PROOF.AT_SPI_ROLE_BUTTON
                   for path in ("/owned-reveal", "/other-reveal")},
            clickable={"/owned-reveal", "/other-reveal"},
            owned_route=route,
            process=84,
            expect_root_read=False,
        ))

    def test_retired_object_is_rediscovered_before_any_focus_or_activation(self):
        self.assertIsNone(self.observe(error=PROOF.atspi.GateError(
            "call failed: Error: GDBus.Error:org.freedesktop.DBus.Error.UnknownObject: retired")))
        self.assertIsNone(self.observe(reply="(<'Hide conversation navigation'>,)"))

    def test_other_errors_and_unparseable_identity_are_not_stale_readiness(self):
        for code in ("AccessDenied", "NoReply", "UnknownInterface"):
            with self.subTest(code=code), self.assertRaises(PROOF.atspi.GateError):
                self.observe(error=PROOF.atspi.GateError(
                    f"call failed: Error: GDBus.Error:org.freedesktop.DBus.Error.{code}: private-error"))
        with self.assertRaisesRegex(RuntimeError, "current Chat control identity is unobservable"):
            self.observe(reply="malformed-private-identity")


class CurrentChatCategoryTests(unittest.TestCase):
    def observe(self, role="(uint32 62,)", actions="([('click',)],)",
                current="(<'Collapse Lifecycle proof'>,)", duplicate=False):
        client = PROOF.atspi.AtspiClient("owned-address", "owned-chat")
        tree = {"/category": "(<'Collapse Lifecycle proof'>,)"}
        if duplicate:
            tree["/duplicate"] = tree["/category"]

        def wire(_path, method, *_args, **_kwargs):
            if method == "org.a11y.atspi.Accessible.GetRole":
                return role
            if method == "org.a11y.atspi.Action.GetActions":
                return actions
            raise AssertionError(method)

        route = {"bus": "owned-chat", "process": 42, "window": "/window"}
        with patch.object(PROOF, "chat_accessibility_windows", return_value=[
                ("owned-chat", 42, client, "/window")]) as windows, \
                patch.object(PROOF, "accessible_subtree", return_value=tree), \
                patch.object(client, "call", side_effect=wire) as calls, \
                patch.object(client, "property", return_value=current):
            result = PROOF.chat_category_accessibility("Lifecycle proof", owned_route=route)
            windows.assert_called_once_with(route)
            self.assertFalse(any("DoAction" in entry.args[1] for entry in calls.call_args_list))
            return result

    def test_actual_toggle_button_wire_role_is_required_for_owned_category(self):
        self.assertEqual(self.observe(), {"bus": "owned-chat", "window": "/window",
                                         "control": "/category", "label": "Collapse Lifecycle proof"})
        for role in ("(uint32 43,)", "(uint32 29,)", "(uint32 66,)", "malformed role"):
            with self.subTest(role=role):
                self.assertIsNone(self.observe(role=role))

    def test_toggle_requires_click_and_current_exact_name(self):
        self.assertIsNone(self.observe(actions="(@a(sss) [],)"))
        self.assertIsNone(self.observe(current="(<'Collapse Other category'>,)"))
        with self.assertRaisesRegex(RuntimeError, "category identity is unobservable"):
            self.observe(current="private malformed reply")

    def test_duplicate_current_owned_category_toggles_fail_closed(self):
        with self.assertRaisesRegex(RuntimeError, "duplicate exact category toggles"):
            self.observe(duplicate=True)


class NativeChatPublicationTests(unittest.TestCase):
    @staticmethod
    def creator():
        return {"provider": "chat", "id": "creator", "application": "so.tilo.chat",
                "room": "room", "record": "record", "resource": "workspace",
                "operation": "new-category", "semantic_identity": None}

    def test_incomplete_or_unsettled_publication_waits_for_actual_creator(self):
        creator = self.creator()
        for flags in ({"complete": False}, {"settled": False}):
            with self.subTest(flags=flags), patch.object(PROOF, "call", side_effect=[
                    PermissionSnapshotTests.page([creator], **flags),
                    PermissionSnapshotTests.page([creator])]) as observe:
                self.assertIsNone(PROOF.native_chat_semantic_publication("socket", "room"))
                self.assertEqual(PROOF.native_chat_semantic_publication("socket", "room"),
                                 [creator])
                self.assertEqual(observe.call_count, 2)

    def test_complete_creator_snapshot_rejects_duplicates_foreign_room_and_malformed(self):
        creator = self.creator()
        for rows in ([creator, creator], [{**creator, "room": "foreign"}],
                     [{key: value for key, value in creator.items() if key != "provider"}]):
            with self.subTest(rows=rows), patch.object(
                    PROOF, "call", return_value=PermissionSnapshotTests.page(rows)):
                with self.assertRaises(RuntimeError):
                    PROOF.native_chat_semantic_publication("socket", "room")
        with patch.object(PROOF, "call", return_value=PermissionSnapshotTests.page([])):
            self.assertIsNone(PROOF.native_chat_semantic_publication("socket", "room"))

    def test_bootstrap_reuses_same_ready_publication_for_creator(self):
        source = inspect.getsource(PROOF.bootstrap)
        start = source.index('category = [row for row in published')
        end = source.index('before_category_frame =', start)
        self.assertNotIn("actions(socket)", source[start:end])
        self.assertIn("if len(category) != 1:", source[start:end])
        self.assertIn('native_chat_semantic_publication(socket, enrollment["room"])', source)


class CoherentAgentCatalogueTests(unittest.TestCase):
    @staticmethod
    def member():
        return {"provider": "desktop", "id": "remove", "application": "so.tilo.desktop",
                "room": "room", "record": "membership", "resource": "desktop:agent:tide",
                "operation": "remove", "semantic_identity": None}

    def test_actual_membership_and_chat_rows_beyond_first_page_are_discovered(self):
        member = self.member()
        message, _queue = PermissionSnapshotTests.offers()
        filler = [{**member, "id": f"other-{index}", "resource": f"other-{index}"}
                  for index in range(200)]
        for target, observe in ((member, lambda: PROOF.offered_membership_action(
                "socket", "room", "desktop:agent:tide", "remove")),
                (message, lambda: PROOF.offered_agent_action(
                    "socket", "room", "record", "tide", "message"))):
            with self.subTest(target=target["id"]), patch.object(PROOF, "call", side_effect=[
                    PermissionSnapshotTests.page(filler, total=201, cursor="next"),
                    PermissionSnapshotTests.page([target], total=201)]) as read:
                self.assertEqual(observe(), target)
                self.assertEqual(read.call_count, 2)
                self.assertEqual(read.call_args_list[1].args[1]["cursor"], "next")

    def test_unsettled_discovery_cannot_prove_absence_and_duplicates_fail(self):
        member = self.member()
        message, _queue = PermissionSnapshotTests.offers()
        for target, observe in ((member, lambda observation: PROOF.offered_membership_action(
                "socket", "room", "desktop:agent:tide", "remove", observation=observation)),
                (message, lambda observation: PROOF.offered_agent_action(
                    "socket", "room", "record", "tide", "message", observation=observation))):
            for flags in ({"complete": False}, {"settled": False}):
                with self.subTest(target=target["id"], flags=flags), patch.object(
                        PROOF, "call", return_value=PermissionSnapshotTests.page([target], **flags)):
                    observation = {}
                    self.assertIsNone(observe(observation))
                    self.assertIs(observation["complete_settled_snapshot"], False)
            with patch.object(PROOF, "call", return_value=PermissionSnapshotTests.page([target, target])):
                with self.assertRaisesRegex(RuntimeError, "ambiguous"):
                    observe({})
            with patch.object(PROOF, "call", return_value=PermissionSnapshotTests.page([])):
                observation = {}
                self.assertIsNone(observe(observation))
                self.assertIs(observation["complete_settled_snapshot"], True)

    def test_failure_evidence_marks_unavailable_and_projects_full_snapshot_safely(self):
        member = {**self.member(), "params": "PRIVATE-CANARY"}
        with patch.object(PROOF, "call", return_value=PermissionSnapshotTests.page([member])):
            receipt = PROOF.native_agent_catalogue_observation("socket", "room")
            self.assertTrue(receipt["complete_settled_snapshot"])
            self.assertEqual(receipt["actions"][0]["id"], "remove")
            self.assertNotIn("PRIVATE-CANARY", json.dumps(receipt))
        for response, failed in ((PermissionSnapshotTests.page([], complete=False), False),
                                 ({"err": {"code": "not_permitted", "message": "PRIVATE-CANARY"}}, True)):
            with patch.object(PROOF, "call", return_value=response):
                self.assertEqual(PROOF.native_agent_catalogue_observation("socket", "room"),
                                 {"complete_settled_snapshot": False,
                                  "observation_failed": failed, "actions": []})

    def test_desktop_and_governance_positive_selectors_read_later_pages(self):
        task = {**self.member(), "id": "read-task", "record": "tasks",
                "resource": "desktop:task:owned", "operation": "read"}
        governance = {**task, "provider": "app-actions", "id": "profile-save", "application": "tilo.workspace",
                      "resource": "workspace.permissions:room", "operation": "profile.save"}
        filler = [{**task, "id": f"other-{index}", "resource": f"other-{index}"}
                  for index in range(200)]
        for target, observe in ((task, lambda: PROOF.offered_desktop_action(
                "socket", "room", "tasks", "desktop:task:owned", "read")),
                (governance, lambda: PROOF.offered_governance_action(
                    "socket", "room", "profile.save"))):
            with self.subTest(target=target["id"]), patch.object(PROOF, "call", side_effect=[
                    PermissionSnapshotTests.page(filler, total=201, cursor="next"),
                    PermissionSnapshotTests.page([target], total=201)]):
                self.assertEqual(observe(), target)
            with patch.object(PROOF, "call", return_value=PermissionSnapshotTests.page([], settled=False)):
                self.assertIsNone(observe())

    def test_governance_requires_exact_broker_provenance_without_builtin_fallback(self):
        broker = {**self.member(), "provider": "app-actions", "id": "broker-save",
                  "application": "tilo.workspace", "resource": "workspace.permissions:room",
                  "operation": "profile.save"}
        builtin = {**broker, "provider": "actions", "id": "builtin-save"}
        for rows, expected in (([builtin, broker], broker), ([broker, builtin], broker),
                               ([builtin], None), ([], None)):
            with self.subTest(rows=rows), patch.object(
                    PROOF, "call", return_value=PermissionSnapshotTests.page(rows)):
                self.assertEqual(PROOF.offered_governance_action("socket", "room", "profile.save"),
                                 expected)
        with patch.object(PROOF, "call", return_value=PermissionSnapshotTests.page([builtin, broker, broker])):
            with self.assertRaisesRegex(RuntimeError, "ambiguous workspace governance"):
                PROOF.offered_governance_action("socket", "room", "profile.save")

    def test_canonical_saved_request_and_reply_read_actual_later_page(self):
        read = {**self.member(), "provider": "chat", "id": "read-messages",
                "application": "so.tilo.chat", "record": "record",
                "resource": "conversation", "operation": "read-messages"}
        request = {"id": "m_00000000000000000000000000000021", "author": "person:fixture",
                   "parent": "", "agentIntent": "message", "text": "PRIVATE-DRAFT", "complete": True}
        reply = {"id": "m_reply", "author": "agent:tide", "parent": request["id"],
                 "agentIntent": None, "text": "Fixture response."}
        filler = [{**self.member(), "id": f"other-{index}"} for index in range(200)]
        for message, observe in ((request, lambda: PROOF.canonical_physical_message(
                "socket", "room", "record", "PRIVATE-DRAFT", None)),
                (reply, lambda: PROOF.agent_reply_observation(
                    "socket", "room", "record", request["id"], "tide", None))):
            with self.subTest(author=message["author"]), patch.object(PROOF, "call", side_effect=[
                    PermissionSnapshotTests.page(filler, total=201, cursor="next"),
                    PermissionSnapshotTests.page([read], total=201)]), \
                    patch.object(PROOF, "invoke", return_value={}) as invoke, \
                    patch.object(PROOF, "wait_terminal", return_value={}), \
                    patch.object(PROOF, "terminal_value", return_value={
                        "content": {"channel": "owned-channel", "messages": [message]}}):
                evidence = observe()
                self.assertIsNotNone(evidence)
                self.assertNotIn("PRIVATE-DRAFT", json.dumps(evidence))
                invoke.assert_called_once_with("socket", read, {"pageSize": 32})
            with patch.object(PROOF, "call", return_value=PermissionSnapshotTests.page([], complete=False)), \
                    patch.object(PROOF, "invoke") as invoke:
                self.assertIsNone(observe())
                invoke.assert_not_called()


class PermissionSnapshotTests(unittest.TestCase):
    @staticmethod
    def page(rows, *, revision="authority", total=None, cursor=None, **extra):
        return {"answer": {"actions": rows, "count": len(rows),
                           "total": len(rows) if total is None else total,
                           "revision": revision, "complete": True, "settled": True,
                           "truncated": cursor is not None, "next_cursor": cursor, **extra}}

    @staticmethod
    def offers():
        return ({"provider": "chat", "id": "message-id", "resource": "agent:tide",
                 "application": "so.tilo.chat", "room": "room", "record": "record",
                 "semantic_identity": {"kind": "chat.agent", "key": "chat.person:tide"},
                 "operation": "message"},
                {"provider": "desktop", "id": "queue-id", "semantic_identity": None,
                 "application": "so.tilo.desktop", "room": "room", "record": "tasks",
                 "resource": "desktop:tasks", "operation": "queue"})

    def test_complete_pair_is_sampled_once_and_independent_absence_is_explicit(self):
        message, queue = self.offers()
        for rows, expected in (([message, queue], (message, queue)),
                               ([queue], (None, queue)), ([message], (message, None)),
                               ([], (None, None))):
            with self.subTest(rows=rows), patch.object(
                    PROOF, "call", return_value=self.page(rows)) as observe:
                self.assertEqual(PROOF.permission_offer_pair("socket", "room", "record"), expected)
                observe.assert_called_once_with("socket", {
                    "op": "actions_enumerate", "query": "", "wait_ms": 250,
                    "limit": 2000, "detail": "compact"})

    def test_incomplete_or_unsettled_listing_is_not_permission_absence(self):
        for extra in ({"complete": False}, {"settled": False}):
            with self.subTest(extra=extra), patch.object(
                    PROOF, "call", return_value=self.page([], **extra)):
                self.assertIsNone(PROOF.permission_offer_pair("socket", "room", "record"))

    def test_later_page_retains_queue_under_the_same_revision(self):
        message, queue = self.offers()
        with patch.object(PROOF, "call", side_effect=[
                self.page([message], total=2, cursor="page-two"),
                self.page([queue], total=2)]) as observe:
            self.assertEqual(PROOF.permission_offer_pair("socket", "room", "record"),
                             (message, queue))
        self.assertEqual(observe.call_args_list[1].args[1]["cursor"], "page-two")
        self.assertNotIn("after_revision", observe.call_args_list[1].args[1])

    def test_changed_revision_total_or_stale_cursor_discards_the_whole_snapshot(self):
        message, queue = self.offers()
        for second in (self.page([queue], revision="changed", total=2),
                       self.page([queue], total=3),
                       {"err": {"code": "stale_cursor"}}):
            with self.subTest(second=second), patch.object(PROOF, "call", side_effect=[
                    self.page([message], total=2, cursor="next"), second]):
                self.assertIsNone(PROOF.permission_actions_snapshot("socket"))

    def test_malformed_metadata_and_other_refusals_fail_closed(self):
        for response in (None, ["PRIVATE"], {"err": ["PRIVATE"]},
                         {"answer": {}}, {"err": {"code": "not_permitted"}},
                         self.page([], complete=None), self.page([], settled=None),
                         self.page([], count=True), self.page([], total=True),
                         self.page([], revision=""), self.page([], next_cursor="unexpected"),
                         self.page(["not a row"]), self.page([], total=1)):
            with self.subTest(response=response), patch.object(PROOF, "call", return_value=response):
                with self.assertRaises(RuntimeError) as refused:
                    PROOF.permission_actions_snapshot("socket")
                self.assertNotIn("PRIVATE", str(refused.exception))

    def test_duplicate_exact_offers_fail_closed(self):
        message, queue = self.offers()
        for rows in ([message, message, queue], [message, queue, queue]):
            with self.subTest(rows=rows), patch.object(PROOF, "call", return_value=self.page(rows)):
                with self.assertRaisesRegex(RuntimeError, "ambiguous permission proof action"):
                    PROOF.permission_offer_pair("socket", "room", "record")

    def test_malformed_matching_owner_rows_cannot_be_treated_as_absent(self):
        message, queue = self.offers()
        malformed = [
            {**message, "semantic_identity": {"kind": "chat.agent", "key": 37}},
            {**message, "semantic_identity": {"kind": "chat.agent"}},
            {**message, "semantic_identity": ["PRIVATE"]},
            {**message, "semantic_identity": {"kind": "chat.agent", "key": "chat.person:tide", "extra": True}},
            {**message, "provider": None}, {**message, "id": ["PRIVATE"]},
            {**message, "application": 37}, {**message, "room": None},
            {field: value for field, value in message.items() if field != "resource"},
        ]
        for row in malformed:
            with self.subTest(row=row), patch.object(PROOF, "call", return_value=self.page([row, queue])):
                with self.assertRaisesRegex(RuntimeError, "identity is unobservable") as refused:
                    PROOF.permission_offer_pair("socket", "room", "record")
                self.assertNotIn("PRIVATE", str(refused.exception))

    def test_nonhierarchical_wire_row_remains_observable(self):
        row = {"provider": "system", "id": "system-action", "semantic_identity": None,
               **{field: None for field in ("application", "room", "record", "resource", "operation")}}
        with patch.object(PROOF, "call", return_value=self.page([row])):
            self.assertEqual(PROOF.permission_offer_pair("socket", "room", "record"), (None, None))

    def test_cursor_loop_and_page_bound_fail_closed(self):
        row = self.offers()[1]
        with patch.object(PROOF, "call", return_value=self.page([row], total=3, cursor="same")):
            with self.assertRaisesRegex(RuntimeError, "continuation is inconsistent"):
                PROOF.permission_actions_snapshot("socket")
        with patch.object(PROOF, "call", side_effect=[
                self.page([row], total=17, cursor=f"next-{n}") for n in range(16)]):
            with self.assertRaisesRegex(RuntimeError, "observation bound"):
                PROOF.permission_actions_snapshot("socket")

    def test_initial_pair_requires_both_offers_without_interpreting_loading_as_absence(self):
        message, queue = self.offers()
        for pair in (None, (None, queue), (message, None), (None, None)):
            with self.subTest(pair=pair), patch.object(PROOF, "permission_offer_pair", return_value=pair):
                self.assertIsNone(PROOF.initial_permission_offer_pair("socket", "room", "record"))


    def test_restriction_proof_requires_the_independent_offer_in_the_same_snapshot(self):
        message, queue = self.offers()
        for scope, pair in (("message", (None, queue)), ("queue", (message, None))):
            with self.subTest(scope=scope), patch.object(PROOF, "permission_offer_pair", return_value=pair):
                self.assertEqual(PROOF.restricted_permission_offer_pair(
                    "socket", "room", "record", scope), pair)
        for scope in ("message", "queue"):
            with self.subTest(scope=scope), patch.object(PROOF, "permission_offer_pair", return_value=None):
                self.assertIsNone(PROOF.restricted_permission_offer_pair(
                    "socket", "room", "record", scope))
            with self.subTest(scope=scope), patch.object(PROOF, "permission_offer_pair", return_value=(None, None)):
                with self.assertRaisesRegex(RuntimeError, "also removed the independent"):
                    PROOF.restricted_permission_offer_pair("socket", "room", "record", scope)
            with self.subTest(scope=scope), patch.object(PROOF, "permission_offer_pair", return_value=(message, queue)):
                self.assertIsNone(PROOF.restricted_permission_offer_pair(
                    "socket", "room", "record", scope))


class RestrictedQueueRoleTests(unittest.TestCase):
    def observe(self, role, *, revoked=False, editable=False, forbidden=None, queue_button=False,
                observation=None):
        client = PROOF.atspi.AtspiClient("address", "owned")
        notice = ("This agent is no longer available here. Your draft is kept on this device."
                  if revoked else "You cannot send messages in this conversation.")
        tree = {"/heading": "(<'Tide, agent'>,)", "/notice": f"(<'{notice}'>,)",
                "/return": "(<'Return conversation'>,)", "/draft": "(<'PRIVATE-DRAFT'>,)",
                "/queue": "(<'Queue task'>,)"}
        if forbidden:
            tree["/mutable"] = f"(<'{forbidden}'>,)"
        if queue_button:
            tree["/queue-button"] = "(<'Queue task'>,)"

        def wire_call(path, method, *args, **kwargs):
            if method == "org.a11y.atspi.Accessible.GetChildren":
                return "['/window']"
            if method == "org.a11y.atspi.Accessible.GetRole":
                if path == "/queue-button":
                    return "(uint32 43,)"
                if isinstance(role, Exception):
                    raise role
                return role
            raise AssertionError("unexpected accessibility wire method")

        with patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF.atspi, "bus_pids", return_value={"owned": 7}), \
                patch.object(PROOF.atspi, "app_process", return_value=7), \
                patch.object(PROOF, "process_program", return_value="chat"), \
                patch.object(client, "call", side_effect=wire_call), \
                patch.object(client, "interfaces", return_value=(
                    "org.a11y.atspi.EditableText" if editable else "([],)")), \
                patch.object(PROOF, "accessible_subtree", return_value=tree):
            return (PROOF.agent_read_only_accessibility("Tide", detached=True) if revoked else
                    PROOF.agent_write_restricted_accessibility(
                        "Tide", "PRIVATE-DRAFT", observation=observation))

    def test_queue_tab_is_only_allowed_in_a_write_restricted_view(self):
        observed = self.observe("(uint32 37,)")
        self.assertTrue(observed["notice_present"])
        self.assertTrue(observed["draft_matches"])
        self.assertEqual(observed["mutable_controls"], 0)
        self.assertEqual(observed["editable_composers"], 0)
        self.assertNotIn("PRIVATE-DRAFT", json.dumps(observed))
        self.assertIsNone(self.observe("(uint32 37,)", revoked=True))

    def test_queue_send_button_and_other_mutating_controls_stay_forbidden(self):
        self.assertIsNone(self.observe("(uint32 43,)"))
        self.assertIsNone(self.observe("(uint32 37,)", queue_button=True))
        for label in ("Send message", "Send direction", "Stop current task"):
            with self.subTest(label=label):
                self.assertIsNone(self.observe("(uint32 37,)", forbidden=label))
        self.assertIsNone(self.observe("(uint32 37,)", editable=True))

    def test_unknown_malformed_and_unreadable_queue_roles_fail_closed(self):
        for role in ("()", "(uint32 0,)", "(uint32 36,)", "('37',)",
                     "(true,)", "PRIVATE malformed role", "(uint32 -37,)"):
            with self.subTest(role=role), self.assertRaisesRegex(RuntimeError, "role is unobservable"):
                self.observe(role)
        with self.assertRaisesRegex(RuntimeError, "role is unobservable") as refused:
            self.observe(PROOF.atspi.GateError("PRIVATE unreadable role"))
        self.assertNotIn("PRIVATE", str(refused.exception))


class PermissionPhaseDiagnosticsTests(unittest.TestCase):
    def test_bounded_samples_record_expiry_phase_timing_without_private_state(self):
        with tempfile.TemporaryDirectory() as directory:
            state = PROOF.permission_phase_observation(pathlib.Path(directory), "chat", 100)
            with patch.object(PROOF.time, "monotonic", side_effect=[1, 1.25, 1.75, 2]), \
                    patch.object(PROOF.time, "time", side_effect=[99, 99.5, 100]):
                with PROOF.permission_phase_sample(state) as sample:
                    sample["PRIVATE-DRAFT"] = "PRIVATE-DRAFT"
                    PROOF.permission_count(sample, "catalogue_complete")
                    sample["message_present"], sample["queue_present"] = False, True
                    PROOF.permission_phase_checkpoint(state, sample, "catalogue")
                    sample["at_attempted"] = True
                    PROOF.permission_phase_checkpoint(state, sample, "at")
            payload = json.loads(state["path"].read_text())
            observed = payload["samples"][0]
            self.assertEqual((observed["catalogue_elapsed_ms"], observed["at_elapsed_ms"],
                              observed["sample_elapsed_ms"]), (250, 500, 1000))
            self.assertTrue(observed["assignment_unexpired_start"])
            self.assertTrue(observed["assignment_unexpired_after_catalogue"])
            self.assertFalse(observed["assignment_unexpired_after_at"])
            self.assertFalse(payload["private_content_recorded"])
            self.assertNotIn("PRIVATE", json.dumps(payload))
            self.assertNotIn("expires", json.dumps(payload))
            for _ in range(35):
                with PROOF.permission_phase_sample(state):
                    pass
            payload = json.loads(state["path"].read_text())
            self.assertEqual(len(payload["samples"]), 32)
            self.assertEqual(payload["samples_seen"], 36)
            self.assertEqual(payload["dropped_samples"], 4)
            self.assertEqual(payload["samples"][0]["catalogue_elapsed_ms"], 250)
            self.assertEqual(payload["totals"]["catalogue_complete"], 1)

    def test_original_refusal_is_preserved_even_when_diagnostic_write_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            state = PROOF.permission_phase_observation(pathlib.Path(directory), "chat", 100)
            original = RuntimeError("original refusal")
            with patch.object(pathlib.Path, "write_text", side_effect=OSError("write failed")):
                with self.assertRaises(RuntimeError) as refused:
                    with PROOF.permission_phase_sample(state):
                        raise original
            self.assertIs(refused.exception, original)

    def test_actual_catalogue_reads_count_complete_incomplete_and_stale_outcomes(self):
        message, queue = PermissionSnapshotTests.offers()
        cases = ((PermissionSnapshotTests.page([queue]), "catalogue_complete"),
                 (PermissionSnapshotTests.page([], complete=False), "catalogue_incomplete"),
                 ({"err": {"code": "stale_cursor"}}, "catalogue_stale"))
        for response, counter in cases:
            observed = {}
            with self.subTest(counter=counter), patch.object(PROOF, "call", return_value=response) as call:
                pair = PROOF.permission_offer_pair("socket", "room", "record", observation=observed)
            self.assertEqual(observed["catalogue_pages"], 1)
            self.assertEqual(observed[counter], 1)
            call.assert_called_once()
            if counter == "catalogue_complete":
                self.assertEqual(pair, (None, queue))
                self.assertFalse(observed["message_present"])
                self.assertTrue(observed["queue_present"])
            else:
                self.assertIsNone(pair)
                self.assertNotIn("message_present", observed)

    def test_actual_read_only_observer_counts_roles_and_retained_matches_without_text(self):
        observed = {}
        result = RestrictedQueueRoleTests().observe("(uint32 37,)", observation=observed)
        self.assertIsNotNone(result)
        self.assertEqual(observed["chat_processes"], 1)
        self.assertEqual(observed["windows"], 1)
        self.assertEqual(observed["headings"], 1)
        self.assertEqual(observed["notices"], 1)
        self.assertEqual(observed["retained"], 1)
        self.assertEqual(observed["queue_tabs"], 1)
        self.assertEqual(observed["interface_queries"], 5)
        self.assertEqual(observed["mutable"], 0)
        self.assertEqual(observed["editable"], 0)
        self.assertNotIn("PRIVATE", json.dumps(observed))

    def test_subtree_instrumentation_counts_existing_reads_only(self):
        client = unittest.mock.Mock()
        client.property.return_value = "(<'PRIVATE'>,)"
        client.interfaces.return_value = "([],)"
        client.call.return_value = "[]"
        observed = {}
        self.assertEqual(PROOF.accessible_subtree(client, "/window", 10, observation=observed),
                         {"/window": "(<'PRIVATE'>,)"})
        self.assertEqual(observed, {"nodes": 1, "interface_queries": 1})
        client.property.assert_called_once_with("/window", "Name")
        client.interfaces.assert_called_once_with("/window")
        client.call.assert_called_once_with("/window", "org.a11y.atspi.Accessible.GetChildren")


class FixtureAppearanceTests(unittest.TestCase):
    def test_normal_settings_merge_preserves_provider_input_and_accessibility(self):
        for theme, mode, contrast in (("light", "light", False),
                                      ("dark", "dark", False),
                                      ("high-contrast", "dark", True)):
            with self.subTest(theme=theme), tempfile.TemporaryDirectory() as directory:
                profile = pathlib.Path(directory)
                path = profile / "config/tilo/settings.json"
                path.parent.mkdir(parents=True)
                original = {"version": 1, "model": {"provider": "preserved"},
                            "input": {"keyboard_layout": "ime/rime"},
                            "appearance": {"mode": "automatic", "ui_scale": 1.25,
                                           "accessibility": {"reduce_motion": True}}}
                path.write_text(json.dumps(original))
                PROOF.configure_fixture_appearance(profile, theme, 2)
                settings = json.loads(path.read_text())
                self.assertEqual(settings["model"], original["model"])
                self.assertEqual(settings["input"], original["input"])
                self.assertEqual(settings["version"], 1)
                self.assertEqual(settings["appearance"]["mode"], mode)
                self.assertEqual(settings["appearance"]["accessibility"], {
                    "schema_version": 1, "high_contrast": contrast,
                    "text_scale": 2.0, "ui_scale": 1.25, "reduce_motion": True})
                self.assertEqual(list(profile.rglob("*.json")), [path])

    def test_defaults_do_not_create_or_rewrite_settings(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = pathlib.Path(directory)
            PROOF.configure_fixture_appearance(profile)
            self.assertEqual(list(profile.iterdir()), [])
            path = profile / "config/tilo/settings.json"
            path.parent.mkdir(parents=True)
            path.write_text('{"version": 1}\n')
            PROOF.configure_fixture_appearance(profile)
            self.assertEqual(path.read_text(), '{"version": 1}\n')

    def test_invalid_requests_and_malformed_settings_never_write(self):
        for invalid in ("nan", "inf", "-inf", "bad", True, False, 0.49, 4.01):
            with self.subTest(scale=invalid):
                with self.assertRaises(PROOF.argparse.ArgumentTypeError):
                    PROOF.fixture_text_scale(invalid)
        for scale in (0.5, 4):
            self.assertEqual(PROOF.fixture_text_scale(str(scale)), float(scale))
        for settings in ([], {"appearance": []},
                         {"appearance": {"accessibility": []}},
                         {"appearance": {"accessibility": {"schema_version": True}}},
                         {"appearance": {"accessibility": {"schema_version": 0}}}):
            with self.subTest(settings=settings), tempfile.TemporaryDirectory() as directory:
                profile = pathlib.Path(directory)
                path = profile / "config/tilo/settings.json"
                path.parent.mkdir(parents=True)
                original = json.dumps(settings)
                path.write_text(original)
                with self.assertRaises(RuntimeError):
                    PROOF.configure_fixture_appearance(profile, "light", 1)
                self.assertEqual(path.read_text(), original)

    def test_cli_projects_public_options_and_preserves_default_behavior(self):
        for arguments, appearance in (([], (None, None)),
                                     (["--theme", "high-contrast", "--text-scale", "2"],
                                      ("high-contrast", 2.0))):
            with tempfile.TemporaryDirectory() as directory, patch.object(
                    sys, "argv", ["fixture", "--binaries", directory] + arguments), \
                    patch.object(PROOF, "evidence_directory", return_value=pathlib.Path(directory)), \
                    patch.object(PROOF, "bootstrap", return_value=True) as bootstrap, \
                    patch("sys.stdout", new=io.StringIO()):
                PROOF.main()
                self.assertEqual(bootstrap.call_args.kwargs["appearance"], appearance)
                manifest = json.loads((pathlib.Path(directory) / "proof-source.json").read_text())
                self.assertEqual(manifest["requested_theme"], appearance[0])
                self.assertEqual(manifest["requested_text_scale"], appearance[1])
                self.assertFalse(manifest["private_content_recorded"])
        source = inspect.getsource(PROOF.bootstrap)
        self.assertLess(source.index('run(binaries / "examples/proof_provider"'),
                        source.index("configure_fixture_appearance(profile, *appearance)"))
        self.assertLess(source.index("configure_fixture_appearance(profile, *appearance)"),
                        source.index("shell_args ="))

    def test_cli_rejects_invalid_appearance_before_running_fixture(self):
        for arguments in (["--theme", "unknown"], ["--text-scale", "nan"],
                          ["--text-scale", "inf"], ["--text-scale", "0.49"],
                          ["--text-scale", "4.01"], ["--text-scale", "true"]):
            with self.subTest(arguments=arguments), patch.object(
                    sys, "argv", ["fixture", "--binaries", "/unused"] + arguments), \
                    patch.object(PROOF, "bootstrap") as bootstrap, \
                    patch.object(PROOF, "evidence_directory") as evidence, \
                    patch("sys.stderr", new=io.StringIO()):
                with self.assertRaises(SystemExit) as refused:
                    PROOF.main()
                self.assertEqual(refused.exception.code, 2)
                bootstrap.assert_not_called()
                evidence.assert_not_called()


class FixtureFailureDiagnosticsTests(unittest.TestCase):
    def test_canonical_read_static_refusal_classes_exclude_private_suffixes(self):
        cases = {
            "workspace action admission timed out": "admission_timeout",
            "workspace action refused: no such action is offered": "unknown_action",
            "workspace action refused: the caller may not invoke this action": "not_permitted",
            "workspace action refused: refused: The document has an uncommitted edit; retry the read": "document_edit_pending",
            "workspace action refused: invalid request: PRIVATE-CANARY": "invalid_request",
            "workspace action refused: refused: PRIVATE-CANARY": "refused",
            "workspace action refused: no such action is offered PRIVATE-CANARY": "other",
        }
        read = {"application": "so.tilo.chat", "room": "private-room", "record": "private-record",
                "operation": "read-messages", "provider": "private-provider", "id": "read"}
        for message, kind in cases.items():
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "permission_actions_snapshot", return_value=[read]), \
                    patch.object(PROOF, "call", return_value={"err": {
                        "code": "action_failed", "message": message}}) as invoke:
                path = pathlib.Path(directory) / "refusal.json"
                with self.assertRaises(PROOF.ActionInvocationRefusal) as raised:
                    PROOF.canonical_physical_message(
                        "socket", "private-room", "private-record", "PRIVATE-DRAFT", None,
                        diagnostic_path=path)
                self.assertEqual(raised.exception.refusal_kind, kind)
                projected = json.loads(path.read_text())
                self.assertEqual(projected["refusal_kind"], kind)
                self.assertNotIn("PRIVATE", path.read_text())
                self.assertNotIn("private-", path.read_text())
                self.assertEqual(invoke.call_count, 1)
        for value in (None, {}, True, "PRIVATE-CANARY"):
            self.assertEqual(PROOF.canonical_read_refusal_kind(value), "other")

    def test_atomic_return_diagnostic_reads_one_seat_and_excludes_payloads(self):
        seat = {"text_input_active": True, "input_method_connected": True,
                "input_method_keyboard_grabbed": True, "input_method_preedit_active": True,
                "keyboard": {"app_id": "other", "id": "PRIVATE-CANARY"},
                "text_input": {"app_id": "so.tilo.chat", "text": "PRIVATE-CANARY"}}
        with patch.object(PROOF, "chrome_state", return_value={"seat_focus": seat}) as read:
            observed = PROOF.closed_atomic_input_method_observation("socket")
        read.assert_called_once_with("socket")
        self.assertTrue(observed["input_method_preedit_accepted"])
        self.assertFalse(observed["keyboard_is_chat"])
        self.assertTrue(observed["text_input_is_chat"])
        self.assertTrue(all(type(value) is bool for value in observed.values()))
        self.assertNotIn("PRIVATE", json.dumps(observed))
        with patch.object(PROOF, "chrome_state", side_effect=RuntimeError("PRIVATE-CANARY")):
            self.assertEqual(PROOF.closed_atomic_input_method_observation("socket"), {"observable": False})

    def test_category_activation_records_refusal_without_replaying_or_changing_result(self):
        for focus, expected_calls in (("(false,)", 1), ("(true,)", 3)):
            client = SimpleNamespace(property=lambda *_args: "(<'PRIVATE-CANARY'>,)")
            with tempfile.TemporaryDirectory() as directory, patch.object(
                    PROOF.atspi, "AtspiClient", return_value=client), \
                    patch.object(PROOF.atspi, "atspi_address", return_value="owned"):
                client.call = unittest.mock.Mock(side_effect=[focus, "[('click',)]", "(false,)"])
                path = pathlib.Path(directory) / "category.json"
                with self.assertRaises(RuntimeError):
                    PROOF.activate_accessibility_control(
                        {"bus": "private-bus", "control": "/private-path"},
                        "Show conversation navigation", diagnostic_path=path)
                observed = json.loads(path.read_text())
                self.assertTrue(observed["identity_observable"])
                self.assertFalse(observed["expected_name_matches"])
                self.assertEqual(observed["focus_accepted"], focus == "(true,)")
                self.assertEqual(client.call.call_count, expected_calls)
                self.assertNotIn("PRIVATE", path.read_text())
                self.assertNotIn("private-", path.read_text())

    def test_revocation_float_observes_fresh_control_without_replacing_captured_target(self):
        owner = {"bus": "private-bus", "control": "/old", "process": 7,
                 "agent": "Tide", "detached": False}
        current = {**owner, "control": "/new"}
        client = SimpleNamespace(property=lambda *_args: "(<'Float conversation'>,)")
        with tempfile.TemporaryDirectory() as directory, patch.object(
                PROOF, "current_owned_conversation_accessibility", return_value=current), \
                patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                patch.object(PROOF.atspi, "atspi_address", return_value="owned"), \
                patch.object(PROOF, "call") as input_call:
            observed = PROOF.retain_float_control_observation(owner, pathlib.Path(directory))
            self.assertTrue(observed["same_owned_route"])
            self.assertTrue(observed["expected_name_matches"])
            self.assertFalse(observed["same_control_path"])
            self.assertEqual(owner["control"], "/old")
            self.assertNotIn("private-", json.dumps(observed))
            input_call.assert_not_called()
        source = inspect.getsource(PROOF.native_agent_membership_revocation)
        self.assertIn('diagnostic_name="revocation-float-native-observations.json"', source)
        self.assertEqual(source.count('activate_agent_conversation_control(access, "Float")'), 1)


class PresentedEditorTargetTests(unittest.TestCase):
    def test_pre_float_baseline_waits_for_exact_caret_focus_and_refuses_reacquire_mutation(self):
        for changed, false_field in (({"draft_matches": False}, "content_matches"),
                                     ({"caret": 0}, "caret_equal"),
                                     ({"selection": None}, "selection_equal"),
                                     ({"accessibility_focused": False}, "accessibility_focus_equal")):
            final_reads = []
            access = {"bus": "owned", "process": 42, "composer": "/editor", "drag": "/float"}

            def editor(_access, expected, **_kwargs):
                final = expected.endswith("!")
                width = 1 if final or expected.endswith("👍") else 5
                exact = {"draft_matches": True, "caret": len(expected) - width,
                         "selection": [len(expected) - width, len(expected)],
                         "accessibility_focused": True}
                if not final:
                    return exact
                final_reads.append(expected)
                if len(final_reads) == 1:
                    return {**exact, "caret": len(expected)}
                if len(final_reads) == 2:
                    return {**exact, "accessibility_focused": False}
                if len(final_reads) == 3:
                    return exact
                return {**exact, **changed, "private": "PRIVATE-CANARY"}

            def observe(_label, predicate, _process):
                for _ in range(4):
                    result = predicate()
                    if result:
                        return result
                self.fail("the actual production observer did not reach its expected state")

            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as directory, ExitStack() as stack:
                output = pathlib.Path(directory)
                stack.enter_context(patch.object(PROOF, "wait_for", side_effect=observe))
                stack.enter_context(patch.object(PROOF, "agent_conversation_accessibility", return_value=access))
                stack.enter_context(patch.object(PROOF, "presented_primary_frame", return_value={"frame_seq": 1}))
                stack.enter_context(patch.object(PROOF, "presented_content_geometry", return_value={}))
                for name in ("physically_click_editor", "require_chat_keyboard_focus_after_click", "require_ok"):
                    stack.enter_context(patch.object(PROOF, name))
                stack.enter_context(patch.object(PROOF, "editor_accessibility_focus", return_value=True))
                stack.enter_context(patch.object(PROOF, "private_editor_value_matches", return_value=True))
                stack.enter_context(patch.object(PROOF, "private_editor_observation", side_effect=editor))
                selection = stack.enter_context(patch.object(
                    PROOF, "wait_for_private_selection", wraps=PROOF.wait_for_private_selection))
                controls = stack.enter_context(patch.object(PROOF, "call", return_value={"ok": {}}))
                with self.assertRaisesRegex(RuntimeError, "refreshing Float changed private editor state"):
                    PROOF.native_float_return("socket", "room", "record", output, "1600x900", None)
                self.assertEqual(len(final_reads), 4)
                final_selection = selection.call_args_list[-1]
                draft = final_selection.args[2]
                self.assertEqual(final_selection.kwargs,
                                 {"expected_caret": len(draft) - 1, "require_focused": True})
                self.assertEqual(final_selection.args[3], [len(draft) - 1, len(draft)])
                receipt = json.loads((output / "float-reacquire-editor-continuity.json").read_text())
                self.assertEqual(set(receipt), {"content_matches", "caret_equal", "selection_equal",
                    "accessibility_focus_equal", "accessibility_focused_before",
                    "accessibility_focused_after", "private_content_recorded"})
                self.assertTrue(all(type(value) is bool for value in receipt.values()))
                self.assertIs(receipt[false_field], False)
                self.assertIs(receipt["private_content_recorded"], False)
                self.assertNotIn(draft, json.dumps(receipt))
                self.assertNotIn("PRIVATE-CANARY", json.dumps(receipt))
                self.assertFalse(any(entry.args[1].get("op") == "drag" for entry in controls.call_args_list))

    def test_managed_profile_availability_observes_only_exact_fixed_public_state(self):
        windows = [{"address": "owned", "bus": ":1.1", "root": "/workspace"}]

        class Client:
            def __init__(self, description):
                self.description = description

            def named(self, tree, name):
                return [path for path, value in tree.items()
                        if PROOF.atspi.property_text(value) == name]

            def property(self, _path, name):
                if name != "Description":
                    raise AssertionError("unexpected profile property")
                return self.description

        for label, expected in [
                ("Available for tasks", "available"),
                ("Not currently available for tasks", "unavailable"),
                ("Task availability unknown", "unknown")]:
            with self.subTest(state=expected), patch.object(
                    PROOF.atspi, "AtspiClient", return_value=Client(f"(<'{label}'>,)")), \
                    patch.object(PROOF, "accessible_subtree", return_value={
                        "/other": "(<'reed'>,)", "/exact": "(<'tide'>,)"}):
                self.assertEqual(PROOF.agent_profile_task_availability(
                    "tide", windows=windows), expected)
        with patch.object(PROOF.atspi, "AtspiClient", return_value=Client(
                "(<'Available for tasks'>,)")), patch.object(
                PROOF, "accessible_subtree", return_value={"/other": "(<'reed'>,)"}):
            self.assertIsNone(PROOF.agent_profile_task_availability("tide", windows=windows))

    def test_managed_profile_availability_refuses_ambiguity_without_recording_description(self):
        windows = [{"address": "owned", "bus": ":1.1", "root": "/workspace"}]

        class Client:
            def named(self, _tree, _name):
                return ["/exact"]

            def property(self, _path, _name):
                return "(<'PRIVATE-DESCRIPTION Available for tasks. Task availability unknown'>,)"

        with patch.object(PROOF.atspi, "AtspiClient", return_value=Client()), \
                patch.object(PROOF, "accessible_subtree", return_value={}), \
                self.assertRaises(RuntimeError) as failed:
            PROOF.agent_profile_task_availability("tide", windows=windows)
        self.assertNotIn("PRIVATE-DESCRIPTION", str(failed.exception))

    def test_detached_restoration_requires_both_current_controls_without_repair(self):
        owner = {"process": 7, "bus": "owned", "agent": "Tide", "detached": True, "window": "/window"}
        baseline = {"draft_matches": True, "caret": 3, "selection": [3, 7]}
        expected = {"identity": "replacement", "access": owner, "draft": "PRIVATE-DRAFT", "observation": baseline}
        child = {"app_id": "so.tilo.chat", "floating_id": "PRIVATE-CHILD", "surface_id": 9,
                 "presented": True, "suspended": False, "transferring": False}
        process = type("Process", (), {"pid": 7, "poll": lambda self: None})()
        provider = type("Provider", (), {"request_count": lambda self: 4, "overflow_count": lambda self: 0})()
        trees = [{"/send": "(<'Send message'>,)"}, {"/queue": "(<'Queue task'>,)"},
                 {"/send": "(<'Send message'>,)", "/queue": "(<'Queue task'>,)"}]
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "owned_agent_identity", return_value="replacement"), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=owner), \
                patch.object(PROOF, "private_editor_observation", return_value=baseline) as state, \
                patch.object(PROOF, "call", return_value={"answer": {"frames": [child]}}) as calls, \
                patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF, "accessible_subtree", side_effect=trees), \
                patch.object(PROOF.time, "sleep"):
            output = pathlib.Path(directory)
            result = PROOF.wait_for_restored_detached_editor("socket", expected, child, provider, process, output)
            self.assertEqual(result, (owner, baseline, {"Send message", "Queue task"}))
            self.assertEqual(state.call_count, 3)
            self.assertEqual(calls.call_args_list, [call("socket", {"op": "foreign_frames"})] * 3)
            receipt = json.loads((output / "restored-detached-control-observations.json").read_text())
            self.assertEqual(receipt["initial"]["queue_control_count"], 0)
            self.assertEqual(receipt["last"]["queue_control_count"], 1)
            self.assertNotIn("PRIVATE", json.dumps(receipt))
        for route, observation, native, tree in (
                ({**owner, "process": 8}, baseline, child, trees[-1]),
                (owner, {**baseline, "draft_matches": False}, child, trees[-1]),
                (owner, {**baseline, "caret": 4}, child, trees[-1]),
                (owner, {**baseline, "selection": None}, child, trees[-1]),
                (owner, baseline, {**child, "surface_id": 99}, trees[-1]),
                (owner, baseline, {**child, "presented": None}, trees[-1]),
                (owner, baseline, child, {"/bad": "bare malformed name"}),
                (owner, baseline, child, {**trees[-1], "/extra": "(<'Queue task'>,)"})):
            with tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "owned_agent_identity", return_value="replacement"), \
                    patch.object(PROOF, "agent_conversation_accessibility", return_value=route), \
                    patch.object(PROOF, "private_editor_observation", return_value=observation), \
                    patch.object(PROOF, "call", return_value={"answer": {"frames": [native]}}), \
                    patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                    patch.object(PROOF, "accessible_subtree", return_value=tree):
                with self.assertRaises(RuntimeError):
                    PROOF.wait_for_restored_detached_editor(
                        "socket", expected, child, provider, process, pathlib.Path(directory))

    def test_detached_restoration_persistent_missing_controls_keeps_original_deadline(self):
        owner = {"process": 7, "bus": "owned", "agent": "Tide", "detached": True, "window": "/window"}
        baseline = {"draft_matches": True, "caret": 3, "selection": [3, 7]}
        expected = {"identity": "replacement", "access": owner, "draft": "PRIVATE", "observation": baseline}
        child = {"app_id": "so.tilo.chat", "floating_id": "PRIVATE", "surface_id": 9,
                 "presented": True, "suspended": False, "transferring": False}
        process = type("Process", (), {"pid": 7, "poll": lambda self: None})()
        provider = type("Provider", (), {"request_count": lambda self: 4, "overflow_count": lambda self: 0})()
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "owned_agent_identity", return_value="replacement"), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=owner), \
                patch.object(PROOF, "private_editor_observation", return_value=baseline), \
                patch.object(PROOF, "call", return_value={"answer": {"frames": [child]}}), \
                patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF, "accessible_subtree", return_value={}), \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 10, 20, 30]):
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_restored_detached_editor(
                    "socket", expected, child, provider, process, pathlib.Path(directory))

    def test_keyboard_float_raw_diagnostic_does_not_accept_filtered_capture(self):
        previous = {"app_id": "so.tilo.chat", "floating_id": "PRIVATE", "surface_id": 9, "frame_seq": 10}
        primary = {"app_id": "so.tilo.chat", "floating_id": None, "surface_id": 7,
                   "keyboard_focused": True}
        pending = {**previous, "frame_seq": 11, "presented": True, "pixels_ready": False,
                   "capture_pending": True, "suspended": False, "transferring": False}
        fresh = {**pending, "surface_id": 12, "frame_seq": 1, "pixels_ready": True, "capture_pending": False}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "call", side_effect=[{"answer": {"frames": [pending, primary]}},
                    {"answer": {"frames": [fresh, primary]}}]) as calls, \
                patch.object(PROOF.time, "sleep"):
            output = pathlib.Path(directory)
            self.assertEqual(PROOF.wait_for_keyboard_float_presentation(
                "socket", output, previous, primary, None), fresh)
            receipt = json.loads((output / "keyboard-float-native-observations.json").read_text())
            self.assertTrue(receipt["initial"]["child_capture_pending"])
            self.assertFalse(receipt["initial"]["child_pixels_ready"])
            self.assertFalse(receipt["last"]["same_native_child"])
            self.assertIsNone(receipt["last"]["child_frame_fresh"])
            self.assertNotIn("PRIVATE", json.dumps(receipt))
            self.assertEqual(calls.call_args_list, [call("socket", {"op": "foreign_frames"})] * 2)
        for invalid in ({**fresh, "floating_id": "foreign"}, {**fresh, "presented": None}, pending):
            with tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "call", return_value={"answer": {"frames": [invalid, primary]}}), \
                    patch.object(PROOF.time, "sleep"), \
                    patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 10, 20, 30]):
                with self.assertRaises(PROOF.ProofTimeout):
                    PROOF.wait_for_keyboard_float_presentation(
                        "socket", pathlib.Path(directory), previous, primary, None)

    def test_restored_return_checks_exact_semantic_name_before_one_action(self):
        access = {"bus": "owned", "control": "/return"}
        client = PROOF.atspi.AtspiClient("address", "owned")
        stale = PROOF.atspi.GateError("known retired object")
        for name in ("(<'Queue task'>,)", "bare malformed name", stale):
            with self.subTest(name=type(name).__name__), \
                    patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                    patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                    patch.object(client, "call", return_value="(['click'],)") as actions, \
                    patch.object(client, "property", side_effect=name if isinstance(name, Exception) else None,
                                 return_value=name):
                with self.assertRaises((RuntimeError, PROOF.atspi.GateError)):
                    PROOF.activate_agent_conversation_control(
                        access, "Return", expected_name="Return conversation")
                self.assertEqual(actions.call_args_list, [
                    call("/return", "org.a11y.atspi.Action.GetActions", check=False)])
        with patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                patch.object(client, "call", side_effect=["(['click'],)", "(true,)"]) as actions, \
                patch.object(client, "property", return_value="(<'Return conversation'>,)"):
            PROOF.activate_agent_conversation_control(access, "Return", expected_name="Return conversation")
            self.assertEqual(actions.call_args_list, [
                call("/return", "org.a11y.atspi.Action.GetActions", check=False),
                call("/return", "org.a11y.atspi.Action.DoAction", "0", check=False)])

    def test_restored_return_fresh_owner_and_native_withdrawal_are_exact_and_closed(self):
        owner = {"process": 7, "bus": "owned", "agent": "Tide", "detached": True}
        before = {"draft_matches": True, "caret": 3, "selection": [3, 7]}
        for current in (None, {**owner, "process": 8}, {**owner, "bus": "foreign"}):
            with patch.object(PROOF, "agent_conversation_accessibility", return_value=current), \
                    patch.object(PROOF, "private_editor_observation") as read:
                with self.assertRaises(RuntimeError):
                    PROOF.current_restored_return_access(owner, "PRIVATE-DRAFT", before)
                read.assert_not_called()
        child = {"app_id": "so.tilo.chat", "floating_id": "PRIVATE-CHILD", "surface_id": 9,
                 "presented": True, "suspended": False, "transferring": False}
        primary = {"app_id": "so.tilo.chat", "floating_id": None, "surface_id": 7, "presented": True}
        def response(rows):
            return {"ok": True, "frames": rows}
        with patch.object(PROOF, "foreign_frames", side_effect=lambda value: value["frames"]):
            initial = PROOF.restored_return_observation(response([child, primary]), child, primary)
            pending = {**child, "presented": False, "suspended": True, "transferring": True}
            with tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "call", return_value=response([pending, primary])) as calls:
                output = pathlib.Path(directory)
                result = PROOF.wait_for_restored_return_withdrawal(
                    "socket", output, child, primary, None, initial)
                self.assertTrue(result["withdrawn"])
                self.assertTrue(result["child_return_transfer_pending"])
                receipt = json.loads((output / "revocation-return-withdrawal-observations.json").read_text())
                self.assertNotIn("PRIVATE", json.dumps(receipt))
                calls.assert_called_once_with("socket", {"op": "foreign_frames"})
            for rows in ([child], [{**child, "surface_id": 99}, primary],
                         [{**child, "suspended": None}, primary]):
                with self.assertRaises(RuntimeError):
                    PROOF.restored_return_observation(response(rows), child, primary)

    def test_canonical_read_refusal_records_only_closed_class_then_rethrows_without_retry(self):
        read = {"application": "so.tilo.chat", "room": "room", "record": "record",
                "operation": "read-messages", "provider": "PRIVATE-PROVIDER", "id": "PRIVATE-ID"}
        for code, expected in (("not_permitted", "not_permitted"), ("PRIVATE-ERROR", "other")):
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "permission_actions_snapshot", return_value=[read]), \
                    patch.object(PROOF, "call", return_value={"err": {
                        "code": code, "message": "PRIVATE-PROMPT"}}) as calls:
                path = pathlib.Path(directory) / "refusal.json"
                with self.assertRaises(PROOF.ActionInvocationRefusal) as raised:
                    PROOF.canonical_physical_message("socket", "room", "record", "PRIVATE-DRAFT", None,
                                                     diagnostic_path=path)
                self.assertEqual(raised.exception.closed_code, expected)
                receipt = json.loads(path.read_text())
                self.assertEqual(receipt["refusal_code"], expected)
                self.assertTrue(receipt["read_messages_operation"])
                self.assertTrue(receipt["exact_route_matches"])
                self.assertNotIn("PRIVATE", json.dumps(receipt))
                self.assertEqual(calls.call_count, 1)

    def test_selection_requires_focus_only_when_explicitly_requested(self):
        pending = {"selection": [4, 9], "caret": 4, "draft_matches": True,
                   "accessibility_focused": False}
        ready = {**pending, "accessibility_focused": True}
        with patch.object(PROOF, "private_editor_observation", side_effect=[pending, ready]) as read, \
                patch.object(PROOF.time, "sleep"), patch.object(PROOF, "call") as calls:
            self.assertEqual(PROOF.wait_for_private_selection(
                "socket", {}, "fixture", [4, 9], None, pathlib.Path("unused"),
                require_focused=True), ready)
            self.assertEqual(read.call_count, 2)
            calls.assert_not_called()
        with patch.object(PROOF, "private_editor_observation", return_value=pending):
            self.assertEqual(PROOF.wait_for_private_selection(
                "socket", {}, "fixture", [4, 9], None, pathlib.Path("unused")), pending)

    def test_owned_progress_reads_scheduled_telemetry_without_input_or_accessibility(self):
        process = type("ManagedProcess", (), {"pid": 123})()
        latest = {"app": "so.tilo.chat", "agent_message_progress": [{"phase": "committed"}]}
        with patch.object(PROOF, "owned_agent_identity", return_value="actual-agent"), \
                patch.object(PROOF, "chat_process_identity", return_value=(7, "started")), \
                patch.object(PROOF, "latest_chat_input_state", return_value=latest) as telemetry, \
                patch.object(PROOF, "call") as inputs, \
                patch.object(PROOF, "agent_conversation_accessibility") as accessibility:
            self.assertEqual(PROOF.owned_chat_message_progress(
                {"process": 7}, (7, "started"), process, "actual-agent", pathlib.Path("unused")), latest)
            telemetry.assert_called_once()
            inputs.assert_not_called()
            accessibility.assert_not_called()
        for agent, chat in (("replacement", (7, "started")), ("actual-agent", (7, "new-start"))):
            with self.subTest(agent=agent, chat=chat), \
                    patch.object(PROOF, "owned_agent_identity", return_value=agent), \
                    patch.object(PROOF, "chat_process_identity", return_value=chat), \
                    patch.object(PROOF, "latest_chat_input_state") as telemetry:
                with self.assertRaisesRegex(RuntimeError, "replaced its actual owner"):
                    PROOF.owned_chat_message_progress(
                        {"process": 7}, (7, "started"), process, "actual-agent", pathlib.Path("unused"))
                telemetry.assert_not_called()

    def test_restored_offers_wait_for_both_in_same_catalogue_and_reject_state_changes(self):
        process = type("ManagedProcess", (), {"pid": 123, "poll": lambda self: None})()
        provider = type("Provider", (), {"request_count": lambda self: 4,
                                         "overflow_count": lambda self: 0})()
        owner = {"process": 7, "bus": "owned", "agent": "Tide", "detached": False}
        before = {"draft_matches": True, "caret": 3, "selection": [3, 7]}
        expected = {"identity": "agent", "access": owner, "draft": "PRIVATE-DRAFT", "observation": before}
        message = {"application": "so.tilo.chat", "room": "room", "record": "record",
                   "semantic_identity": {"kind": "chat.agent", "key": "chat.person:tide"},
                   "operation": "message"}
        queue = {"application": "so.tilo.desktop", "room": "room", "record": "tasks",
                 "resource": "desktop:tasks", "operation": "queue"}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "owned_agent_identity", return_value="agent"), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=owner), \
                patch.object(PROOF, "private_editor_observation", return_value=before) as read, \
                patch.object(PROOF, "permission_actions_snapshot", side_effect=[[message], [queue], [message, queue]]) as catalogue, \
                patch.object(PROOF.time, "sleep"), patch.object(PROOF, "invoke") as invokes:
            output = pathlib.Path(directory)
            self.assertEqual(PROOF.wait_for_restored_agent_offers(
                "socket", "room", "record", provider, process, expected, output), owner)
            self.assertEqual(read.call_count, 3)
            self.assertEqual(catalogue.call_count, 3)
            invokes.assert_not_called()
            receipt = json.loads((output / "restored-agent-offer-observations.json").read_text())
            self.assertFalse(receipt["initial"]["queue_offer_present"])
            self.assertTrue(receipt["last"]["message_offer_present"])
            self.assertTrue(receipt["last"]["queue_offer_present"])
            self.assertNotIn("PRIVATE-DRAFT", json.dumps(receipt))
        for identity, access, observation in (
                ("replacement", owner, before), ("agent", {**owner, "process": 8}, before),
                ("agent", owner, {**before, "draft_matches": False}),
                ("agent", owner, {**before, "caret": 4}),
                ("agent", owner, {**before, "selection": None})):
            with tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "owned_agent_identity", return_value=identity), \
                    patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                    patch.object(PROOF, "private_editor_observation", return_value=observation), \
                    patch.object(PROOF, "permission_actions_snapshot") as catalogue:
                with self.assertRaises(RuntimeError):
                    PROOF.wait_for_restored_agent_offers(
                        "socket", "room", "record", provider, process, expected, pathlib.Path(directory))
                catalogue.assert_not_called()
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "owned_agent_identity", return_value="agent"), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=owner), \
                patch.object(PROOF, "private_editor_observation", return_value=before), \
                patch.object(PROOF, "permission_actions_snapshot", return_value=[message]), \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 10, 20, 30]):
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_restored_agent_offers(
                    "socket", "room", "record", provider, process, expected, pathlib.Path(directory))

    def test_attached_float_control_requires_actual_focus_and_valid_current_target(self):
        owner = {"process": 7, "bus": "owned", "agent": "Tide", "detached": False, "drag": "/old"}
        current = {**owner, "drag": "/current"}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=current), \
                patch.object(PROOF, "_editor_accessibility_focus", side_effect=[False, True]) as read, \
                patch.object(PROOF.time, "sleep"):
            self.assertEqual(PROOF.wait_for_conversation_control_focus(
                owner, pathlib.Path(directory), None, detached=False, control="drag"), current)
            read.assert_called_with({"bus": "owned", "composer": "/current"})
            receipt = json.loads((pathlib.Path(directory) / "keyboard-float-control-focus.json").read_text())
            self.assertFalse(receipt["last"]["requested_control_matches_current"])
            self.assertTrue(receipt["last"]["control_focused"])
            self.assertNotIn("/current", json.dumps(receipt))
            self.assertNotIn('"owned"', json.dumps(receipt))
        for changed in ({"process": 8}, {"drag": None}, {"detached": True}):
            with tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "agent_conversation_accessibility", return_value={**current, **changed}), \
                    patch.object(PROOF, "_editor_accessibility_focus") as read:
                with self.assertRaises(RuntimeError):
                    PROOF.wait_for_conversation_control_focus(
                        owner, pathlib.Path(directory), None, detached=False, control="drag")
                read.assert_not_called()
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=current), \
                patch.object(PROOF, "_editor_accessibility_focus", return_value=False), \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 10, 20, 30]):
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_conversation_control_focus(
                    owner, pathlib.Path(directory), None, detached=False, control="drag")

    def test_accessible_names_decodes_serialized_wire_labels_and_refuses_malformed(self):
        tree = {"/send": "(<'Send message'>,)", "/queue": "(<'Queue task'>,)"}
        with patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF, "accessible_subtree", return_value=tree):
            self.assertEqual(PROOF.accessible_names({"bus": "owned", "window": "/window"}),
                             {"Send message", "Queue task"})
        for malformed in ("Send message", "(uint32 7,)", "PRIVATE malformed reply"):
            with self.subTest(malformed=malformed), \
                    patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                    patch.object(PROOF, "accessible_subtree", return_value={"/node": malformed}):
                with self.assertRaisesRegex(RuntimeError, "node name is unobservable") as error:
                    PROOF.accessible_names({"bus": "owned", "window": "/window"})
                self.assertNotIn(malformed, str(error.exception))

    def test_read_only_and_restricted_views_cannot_hide_serialized_mutable_controls(self):
        client = PROOF.atspi.AtspiClient("address", "owned")
        revoked = "This agent is no longer available here. Your draft is kept on this device."
        restricted = "You cannot send messages in this conversation."
        for mode, notice in (("revoked", revoked), ("restricted", restricted)):
            base = {"/heading": "(<'Tide, agent'>,)", "/notice": f"(<'{notice}'>,)",
                    "/return": "(<'Return conversation'>,)", "/draft": "(<'PRIVATE-DRAFT'>,)"}
            for forbidden in (None, "Send message", "Queue task", "Send direction", "Stop current task"):
                tree = dict(base)
                if forbidden is not None:
                    tree["/mutable"] = f"(<'{forbidden}'>,)"
                with self.subTest(mode=mode, forbidden=forbidden), \
                        patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                        patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                        patch.object(PROOF.atspi, "bus_pids", return_value={"owned": 7}), \
                        patch.object(PROOF.atspi, "app_process", return_value=7), \
                        patch.object(PROOF, "process_program", return_value="chat"), \
                        patch.object(client, "call", return_value="['/window']"), \
                        patch.object(client, "interfaces", return_value="([],)"), \
                        patch.object(client, "role", return_value=PROOF.AT_SPI_ROLE_BUTTON), \
                        patch.object(PROOF, "accessible_subtree", return_value=tree):
                    result = PROOF.agent_read_only_accessibility("Tide", detached=True) \
                        if mode == "revoked" else PROOF.agent_write_restricted_accessibility(
                            "Tide", "PRIVATE-DRAFT")
                    self.assertEqual(result is not None, forbidden is None)
                    if result is not None:
                        self.assertEqual(result["mutable_controls"], 0)
                        self.assertNotIn("PRIVATE-DRAFT", json.dumps(result))
            with self.subTest(mode=mode, malformed=True), \
                    patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                    patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                    patch.object(PROOF.atspi, "bus_pids", return_value={"owned": 7}), \
                    patch.object(PROOF.atspi, "app_process", return_value=7), \
                    patch.object(PROOF, "process_program", return_value="chat"), \
                    patch.object(client, "call", return_value="['/window']"), \
                    patch.object(PROOF, "accessible_subtree", return_value={**base, "/bad": "bare name"}):
                with self.assertRaisesRegex(RuntimeError, "node name is unobservable"):
                    if mode == "revoked":
                        PROOF.agent_read_only_accessibility("Tide", detached=True)
                    else:
                        PROOF.agent_write_restricted_accessibility("Tide", "PRIVATE-DRAFT")

    def test_managed_agent_restart_failure_path_reaches_original_terminal_observation(self):
        offered = {"operation": "start-agent"}
        failed = {"update": {"invocation": "public-start", "state": "failed",
                             "error": {"code": "public-launch-refused"}}}
        acknowledgement = {"receipt": failed}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "offered_desktop_action", return_value=offered), \
                patch.object(PROOF, "invoke", return_value=acknowledgement) as invoke, \
                patch.object(PROOF, "call") as status:
            path = pathlib.Path(directory) / "failed-agent-tide-restart-terminal.json"
            with self.assertRaisesRegex(RuntimeError, "ordinary action terminated failed"):
                PROOF.start_managed_agent("socket", "room", "tide", None,
                                          public_failure_path=path)
            self.assertEqual(json.loads(path.read_text()), failed)
            invoke.assert_called_once_with("socket", offered, {"agent": "tide"})
            status.assert_not_called()
            path.unlink()
            with self.assertRaisesRegex(RuntimeError, "ordinary action terminated failed"):
                PROOF.start_managed_agent("socket", "room", "tide", None)
            self.assertFalse(path.exists())

    def test_return_control_focus_observes_current_bit_after_known_stale_object(self):
        owner = {"process": 7, "bus": "owned", "agent": "Tide",
                 "detached": True, "control": "/retired"}
        current = {**owner, "control": "/current"}
        stale = PROOF.atspi.GateError(
            "call failed: Error: GDBus.Error:org.freedesktop.DBus.Error.UnknownObject: PRIVATE")
        client = PROOF.atspi.AtspiClient("address", "owned")
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "agent_conversation_accessibility",
                             side_effect=[owner, current, current]), \
                patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(client, "call", side_effect=[stale, "([uint32 0, 0],)",
                    f"([uint32 {PROOF.atspi.FOCUSED_STATE_BIT}, 0],)"]) as read, \
                patch.object(PROOF.time, "sleep"):
            output = pathlib.Path(directory)
            self.assertEqual(PROOF.wait_for_floating_return_control_focus(
                owner, output, None), current)
            self.assertEqual(read.call_args_list, [
                call("/retired", "org.a11y.atspi.Accessible.GetState"),
                call("/current", "org.a11y.atspi.Accessible.GetState"),
                call("/current", "org.a11y.atspi.Accessible.GetState")])
            receipt = json.loads((output / "keyboard-return-control-focus.json").read_text())
            self.assertEqual(receipt["observation_count"], 3)
            self.assertTrue(receipt["initial"]["known_stale_object"])
            self.assertTrue(receipt["last"]["control_focused"])
            self.assertNotIn("PRIVATE", json.dumps(receipt))
            self.assertNotIn("/current", json.dumps(receipt))

    def test_return_control_missing_focus_expires_and_foreign_owner_refuses(self):
        owner = {"process": 7, "bus": "owned", "agent": "Tide",
                 "detached": True, "control": "/return"}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=owner), \
                patch.object(PROOF, "_editor_accessibility_focus", return_value=False) as read, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 10, 20, 30]):
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_floating_return_control_focus(owner, pathlib.Path(directory), None)
            self.assertEqual(read.call_count, 3)
        for changed in ({"process": 8}, {"bus": "foreign"},
                        {"agent": "Other"}, {"detached": False}):
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "agent_conversation_accessibility",
                                 return_value={**owner, **changed}), \
                    patch.object(PROOF, "_editor_accessibility_focus") as read:
                with self.assertRaisesRegex(RuntimeError, "changed the owned Chat route"):
                    PROOF.wait_for_floating_return_control_focus(owner, pathlib.Path(directory), None)
                read.assert_not_called()
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=owner), \
                patch.object(PROOF, "_editor_accessibility_focus",
                             side_effect=PROOF.atspi.GateError("unrelated refusal")):
            with self.assertRaises(PROOF.atspi.GateError):
                PROOF.wait_for_floating_return_control_focus(owner, pathlib.Path(directory), None)

    def test_ime_keyboard_return_and_revocation_use_existing_native_continuity_gates(self):
        source = inspect.getsource(PROOF.native_ime_float_return)
        branch = source[source.index('"keyboard-activated Rime proof Float"'):]
        before = branch.index("wait_for_floating_keyboard_seat(")
        focus = branch.index("focus_agent_conversation_control(")
        after = branch.index("wait_for_floating_keyboard_seat(", before + 1)
        observed = branch.index("wait_for_floating_return_control_focus(")
        enter = branch.index('"key": "Enter"')
        self.assertLess(before, focus)
        self.assertLess(focus, after)
        self.assertLess(after, observed)
        self.assertLess(observed, enter)
        revocation = inspect.getsource(PROOF.native_agent_membership_revocation)
        self.assertLess(revocation.index("wait_for_semantic_float_editor_continuity("),
                        revocation.index("if detached_before != before:"))
        self.assertLess(revocation.index('"revocation-float-editor-continuity.json"'),
                        revocation.index("if detached_before != before:"))

    def test_message_phase_reads_only_exact_request_description(self):
        access = {"bus": "owned", "window": "/window"}
        client = PROOF.atspi.AtspiClient("address", "owned")
        for name, description, expected in (
                ("Person: PRIVATE-DRAFT", "Waiting for reply", True),
                ("Person: OTHER-DRAFT", "Waiting for reply", False),
                ("Person: PRIVATE-DRAFT", "Waiting for reply. Other status", False),
                ("Person: PRIVATE-DRAFT", "Publishing reply", False)):
            with self.subTest(name=name, description=description), \
                    patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                    patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                    patch.object(PROOF, "accessible_subtree",
                                 return_value={"/messages": "(<'Messages'>,)"}), \
                    patch.object(client, "call", return_value="['/request']"), \
                    patch.object(client, "property", side_effect=lambda path, field:
                                 f"(<'{name if field == 'Name' else description}'>,)") as read:
                self.assertEqual(PROOF.message_phase_accessibility(
                    access, "PRIVATE-DRAFT", "Waiting for reply"), expected)
                if name == "Person: OTHER-DRAFT":
                    self.assertEqual(read.call_args_list, [call("/request", "Name")])

    def test_message_phase_duplicate_request_refuses_and_only_known_stale_retries(self):
        access = {"process": 7, "bus": "owned", "agent": "Tide",
                  "detached": False, "window": "/window"}
        client = PROOF.atspi.AtspiClient("address", "owned")
        tree = {"/messages": "(<'Messages'>,)"}
        stale = PROOF.atspi.GateError(
            "call failed: Error: GDBus.Error:org.freedesktop.DBus.Error.UnknownObject: retired")
        with patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "accessible_subtree", side_effect=[stale, tree]), \
                patch.object(client, "call", return_value="['/request']"), \
                patch.object(client, "property", side_effect=[
                    "(<'Person: PRIVATE-DRAFT'>,)", "(<'Waiting for reply'>,)"]), \
                patch.object(PROOF.time, "sleep"):
            self.assertEqual(PROOF.wait_for_current_message_phase_accessibility(
                access, "PRIVATE-DRAFT", "Waiting for reply", None), access)
        with patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF, "accessible_subtree", return_value=tree), \
                patch.object(client, "call", return_value="['/request', '/duplicate']"), \
                patch.object(client, "property", return_value="(<'Person: PRIVATE-DRAFT'>,)") as read:
            with self.assertRaisesRegex(RuntimeError, "duplicate saved request rows"):
                PROOF.message_phase_accessibility(access, "PRIVATE-DRAFT", "Waiting for reply")
            self.assertTrue(all(args.args[1] == "Name" for args in read.call_args_list))
        with patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF, "accessible_subtree",
                             side_effect=PROOF.atspi.GateError("unrelated transport refusal")):
            with self.assertRaises(PROOF.atspi.GateError):
                PROOF.message_phase_accessibility(access, "PRIVATE-DRAFT", "Waiting for reply")

    def test_current_message_phase_refreshes_retired_accessibility_snapshot(self):
        owner = {"process": 7, "bus": "owned", "agent": "Tide",
                 "detached": False, "window": "retired"}
        current = {**owner, "window": "current"}
        with patch.object(PROOF, "agent_conversation_accessibility",
                          side_effect=[owner, current]) as discover, \
                patch.object(PROOF, "message_phase_accessibility",
                             side_effect=[False, True]) as phase, \
                patch.object(PROOF.time, "sleep"):
            self.assertEqual(PROOF.wait_for_current_message_phase_accessibility(
                owner, "PRIVATE-DRAFT", "Waiting for reply", None), current)
            self.assertEqual(discover.call_args_list, [call("Tide"), call("Tide")])
            self.assertEqual(phase.call_args_list,
                             [call(owner, "PRIVATE-DRAFT", "Waiting for reply"),
                              call(current, "PRIVATE-DRAFT", "Waiting for reply")])

    def test_current_message_phase_missing_label_expires_without_skipping(self):
        owner = {"process": 7, "bus": "owned", "agent": "Tide", "detached": False}
        with patch.object(PROOF, "agent_conversation_accessibility", return_value=owner), \
                patch.object(PROOF, "message_phase_accessibility", return_value=False) as phase, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 10, 20, 30]):
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_current_message_phase_accessibility(
                    owner, "PRIVATE-DRAFT", "Waiting for reply", None)
            self.assertEqual(phase.call_count, 3)

    def test_current_message_phase_refuses_foreign_route_before_label_read(self):
        owner = {"process": 7, "bus": "owned", "agent": "Tide", "detached": False}
        for changed in ({"process": 8}, {"bus": "foreign"},
                        {"agent": "Other"}, {"detached": True}):
            with self.subTest(changed=changed), \
                    patch.object(PROOF, "agent_conversation_accessibility",
                                 return_value={**owner, **changed}), \
                    patch.object(PROOF, "message_phase_accessibility") as phase:
                with self.assertRaisesRegex(RuntimeError, "changed the owned Chat route"):
                    PROOF.wait_for_current_message_phase_accessibility(
                        owner, "PRIVATE-DRAFT", "Waiting for reply", None)
                phase.assert_not_called()

    def test_physical_message_admission_observes_lagging_canonical_projection(self):
        process = type("ManagedProcess", (), {"pid": 123, "poll": lambda self: None})()
        identity = {"private_identity": "PRIVATE-CANARY"}
        original = {"invocation": "PRIVATE-ORIGINAL", "request_row": "PRIVATE-ROW"}
        with patch.object(PROOF, "owned_agent_identity", return_value=identity) as owner, \
                patch.object(PROOF, "canonical_physical_message",
                             side_effect=[None, None, original]) as read, \
                patch.object(PROOF.time, "sleep"):
            self.assertEqual(PROOF.wait_for_physical_message_admission(
                "socket", "room", "record", "PRIVATE-DRAFT", process, identity), original)
            self.assertEqual(owner.call_count, 3)
            self.assertEqual(read.call_count, 3)
            read.assert_called_with("socket", "room", "record", "PRIVATE-DRAFT", process)

    def test_physical_message_admission_rejects_replaced_owner_before_read(self):
        process = type("ManagedProcess", (), {"pid": 123, "poll": lambda self: None})()
        identity = {"owner": "original"}
        with patch.object(PROOF, "owned_agent_identity",
                          side_effect=[identity, {"owner": "replacement"}]), \
                patch.object(PROOF, "canonical_physical_message", return_value=None) as read, \
                patch.object(PROOF.time, "sleep"):
            with self.assertRaisesRegex(RuntimeError, "replaced the managed Agent"):
                PROOF.wait_for_physical_message_admission(
                    "socket", "room", "record", "PRIVATE-DRAFT", process, identity)
            self.assertEqual(read.call_count, 1)

    def test_physical_message_admission_expires_without_fabricating_admission(self):
        process = type("ManagedProcess", (), {"pid": 123, "poll": lambda self: None})()
        identity = {"owner": "original"}
        with patch.object(PROOF, "owned_agent_identity", return_value=identity) as owner, \
                patch.object(PROOF, "canonical_physical_message", return_value=None) as read, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 10, 20, 30]):
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_physical_message_admission(
                    "socket", "room", "record", "PRIVATE-DRAFT", process, identity)
            self.assertEqual(owner.call_count, 3)
            self.assertEqual(read.call_count, 3)

    def test_native_marker_bootstrap_verdict_requires_three_completed_turns(self):
        # Execute the actual bootstrap entry point and its persisted verdict.
        # Native startup is stubbed here; its boundary is exercised separately
        # by the real packaged proof, not by spawning services in a unit test.
        for count, activated, accessible, status, expected in (
                (3, True, True, "done", True),
                (1, True, True, "done", False),
                (4, True, True, "done", False),
                (3, False, True, "done", False),
                (3, True, False, "done", False),
                (3, True, True, "cancelled", False)):
            with self.subTest(count=count, activated=activated,
                              accessible=accessible, status=status), \
                    tempfile.TemporaryDirectory() as directory, ExitStack() as stack:
                profile = pathlib.Path(directory) / "profile"
                profile.mkdir()
                output = pathlib.Path(directory) / "output"
                output.mkdir()
                frame = {"surface_id": 7, "frame_seq": 9}
                category = {"application": "so.tilo.chat", "room": "room",
                            "record": "record", "operation": "new-category"}
                action = {**category, "provider": "broker:1", "id": "category"}
                evidence = {"provider_request_count": count,
                            "marker_activated": activated,
                            "marker_pointer_activated": activated,
                            "quick_composer_accessible": accessible,
                            "terminal_status": status}

                def wait(label, _predicate, *_args):
                    if label == "bounded signed native governor fixture":
                        (profile / "native-invitation.json").write_text(
                            json.dumps({"empty_app_records": True}))
                        return True
                    if label == "Chat's ordinary semantic publication":
                        return [action]
                    if label == "actual durable Category and exact outgoing publication":
                        return {"category_durable": True,
                                "outgoing_publication_retained": True,
                                "room": "room", "app": "so.tilo.chat", "record": "record"}
                    if label in {"normal signed Chat to paint",
                                 "Category edit's actual presented Chat frame",
                                 "agent conversation's actual presented Chat frame"}:
                        return frame
                    return action

                def finalize(_profile, _output, _processes, logs, _provider,
                             runtime_owner, _fcitx):
                    for log in logs:
                        log.close()
                    runtime_owner.cleanup()

                replacements = {
                    "scratch_session_env": ({"TILO_STATE_PATH": "owned"}, profile),
                    "enroll_native": {"room": "room"},
                    "actions": [action],
                    "presented_primary_frame": frame,
                    "call": {"answer": {"throwaway": True}},
                    "invoke": {}, "wait_terminal": {},
                    "admit_native_agents": [{"agent": "tide"}],
                    "offered_agent_action": action,
                    "public_action_observation": {},
                    "managed_agent_marker_stage": evidence,
                }
                for name, value in replacements.items():
                    stack.enter_context(patch.object(PROOF, name, return_value=value))
                for name in ("enable_isolated_accessibility", "start_isolated_secret_service",
                             "require_locked_cargo_metadata", "install_packages", "run",
                             "capture_owned_chat_accessibility_transport", "require_ok"):
                    stack.enter_context(patch.object(PROOF, name))
                stack.enter_context(patch.object(PROOF, "HeldProvider"))
                stack.enter_context(patch.object(PROOF, "wait_for", side_effect=wait))
                stack.enter_context(patch.object(PROOF, "finalize_fixture", side_effect=finalize))
                stack.enter_context(patch.object(PROOF.subprocess, "Popen"))
                stack.enter_context(patch.object(
                    PROOF.subprocess, "check_output", return_value="so.tilo.chat so.tilo.agent"))
                stack.enter_context(patch.object(PROOF.time, "sleep"))
                stack.enter_context(patch("sys.stdout", new=io.StringIO()))
                proven = PROOF.bootstrap(pathlib.Path(directory), output,
                                         native=True, marker_only=True)
                retained = json.loads((output / "bootstrap.json").read_text())
                self.assertIs(proven, expected)
                self.assertIs(retained["native_agent_marker_proven"], expected)
                self.assertEqual(retained["stage"], "native-marker")

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
        with patch.object(PROOF, "permission_actions_snapshot", return_value=[{}, opposite, read]), \
                patch.object(PROOF, "chat_process_identity", return_value=(7, "start")), \
                patch.object(PROOF.time, "monotonic", return_value=100):
            observed = PROOF.observe_hidden_permission_offer(
                "socket", original, opposite, "open-agent-conversation",
                access, access, (7, "start"),
            )
        self.assertEqual(observed.original, PROOF.permission_offer_identity(original))
        self.assertEqual(observed.observed_at, 100)
        for catalogue in (None, [original, opposite, read], [read], [opposite],
                          [opposite, {**read, "room": "foreign"}],
                          [opposite, {**read, "resource": "other"}],
                          [opposite, {**read, "provider": "foreign"}],
                          [opposite, read, read]):
            with self.subTest(catalogue=catalogue), \
                    patch.object(PROOF, "permission_actions_snapshot", return_value=catalogue), \
                    patch.object(PROOF, "chat_process_identity", return_value=(7, "start")), \
                    self.assertRaises(RuntimeError):
                PROOF.observe_hidden_permission_offer(
                    "socket", original, opposite, "open-agent-conversation",
                    access, access, (7, "start"),
                )
        with patch.object(PROOF, "permission_actions_snapshot") as catalogue, self.assertRaises(RuntimeError):
            PROOF.observe_hidden_permission_offer(
                "socket", original, opposite, "open-agent-conversation",
                access, {**access, "window": "/other"}, (7, "start"),
            )
        catalogue.assert_not_called()
        with patch.object(PROOF, "permission_actions_snapshot") as catalogue, \
                patch.object(PROOF, "chat_process_identity", return_value=(7, "replaced")), \
                self.assertRaises(RuntimeError):
            PROOF.observe_hidden_permission_offer(
                "socket", original, opposite, "open-agent-conversation",
                access, access, (7, "start"),
            )
        catalogue.assert_not_called()
        private = "private-catalogue-error"
        with patch.object(PROOF, "permission_actions_snapshot", side_effect=RuntimeError(private)), \
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
                patch.object(PROOF, "call", return_value={"ok": {}}) as input_call, \
                patch.object(PROOF, "wait_for_private_selection", return_value=selected) as observe:
            self.assertEqual(PROOF.prepare_private_permission_draft(
                "socket", access, "abcdef", 4, output, None,
            ), selected)
        selection_input = [
            row.args[1] for row in input_call.call_args_list
            if row.args[1]["op"] in {"key_down", "key", "key_up"}
        ]
        self.assertEqual(selection_input, [
            {"op": "key_down", "key": "Shift"},
            *({"op": "key", "key": "ArrowLeft"} for _ in range(4)),
            {"op": "key_up", "key": "Shift"},
        ])
        observe.assert_called_once_with("socket", access, "abcdef", [2, 6], None, output)

    def test_physical_chord_holds_real_modifiers_and_always_releases_in_reverse(self):
        events = []

        def record(_socket, request):
            events.append(request)
            return {"ok": {}}

        with patch.object(PROOF, "call", side_effect=record):
            PROOF.physical_chord(
                "socket", "x", ctrl=True, shift=True, alt=True, logo=True,
            )
        self.assertEqual(events, [
            {"op": "key_down", "key": "Control"},
            {"op": "key_down", "key": "Shift"},
            {"op": "key_down", "key": "Alt"},
            {"op": "key_down", "key": "Super"},
            {"op": "key", "key": "x"},
            {"op": "key_up", "key": "Super"},
            {"op": "key_up", "key": "Alt"},
            {"op": "key_up", "key": "Shift"},
            {"op": "key_up", "key": "Control"},
        ])

        events.clear()

        def fail_key(_socket, request):
            events.append(request)
            if request["op"] == "key":
                return {"err": {"code": "fixture_failure"}}
            return {"ok": {}}

        with patch.object(PROOF, "call", side_effect=fail_key), \
                self.assertRaises(RuntimeError):
            PROOF.physical_chord("socket", "x", ctrl=True, shift=True)
        self.assertEqual(events[-2:], [
            {"op": "key_up", "key": "Shift"},
            {"op": "key_up", "key": "Control"},
        ])

        events.clear()

        def fail_first_release(_socket, request):
            events.append(request)
            if request == {"op": "key_up", "key": "Shift"}:
                return {"err": {"code": "fixture_failure"}}
            return {"ok": {}}

        with patch.object(PROOF, "call", side_effect=fail_first_release), \
                self.assertRaises(RuntimeError):
            PROOF.physical_chord("socket", "x", ctrl=True, shift=True)
        self.assertEqual(events[-2:], [
            {"op": "key_up", "key": "Shift"},
            {"op": "key_up", "key": "Control"},
        ])

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

        for app in ("so.tilo.chat", "so.tilo.desktop"):
            calls.clear()
            with self.subTest(app=app), patch.object(PROOF, "wait_for", side_effect=[save, assign]), \
                    patch.object(PROOF, "invoke", side_effect=invoke), \
                    patch.object(PROOF, "wait_terminal", side_effect=lambda _s, row, _p: row), \
                    patch.object(PROOF, "terminal_invocation", side_effect=["saved", "assigned"]), \
                    patch.object(PROOF.time, "time", return_value=100):
                result = PROOF.assign_expiring_app_write_restriction(
                    "socket", "room", "b" * 64, app, "profile", None
                )
            self.assertEqual(len(calls), 2)
            self.assertEqual(calls[0], (save, {
                "name": "profile", "description": "Native independent app permission proof",
                "rules": [{"scope": {"App": app}, "effect": "Restrict", "verb": "Read"}],
                "confirm": True,
            }))
            self.assertEqual(calls[1], (assign, {
                "profile": "profile", "subject_kind": "person", "subject": "b" * 64,
                "expires_unix": 112, "confirm": True,
            }))
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

    def test_marker_binding_requires_actual_category_extent_intersection(self):
        class Client:
            def __init__(self, rect):
                self.rect = rect

            def call(self, _path, _interface, _coord_type):
                return f"(({', '.join(str(value) for value in self.rect)}),)"

        geometry = {
            "origin": {"x": 100.0, "y": 200.0},
            "content": {"x": 100.0, "y": 200.0, "w": 400.0, "h": 300.0},
            "placement": {"x": 90.0, "y": 190.0, "w": 420.0, "h": 320.0},
        }
        category = {"bus": "chat", "control": "/category"}
        marker = {"bus": "shell", "control": "/marker"}

        def client(_address, bus):
            return Client((40, 20, 60, 30) if bus == "chat" else (155, 225, 22, 20))

        with patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF.atspi, "AtspiClient", side_effect=client):
            bound = PROOF.marker_binds_presented_category(marker, category, geometry)
        self.assertEqual(bound, {
            "marker_rect": {"x": 155, "y": 225, "w": 22, "h": 20},
            "category_rect": {"x": 140.0, "y": 220.0, "w": 60, "h": 30},
            "category_bound": True,
            "private_content_recorded": False,
        })

        def separated(_address, bus):
            return Client((40, 20, 60, 30) if bus == "chat" else (300, 400, 22, 20))

        with patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF.atspi, "AtspiClient", side_effect=separated):
            refused = PROOF.marker_binds_presented_category(marker, category, geometry)
        self.assertEqual(refused, {
            "marker_rect": {"x": 300, "y": 400, "w": 22, "h": 20},
            "category_rect": {"x": 140.0, "y": 220.0, "w": 60, "h": 30},
            "category_bound": False,
            "private_content_recorded": False,
        })

    def test_automatic_steer_requires_one_exact_owner_frontier_and_visible_status_without_picker(self):
        task = {"task_id": "owner-task", "run_epoch": 7, "after_revision": 3,
                "title": "Agent marker proof", "status": "in_progress",
                "active_agent": "tide", "assigned_agent": "tide"}
        current = {"complete": True, "tasks": [task]}
        self.assertEqual(PROOF.automatic_single_task_status({"task": None}, current, task, "tide"),
                         "Steer current task: Agent marker proof")
        invalid = [{"complete": False, "tasks": [task]}, {"complete": True, "tasks": []},
                   {"complete": True, "tasks": [task, task]}]
        for changed in ({"task_id": "foreign"}, {"run_epoch": 8}, {"after_revision": 4},
                        {"title": "Different task"}, {"active_agent": "other"},
                        {"assigned_agent": "other"}, {"status": "done"}):
            invalid.append({"complete": True, "tasks": [{**task, **changed}]})
        for value in invalid:
            with self.subTest(value=value), self.assertRaises(RuntimeError):
                PROOF.automatic_single_task_status({"task": None}, value, task, "tide")
        with self.assertRaisesRegex(RuntimeError, "reconfirmation"):
            PROOF.automatic_single_task_status({"task": "/actual-picker"}, current, task, "tide")
        source = inspect.getsource(PROOF.exercise_agent_quick_composer)
        binding = source.index("automatic_single_task_status(access, current_tasks(), before, agent)")
        visible = source.index("quick_composer_steer_ready(", binding)
        send = source.index('"name": "agent-quick-send"', visible)
        self.assertLess(binding, visible)
        self.assertLess(visible, send)
        self.assertIn("applied_revision == receipt_revision", source)

    def test_physical_steer_requires_same_popup_status_draft_and_enabled_send(self):
        access = {"bus": "owned", "root": "/popup", "composer": "/editor",
                  "controls": {"send": "/send"}}
        for status, draft, enabled, expected in (
                ("owner-status", True, True, True),
                (None, True, True, False),
                ("owner-status", False, True, False),
                ("owner-status", True, False, False)):
            with self.subTest(status=status, draft=draft, enabled=enabled), \
                    patch.object(PROOF, "quick_composer_status_at", return_value=status) as label, \
                    patch.object(PROOF, "private_editor_value_matches", return_value=draft) as value, \
                    patch.object(PROOF, "accessibility_control_enabled", return_value=enabled) as control:
                self.assertEqual(PROOF.quick_composer_steer_ready(
                    access, "owner-status", "PRIVATE-DRAFT-CANARY"), expected)
                label.assert_called_once_with(access, {"owner-status"})
                if status == "owner-status":
                    value.assert_called_once_with(access, "PRIVATE-DRAFT-CANARY")
                if status == "owner-status" and draft:
                    control.assert_called_once_with(access, "/send")
        source = inspect.getsource(PROOF.exercise_agent_quick_composer)
        self.assertLess(source.index("access = wait_for(\"automatic Steer's exact interactive"),
                        source.index('"name": "agent-quick-send"'))

    def test_steer_failure_capture_is_closed_and_does_not_resubmit(self):
        before = {"task_id": "PRIVATE-ID", "run_epoch": 7, "after_revision": 3,
                  "title": "PRIVATE-TITLE"}
        access = {"controls": {"send": "/send"}}
        operations = []
        def actual_call(_socket, operation):
            operations.append(operation["op"])
            return {"ok": True}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "agent_quick_composer_accessibility", return_value=access), \
                patch.object(PROOF, "accessibility_control_enabled", return_value=False), \
                patch.object(PROOF, "private_editor_value_matches", return_value=False), \
                patch.object(PROOF, "quick_composer_status_at",
                             return_value="This action is unavailable. Your draft is still here."), \
                patch.object(PROOF, "quick_composer_targets"), \
                patch.object(PROOF, "call", side_effect=actual_call):
            output = pathlib.Path(directory)
            PROOF.capture_steer_receipt_failure("socket", "tide",
                PROOF.closed_steer_owner_observation({**before, "status": "failed"}, before),
                before, output, "1600x900")
            raw = (output / "agent-steer-receipt-failed.json").read_text()
            observed = json.loads(raw)
        self.assertEqual(operations, ["list_targets", "snapshot"])
        self.assertEqual(observed["status"], "refused")
        self.assertFalse(observed["send_enabled"])
        self.assertFalse(observed["draft_empty"])
        self.assertFalse(observed["task_active"])
        self.assertTrue(observed["same_task"])
        self.assertTrue(observed["revision_equal"])
        self.assertFalse(observed["revision_advanced"])
        self.assertEqual(observed["physical_send_count"], 1)
        self.assertNotIn("PRIVATE", raw)
        self.assertNotIn("/send", raw)

    def test_steer_both_owner_waits_capture_transport_failure_and_rethrow_original(self):
        for stage in ("receipt", "applied"):
            for error in (TimeoutError("PRIVATE-ERROR"), OSError("PRIVATE-ERROR"),
                          PROOF.ProofTimeout("PRIVATE-ERROR")):
                captures = []
                calls = []
                def owner_read():
                    calls.append(stage)
                    raise error
                def diagnostic():
                    captures.append(stage)
                    raise OSError("PRIVATE-DIAGNOSTIC")
                with self.subTest(stage=stage, kind=type(error)), \
                        self.assertRaises(type(error)) as caught:
                    PROOF.wait_steer_owner_observation(stage, owner_read, None, diagnostic)
                self.assertIs(caught.exception, error)
                self.assertEqual(calls, [stage])
                self.assertEqual(captures, [stage])
        source = inspect.getsource(PROOF.exercise_agent_quick_composer)
        self.assertIn('lambda: capture_failure("receipt")', source)
        self.assertIn('lambda: capture_failure("applied")', source)
        self.assertIn("applied_revision == receipt_revision", source)
        self.assertNotIn("exact_task", inspect.getsource(PROOF.capture_steer_receipt_failure))
        with patch.object(PROOF, "wait_for", return_value="actual-result") as wait:
            capture = unittest.mock.Mock()
            predicate = lambda: "actual-result"
            self.assertEqual(PROOF.wait_steer_owner_observation(
                "applied", predicate, None, capture), "actual-result")
            wait.assert_called_once_with("applied", predicate, None)
            capture.assert_not_called()

    def test_steer_applied_stall_receipt_retains_only_cached_closed_owner_evidence(self):
        before = {"task_id": "PRIVATE-ID", "title": "PRIVATE-TITLE", "run_epoch": 7,
                  "after_revision": 3}
        last_owner = PROOF.closed_steer_owner_observation({
            **before, "status": "in_progress", "after_revision": 4, "applied_revision": 3,
            "instruction": "PRIVATE-PROMPT"}, before, 4)
        self.assertTrue(last_owner["revision_advanced"])
        self.assertTrue(last_owner["revision_matches_receipt"])
        self.assertFalse(last_owner["applied_matches_receipt"])
        operations = []
        def stalled_call(_socket, operation):
            operations.append(operation["op"])
            raise TimeoutError("PRIVATE-SOCKET")
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "agent_quick_composer_accessibility", return_value=None), \
                patch.object(PROOF, "call", side_effect=stalled_call):
            output = pathlib.Path(directory)
            PROOF.capture_steer_receipt_failure("socket", "tide", last_owner, before,
                                               output, "1600x900", stage="applied")
            raw = (output / "agent-steer-receipt-failed.json").read_text()
            observed = json.loads(raw)
            with self.assertRaisesRegex(RuntimeError, "malformed"):
                PROOF.capture_steer_receipt_failure("socket", "tide",
                    {**last_owner, "private": "PRIVATE-CANARY"}, before, output, "1600x900")
        self.assertEqual(operations, ["list_targets", "snapshot"])
        self.assertEqual(observed["failed_stage"], "applied")
        self.assertTrue(observed["owner_observation_is_cached"])
        self.assertTrue(observed["revision_advanced"])
        self.assertFalse(observed["applied_matches_receipt"])
        self.assertFalse(observed["snapshot_captured"])
        self.assertNotIn("PRIVATE", raw)

    def test_prime_chat_frontier_restarts_only_stale_same_owned_tree_under_original_deadline(self):
        access = {"process": 123, "agent": "Tide", "detached": False,
                  "bus": "chat", "window": "/current"}
        stale = PROOF.atspi.GateError(
            "gdbus failed: Error: GDBus.Error:org.freedesktop.DBus.Error.UnknownObject: PRIVATE-CANARY")
        tree = {"/phase": "(<'Planning: Agent marker proof'>,)"}
        client = PROOF.atspi.AtspiClient("owned", "chat")
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "process_program", return_value="chat"), \
                patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                patch.object(PROOF.atspi, "atspi_address", return_value="owned"), \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF, "accessible_subtree", side_effect=[stale, tree]):
            output = pathlib.Path(directory)
            prepared = PROOF.prime_chat_frontier_access("Agent marker proof", None, output=output)
            self.assertEqual(prepared["phase_control"], "/phase")
            receipt = json.loads((output / "marker-chat-prime-observation.json").read_text())
            self.assertEqual(receipt["probe_count"], 2)
            self.assertTrue(receipt["first"]["unknown_object"])
            self.assertTrue(receipt["last"]["unique_planning_control"])
            self.assertNotIn("PRIVATE-CANARY", json.dumps(receipt))
            self.assertNotIn("/phase", json.dumps(receipt))
            with patch.object(PROOF, "accessible_subtree", side_effect=stale), \
                    patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 10, 20, 30]):
                with self.assertRaises(PROOF.ProofTimeout):
                    PROOF.prime_chat_frontier_access("Agent marker proof", None, output=output)
            with patch.object(PROOF, "accessible_subtree", side_effect=[stale, tree]), \
                    patch.object(PROOF, "agent_conversation_accessibility",
                                 side_effect=[access, {**access, "process": 456}]):
                with self.assertRaisesRegex(RuntimeError, "foreign Chat route"):
                    PROOF.prime_chat_frontier_access("Agent marker proof", None)
            with patch.object(PROOF, "accessible_subtree", return_value={}):
                with self.assertRaisesRegex(RuntimeError, "ambiguous or absent"):
                    PROOF.prime_chat_frontier_access("Agent marker proof", None)
            denied = PROOF.atspi.GateError("org.freedesktop.DBus.Error.AccessDenied PRIVATE-CANARY")
            with patch.object(PROOF, "accessible_subtree", side_effect=denied):
                with self.assertRaises(PROOF.atspi.GateError):
                    PROOF.prime_chat_frontier_access("Agent marker proof", None, output=output)
            receipt = json.loads((output / "marker-chat-prime-observation.json").read_text())
            self.assertTrue(receipt["last"]["other_transport_failure"])
            self.assertNotIn("PRIVATE-CANARY", json.dumps(receipt))

    def test_quick_composer_targets_use_actual_unwrapped_wire_and_real_control_geometry(self):
        names = ("agent-quick-editor", "agent-quick-send", "agent-quick-close")
        rows = [{"name": name, "rect": [10.0, 20.0, 100.0, 30.0]} for name in names]
        response = {"targets": rows + [{"name": "PRIVATE-CANARY", "rect": [0, 0, 1, 1]}]}
        self.assertEqual(PROOF.quick_composer_targets(response), {
            name: [10.0, 20.0, 100.0, 30.0] for name in names})
        invalid = [{"ok": response}, {"targets": "PRIVATE-CANARY"},
                   {"targets": rows[:-1]}, {"targets": rows + [rows[0]]}]
        for rect in (None, [0, 0, 0, 30], [0, 0, 30, -1], [True, 0, 30, 30],
                     [0, float("nan"), 30, 30], [0, 0, 30], {"x": 0}):
            invalid.append({"targets": [{"name": names[0], "rect": rect}, *rows[1:]],
                            "private": "PRIVATE-CANARY"})
            invalid.append({"targets": [{"name": names[0], "rect": rect}, *rows[1:]]})
        for value in invalid:
            with self.subTest(value=value), self.assertRaises(RuntimeError) as refused:
                PROOF.quick_composer_targets(value)
            self.assertNotIn("PRIVATE-CANARY", str(refused.exception))
            self.assertNotIn("targets", str(refused.exception))
        source = inspect.getsource(PROOF.exercise_agent_quick_composer)
        self.assertIn('quick_composer_targets(call(socket, {"op": "list_targets"}))', source)
        self.assertNotIn('require_ok(call(socket, {"op": "list_targets"}))', source)
        self.assertIn('for intent in ("message", "queue", "steer"):', source)

    def test_return_editor_continuity_reports_only_actual_field_equalities(self):
        before = {"draft_matches": True, "caret": 8, "selection": [7, 8],
                  "accessibility_focused": True, "private_content": "PRIVATE-CANARY"}
        good = PROOF.closed_editor_continuity(before, before.copy())
        self.assertTrue(all(good.values()))
        for changed, field in (({"draft_matches": False}, "content_matches"),
                               ({"caret": 7}, "caret_equal"),
                               ({"selection": None}, "selection_equal"),
                               ({"accessibility_focused": False}, "accessibility_focus_equal")):
            with self.subTest(changed=changed):
                observed = PROOF.closed_editor_continuity(before, {**before, **changed})
                self.assertFalse(observed[field])
                self.assertTrue(all(type(value) is bool for value in observed.values()))
                self.assertNotIn("PRIVATE-CANARY", json.dumps(observed))
                self.assertNotIn("[7", json.dumps(observed))
        source = inspect.getsource(PROOF.native_float_return)
        receipt = source.index('"return-editor-continuity.json"')
        strict = source.index("if attached != before_return_editor:", receipt)
        refusal = source.index('"Return changed private editor content, caret, selection or keyboard focus"', strict)
        self.assertLess(receipt, strict)
        self.assertLess(strict, refusal)

    def test_quick_composer_observation_counts_actual_wire_controls_without_private_names(self):
        wire_named = PROOF.atspi.AtspiClient.named
        tree = {f"/node/{index}": f"(<'{label}'>,)" for index, label in enumerate((
            "Message", "Queue", "Steer", "Send", "Close", "Open conversation",
            "Open Task Center", "Message or instruction", "tide", "Agent", "PRIVATE-CANARY"))}
        class Client:
            def __init__(self, *_args):
                pass
            named = staticmethod(wire_named)
            def call(self, *_args):
                return "([(':1.1', '/quick')],)"
            def property(self, *_args):
                return "(<'Agent quick composer'>,)"
            def interfaces(self, *_args):
                return "(['org.a11y.atspi.Text', 'org.a11y.atspi.EditableText'],)"
        windows = [{"address": "owned", "bus": ":1.1", "window": "/window", "root": "/workspace"}]
        with patch.object(PROOF.atspi, "AtspiClient", Client), \
                patch.object(PROOF, "accessible_subtree", return_value=tree):
            observed = {}
            self.assertIsNotNone(PROOF.agent_quick_composer_accessibility(
                "tide", windows=windows, observation=observed))
            self.assertEqual(observed["composer_root_count"], 1)
            self.assertEqual(observed["editor_count"], 1)
            self.assertEqual(observed["agent_count"], 1)
            self.assertEqual(observed["kind_count"], 1)
            self.assertEqual(observed["task_count"], 0)
            self.assertTrue(all(count == 1 for count in observed["controls"].values()))
            self.assertNotIn("PRIVATE-CANARY", json.dumps(observed))
            self.assertNotIn("/node", json.dumps(observed))
            missing = {path: name for path, name in tree.items() if "'Queue'" not in name}
            with patch.object(PROOF, "accessible_subtree", return_value=missing):
                self.assertIsNone(PROOF.agent_quick_composer_accessibility(
                    "tide", windows=windows, observation=observed))
            self.assertEqual(observed["controls"]["queue"], 0)
            self.assertEqual(observed["controls"]["open_tasks"], 1)
            self.assertEqual(observed["match_count"], 0)
        stage = inspect.getsource(PROOF.managed_agent_marker_stage)
        activate = stage.index('activate_shell_accessibility_control(marker, marker["label"])')
        snapshot = stage.index('agent-action-marker-activated-', activate)
        wait = stage.index("reopen_marker_composer_while_live(", activate)
        self.assertLess(wait, snapshot)

    def test_marker_direct_children_decode_wire_names_and_refuse_ambiguity(self):
        class Client:
            def __init__(self, names):
                self.names = names
                self.walks = []

            def call(self, path, interface):
                self.walks.append((path, interface))
                return "([" + ", ".join(f"(':1.1', '{node}')" for node in self.names) + "],)"

            def property(self, path, _name):
                return self.names[path]

        windows = [{"address": "owned", "bus": ":1.1", "window": "/window", "root": "/workspace"}]
        for names, expected in (
                ({"/content": "(<'Other control'>,)",
                  "/marker": "(<'Agent action: tide · Agent'>,)"}, "/marker"),
                ({"/marker": "(<'Agent action: reed · Agent'>,)"}, None),
                ({"/marker": "Agent action: tide · Agent"}, None)):
            client = Client(names)
            with self.subTest(names=names), \
                    patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                    patch.object(PROOF, "accessible_subtree", side_effect=AssertionError("full walk")), \
                    patch.object(PROOF, "owned_shell_accessibility_windows",
                                 side_effect=AssertionError("rediscovery")):
                result = PROOF.agent_marker_accessibility("tide", windows=windows)
            self.assertEqual(result["control"] if result else None, expected)
            self.assertEqual(client.walks, [
                ("/workspace", "org.a11y.atspi.Accessible.GetChildren")])
            if result:
                self.assertEqual(result["label"], "Agent action: tide · Agent")
        client = Client({"/one": "(<'Agent action: tide'>,)",
                         "/two": "(<'Agent action: tide · Agent'>,)"})
        with patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                self.assertRaisesRegex(RuntimeError, "duplicate exact Agent markers"):
            PROOF.agent_marker_accessibility("tide", windows=windows)

    def test_marker_observation_distinguishes_stale_root_from_absent_marker_without_content(self):
        class Client:
            def __init__(self, stale):
                self.stale = stale

            def call(self, _path, _interface):
                if self.stale:
                    raise PROOF.atspi.GateError("PRIVATE stale root transport detail")
                return "([(':1.1', '/private_child')],)"

            def property(self, _path, _name):
                return "(<'PRIVATE unrelated control content'>,)"

        windows = [{"address": "PRIVATE", "bus": ":1.1", "window": "/window",
                    "root": "/workspace"}]
        for stale in [False, True]:
            observed = {}
            with self.subTest(stale=stale), patch.object(
                    PROOF.atspi, "AtspiClient", return_value=Client(stale)):
                self.assertIsNone(PROOF.agent_marker_accessibility(
                    "tide", windows=windows, observation=observed))
            self.assertEqual(observed, {
                "owned_root_count": 1, "readable_root_count": int(not stale),
                "root_read_failure_count": int(stale),
                "direct_child_count": int(not stale), "child_name_read_failure_count": 0,
                "exact_marker_candidate_count": 0,
            })
            self.assertNotIn("PRIVATE", json.dumps(observed))
            self.assertNotIn("/private_child", json.dumps(observed))

    def test_owned_shell_windows_prime_the_actual_workspace_under_the_window_wrapper(self):
        class Client(PROOF.atspi.AtspiClient):
            def __init__(self):
                self.walks = []

            def call(self, path, interface):
                self.walks.append((path, interface))
                if path == PROOF.atspi.ROOT_PATH:
                    return "([(':1.1', '/window')],)"
                if path == "/window":
                    return "([(':1.1', '/workspace'), (':1.1', '/unrelated')],)"
                raise AssertionError("a full workspace walk must not occur during priming")

            def property(self, path, _name):
                return "(<'Tilo workspace'>,)" if path == "/workspace" else "(<'Other'>,)"

        client = Client()
        with patch.object(PROOF.atspi, "atspi_address", return_value="owned"), \
                patch.object(PROOF.atspi, "bus_pids", return_value={":1.1": 1, ":1.2": 2}), \
                patch.object(PROOF.atspi, "app_process", side_effect=lambda pid, **_kwargs: pid), \
                patch.object(PROOF, "process_program", side_effect=lambda pid: "shell" if pid == 1 else "chat"), \
                patch.object(PROOF.atspi, "AtspiClient", return_value=client) as bound:
            windows = PROOF.owned_shell_accessibility_windows()
        self.assertEqual(windows, [{"address": "owned", "bus": ":1.1",
                                   "window": "/window", "root": "/workspace"}])
        bound.assert_called_once_with("owned", ":1.1")
        self.assertEqual(client.walks, [
            (PROOF.atspi.ROOT_PATH, "org.a11y.atspi.Accessible.GetChildren"),
            ("/window", "org.a11y.atspi.Accessible.GetChildren")])

    def test_physical_marker_click_uses_current_semantic_subject_and_live_extents(self):
        marker = {"bus": "owned", "address": "owned-address", "control": "/marker",
                  "label": "Agent action: tide"}
        for name, rect, accepted in (("Agent action: tide", (10, 20, 22, 20), True),
                                     ("Foreign control", (10, 20, 22, 20), False),
                                     ("Agent action: tide", None, False),
                                     ("Agent action: tide", (10, 20, 0, 20), False),
                                     ("Agent action: tide", (10, 20, float("nan"), 20), False)):
            client = SimpleNamespace(property=lambda *_args: f"(<{name!r}>,)",
                                     call=lambda *_args: "actual wire extents")
            with patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                    patch.object(PROOF.atspi, "extents_reply", return_value=rect), \
                    patch.object(PROOF, "call", return_value={"ok": True}) as calls, \
                    patch.object(PROOF, "require_ok"):
                if accepted:
                    PROOF.click_current_agent_marker("socket", marker)
                    calls.assert_called_once_with("socket", {"op": "click_at", "x": 21.0, "y": 30.0})
                else:
                    with self.assertRaises(RuntimeError):
                        PROOF.click_current_agent_marker("socket", marker)
                    calls.assert_not_called()

    def test_early_marker_close_reopen_uses_real_input_and_checks_retained_draft(self):
        for retained, expired, pointer in ((True, False, False), (False, False, False),
                                          (True, True, False), (True, False, True),
                                          (False, False, True), (True, True, True)):
            state = {"open": True, "draft": ""}
            marker = {"bus": "owned", "control": "/marker", "label": "Agent action: tide"}
            access = {"bus": "owned", "composer": "/editor"}

            def control(_socket, request):
                if request.get("op") == "type":
                    state["draft"] = request["text"]
                if request.get("name") == "agent-quick-close":
                    state["open"] = False
                return {}

            def activate(actual, label):
                self.assertEqual(actual, marker)
                self.assertEqual(label, marker["label"])
                self.assertFalse(state["open"])
                if expired:
                    raise RuntimeError("the real marker activation expired")
                state["open"] = True
                if not retained:
                    state["draft"] = ""

            with self.subTest(retained=retained, expired=expired), \
                    patch.object(PROOF, "agent_quick_composer_accessibility", side_effect=
                                 lambda *_args, **_kwargs: access if state["open"] else None), \
                    patch.object(PROOF, "private_editor_value_matches", side_effect=
                                 lambda _access, value: state["draft"] == value), \
                    patch.object(PROOF, "call", side_effect=control) as called, \
                    patch.object(PROOF, "require_ok"), \
                    patch.object(PROOF, "agent_marker_accessibility", side_effect=
                                 lambda *_args, **_kwargs: marker if not state["open"] else None), \
                    patch.object(PROOF, "click_current_agent_marker",
                                 side_effect=lambda _socket, actual: activate(actual, actual["label"])), \
                    patch.object(PROOF, "activate_shell_accessibility_control", side_effect=activate):
                if expired:
                    with self.assertRaisesRegex(RuntimeError, "activation expired"):
                        PROOF.reopen_marker_composer_while_live("socket", "tide", marker, [], None,
                                                               pointer=pointer)
                elif retained:
                    result = PROOF.reopen_marker_composer_while_live(
                        "socket", "tide", marker, [], None, pointer=pointer)
                    self.assertEqual(result, "Keep this marker Message draft private.")
                else:
                    with self.assertRaisesRegex(RuntimeError, "draft was lost"):
                        PROOF.reopen_marker_composer_while_live("socket", "tide", marker, [], None,
                                                               pointer=pointer)
                self.assertIn(call("socket", {"op": "type", "text":
                    "Keep this marker Message draft private."}), called.call_args_list)
                self.assertIn(call("socket", {"op": "click_named", "name":
                    "agent-quick-close"}), called.call_args_list)

    def test_physical_marker_reopen_fails_when_no_fresh_marker_returns(self):
        state = {"open": True, "draft": ""}
        marker = {"bus": "owned", "window": "/window", "root": "/workspace",
                  "control": "/marker", "label": "Agent action: tide"}
        access = {"bus": "owned", "composer": "/editor"}

        def control(_socket, request):
            if request.get("op") == "type":
                state["draft"] = request["text"]
            if request.get("name") == "agent-quick-close":
                state["open"] = False
            return {}

        def bounded_wait(label, predicate, _process=None, **_kwargs):
            result = predicate()
            if result:
                return result
            raise PROOF.ProofTimeout(f"timed out waiting for {label}")

        with patch.object(PROOF, "agent_quick_composer_accessibility", side_effect=
                          lambda *_args, **_kwargs: access if state["open"] else None), \
                patch.object(PROOF, "private_editor_value_matches", side_effect=
                             lambda _access, value: state["draft"] == value), \
                patch.object(PROOF, "call", side_effect=control) as called, \
                patch.object(PROOF, "require_ok"), \
                patch.object(PROOF, "agent_marker_accessibility", return_value=None), \
                patch.object(PROOF, "wait_for", side_effect=bounded_wait), \
                patch.object(PROOF, "click_current_agent_marker") as click_marker:
            with self.assertRaisesRegex(
                    PROOF.ProofTimeout, "exact marker return"):
                PROOF.reopen_marker_composer_while_live(
                    "socket", "tide", marker, [], None, pointer=True)
        self.assertEqual(called.call_args_list.count(call(
            "socket", {"op": "click_named", "name": "agent-quick-close"})), 1)
        click_marker.assert_not_called()

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

    def test_agent_conversation_selects_heading_when_roster_name_is_duplicated(self):
        tree = {
            "/heading": "('tide, Agent',)",
            "/roster": "('tide, Agent',)",
            "/composer": "('Message composer',)",
            "/float": "('Float conversation',)",
            "/drag": "('Drag conversation',)",
        }

        class Client:
            def __init__(self, *_args):
                pass

            @staticmethod
            def named(nodes, label):
                needle = f"'{label}'"
                return [path for path, name in nodes.items() if needle in name]

            @staticmethod
            def role(path):
                return PROOF.AT_SPI_ROLE_HEADING if path == "/heading" else 30

            @staticmethod
            def interfaces(path):
                return "org.a11y.atspi.EditableText" if path == "/composer" else ""

            @staticmethod
            def call(_path, interface, *_args, **_kwargs):
                if interface.endswith("GetChildren"):
                    return "(['/window'],)"
                raise AssertionError(interface)

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF.atspi, "bus_pids", return_value={"bus": 7}), \
                patch.object(PROOF.atspi, "app_process", return_value=8), \
                patch.object(PROOF.atspi, "AtspiClient", Client), \
                patch.object(PROOF, "process_program", return_value="chat"), \
                patch.object(PROOF, "accessible_subtree", return_value=tree):
            diagnostic = pathlib.Path(directory) / "accessibility.json"
            access = PROOF.agent_conversation_accessibility(
                "Tide", diagnostic_path=diagnostic,
            )
            observed = json.loads(diagnostic.read_text())

        self.assertEqual(access["heading"], "/heading")
        self.assertEqual(observed["heading_matches"], 1)
        self.assertEqual(observed["composer_matches"], 1)
        self.assertEqual(observed["control_matches"], 1)
        self.assertEqual(observed["drag_handle_matches"], 1)
        self.assertFalse(observed["private_content_recorded"])

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

    def test_physical_selection_expected_caret_waits_for_exact_shift_left_baseline(self):
        stale = {"selection": [4, 9], "caret": 9, "draft_matches": True,
                 "accessibility_focused": True}
        exact = dict(stale, caret=4)
        with patch.object(PROOF, "private_editor_observation", side_effect=[stale, exact]) as read, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF, "call") as calls:
            result = PROOF.wait_for_private_selection(
                "socket", {}, "test text", [4, 9], None, pathlib.Path("unused"),
                expected_caret=4)
            self.assertEqual(result, exact)
            self.assertEqual(read.call_count, 2)
            calls.assert_not_called()
        # Ctrl+A and callers without an independently required caret keep their
        # existing selection-only contract; there is no global lower-end rule.
        with patch.object(PROOF, "private_editor_observation", return_value=stale) as read:
            self.assertEqual(PROOF.wait_for_private_selection(
                "socket", {}, "test text", [4, 9], None, pathlib.Path("unused")), stale)
            self.assertEqual(read.call_count, 1)

    def test_physical_selection_expected_caret_expires_without_accepting_mixed_baseline(self):
        stale = {"selection": [4, 9], "caret": 9, "draft_matches": True,
                 "accessibility_focused": True, "private": "PRIVATE-CANARY"}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "private_editor_observation", return_value=stale) as read, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 0, 10, 20, 30]), \
                patch.object(PROOF, "call", return_value={"ok": True}) as calls, \
                patch.object(PROOF, "require_ok"):
            output = pathlib.Path(directory)
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_private_selection(
                    "socket", {}, "test text", [4, 9], None, output, expected_caret=4)
            self.assertEqual(read.call_count, 3)
            receipt = json.loads((output / "physical-selection-refused.json").read_text())
            self.assertEqual(receipt["expected_caret"], 4)
            self.assertEqual(receipt["observed_caret"], 9)
            self.assertNotIn("PRIVATE-CANARY", json.dumps(receipt))
            self.assertEqual(calls.call_count, 1)
            self.assertEqual(calls.call_args.args[1]["op"], "snapshot")

    def test_membership_revocation_requires_independent_shift_left_caret_before_float(self):
        source = inspect.getsource(PROOF.native_agent_membership_revocation)
        self.assertIn("expected_caret=len(draft) - 10", source)
        self.assertLess(source.index("before = wait_for_private_selection("),
                        source.index("activate_agent_conversation_control(access, \"Float\")"))
        self.assertIn("if detached_before != before:", source)

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

    def test_metadata_shim_removes_compiler_wrappers_only_for_locked_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = pathlib.Path(directory)
            cargo = profile / "pinned-cargo"
            cargo.write_text(
                f"#!{sys.executable}\n"
                "import json, os, sys\n"
                "print(json.dumps({'args': sys.argv[1:], 'environment': "
                "{key: os.environ.get(key) for key in "
                "('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'METADATA_TEST_UNRELATED')}}))\n")
            cargo.chmod(0o700)
            env = {"CARGO": str(cargo), "PATH": PROOF.os.environ["PATH"],
                   "RUSTC_WRAPPER": "original-build-wrapper",
                   "RUSTC_WORKSPACE_WRAPPER": "original-workspace-wrapper",
                   "METADATA_TEST_UNRELATED": "preserved"}
            original = dict(env)
            PROOF.require_locked_cargo_metadata(env, profile)
            shim = env["CARGO"]
            self.assertEqual(env, {**original, "CARGO": str(profile / "locked-cargo-metadata")})
            for arguments in (("metadata", "--format-version", "1"),
                              ("build", "--locked"), ("--version",)):
                observed = json.loads(PROOF.subprocess.check_output(
                    [sys.executable, shim, *arguments], env=env, text=True))
                metadata = arguments[0] == "metadata"
                self.assertEqual(observed["args"],
                                 ["metadata", "--locked", "--format-version", "1"]
                                 if metadata else list(arguments))
                self.assertEqual(observed["environment"], {
                    "RUSTC_WRAPPER": None if metadata else original["RUSTC_WRAPPER"],
                    "RUSTC_WORKSPACE_WRAPPER": None if metadata else original["RUSTC_WORKSPACE_WRAPPER"],
                    "METADATA_TEST_UNRELATED": "preserved",
                })
                self.assertEqual(env, {**original, "CARGO": shim})

    def test_lifecycle_budget_uses_real_fenced_owner_actions_and_closed_proof(self):
        original = {"task": {"id": "task:PRIVATE", "status": "queued", "route": "model",
                             "instruction": "PRIVATE-CANARY"},
                    "active_run": False, "run_epoch": 0, "execution_budget": None,
                    "managed_route": True}
        budget = {"turn_ms": 120000, "total_ms": 300000}
        result = {"result": {"execution_budget_set": {"task_id": "task:PRIVATE"}},
                  "resource": "desktop:task:PRIVATE", "execution_started": False}

        def ack(value):
            return {"update": {"output": {"value": value}}}

        for fault in (None, "frontier", "terminal", "saved"):
            before = dict(original, run_epoch=1) if fault == "frontier" else original
            terminal = dict(result, result={"queued": {"task_id": "task:PRIVATE"}}) \
                if fault == "terminal" else result
            saved = dict(original, execution_budget=budget if fault != "saved" else None)
            with tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "offered_membership_action",
                                 side_effect=lambda _s, _r, resource, op: {"resource": resource, "id": op}), \
                    patch.object(PROOF, "wait_for", side_effect=lambda _l, fn, _p: fn()), \
                    patch.object(PROOF, "invoke", side_effect=[ack(before), ack(terminal), ack(saved)]) as invoke, \
                    patch.object(PROOF, "wait_terminal", side_effect=lambda _s, row, _p: row):
                output = pathlib.Path(directory)
                if fault:
                    with self.assertRaises(RuntimeError):
                        PROOF.configure_lifecycle_task_budget("socket", "room", "task:PRIVATE", None, output)
                    self.assertFalse((output / "managed-task-budget.json").exists())
                else:
                    PROOF.configure_lifecycle_task_budget("socket", "room", "task:PRIVATE", None, output)
                    self.assertEqual(json.loads((output / "managed-task-budget.json").read_text()), {
                        "queued_frontier_matches": True, "exact_terminal_result": True,
                        "saved_budget_matches": True, "turn_ms": 120000, "total_ms": 300000})
                    self.assertNotIn("PRIVATE", (output / "managed-task-budget.json").read_text())
                    self.assertEqual(invoke.call_args_list[1].args, (
                        "socket", {"resource": "desktop:task:PRIVATE", "id": "set-execution-budget"},
                        {"expected_run_epoch": 0, "expected_budget": None, "budget": budget}))
                self.assertEqual(invoke.call_count, {None: 3, "frontier": 1, "terminal": 2, "saved": 3}[fault])

    def test_parallel_message_requires_actual_original_admission_and_held_response(self):
        provider = PROOF.HeldProvider.__new__(PROOF.HeldProvider)
        provider.request_lock = PROOF.threading.Condition()
        provider.refused_requests = 0
        provider.requests = 1
        provider.waiting_ordinals = {1}
        provider.owner_admissions = {}
        self.assertIsNone(PROOF.parallel_message_admitted(provider, 7))
        provider.requests = 2
        for admission in (None, {"run_epoch": 8}, {"run_epoch": True}):
            provider.owner_admissions[2] = admission
            with self.assertRaises(RuntimeError):
                PROOF.parallel_message_admitted(provider, 7)
        provider.owner_admissions[2] = {"run_epoch": 7, "private": "PRIVATE-CANARY"}
        self.assertIsNone(PROOF.parallel_message_admitted(provider, 7))
        provider.waiting_ordinals.add(2)
        self.assertIs(PROOF.parallel_message_admitted(provider, 7), True)
        provider.requests = 3
        with self.assertRaises(RuntimeError):
            PROOF.parallel_message_admitted(provider, 7)

    def test_direction_owner_observation_reads_actual_hold_and_excludes_private_projection(self):
        provider = PROOF.HeldProvider.__new__(PROOF.HeldProvider)
        provider.request_lock = PROOF.threading.Condition()
        provider.waiting_ordinals = {1, 2}
        provider.allowed = 0
        provider.exact_releases = set()
        provider.closed = False
        provider.requests = provider.admission_attempts = provider.admitted_attempts = 2
        provider.refused_admissions = provider.failed_admission_probes = 0
        task = {"status": "in_progress", "run_epoch": 5, "after_revision": 0,
                "active_agent": "PRIVATE-AGENT", "instruction": "PRIVATE-CANARY",
                "task_id": "PRIVATE-TASK", "title": "PRIVATE-CANARY"}
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            PROOF.retain_direction_owner_observation(task, provider, (5, 0, "PRIVATE-AGENT"), output)
            path = output / "managed-direction-owner-before.json"
            data = json.loads(path.read_text())
            self.assertEqual(data, {
                "owner_observable": True, "task_status": "in_progress",
                "epoch_matches_original": True, "after_revision_matches_original": True,
                "active_membership_matches_original": True,
                "provider": {"first_request_held": True, "any_release_allowed": False,
                             "first_request_release_allowed": False, "provider_closed": False,
                             "request_count": 2, "waiting_response_count": 2,
                             "admission_attempt_count": 2, "admitted_count": 2,
                             "refused_count": 0, "probe_failure_count": 0},
            })
            self.assertNotIn("PRIVATE", path.read_text())
            provider.waiting_ordinals.remove(1)
            provider.exact_releases.add(2)
            provider.requests = 10001
            task.update(status="PRIVATE-CANARY", run_epoch=6, after_revision=True,
                        active_agent="FOREIGN-PRIVATE")
            PROOF.retain_direction_owner_observation(task, provider, (5, 0, "PRIVATE-AGENT"), output)
            data = json.loads(path.read_text())
            self.assertEqual(data["task_status"], "other")
            for flag in ("epoch_matches_original", "after_revision_matches_original",
                         "active_membership_matches_original"):
                self.assertFalse(data[flag])
            self.assertFalse(data["provider"]["first_request_held"])
            self.assertTrue(data["provider"]["any_release_allowed"])
            self.assertFalse(data["provider"]["first_request_release_allowed"])
            self.assertEqual(data["provider"]["request_count"], 10000)
            self.assertNotIn("PRIVATE", path.read_text())

    def test_direction_terminal_success_invokes_once_without_failure_artifact(self):
        receipt = {"update": {"invocation": "original-private-id", "state": "succeeded",
                              "output": {"value": "PRIVATE-CANARY"}}}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "invoke", return_value={"receipt": receipt}) as invoke, \
                patch.object(PROOF, "call") as calls, \
                patch.object(PROOF, "wait_for", side_effect=lambda _l, fn, _p: fn()):
            output = pathlib.Path(directory)
            self.assertIs(PROOF.wait_for_direction_terminal(
                "socket", {"id": "direct"}, {"text": "PRIVATE-CANARY"}, None, output), receipt)
            invoke.assert_called_once_with("socket", {"id": "direct"}, {"text": "PRIVATE-CANARY"})
            calls.assert_not_called()
            self.assertFalse((output / "managed-direction-terminal-failure.json").exists())

    def test_direction_terminal_failure_is_closed_and_never_reinvokes(self):
        for code in ("action_unavailable", "bound_broker_route_unavailable", "workspace_task_refused",
                     "PRIVATE-CANARY", {"private": "PRIVATE-CANARY"}):
            initial = {"broker_invocation_id": 37, "update": {
                "invocation": "original-private-id", "state": "accepted"}}
            failed = {"update": {"invocation": "original-private-id", "state": "failed",
                                  "error": {"code": code, "message": "PRIVATE-CANARY",
                                            "details": {"text": "PRIVATE-CANARY"}},
                                  "output": "PRIVATE-CANARY"}}

            def observe(_label, predicate, _process):
                self.assertIsNone(predicate())
                return predicate()

            with tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "invoke", return_value={"receipt": initial}) as invoke, \
                    patch.object(PROOF, "call", return_value={"answer": {"receipt": failed}}) as calls, \
                    patch.object(PROOF, "wait_for", side_effect=observe), \
                    self.assertRaisesRegex(RuntimeError, "ordinary action terminated failed"):
                output = pathlib.Path(directory)
                try:
                    PROOF.wait_for_direction_terminal(
                        "socket", {"id": "direct"}, {"text": "PRIVATE-CANARY"}, None, output)
                finally:
                    invoke.assert_called_once()
                    calls.assert_called_once_with("socket", {"op": "actions_status", "invocation_id": 37})
                    serialized = (output / "managed-direction-terminal-failure.json").read_text()
                    self.assertEqual(json.loads(serialized), {
                        "terminal_state": "failed",
                        "terminal_error_code": code if isinstance(code, str) and code in {
                            "action_unavailable", "bound_broker_route_unavailable",
                            "workspace_task_refused"} else "other",
                        "refusal_reason": "other",
                        "original_invocation_matches": True,
                        "states_seen": {"accepted": 1, "failed": 1},
                    })
                    self.assertNotIn("PRIVATE-CANARY", serialized)
                    self.assertNotIn("original-private-id", serialized)
                    self.assertNotIn("37", serialized)

    def test_direction_terminal_foreign_identity_is_not_classified_as_original_failure(self):
        initial = {"broker_invocation_id": 37, "update": {
            "invocation": "original-private-id", "state": "accepted"}}
        foreign = {"update": {"invocation": "FOREIGN-PRIVATE-CANARY", "state": "failed",
                               "error": {"code": "action_failed", "message": "PRIVATE-CANARY"}}}

        def observe(_label, predicate, _process):
            self.assertIsNone(predicate())
            return predicate()

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "invoke", return_value={"receipt": initial}) as invoke, \
                patch.object(PROOF, "call", return_value={"answer": {"receipt": foreign}}) as calls, \
                patch.object(PROOF, "wait_for", side_effect=observe):
            output = pathlib.Path(directory)
            with self.assertRaisesRegex(RuntimeError, "observation changed its invocation"):
                PROOF.wait_for_direction_terminal("socket", {}, {}, None, output)
            invoke.assert_called_once()
            calls.assert_called_once()
            self.assertEqual(json.loads((output / "managed-direction-terminal-failure.json").read_text()), {
                "terminal_state": None, "terminal_error_code": None, "refusal_reason": None,
                "original_invocation_matches": False, "states_seen": {"accepted": 1},
            })

    def test_direction_terminal_refusal_reason_matches_exact_public_literals_only(self):
        known = "task run or instruction frontier changed; review before steering"
        for message, reason in ((known, "frontier_changed"),
                                ("workspace task agent is not a current room member", "agent_not_current_member"),
                                ("task owner lost its exact committed result", "exact_committed_result_missing"),
                                (known + " PRIVATE-CANARY", "other"),
                                ("PRIVATE-CANARY", "other"), ({"text": "PRIVATE-CANARY"}, "other")):
            receipt = {"update": {"invocation": "PRIVATE-IDENTITY", "state": "failed",
                                  "error": {"code": "workspace_task_refused", "message": message,
                                            "details": "PRIVATE-CANARY"}}}
            with tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "invoke", return_value={"receipt": receipt}) as invoke, \
                    patch.object(PROOF, "call") as calls, \
                    patch.object(PROOF, "wait_for", side_effect=lambda _l, fn, _p: fn()):
                output = pathlib.Path(directory)
                with self.assertRaisesRegex(RuntimeError, "ordinary action terminated failed"):
                    PROOF.wait_for_direction_terminal("socket", {}, {}, None, output)
                serialized = (output / "managed-direction-terminal-failure.json").read_text()
                self.assertEqual(json.loads(serialized), {
                    "terminal_state": "failed", "terminal_error_code": "workspace_task_refused",
                    "refusal_reason": reason, "original_invocation_matches": True,
                    "states_seen": {"failed": 1},
                })
                self.assertNotIn("PRIVATE", serialized)
                self.assertNotIn(known, serialized)
                invoke.assert_called_once()
                calls.assert_not_called()

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
                        self.assertRaisesRegex(
                            RuntimeError,
                            "terminal observation was refused .*status_refusal="
                            + (code if code in {
                                "unknown_action", "invalid_arguments", "action_failed"
                            } else "other"),
                        ):
                    PROOF.wait_terminal("socket", {"receipt": receipt}, None, diagnostic_path=path)
                self.assertEqual(json.loads(path.read_text()), {
                    "original_invocation": original,
                    "broker_invocation_id": 37,
                    "states_seen": {"accepted": 1},
                    "original_unchanged": True,
                    "status_refusal": code if code in {
                        "unknown_action", "invalid_arguments", "action_failed"
                    } else "other",
                    "status_refusal_detail": "other",
                })
                self.assertNotIn(private, path.read_text())

    def test_status_refusal_detail_recognizes_only_exact_collapsed_production_visibility_error(self):
        known = "invocation is unavailable or not visible; this does not establish the original action's outcome"
        receipt = {"broker_invocation_id": 37, "update": {
            "invocation": "00000000-0000-0000-0000-000000000021", "state": "accepted"}}
        for message, expected in ((known, "job_unavailable_or_not_visible"),
                                  (known + " PRIVATE-CANARY", "other"),
                                  ("PRIVATE-CANARY", "other")):
            error = {"code": "action_failed", "message": message}
            with self.subTest(message=message), \
                    patch.object(PROOF, "wait_for", side_effect=lambda _l, fn, _p: fn()), \
                    patch.object(PROOF, "call", return_value={"err": error}), \
                    self.assertRaises(RuntimeError) as refusal:
                PROOF.wait_terminal("socket", {"receipt": receipt}, None)
            self.assertIn(f"status_refusal_detail={expected}", str(refusal.exception))
            self.assertNotIn("PRIVATE-CANARY", str(refusal.exception))
            self.assertNotIn(known, str(refusal.exception))
        self.assertEqual(PROOF.terminal_status_refusal_detail({
            "code": "invalid_arguments", "message": known}), "other")

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

    def test_agent_reply_requires_exact_controlled_response_and_correlation(self):
        action = {"application": "so.tilo.chat", "room": "room", "record": "",
                  "operation": "read-messages"}
        reply = {"id": "m_reply", "parent": "m_parent", "author": "agent:tide",
                 "agentIntent": None, "text": "Fixture response."}

        def observe(message):
            with patch.object(PROOF, "permission_actions_snapshot", return_value=[action]), \
                    patch.object(PROOF, "invoke", return_value={}), \
                    patch.object(PROOF, "wait_terminal", return_value={}), \
                    patch.object(PROOF, "terminal_value", return_value={
                        "content": {"messages": [message]},
                    }):
                return PROOF.agent_reply_observation(
                    "socket", "room", "", "m_parent", "tide", None,
                )

        evidence = observe(reply)
        self.assertEqual(evidence, {
            "typed_actor": "agent:tide",
            "parent": "m_parent",
            "reply_id": "m_reply",
            "reply_matches_fixture": True,
        })
        self.assertNotIn(reply["text"], json.dumps(evidence))

        private = "private model fallback or wrong response"
        for changed in ({"text": private}, {"text": ""}, {"text": None}):
            with self.subTest(changed=changed), self.assertRaisesRegex(
                    RuntimeError, "controlled response"):
                observe({**reply, **changed})
        self.assertIsNone(observe({**reply, "parent": "m_foreign"}))
        self.assertIsNone(observe({**reply, "author": "person:fixture"}))

        ordinary = inspect.getsource(PROOF.wait_for_agent_reply)
        physical = inspect.getsource(PROOF.physical_send_after_stop)
        self.assertIn("agent_reply_observation(", ordinary)
        self.assertIn("agent_reply_observation(", physical)
        self.assertNotIn("expected_reply=", ordinary)
        self.assertNotIn("expected_reply=", physical)
        self.assertIn('"reply_matches_fixture": reply["reply_matches_fixture"]',
                      inspect.getsource(PROOF.managed_task_and_message_lifecycle))
        self.assertIn('"reply_matches_fixture": reply["reply_matches_fixture"]', physical)

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
                "DEBUG tilo_agent_catalogue: SDK watched catalogue boundary "
                "phase=2 surfaces=3 actions=5\n"
                "DEBUG tilo_agent_catalogue: Agent observed catalogue boundary "
                "phase=3 surfaces=3 actions=5\n"
                + private_content + "\n"
            )
            (output / "shell.log").write_text(
                "DEBUG tilo_agent_catalogue: broker watched catalogue boundary "
                "phase=1 published_surfaces=7 matching_live_surfaces=6 "
                "scoped_actions=5 permission_accepted_actions=4 final_surfaces=3\n"
                + private_content
                + " DEBUG tilo_agent_catalogue: broker watched catalogue boundary "
                "phase=1 published_surfaces=99 matching_live_surfaces=99 "
                "scoped_actions=99 permission_accepted_actions=99 final_surfaces=99\n"
            )
            PROOF.finalize_fixture(profile, output, [], [], None, None)
            retained = (output / "managed-child-final.json").read_text()
            evidence = json.loads(retained)
            self.assertEqual(evidence["log_count"], 1)
            self.assertEqual(evidence["line_count"], 5)
            self.assertTrue(evidence["subscription_acknowledged"])
            self.assertTrue(evidence["execution_entered"])
            self.assertFalse(evidence["model_turn_entered"])
            self.assertEqual(evidence["catalogue_boundaries"], [
                {"phase": 2, "surfaces": 3, "actions": 5},
                {"phase": 3, "surfaces": 3, "actions": 5},
            ])
            self.assertEqual(evidence["broker_catalogue_boundaries"], [{
                "phase": 1,
                "published_surfaces": 7,
                "matching_live_surfaces": 6,
                "scoped_actions": 5,
                "permission_accepted_actions": 4,
                "final_surfaces": 3,
            }])
            self.assertIsNone(evidence["first_error_kind"])
            self.assertNotIn(private_content, retained)
            self.assertFalse(profile.exists())

    def test_catalogue_boundary_parser_is_exact_and_bounded(self):
        private = "private catalogue text must not become evidence"
        lines = [
            "DEBUG tilo_agent_catalogue: SDK watched catalogue boundary "
            f"phase=2 surfaces={index} actions={index + 1}"
            for index in range(20)
        ]
        lines.extend([
            private + " DEBUG tilo_agent_catalogue: Agent observed catalogue boundary "
            "phase=3 surfaces=9 actions=9",
            "DEBUG another_target: Agent observed catalogue boundary "
            "phase=3 surfaces=9 actions=9",
            "DEBUG tilo_agent_catalogue: Agent observed catalogue boundary "
            "phase=3 surfaces=999999 actions=1",
        ])
        rows = PROOF.catalogue_boundary_rows(lines, (2, 3))
        self.assertEqual(len(rows), PROOF.CATALOGUE_BOUNDARY_ROW_LIMIT)
        self.assertEqual(rows[0], {"phase": 2, "surfaces": 4, "actions": 5})
        self.assertEqual(rows[-1], {"phase": 2, "surfaces": 19, "actions": 20})
        retained = json.dumps(rows)
        self.assertNotIn(private, retained)
        self.assertNotIn("another_target", retained)
        self.assertNotIn("999999", retained)

    def test_fixture_enables_catalogue_debug_for_managed_children(self):
        self.assertIn(
            '"tilo_agent_catalogue=debug,"', inspect.getsource(PROOF.bootstrap)
        )

    def test_managed_message_boundaries_are_bounded_and_payload_free(self):
        lines = [
            "private text outcome=PendingModel managed Agent Message stage observed"
        ] * 40
        lines.extend([
            "outcome=RefusedPrivate managed Agent Message stage observed",
            "outcome=Completed private unrelated log",
            "outcome=Uncertain(Acceptance) managed Agent Message settlement observed",
        ])
        rows = PROOF.managed_message_boundaries(lines)
        self.assertEqual(len(rows), 32)
        self.assertEqual(rows[0], {"stage": "stage", "outcome": "PendingModel"})
        self.assertEqual(rows[-1], {
            "stage": "settlement", "outcome": "Uncertain(Acceptance)",
        })
        self.assertNotIn("private", json.dumps(rows))
        self.assertNotIn("RefusedPrivate", json.dumps(rows))

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
        with patch.object(PROOF, "permission_actions_snapshot", return_value=rows):
            self.assertEqual(PROOF.offered_membership_action(
                "socket", "room-a", "desktop:agent:tide", "remove"
            )["id"], "remove")
        source = inspect.getsource(PROOF.native_agent_membership_revocation)
        for required in (
                "offered_membership_action", "wait_terminal", "owned_agent_identities",
                "agent_read_only_accessibility", "start_managed_agent",
                "agent_conversation_accessibility", "private_editor_observation",
                "provider.request_count() != 4", "provider.overflow_count() != 0",
                "before-agent-membership-revocation-", "wait_for_keyboard_float_presentation",
                "before_restored_return", "restored_primary_repainted"):
            self.assertIn(required, source)
        self.assertIn("socket, output, previous_float, primary, process", source)
        observer = inspect.getsource(PROOF.wait_for_keyboard_float_presentation)
        self.assertIn('return fresh_floating_presentation(presented_floating_frame(response, "so.tilo.chat"), previous)',
                      observer)
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

    def test_latest_mounted_poll_snapshot_needs_no_new_input_or_paint(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            latest = {"app": "so.tilo.chat", "agent_message_progress": [
                {"phase": "committed", "generation": 4}]}
            (output / "shell.log").write_text(
                '[app so.tilo.chat stderr] tilo-physical-frame ' + json.dumps({
                    "state": {"app": "so.tilo.chat", "old": True}}) + "\n"
                + '[app so.tilo.chat stderr] tilo-physical-poll ' + json.dumps({"state": latest}) + "\n"
                + '[app so.tilo.chat stderr] tilo-physical-poll malformed\n'
                + '[app so.tilo.chat stderr] tilo-physical-poll ' + json.dumps({
                    "state": {"app": "so.tilo.store", "foreign": True}}) + "\n"
                + '[app so.tilo.store stderr] tilo-physical-poll ' + json.dumps({
                    "state": {"app": "so.tilo.chat", "foreign": True}}) + "\n")
            self.assertEqual(PROOF.latest_chat_input_state(output), latest)

    def test_physical_message_original_is_derived_from_saved_canonical_person_row(self):
        request = {"id": "m_00000000000000000000000000000021", "author": "person:fixture",
                   "parent": "", "agentIntent": "message", "text": "fixture",
                   "complete": True}
        messages = [request]
        action = {"application": "so.tilo.chat", "room": "room", "record": "",
                  "operation": "read-messages"}
        with patch.object(PROOF, "permission_actions_snapshot", return_value=[action]), \
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

    def test_provider_counts_http_arrival_before_blocking_owner_admission_without_content(self):
        provider = PROOF.HeldProvider()
        entered, release, refuse = threading.Event(), threading.Event(), threading.Event()
        def probe():
            entered.set()
            if not release.wait(5):
                raise RuntimeError("bounded test probe did not release")
            return None if refuse.is_set() else {"phase": "planning", "private": "PRIVATE-CANARY"}
        provider.require_owner_admission(probe)
        def post():
            request = urllib.request.Request(
                provider.endpoint + "/chat/completions",
                json.dumps({"model": "bounded-fixture", "private": "PRIVATE-CANARY"}).encode(),
                {"Authorization": "Bearer isolated-fixture-not-a-real-provider-key"})
            with urllib.request.urlopen(request, timeout=5) as response:
                return response.status
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            try:
                result = pool.submit(post)
                self.assertTrue(entered.wait(5))
                pending = provider.admission_diagnostic()
                self.assertEqual(pending["admission_attempt_count"], 1)
                self.assertEqual(pending["pending_probe_count"], 1)
                self.assertEqual(pending["counted_request_count"], 0)
                release.set()
                PROOF.wait_for("actual owner admitted POST", lambda: provider.request_count() == 1, timeout=5)
                provider.release_through(1)
                self.assertEqual(result.result(timeout=5), 200)
                admitted = provider.admission_diagnostic()
                self.assertEqual(admitted["admitted_count"], 1)
                self.assertEqual(admitted["pending_probe_count"], 0)
                refuse.set()
                with self.assertRaises(urllib.error.HTTPError) as refused:
                    post()
                self.assertEqual(refused.exception.code, 409)
                closed = provider.admission_diagnostic()
                self.assertEqual(closed["refused_count"], 1)
                self.assertEqual(closed["admission_attempt_count"], 2)
                self.assertTrue(all(type(value) is int and 0 <= value <= 64 for value in closed.values()))
                self.assertNotIn("PRIVATE-CANARY", json.dumps(closed))
            finally:
                release.set()
                provider.close()
        source = inspect.getsource(PROOF.managed_task_and_message_lifecycle)
        wait = source.index('"parallel direct Message model request"')
        receipt = source.index('"parallel-message-model-admission.json"', wait)
        self.assertLess(wait, receipt)

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

    def test_detached_provider_extension_is_explicit_exact_and_bounded_to_six(self):
        provider = PROOF.HeldProvider()
        try:
            with self.assertRaises(RuntimeError):
                provider.arm_detached_task_phase(lambda: {"phase": "planning"})
            self.assertEqual(provider.request_limit, 4)
            provider.requests = 4
            provider.allowed = 2
            provider.exact_releases = {4}
            provider.owner_admission_probe = lambda: {"phase": "WaitingForReply"}
            provider.arm_detached_task_phase(lambda: {"run_epoch": 7, "phase": "planning", "sequence": 1})
            self.assertEqual(provider.request_limit, 6)
            with self.assertRaises(RuntimeError):
                provider.arm_detached_task_phase(lambda: {})
            with self.assertRaises(RuntimeError):
                provider.release_detached_task_turn(3)

            def post():
                request = urllib.request.Request(
                    provider.endpoint + "/chat/completions",
                    json.dumps({"model": "bounded-fixture"}).encode(),
                    {"Authorization": "Bearer isolated-fixture-not-a-real-provider-key"})
                with urllib.request.urlopen(request, timeout=5) as response:
                    return response.status

            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                fifth = pool.submit(post)
                PROOF.wait_for("real fifth task request", lambda: provider.request_count() == 5, timeout=5)
                self.assertEqual(provider.owner_admission(5)["run_epoch"], 7)
                with self.assertRaises(RuntimeError):
                    provider.release_detached_task_turn(6)
                provider.release_detached_task_turn(5)
                self.assertEqual(fifth.result(timeout=5), 200)
                sixth = pool.submit(post)
                PROOF.wait_for("real sixth rebuilt task request", lambda: provider.request_count() == 6, timeout=5)
                provider.release_detached_task_turn(6)
                self.assertEqual(sixth.result(timeout=5), 200)
                self.assertEqual(provider.allowed, 2)
                self.assertNotIn(3, provider.exact_releases)
                with self.assertRaises(urllib.error.HTTPError) as refused:
                    post()
                self.assertEqual(refused.exception.code, 429)
                refused.exception.close()
                self.assertEqual(provider.request_count(), 6)
        finally:
            provider.close()

    def test_detached_provider_extension_refuses_wrong_phase_or_marker(self):
        for changed in ({"requests": 3}, {"allowed": 3}, {"exact_releases": set()},
                        {"refused_requests": 1}, {"closed": True}, {"marker_agent": "tide"},
                        {"owner_admission_probe": None}):
            provider = PROOF.HeldProvider()
            try:
                provider.requests = 4
                provider.allowed = 2
                provider.exact_releases = {4}
                provider.owner_admission_probe = lambda: {}
                for field, value in changed.items():
                    setattr(provider, field, value)
                with self.subTest(changed=changed), self.assertRaises(RuntimeError):
                    provider.arm_detached_task_phase(lambda: {})
                self.assertEqual(provider.request_limit, 4)
            finally:
                provider.close()

    def test_detached_task_submission_is_additive_physical_and_owner_consumed(self):
        source = inspect.getsource(PROOF.native_detached_queue_and_steer)
        self.assertNotIn("invoke(", source)
        for required in ("wait_for_semantic_float_editor_continuity(",
                         "wait_for_floating_keyboard_seat(", "physical_detached_task_control(",
                         '"Queue task", 37', '"Queue task", 43',
                         '"Steer current task", 37', '"Save direction", 43',
                         "provider.arm_detached_task_phase(owner_admission)",
                         "provider.owner_admission(5)", "provider.owner_admission(6)",
                         "provider.release_detached_task_turn(5)",
                         "provider.release_detached_task_turn(6)",
                         'task.get("applied_revision") == revision',
                         '"Direction saved"', '"Direction applied"',
                         'task.get("status") == "done"', "provider.overflow_count() != 0"):
            self.assertIn(required, source)
        bootstrap = inspect.getsource(PROOF.bootstrap)
        self.assertLess(bootstrap.index("native_person_permission_downgrades("),
                        bootstrap.index("native_detached_queue_and_steer("))
        self.assertIn('detached_task_evidence.get("provider_request_count") == 6', bootstrap)
        control = inspect.getsource(PROOF.physical_detached_task_control)
        self.assertEqual(control.count('"op": "click_at"'), 1)
        for forbidden in ("DoAction", "SetText", "invoke(", '"op": "type"'):
            self.assertNotIn(forbidden, control)

    def test_task_mode_editor_contract_is_exact_not_any_editable_text(self):
        for mode in ("Message composer", "Task instruction composer", "Task direction composer"):
            owner = {"bus": "private-bus", "composer": "/editor", "composer_name": mode}
            client = unittest.mock.Mock()
            client.interfaces.return_value = "org.a11y.atspi.Text org.a11y.atspi.EditableText"
            for observed in (mode, "Unrelated editor"):
                client.property.return_value = "(<" + repr(observed) + ">,)"
                with self.subTest(mode=mode, observed=observed), \
                        patch.object(PROOF.atspi, "atspi_address", return_value="private-address"), \
                        patch.object(PROOF.atspi, "AtspiClient", return_value=client):
                    self.assertEqual(PROOF.chat_editor_contract(owner), observed == mode)
            owner["composer_name"] = "Unrelated editor"
            client.property.return_value = "(<'Unrelated editor'>,)"
            with patch.object(PROOF.atspi, "atspi_address", return_value="private-address"), \
                    patch.object(PROOF.atspi, "AtspiClient", return_value=client):
                self.assertFalse(PROOF.chat_editor_contract(owner))

    def test_detached_task_target_uses_exact_native_seat_owner_and_control_role(self):
        owner = {"bus": "private-bus", "process": 9975, "agent": "Tide", "detached": True,
                 "window": "/private-window"}
        child = {"app_id": "so.tilo.chat", "floating_id": "private-child", "surface_id": 9974,
                 "frame_seq": 12, "presented": True, "pixels_ready": True, "keyboard_focused": True,
                 "suspended": False, "transferring": False, "last_error": None,
                 "rect": {"x": 16, "y": 170, "w": 344, "h": 360},
                 "content_rect": {"x": 16, "y": 170, "w": 344, "h": 360},
                 "content_origin": {"x": 16, "y": 170}}
        client = unittest.mock.Mock()
        client.named.return_value = ["/queue-tab", "/queue-send"]
        client.role.side_effect = lambda path: 37 if path == "/queue-tab" else 43
        target = {"client_rect": {"x": 40, "y": 240, "w": 100, "h": 30},
                  "global_center": {"x": 106, "y": 425}}
        with patch.object(PROOF, "call", return_value={"answer": {"frames": [child]}}), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=owner) as discover, \
                patch.object(PROOF.atspi, "atspi_address", return_value="private-address"), \
                patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                patch.object(PROOF, "accessible_subtree", return_value={}), \
                patch.object(PROOF, "accessibility_control_enabled", return_value=True), \
                patch.object(PROOF, "component_global_target", return_value=target) as component:
            self.assertEqual(PROOF.current_detached_task_target(
                "socket", owner, child, "Task instruction composer", "Queue task", 43),
                (child, owner, target))
            discover.assert_called_once_with("Tide", detached=True,
                composer_name="Task instruction composer", stale_only=True)
            self.assertEqual(component.call_args.args[1], "/queue-send")
        for changed in ({"keyboard_focused": False}, {"suspended": True},
                        {"transferring": True}, {"input_route": "foreign"}):
            with self.subTest(changed=changed), \
                    patch.object(PROOF, "call", return_value={"answer": {"frames": [{**child, **changed}]}}), \
                    patch.object(PROOF, "agent_conversation_accessibility") as discover:
                with self.assertRaises(RuntimeError):
                    PROOF.current_detached_task_target(
                        "socket", owner, child, "Task instruction composer", "Queue task", 43)
                discover.assert_not_called()
        for changed in ({"process": 9976}, {"bus": "foreign-bus"}, {"agent": "Foreign"}):
            with self.subTest(changed=changed), \
                    patch.object(PROOF, "call", return_value={"answer": {"frames": [child]}}), \
                    patch.object(PROOF, "agent_conversation_accessibility", return_value={**owner, **changed}), \
                    patch.object(PROOF, "component_global_target") as component:
                with self.assertRaises(RuntimeError):
                    PROOF.current_detached_task_target(
                        "socket", owner, child, "Task instruction composer", "Queue task", 43)
                component.assert_not_called()

    def test_stale_task_editor_refresh_retains_exact_selected_mode(self):
        for mode in ("Task instruction composer", "Task direction composer"):
            original = {"bus": "private-bus", "process": 9975, "agent": "Tide",
                        "detached": True, "composer": "/retired", "composer_name": mode}
            fresh = {**original, "composer": "/fresh"}
            with self.subTest(mode=mode), \
                    patch.object(PROOF, "chat_editor_contract", side_effect=[False, True, True]), \
                    patch.object(PROOF, "process_program", return_value="chat"), \
                    patch.object(PROOF, "agent_conversation_accessibility", return_value=fresh) as discover:
                access = original.copy()
                self.assertEqual(PROOF.owned_editor_read(access, lambda: "fixture-local"), "fixture-local")
                discover.assert_called_once_with("Tide", detached=True, stale_only=True, composer_name=mode)
                self.assertEqual(access, fresh)
            with patch.object(PROOF, "chat_editor_contract", return_value=False), \
                    patch.object(PROOF, "process_program", return_value="chat"), \
                    patch.object(PROOF, "agent_conversation_accessibility",
                                 return_value={**fresh, "composer_name": "Message composer"}):
                with self.assertRaisesRegex(RuntimeError, "foreign Chat route"):
                    PROOF.owned_editor_read(original.copy(), lambda: "never-read")

    def test_marker_provider_invokes_one_discovered_exact_chat_action(self):
        resource = "tilo-ui-bound:v1:current-category"
        provider = PROOF.HeldProvider(marker_agent="tide", marker_resource=resource)
        provider.require_owner_admission(
            lambda: {"phase": "planning", "sequence": 1, "run_epoch": 7}
        )
        operation = f"broker:42:{resource}#rename"

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
                                  "In so.tilo.chat: Lifecycle proof (chat.category) — "
                                  f"resource {resource}\n"
                                  f"  {operation} — Rename category\n"
                                  "The request:\n\nRename that category."}],
                })
                PROOF.wait_for("one marker provider request",
                               lambda: provider.request_count() == 1, timeout=5)
                provider.release_through(1)
                tool = response.result(timeout=5)["choices"][0]["message"]["tool_calls"][0]
                self.assertEqual(tool["function"]["name"], "invoke")
                arguments = json.loads(tool["function"]["arguments"])
                self.assertEqual(arguments["operation"], operation)
                self.assertEqual(arguments["parameters"], {
                    "value": "Agent marker proof",
                })
                self.assertNotIn("reply", arguments)
                self.assertEqual(provider.request_count(), 1)
                self.assertEqual(provider.overflow_count(), 0)
            finally:
                provider.close()

    def test_marker_provider_finishes_third_turn_without_releasing_superseded_second(self):
        resource = "tilo-ui-bound:v1:current-category"
        provider = PROOF.HeldProvider(marker_agent="tide", marker_resource=resource)
        provider.require_owner_admission(
            lambda: {"phase": "planning", "sequence": 1, "run_epoch": 7}
        )
        operation = f"broker:42:{resource}#rename"
        payload = {
            "model": "bounded-fixture",
            "messages": [{"role": "user", "content":
                          "Operations published right now:\n\n"
                          "In so.tilo.chat: Lifecycle proof (chat.category) — "
                          f"resource {resource}\n"
                          f"  {operation} — Rename category\n"
                          "The request:\n\nRename that category."}],
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
        resource = "tilo-ui-bound:v1:current-category"
        expected = f"broker:9:{resource}#rename"
        self.assertEqual(PROOF.marker_operation_from_request(
            {"messages": [{"role": "user", "content":
                            "Operations published right now:\n\n"
                            "In so.tilo.chat: Lifecycle proof (chat.category) — resource "
                            + resource + "\n"
                            "  " + expected + " — Rename category\n"
                            "  " + expected + " — Rename category\n" +
                            "\nThe request:\n\nRename it."}]}, "tide", resource
        ), expected)
        for request in (
                {"messages": [{"role": "user", "content":
                                "Operations published right now:\n\nno operation\nThe request:\n\n"}]},
                {"messages": [{"role": "user", "content":
                                "Operations published right now:\n\n"
                                "  " + expected + " — Rename category\n"
                                f"  broker:8:{resource}#rename — Rename category\n"
                                "The request:\n\n"}]},
                {"messages": [{"role": "user", "content":
                                "Operations published right now:\n\n"
                                "  broker:9:tilo-ui-bound:v1:other-category#rename — Rename category\n"
                                "The request:\n\n"}]},
                {"messages": [{"role": "user", "content":
                                "Operations published right now:\n\n"
                                "In so.tilo.chat: " + expected + " (chat.category) — resource "
                                + resource + "\n"
                                "  broker:9:somewhere#else — label " + expected + "\n"
                                "The request:\n\n" + expected}]}):
            with self.subTest(request=request), self.assertRaises(RuntimeError):
                PROOF.marker_operation_from_request(request, "tide", resource)
        with self.assertRaises(RuntimeError):
            PROOF.marker_operation_from_request({"messages": []}, "Tide with spaces", resource)

    def test_marker_operation_refusals_are_closed_and_content_free(self):
        private = "private marker request text must not escape"
        cases = [
            ({"messages": []}, "Tide with spaces", "tilo-ui-bound:v1:category",
             "invalid_configured_address", 0, 0),
            ({"messages": [{"role": "user", "content": private}]},
             "tide", "tilo-ui-bound:v1:category", "no_trusted_catalogue", 0, 0),
            ({"messages": [{"role": "user", "content":
                             "Operations published right now:\n\n" + private +
                             "\nThe request:\n\n" + private}]},
             "tide", "tilo-ui-bound:v1:category", "no_exact_resource_action", 0, 0),
            ({"messages": [{"role": "user", "content":
                             "Operations published right now:\n\n"
                             "  broker:8:tilo-ui-bound:v1:other#rename — Rename\n"
                             "The request:\n\n" + private}]},
             "tide", "tilo-ui-bound:v1:category", "no_exact_resource_action", 1, 0),
            ({"messages": [{"role": "user", "content":
                             "Operations published right now:\n\n"
                             "  broker:8:tilo-ui-bound:v1:category#rename — Rename\n"
                             "  broker:9:tilo-ui-bound:v1:category#rename — Rename\n"
                             "The request:\n\n" + private}]},
             "tide", "tilo-ui-bound:v1:category", "no_exact_resource_action", 2, 2),
        ]
        for request, agent, resource, expected, operation_count, exact_count in cases:
            with self.subTest(reason=expected), \
                    self.assertRaises(PROOF.MarkerOperationRefusal) as refused:
                PROOF.marker_operation_from_request(request, agent, resource)
            self.assertEqual(refused.exception.reason, expected)
            self.assertEqual(refused.exception.matching_operation_count, operation_count)
            self.assertEqual(refused.exception.matching_exact_resource_count, exact_count)
            self.assertEqual(str(refused.exception), "marker operation unavailable")
            self.assertNotIn(private, str(refused.exception))

    def test_marker_provider_retains_only_bounded_rejection_diagnostic(self):
        private = "private provider body must not be retained"
        cases = [
            (private, private, "invalid_configured_address", 0, 0),
            ("tilo-ui-bound:v1:category", private, "no_trusted_catalogue", 0, 0),
            ("tilo-ui-bound:v1:category",
             "Operations published right now:\n\n"
             "  broker:8:tilo-ui-bound:v1:other#rename — Rename\n"
             "The request:\n\n" + private,
             "no_exact_resource_action", 1, 0),
            ("tilo-ui-bound:v1:category",
             "Operations published right now:\n\n"
             "  broker:8:tilo-ui-bound:v1:category#rename — Rename\n"
             "  broker:9:tilo-ui-bound:v1:category#rename — Rename\n"
             "The request:\n\n" + private,
             "no_exact_resource_action", 2, 2),
        ]
        for resource, content, reason, operation_count, exact_count in cases:
            provider = PROOF.HeldProvider(marker_agent="tide", marker_resource=resource)
            request = urllib.request.Request(
                provider.endpoint + "/chat/completions",
                json.dumps({
                    "model": "bounded-fixture",
                    "messages": [{"role": "user", "content": content}],
                }).encode(),
                {"Authorization": "Bearer isolated-fixture-not-a-real-provider-key"},
            )
            try:
                with self.assertRaises(urllib.error.HTTPError) as refused:
                    urllib.request.urlopen(request, timeout=5)
                self.assertEqual(refused.exception.code, 409)
                refused.exception.close()
                with tempfile.TemporaryDirectory() as directory:
                    output = pathlib.Path(directory)
                    self.assertTrue(PROOF.retain_marker_rejection_diagnostic(provider, output))
                    retained = (output / "marker-provider-rejection.json").read_text()
                self.assertNotIn(private, retained)
                self.assertNotIn("tilo-ui-bound:v1:category", retained)
                diagnostic = json.loads(retained)
                self.assertEqual(diagnostic["rejection_count"], 1)
                self.assertEqual(diagnostic["reasons"][reason], 1)
                self.assertEqual(sum(diagnostic["reasons"].values()), 1)
                self.assertEqual(diagnostic["matching_operation_count"], operation_count)
                self.assertEqual(diagnostic["matching_exact_resource_count"], exact_count)
                self.assertIs(diagnostic["private_content_recorded"], False)
                self.assertEqual(provider.request_count(), 0)
                self.assertEqual(provider.overflow_count(), 1)
            finally:
                provider.close()

        saturated = PROOF.MarkerOperationRefusal(
            "no_exact_resource_action",
            matching_operations=10_000,
            matching_exact_resources=10_000,
        )
        self.assertEqual(
            saturated.matching_operation_count, PROOF.MARKER_DIAGNOSTIC_COUNT_LIMIT
        )
        self.assertEqual(
            saturated.matching_exact_resource_count, PROOF.MARKER_DIAGNOSTIC_COUNT_LIMIT
        )

    def test_marker_operation_keeps_discovered_bound_resource_opaque_and_exact(self):
        resource = "tilo-ui-bound:v1:host:current:636861742e63617465676f72793a70726f6f66"
        expected = f"broker:9:{resource}#rename"
        request = {"messages": [{"role": "user", "content":
            "Operations published right now:\n\n"
            f"In so.tilo.chat: Lifecycle proof (chat.category) — resource {resource}\n"
            f"  {expected} — Rename category\nThe request:\n\nRename it."}]}
        self.assertEqual(PROOF.marker_operation_from_request(request, "tide", resource), expected)
        for stale in [None, "chat.category:proof", resource.replace("current", "retired")]:
            with self.subTest(resource=stale), self.assertRaises(RuntimeError):
                PROOF.marker_operation_from_request(request, "tide", stale)

    def test_marker_governance_grants_only_the_discovered_resource(self):
        resource = "tilo-ui-bound:v1:host:current:opaque-category"
        target = {"application": "so.tilo.chat", "room": "room-a",
                  "operation": "rename", "resource": resource}
        rows = [{"provider": "app-actions", "application": "tilo.workspace", "room": "room-a",
                 "resource": "workspace.permissions:room-a", "operation": operation,
                 "id": operation} for operation in ("profile.save", "profile.assign")]
        requests = []

        def invoked(socket, action, arguments):
            requests.append((action["operation"], arguments))
            return {"update": {"invocation": action["operation"]}}

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "permission_actions_snapshot", return_value=rows), \
                patch.object(PROOF, "wait_for", side_effect=lambda label, probe, process: probe()), \
                patch.object(PROOF, "invoke", side_effect=invoked), \
                patch.object(PROOF, "wait_terminal", side_effect=lambda socket, receipt, process: receipt):
            PROOF.grant_exact_marker_action("socket", "room-a", "tide", target,
                                           object(), pathlib.Path(directory))
        self.assertEqual([operation for operation, _ in requests], ["profile.save", "profile.assign"])
        self.assertEqual(requests[0][1]["rules"], [{
            "scope": {"Action": {"app": "so.tilo.chat", "resource": resource, "action": "rename"}},
            "effect": "Grant", "verb": "Write",
        }])
        self.assertEqual(requests[1][1]["subject"], "tide")
        self.assertEqual(requests[1][1]["subject_kind"], "agent")
        stage = inspect.getsource(PROOF.managed_agent_marker_stage)
        self.assertIn("grant_exact_marker_action(socket, room, agent, target, process, output)", stage)

    def test_marker_governance_refuses_missing_or_foreign_discovered_target(self):
        target = {"application": "so.tilo.chat", "room": "room-a",
                  "operation": "rename", "resource": "opaque-category"}
        for invalid in (None, {}, {**target, "application": "other"},
                        {**target, "room": "room-b"}, {**target, "operation": "message"},
                        {**target, "resource": ""}, {**target, "resource": None}):
            with self.subTest(target=invalid), patch.object(PROOF, "actions") as actions, \
                    self.assertRaises(RuntimeError):
                PROOF.grant_exact_marker_action("socket", "room-a", "tide", invalid,
                                               object(), pathlib.Path("unused"))
            actions.assert_not_called()

    def test_marker_stage_uses_real_governance_and_accessibility_doors(self):
        rows = [{
            "provider": "app-actions", "application": "tilo.workspace", "room": "room-a",
            "resource": "workspace.permissions:room-a", "operation": "profile.save",
            "id": "save",
        }, {
            "provider": "app-actions", "application": "tilo.workspace", "room": "room-b",
            "resource": "workspace.permissions:room-b", "operation": "profile.save",
            "id": "foreign",
        }]
        with patch.object(PROOF, "permission_actions_snapshot", return_value=rows):
            self.assertEqual(PROOF.offered_governance_action(
                "socket", "room-a", "profile.save"
            )["id"], "save")
        grant = inspect.getsource(PROOF.grant_exact_marker_action)
        self.assertIn('"profile.save"', grant)
        self.assertIn('"profile.assign"', grant)
        self.assertIn('"Action"', grant)
        self.assertIn('"rename"', grant)
        self.assertIn('"so.tilo.chat#rename"', grant)
        self.assertIn('"broad_chat_grant": False', grant)
        stage = inspect.getsource(PROOF.managed_agent_marker_stage)
        for required in (
                "start_managed_agent", "agent_marker_accessibility",
                "activate_shell_accessibility_control",
                "actual quick composer with authenticated Steer availability",
                "exercise_agent_quick_composer", "provider.request_count() != 3",
                "provider.marker_second_held", "provider.marker_second_superseded",
                "provider.release_marker_terminal", 'interaction["receipt_revision"]',
                'preceding_result != {', '"result": "succeeded"',
                'offered_chat_category_action', '"category_renamed": True',
                '"Collapse Agent marker proof", "Expand Agent marker proof"',
                'marker_binds_presented_category', '"marker_category_bound"',
                '"marker-category-binding-refusal.json"',
                'ensure_chat_category_visible(socket, "Lifecycle proof", process,',
                '"category-marker-visible-before-task-',
                'observe_chat_task_frontier(', 'title="Agent marker proof"',
                'sample_primed_chat_frontier(',
                'chat_frontier.get("sequence") != second_admission["sequence"]',
                'accessibility_control_enabled(', 'access["controls"]["steer"]',
                'third_admission["applied_revision"] != interaction["receipt_revision"]',
                'admission["sequence"] < second_admission["sequence"]',
                "full_lifecycle_proven"):
            self.assertIn(required, stage)
        for forbidden in ("open_agent_quick_composer_for_shots", "stage_scenario"):
            self.assertNotIn(forbidden, stage)

    def test_category_marker_target_uses_label_query_and_exact_opaque_family(self):
        category = "tilo-ui-bound:v1:current-category"
        foreign = "tilo-ui-bound:v1:foreign-category"
        rows = [{
            "application": "so.tilo.chat", "room": "room-a", "record": "",
            "resource": category, "operation": "open-category",
        }, {
            "application": "so.tilo.chat", "room": "room-a", "record": "",
            "resource": category, "operation": "rename", "id": "exact",
        }, {
            "application": "so.tilo.chat", "room": "room-b", "record": "",
            "resource": foreign, "operation": "open-category",
        }, {
            "application": "so.tilo.chat", "room": "room-b", "record": "",
            "resource": foreign, "operation": "rename", "id": "foreign",
        }, {
            "application": "so.tilo.chat", "room": "room-a", "record": "other",
            "resource": category, "operation": "rename", "id": "wrong-record",
        }]
        with patch.object(PROOF, "actions", return_value=rows) as listed:
            selected = PROOF.offered_chat_category_action(
                "socket", "room-a", "", "Lifecycle proof", "rename"
            )
        self.assertEqual(selected["id"], "exact")
        listed.assert_called_once_with("socket", "Lifecycle proof")
        with patch.object(PROOF, "actions", return_value=[rows[1]]):
            self.assertIsNone(PROOF.offered_chat_category_action(
                "socket", "room-a", "", "Lifecycle proof", "rename"
            ))

    def test_compact_category_reveal_uses_the_actual_chat_control(self):
        category = {"bus": "chat", "control": "/category"}
        reveal = {
            "bus": "chat", "control": "/reveal",
            "label": "Show conversation navigation",
        }
        with patch.object(
                PROOF, "chat_category_accessibility", side_effect=[None, category]
             ) as observed, patch.object(
                 PROOF, "chat_accessibility_control", return_value=reveal
             ), patch.object(PROOF, "activate_accessibility_control") as activated:
            self.assertIs(PROOF.ensure_chat_category_visible(
                "socket", "Lifecycle proof", None
            ), category)
        self.assertEqual(observed.call_count, 2)
        activated.assert_called_once_with(reveal, "Show conversation navigation")

        with patch.object(
                PROOF, "chat_category_accessibility", return_value=category
             ), patch.object(PROOF, "activate_accessibility_control") as activated:
            self.assertIs(PROOF.ensure_chat_category_visible(
                "socket", "Lifecycle proof", None
            ), category)
        activated.assert_not_called()

        route = {"bus": "chat", "process": 42, "window": "/current"}
        with patch.object(
                PROOF, "chat_category_accessibility", side_effect=[None, category]
             ) as observed, patch.object(
                 PROOF, "chat_accessibility_control", return_value=reveal
             ) as revealed, patch.object(
                 PROOF, "activate_accessibility_control") as activated:
            self.assertIs(PROOF.ensure_chat_category_visible(
                "socket", "Lifecycle proof", None, owned_route=route
            ), category)
        self.assertEqual(observed.call_args_list, [
            call("Lifecycle proof", owned_route=route),
            call("Lifecycle proof", owned_route=route),
        ])
        revealed.assert_called_once_with(
            "Show conversation navigation", owned_route=route
        )
        activated.assert_called_once_with(reveal, "Show conversation navigation")

    def test_marker_composer_uses_physical_private_drafts_and_exact_owner_frontier(self):
        source = inspect.getsource(PROOF.exercise_agent_quick_composer)
        source += inspect.getsource(PROOF.quick_composer_targets)
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
        self.assertLess(source.index('"pointer_drag_detach"'), source.index('"keyboard_float"'))
        self.assertLess(source.index('"op": "drag"'),
                        source.index("wait_for_keyboard_float_presentation("))
        self.assertLess(source.index("pre-detach undo history in the detached editor"),
                        source.index("detached edit retained for attached undo"))
        self.assertLess(source.index("detached edit retained for attached undo"),
                        source.index("detached undo history in the returned editor"))
        self.assertGreaterEqual(source.count("before = wait_for_private_selection("), 2)
        self.assertIn("before_return_editor = wait_for_private_selection(", source)
        self.assertGreaterEqual(
            source.count("with hold_physical_modifiers(socket, shift=True):"), 2
        )
        self.assertNotIn("set_physical_modifiers", source)

    def test_semantic_float_return_requires_native_pixels_and_retained_editor(self):
        source = inspect.getsource(PROOF.native_semantic_float_return)
        for required in ("offered_agent_action", "float-agent-conversation",
                         "return-agent-conversation", "fresh_floating_presentation",
                         "floating_surface_presenting", "fresh_primary_presentation"):
            self.assertIn(required, source)
        self.assertIn("if detached != before:", source)
        self.assertIn("if returned != before:", source)
        self.assertEqual(source.count("private_editor_observation(access, draft)"), 1)
        self.assertIn("wait_for_semantic_float_editor_continuity(", source)
        self.assertIn("wait_for_return_editor_continuity(", source)
        for observer in (PROOF.wait_for_semantic_float_editor_continuity,
                         PROOF.wait_for_return_editor_continuity):
            self.assertEqual(inspect.getsource(observer).count(
                'private_editor_observation(access, expected["draft"])'), 1)
        self.assertLess(source.index('"semantic-float-editor-continuity.json"'),
                        source.index("if detached != before:"))
        self.assertLess(source.index('"semantic-return-editor-continuity.json"'),
                        source.index("if returned != before:"))
        self.assertEqual(source.count("wait_terminal(socket, invoke("), 2)
        for forbidden in ("SetText", "AddSelection", "DoAction", '"op": "type"'):
            self.assertNotIn(forbidden, source)
        self.assertIn("native_semantic_float_return(", inspect.getsource(PROOF.native_float_return))

    def test_semantic_float_refusal_persists_closed_editor_and_exact_native_seat_observations(self):
        private = "private-semantic-draft"
        before = {"draft_matches": True, "caret": 5, "selection": [4, 5],
                  "accessibility_focused": True}
        detached = {**before, "accessibility_focused": False}
        access = {"process": 9975, "agent": "Tide"}
        frame = {"app_id": "so.tilo.chat", "floating_id": "private-semantic-native-id",
                 "surface_id": 9974, "frame_seq": 22, "keyboard_focused": False,
                 "presented": True, "pixels_ready": True, "suspended": False,
                 "transferring": True, "last_error": "private-semantic-renderer-error"}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "wait_for", side_effect=[access, {}, frame]), \
                patch.object(PROOF, "private_editor_observation", return_value=before), \
                patch.object(PROOF, "wait_for_semantic_float_editor_continuity",
                             return_value=(frame, access, detached)), \
                patch.object(PROOF, "invoke", return_value={}), \
                patch.object(PROOF, "wait_terminal"), \
                patch.object(PROOF, "call", return_value={"answer": {"frames": [frame]}}):
            output = pathlib.Path(directory)
            with self.assertRaisesRegex(RuntimeError, "semantic Float changed"):
                PROOF.native_semantic_float_return(
                    "socket", "room", {}, private, frame, None, output)
            receipt = json.loads((output / "semantic-float-editor-continuity.json").read_text())
            self.assertTrue(receipt["editor"]["content_matches"])
            self.assertTrue(receipt["editor"]["caret_equal"])
            self.assertTrue(receipt["editor"]["selection_equal"])
            self.assertFalse(receipt["editor"]["accessibility_focus_equal"])
            self.assertTrue(receipt["native"]["exact_child_exists"])
            self.assertTrue(receipt["native"]["child_transferring"])
            self.assertFalse(receipt["native"]["child_keyboard_focused"])
            self.assertTrue(receipt["native"]["child_error_present"])
            for secret in (private, frame["floating_id"], frame["last_error"], "9974", "9975"):
                self.assertNotIn(secret, json.dumps(receipt))
            self.assertTrue(all(type(value) is bool for group in receipt.values()
                                for value in group.values()))

    def test_semantic_continuity_diagnostic_never_confuses_foreign_or_stale_child_with_exact_seat(self):
        child = {"app_id": "so.tilo.chat", "floating_id": "private-id",
                 "surface_id": 9974, "frame_seq": 22, "keyboard_focused": True,
                 "presented": False, "pixels_ready": True, "suspended": True,
                 "transferring": False, "last_error": None}
        before = {"draft_matches": True, "caret": 5, "selection": None,
                  "accessibility_focused": True}
        for current, exact, valid in ((child, True, True),
                ({**child, "surface_id": 9976}, False, False),
                ({**child, "floating_id": "foreign"}, False, False),
                ({**child, "frame_seq": 21}, True, False),
                ({**child, "keyboard_focused": None}, True, False)):
            with self.subTest(current=current):
                receipt = PROOF.semantic_editor_continuity_observation(
                    before, before.copy(), {"answer": {"frames": [current]}}, child, True)
                self.assertEqual(receipt["native"]["exact_child_exists"], exact)
                self.assertEqual(receipt["native"]["observation_valid"], valid)
                self.assertTrue(receipt["editor"]["accessibility_focus_equal"])
                self.assertNotIn("9974", json.dumps(receipt))

    def test_semantic_float_observer_requires_real_focus_publication_on_exact_ordinary_child(self):
        _, child, access, before, expected = self.return_editor_continuity_subjects()
        child = {**child, "presented": True, "pixels_ready": True,
                 "suspended": False, "transferring": False}
        access = {**access, "detached": True}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF, "call", return_value={"answer": {"frames": [child]}}) as native, \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "private_editor_observation", side_effect=[
                    {**before, "accessibility_focused": False}, before.copy()]):
            output = pathlib.Path(directory)
            self.assertEqual(PROOF.wait_for_semantic_float_editor_continuity(
                "socket", output, child, expected, None), (child, access, before))
            self.assertEqual(native.call_args_list, [call("socket", {"op": "foreign_frames"})] * 2)
            receipt = json.loads((output / "semantic-float-editor-adoption-observations.json").read_text())
            self.assertTrue(receipt["initial"]["pending_focus_publication"])
            self.assertFalse(receipt["initial"]["accepted"])
            self.assertTrue(receipt["last"]["accepted"])
            for sample in (receipt["initial"], receipt["last"]):
                self.assertTrue(sample["content_matches"] and sample["caret_equal"]
                                and sample["selection_equal"])
                self.assertTrue(all(type(value) is bool for value in sample.values()))
            for secret in ("draft-secret", "private-native-child", "9974", "9975"):
                self.assertNotIn(secret, json.dumps(receipt))
        source = inspect.getsource(PROOF.wait_for_semantic_float_editor_continuity)
        for forbidden in ("physically_click_editor(", "focus_agent_conversation_control(",
                          '"op": "key"', '"op": "type"'):
            self.assertNotIn(forbidden, source)

    def test_semantic_float_observer_times_out_persistent_unfocused_editor_and_refuses_mutation(self):
        _, child, access, before, expected = self.return_editor_continuity_subjects()
        child = {**child, "presented": True, "pixels_ready": True,
                 "suspended": False, "transferring": False}
        access = {**access, "detached": True}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 31]), \
                patch.object(PROOF, "call", return_value={"answer": {"frames": [child]}}), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "private_editor_observation", return_value={
                    **before, "accessibility_focused": False}):
            output = pathlib.Path(directory)
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_semantic_float_editor_continuity(
                    "socket", output, child, expected, None)
            receipt = json.loads((output / "semantic-float-editor-adoption-observations.json").read_text())
            self.assertFalse(receipt["last"]["accepted"])
            self.assertTrue(receipt["last"]["pending_focus_publication"])
        for mutation in ({"draft_matches": False}, {"caret": 4}, {"selection": None}):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF.time, "sleep") as sleep, \
                    patch.object(PROOF, "call", return_value={"answer": {"frames": [child]}}), \
                    patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                    patch.object(PROOF, "private_editor_observation", return_value={**before, **mutation}):
                with self.assertRaisesRegex(RuntimeError, "content, caret or selection"):
                    PROOF.wait_for_semantic_float_editor_continuity(
                        "socket", pathlib.Path(directory), child, expected, None)
                sleep.assert_not_called()

    def test_semantic_float_observer_refuses_lost_foreign_malformed_or_retired_native_seat(self):
        _, child, access, before, expected = self.return_editor_continuity_subjects()
        child = {**child, "presented": True, "pixels_ready": True,
                 "suspended": False, "transferring": False}
        access = {**access, "detached": True}
        for changes in ({"surface_id": 9976}, {"floating_id": "foreign"},
                        {"frame_seq": 21}, {"keyboard_focused": False},
                        {"keyboard_focused": None}, {"presented": False},
                        {"pixels_ready": False}, {"pixels_ready": None},
                        {"pixels_ready": False, "capture_pending": False},
                        {"pixels_ready": False, "capture_pending": None},
                        {"pixels_ready": False, "capture_pending": 1},
                        {"pixels_ready": True, "capture_pending": True},
                        {"frame_seq": True}, {"suspended": True},
                        {"transferring": True}, {"last_error": "private-error"},
                        {"input_route": "floating-preedit-source"},
                        {"input_source_surface_id": 9976}):
            with self.subTest(changes=changes), tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF.time, "sleep") as sleep, \
                    patch.object(PROOF, "call", return_value={
                        "answer": {"frames": [{**child, **changes}]}}), \
                    patch.object(PROOF, "agent_conversation_accessibility", return_value=access) as accessibility, \
                    patch.object(PROOF, "private_editor_observation", return_value=before) as editor:
                with self.assertRaises(RuntimeError):
                    PROOF.wait_for_semantic_float_editor_continuity(
                        "socket", pathlib.Path(directory), child, expected, None)
                accessibility.assert_not_called()
                editor.assert_not_called()
                sleep.assert_not_called()

    def test_semantic_float_capture_pending_waits_for_ready_without_skipping_editor(self):
        _, child, access, before, expected = self.return_editor_continuity_subjects()
        ready = {**child, "presented": True, "pixels_ready": True, "capture_pending": False,
                 "suspended": False, "transferring": False}
        pending = {**ready, "pixels_ready": False, "capture_pending": True}
        access = {**access, "detached": True}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF, "call", side_effect=[
                    {"answer": {"frames": [pending]}}, {"answer": {"frames": [ready]}}]), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "private_editor_observation", return_value=before) as editor:
            output = pathlib.Path(directory)
            self.assertEqual(PROOF.wait_for_semantic_float_editor_continuity(
                "socket", output, ready, expected, None), (ready, access, before))
            self.assertEqual(editor.call_count, 2)
            receipt = json.loads((output / "semantic-float-editor-adoption-observations.json").read_text())
            self.assertTrue(receipt["initial"]["pending_child_pixels"])
            self.assertTrue(receipt["initial"]["child_capture_pending"])
            self.assertTrue(receipt["initial"]["content_matches"])
            self.assertFalse(receipt["initial"]["accepted"])
            self.assertTrue(receipt["last"]["accepted"])

    def test_semantic_float_capture_pending_still_refuses_editor_mutation(self):
        _, child, access, before, expected = self.return_editor_continuity_subjects()
        pending = {**child, "presented": True, "pixels_ready": False, "capture_pending": True,
                   "suspended": False, "transferring": False}
        access = {**access, "detached": True}
        for mutation in ({"draft_matches": False}, {"caret": 4}, {"selection": None}):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF.time, "sleep") as sleep, \
                    patch.object(PROOF, "call", return_value={"answer": {"frames": [pending]}}), \
                    patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                    patch.object(PROOF, "private_editor_observation", return_value={**before, **mutation}):
                with self.assertRaisesRegex(RuntimeError, "content, caret or selection"):
                    PROOF.wait_for_semantic_float_editor_continuity(
                        "socket", pathlib.Path(directory), pending, expected, None)
                sleep.assert_not_called()

    def test_semantic_float_persistent_capture_pending_times_out_original_deadline(self):
        _, child, access, before, expected = self.return_editor_continuity_subjects()
        pending = {**child, "presented": True, "pixels_ready": False, "capture_pending": True,
                   "suspended": False, "transferring": False}
        access = {**access, "detached": True}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 31]), \
                patch.object(PROOF, "call", return_value={"answer": {"frames": [pending]}}), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "private_editor_observation", return_value=before) as editor:
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_semantic_float_editor_continuity(
                    "socket", pathlib.Path(directory), pending, expected, None)
            editor.assert_called_once()

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
        self.assertIn('diagnostic_path=output / "ime-attached-accessibility.json"', source)
        self.assertLess(source.index("ime-detached-first-frame-"),
                        source.index('"detached Tide editor during Rime preedit"'))
        for required in ("ime-detached-native.json", "ime-detached-accessibility.json",
                         '"keyboard_focused"', '"same_input_method_process"'):
            self.assertIn(required, source)
        self.assertNotIn('"last_error"', source)

    def test_native_ime_waits_for_the_real_input_method_grab_before_typing(self):
        ready = {
            "keyboard": {"app_id": "so.tilo.chat"},
            "text_input": {"app_id": "so.tilo.chat"},
            "text_input_active": True,
            "input_method_connected": True,
            "input_method_keyboard_grabbed": True,
            "input_method_preedit_active": True,
        }
        for changes, accepted in (
                ({}, True),
                ({"input_method_connected": False}, False),
                ({"input_method_keyboard_grabbed": False}, False),
                ({"text_input_active": False}, False),
                ({"keyboard": {"app_id": "so.tilo.store"}}, False),
                ({"text_input": {"app_id": "so.tilo.store"}}, False)):
            with self.subTest(changes=changes), patch.object(
                    PROOF, "chrome_state", return_value={"seat_focus": {**ready, **changes}}):
                self.assertEqual(PROOF.chat_input_method_ready("socket"), accepted)
        for malformed in (None, {}, {**ready, "input_method_connected": None}):
            with self.subTest(malformed=malformed), patch.object(
                    PROOF, "chrome_state", return_value={"seat_focus": malformed}), \
                    self.assertRaises(RuntimeError):
                PROOF.chat_input_method_ready("socket")

        for changes, accepted in (
                ({}, True),
                ({"input_method_preedit_active": False}, False),
                ({"text_input_active": False}, False),
                ({"keyboard": {"app_id": "so.tilo.store"}}, False),
                ({"text_input": {"app_id": "so.tilo.store"}}, False)):
            with self.subTest(preedit=changes), patch.object(
                    PROOF, "chrome_state", return_value={"seat_focus": {**ready, **changes}}):
                self.assertEqual(PROOF.chat_input_method_preedit_active("socket"), accepted)
        for malformed in (None, {}, {**ready, "input_method_preedit_active": None}):
            with self.subTest(preedit_malformed=malformed), patch.object(
                    PROOF, "chrome_state", return_value={"seat_focus": malformed}), \
                    self.assertRaises(RuntimeError):
                PROOF.chat_input_method_preedit_active("socket")

        source = inspect.getsource(PROOF.native_ime_float_return)
        readiness = source.index("chat_input_method_ready(socket)")
        engine = source.index("fcitx_rime_active(env)", readiness)
        raw_baseline = source.index('{"op": "type", "text": committed_before}')
        baseline_preedit = source.index("chat_input_method_preedit_active(socket)", raw_baseline)
        baseline_refusal = source.index("preserve_attached_preedit_refusal(", baseline_preedit)
        ascii_guard = source.index("baseline_is_exact_rime_ascii_passthrough(", baseline_refusal)
        recovery = source.index("recover_rime_chinese_mode(", ascii_guard)
        raw_commit = source.index('{"op": "key", "key": "Enter"}', raw_baseline)
        first_preedit = source.index('{"op": "type", "text": "nihao"}', raw_commit)
        composed_preedit = source.index("chat_input_method_preedit_active(socket)", first_preedit)
        self.assertLess(readiness, engine)
        self.assertLess(engine, raw_baseline)
        self.assertLess(raw_baseline, baseline_preedit)
        self.assertLess(baseline_preedit, baseline_refusal)
        self.assertLess(baseline_refusal, ascii_guard)
        self.assertLess(ascii_guard, recovery)
        self.assertLess(recovery, raw_commit)
        self.assertLess(baseline_preedit, raw_commit)
        self.assertLess(raw_commit, first_preedit)
        self.assertLess(first_preedit, composed_preedit)
        self.assertNotIn('{"op": "key", "key": "Shift"}', source)

    def test_rime_readiness_is_read_only_and_rejects_unknown_or_inactive_engine(self):
        class Result:
            def __init__(self, returncode, stdout=""):
                self.returncode = returncode
                self.stdout = stdout

        env = {"DBUS_SESSION_BUS_ADDRESS": "fixture-bus"}
        with patch.object(
                PROOF.subprocess, "run",
                side_effect=[Result(0, "2\n"), Result(0, "rime\n")]) as run:
            self.assertTrue(PROOF.fcitx_rime_active(env))
        self.assertEqual([call.args[0] for call in run.call_args_list], [
            ["/usr/bin/fcitx5-remote"], ["/usr/bin/fcitx5-remote", "-n"],
        ])
        self.assertFalse(any(
            option in call.args[0] for call in run.call_args_list
            for option in ("-o", "-t", "-c", "-s")
        ))

        for results in (
                [Result(1)],
                [Result(0, "0\n"), Result(0, "rime\n")],
                [Result(0, "1\n"), Result(0, "rime\n")],
                [Result(0, ""), Result(0, "rime\n")],
                [Result(0, "2\n"), Result(1)],
                [Result(0, "2\n"), Result(0, "keyboard-us\n")],
                [Result(0, "2\n"), Result(0, "")]):
            with self.subTest(results=[result.returncode for result in results]), \
                    patch.object(PROOF.subprocess, "run", side_effect=results):
                self.assertFalse(PROOF.fcitx_rime_active(env))
        with patch.object(PROOF.subprocess, "run", side_effect=OSError("absent")):
            self.assertFalse(PROOF.fcitx_rime_active(env))

    def test_rime_ascii_recovery_requires_the_exact_closed_failure(self):
        seat = {
            "keyboard_is_chat": True,
            "text_input_is_chat": True,
            "text_input_active": True,
            "input_method_connected": True,
            "input_method_keyboard_grabbed": True,
            "input_method_preedit_accepted": False,
        }
        engine = {
            "fcitx_state_observable": True,
            "fcitx_state_active": True,
            "fcitx_engine_observable": True,
            "fcitx_engine_rime": True,
        }

        def observed(*, matches=True, bounds=None, seat_changes=None, engine_changes=None):
            with patch.object(PROOF, "private_editor_value_matches", return_value=matches), \
                    patch.object(PROOF, "private_editor_numeric_bounds", return_value=(
                        bounds or {"character_count": 5, "caret": 5, "selection": None}
                    )), \
                    patch.object(PROOF, "chat_input_method_observation", return_value={
                        **seat, **(seat_changes or {}),
                    }), \
                    patch.object(PROOF, "fcitx_rime_observation", return_value={
                        **engine, **(engine_changes or {}),
                    }), \
                    patch.object(PROOF, "same_fcitx_process", return_value=True):
                return PROOF.baseline_is_exact_rime_ascii_passthrough(
                    "socket", {}, "nihao", {}, {},
                )

        self.assertTrue(observed())
        self.assertFalse(observed(matches=False))
        self.assertFalse(observed(bounds={
            "character_count": 5, "caret": 4, "selection": None,
        }))
        self.assertFalse(observed(seat_changes={"input_method_preedit_accepted": True}))
        self.assertFalse(observed(engine_changes={"fcitx_state_active": False}))

    def test_rime_ascii_recovery_uses_physical_chord_then_proves_real_preedit(self):
        frames = iter(({"frame_seq": 7}, {"frame_seq": 8}))

        def immediately(_label, observe, _process):
            result = observe()
            self.assertTrue(result)
            return result

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "baseline_is_exact_rime_ascii_passthrough",
                             return_value=True), \
                patch.object(PROOF, "physical_chord") as physical, \
                patch.object(PROOF, "wait_for_private_selection", return_value={
                    "draft_matches": True, "caret": 0, "selection": [0, 5],
                }), \
                patch.object(PROOF, "wait_for", side_effect=immediately), \
                patch.object(PROOF, "private_editor_value_matches", return_value=True), \
                patch.object(PROOF, "presented_primary_frame",
                             side_effect=lambda *_args: next(frames)), \
                patch.object(PROOF, "chat_input_method_preedit_active", return_value=True), \
                patch.object(PROOF, "same_fcitx_process", return_value=True), \
                patch.object(PROOF, "fcitx_rime_active", return_value=True), \
                patch.object(PROOF, "call", return_value={"ok": {}}) as control, \
                patch.object(PROOF, "require_ok"):
            frame = PROOF.recover_rime_chinese_mode(
                "socket", pathlib.Path(directory), "1600x900", {}, None, {}, {}, "nihao",
            )

        self.assertEqual(frame, {"frame_seq": 8})
        self.assertEqual(physical.call_args_list, [
            call("socket", "a", ctrl=True),
            call("socket", "2", ctrl=True, shift=True),
        ])
        self.assertIn(call("socket", {"op": "key", "key": "Backspace"}),
                      control.call_args_list)
        self.assertIn(call("socket", {"op": "type", "text": "nihao"}),
                      control.call_args_list)

        with patch.object(PROOF, "baseline_is_exact_rime_ascii_passthrough",
                          return_value=True), \
                patch.object(PROOF, "physical_chord"), \
                patch.object(PROOF, "wait_for_private_selection", return_value={
                    "draft_matches": True, "caret": 1, "selection": [1, 5],
                }), \
                self.assertRaisesRegex(RuntimeError, "exact ASCII pass-through"):
            PROOF.recover_rime_chinese_mode(
                "socket", pathlib.Path("unused"), "1600x900", {}, None, {}, {}, "nihao",
            )


    def test_attached_preedit_refusal_records_only_closed_fixture_evidence(self):
        private = "private editor text"
        values = {"nihao": False, "nihaonihao": True, "nihao你好": False}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "call", return_value={"ok": {}}), \
                patch.object(PROOF, "require_ok"), \
                patch.object(PROOF, "private_editor_value_matches",
                             side_effect=lambda _access, value: values[value]), \
                patch.object(PROOF, "private_editor_numeric_bounds", return_value={
                    "character_count": 10, "caret": 10, "selection": None,
                }), \
                patch.object(PROOF, "chat_input_method_observation", return_value={
                    "keyboard_is_chat": True,
                    "text_input_is_chat": True,
                    "text_input_active": True,
                    "input_method_connected": True,
                    "input_method_keyboard_grabbed": True,
                    "input_method_preedit_accepted": False,
                }), \
                patch.object(PROOF, "fcitx_rime_observation", return_value={
                    "fcitx_state_observable": True,
                    "fcitx_state_active": True,
                    "fcitx_engine_observable": True,
                    "fcitx_engine_rime": True,
                }), \
                patch.object(PROOF, "same_fcitx_process", return_value=True):
            output = pathlib.Path(directory)
            PROOF.preserve_attached_preedit_refusal(
                "socket", output, "1600x900", {"private": private},
                {
                    "phase": "attached_preedit_before_float",
                    "baseline": "nihao",
                    "direct_latin": "nihaonihao",
                    "rime_commit": "nihao你好",
                }, {}, {},
            )
            evidence = json.loads((output / "ime-attached-preedit-refused.json").read_text())
        self.assertFalse(evidence["baseline_retained"])
        self.assertTrue(evidence["direct_latin_committed"])
        self.assertFalse(evidence["rime_candidate_committed"])
        self.assertTrue(evidence["fcitx_state_active"])
        self.assertTrue(evidence["fcitx_engine_rime"])
        self.assertFalse(evidence["input_method_preedit_accepted"])
        self.assertEqual(evidence["field_bounds"], {
            "character_count": 10, "caret": 10, "selection": None,
        })
        self.assertFalse(evidence["private_content_recorded"])
        self.assertNotIn(private, json.dumps(evidence))

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "call", side_effect=RuntimeError("snapshot failed")), \
                patch.object(PROOF, "private_editor_value_matches", return_value=False), \
                patch.object(PROOF, "private_editor_numeric_bounds", return_value={
                    "character_count": 0, "caret": 0, "selection": None,
                }), \
                patch.object(PROOF, "chat_input_method_observation", return_value={
                    "input_method_preedit_accepted": False,
                }), \
                patch.object(PROOF, "fcitx_rime_observation", return_value={
                    "fcitx_state_active": True,
                }), \
                patch.object(PROOF, "same_fcitx_process", return_value=True):
            output = pathlib.Path(directory)
            PROOF.preserve_attached_preedit_refusal(
                "socket", output, "1600x900", {}, {
                    "phase": "baseline_preedit_before_float",
                    "baseline": "",
                    "direct_latin": "nihao",
                    "rime_commit": "你好",
                }, {}, {},
            )
            evidence = json.loads((output / "ime-attached-preedit-refused.json").read_text())
        self.assertNotIn("diagnostic_unavailable", evidence)
        self.assertTrue(evidence["fcitx_state_active"])

    def test_detached_commit_refusal_records_only_closed_fixture_evidence(self):
        private = "private editor text"
        values = {
            "nihao": True,
            "nihao你好": False,
            "nihaonihao ": False,
            "nihao你": True,
            "nihao ": False,
        }
        current = {
            "keyboard_focused": False,
            "input_route": "closed route value",
            "input_source_surface_id": 91,
        }
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "call", return_value={"ok": {}}), \
                patch.object(PROOF, "require_ok"), \
                patch.object(PROOF, "current_same_floating_presentation",
                             return_value=current), \
                patch.object(PROOF, "private_editor_value_matches",
                             side_effect=lambda _access, value: values[value]), \
                patch.object(PROOF, "chat_text_input_active", return_value=True), \
                patch.object(PROOF, "same_fcitx_process", return_value=True):
            output = pathlib.Path(directory)
            PROOF.preserve_detached_commit_refusal(
                "socket", output, "1600x900", {"private": private},
                "nihao", "nihao你好", {"surface_id": 91}, {},
            )
            evidence = json.loads(
                (output / "ime-detached-commit-refused.json").read_text()
            )
        self.assertTrue(evidence["baseline_retained"])
        self.assertFalse(evidence["expected_chinese_committed"])
        self.assertFalse(evidence["direct_latin_space_committed"])
        self.assertTrue(evidence["candidate_prefix_committed"])
        self.assertFalse(evidence["literal_space_committed"])
        self.assertTrue(evidence["same_presented_child"])
        self.assertFalse(evidence["keyboard_focused"])
        self.assertTrue(evidence["chat_text_input_active"])
        self.assertTrue(evidence["input_route_present"])
        self.assertTrue(evidence["input_source_present"])
        self.assertTrue(evidence["same_input_method_process"])
        self.assertFalse(evidence["private_content_recorded"])
        serialized = json.dumps(evidence)
        self.assertNotIn(private, serialized)
        self.assertNotIn("closed route value", serialized)
        self.assertNotIn("91", serialized)

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

    def test_float_preedit_guard_classifies_value_activity_and_exact_seat_without_content(self):
        primary = {"app_id": "so.tilo.chat", "surface_id": 7, "frame_seq": 10,
                   "presented": True, "keyboard_focused": True}
        child = {"app_id": "so.tilo.chat", "surface_id": 11, "floating_id": "private",
                 "frame_seq": 20, "presented": True, "pixels_ready": True,
                 "last_error": None, "keyboard_focused": False, "suspended": False,
                 "transferring": False, "input_route": "floating-preedit-source",
                 "input_source_surface_id": 7, "private_text": "secret"}
        response = lambda row: {"answer": {"frames": [primary, row]}}
        observed = PROOF.detached_preedit_guard_observation(
            response(child), primary, child, True, True, False)
        self.assertTrue(observed["baseline_value_matches"])
        self.assertTrue(observed["chat_text_input_active"])
        self.assertFalse(observed["input_method_preedit_active"])
        self.assertTrue(observed["source_preedit_proxy_valid"])
        self.assertFalse(observed["direct_child_seat_valid"])
        self.assertTrue(all(type(value) is bool for value in observed.values()))
        self.assertNotIn("secret", json.dumps(observed))
        self.assertNotIn("surface_id", json.dumps(observed))
        focused = {**child, "keyboard_focused": True, "input_route": None,
                   "input_source_surface_id": None}
        direct = PROOF.detached_preedit_guard_observation(
            response(focused), primary, child, False, True, True)
        self.assertFalse(direct["baseline_value_matches"])
        self.assertTrue(direct["direct_child_seat_valid"])
        self.assertFalse(direct["source_preedit_proxy_valid"])
        foreign = PROOF.detached_preedit_guard_observation(
            response({**child, "input_source_surface_id": 8}), primary, child, True, True, True)
        self.assertFalse(foreign["source_matches_exact"])
        self.assertFalse(foreign["source_preedit_proxy_valid"])
        with self.assertRaises(RuntimeError):
            PROOF.detached_preedit_guard_observation(response(child), primary, child, "secret", True, True)
        source = inspect.getsource(PROOF.native_ime_float_return)
        receipt = source.index('"ime-detached-preedit-guard.json"')
        refusal = source.index('"Float committed or deactivated the live Rime preedit"')
        self.assertLess(receipt, refusal)

    def test_return_input_samples_are_bounded_closed_and_do_not_accept_a_route(self):
        primary = {"app_id": "so.tilo.chat", "surface_id": 7, "frame_seq": 5,
                   "presented": True, "keyboard_focused": False, "private": "secret"}
        child = {"app_id": "so.tilo.chat", "surface_id": 11, "floating_id": "private",
                 "frame_seq": 6, "presented": False, "keyboard_focused": True,
                 "suspended": True, "transferring": False}
        response = {"answer": {"frames": [primary, child]}}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "call", return_value=response) as calls, \
                patch.object(PROOF.time, "sleep") as sleep, \
                patch.object(PROOF, "chat_text_input_active", return_value=True), \
                patch.object(PROOF, "chat_input_method_preedit_active", return_value=False), \
                patch.object(PROOF, "same_fcitx_process", return_value=True):
            output = pathlib.Path(directory)
            self.assertIs(PROOF.capture_return_input_observations(
                "socket", output, primary, child, True, object()), response)
            self.assertEqual(calls.call_count, 4)
            self.assertEqual(sleep.call_args_list, [call(0.05)] * 3)
            samples = json.loads((output / "ime-returned-input-observations.json").read_text())
            self.assertEqual(len(samples), 4)
            self.assertTrue(all(type(value) is bool for sample in samples for value in sample.values()))
            self.assertTrue(all(sample["observation_valid"] for sample in samples))
            self.assertFalse(any(sample["route_matches_expected"] for sample in samples))
            self.assertNotIn("secret", json.dumps(samples))
            self.assertNotIn("surface_id", json.dumps(samples))
            with self.assertRaises(RuntimeError):
                PROOF.returned_ime_input_route(response, primary, child)
            calls.return_value = {"answer": {"frames": [primary]}}
            with self.assertRaisesRegex(RuntimeError, "diagnostic observation failed"):
                PROOF.capture_return_input_observations("socket", output, primary, child, True, object())
            failed = json.loads((output / "ime-returned-input-observations.json").read_text())
            self.assertEqual(len(failed), 1)
            self.assertFalse(failed[0]["observation_valid"])
            calls.return_value = response
            with patch.object(PROOF, "chat_text_input_active", return_value="secret"):
                with self.assertRaisesRegex(RuntimeError, "diagnostic observation failed"):
                    PROOF.capture_return_input_observations("socket", output, primary, child, True, object())
            malformed = (output / "ime-returned-input-observations.json").read_text()
            self.assertNotIn("secret", malformed)
            self.assertTrue(all(type(value) is bool for sample in json.loads(malformed)
                                for value in sample.values()))

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
        refresh = source.index("returned, returned_input = wait_for_return_input_route(", ready)
        diagnostic = source.index('"ime-returned-native.json"', refresh)
        assertion = source.index('returned_access["process"] != chat_process', diagnostic)
        self.assertLess(ready, refresh)
        self.assertLess(refresh, diagnostic)
        self.assertLess(diagnostic, assertion)
        self.assertIn('"ime-returned-first-frame-', source)

    def test_owned_editor_refreshes_only_retired_text_and_retains_real_selection_assertions(self):
        original = {"bus": "chat-bus", "process": 123, "agent": "Tide", "detached": True,
                    "composer": "recycled-button"}
        fresh = {**original, "composer": "current-editor"}
        class Client:
            def __init__(self, *_args):
                pass
            def property(self, *_args):
                return "(<'Message composer'>,)"
            def interfaces(self, *_args):
                return ["org.a11y.atspi.Text", "org.a11y.atspi.EditableText"]
            def call(self, path, method, *_args, **_kwargs):
                if path == "recycled-button":
                    raise PROOF.atspi.GateError(
                        "org.freedesktop.DBus.Error.UnknownInterface: org.a11y.atspi.Text PRIVATE-CANARY")
                if method.endswith("GetText"):
                    return "('nihao你好',)"
                if method.endswith("Properties.Get"):
                    return "(<int32 7>,)"
                if method.endswith("GetNSelections"):
                    return "(int32 1,)"
                if method.endswith("GetSelection"):
                    return "(int32 5, int32 7)"
                if method.endswith("GetState"):
                    return f"([uint32 {PROOF.atspi.FOCUSED_STATE_BIT}, uint32 0],)"
                raise AssertionError(method)
        with patch.object(PROOF.atspi, "atspi_address", return_value="private-address"), \
                patch.object(PROOF.atspi, "AtspiClient", Client), \
                patch.object(PROOF, "process_program", return_value="chat"), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=fresh.copy()) as resolve:
            access = original.copy()
            observed = PROOF.private_editor_observation(access, "nihao你好")
            self.assertEqual(observed["selection"], [5, 7])
            self.assertEqual(observed["caret"], 7)
            self.assertTrue(observed["draft_matches"])
            self.assertTrue(observed["accessibility_focused"])
            self.assertEqual(access, fresh)
            resolve.assert_called_once_with("Tide", detached=True, stale_only=True)
            resolve.reset_mock()
            with self.assertRaisesRegex(RuntimeError, "expected test draft"):
                PROOF.private_editor_observation(fresh.copy(), "PRIVATE-CANARY")
            resolve.assert_not_called()
            for change in ({"process": 456}, {"detached": False}, {"agent": "Other"}):
                with self.subTest(change=change), patch.object(PROOF, "agent_conversation_accessibility",
                                                               return_value={**fresh, **change}):
                    access = original.copy()
                    with self.assertRaisesRegex(RuntimeError, "foreign Chat route"):
                        PROOF.private_editor_observation(access, "nihao你好")
                    self.assertEqual(access, original)
            with patch.object(PROOF, "process_program", return_value=None):
                with self.assertRaisesRegex(RuntimeError, "lost its original Chat process"):
                    PROOF.private_editor_observation(original.copy(), "nihao你好")
            with patch.object(PROOF, "agent_conversation_accessibility", return_value=None), \
                    patch.object(PROOF.time, "sleep"), \
                    patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 31]):
                with self.assertRaises(PROOF.ProofTimeout):
                    PROOF.private_editor_observation(original.copy(), "nihao你好")
        for message in ("org.freedesktop.DBus.Error.AccessDenied",
                        "org.freedesktop.DBus.Error.UnknownInterface: org.a11y.atspi.Action"):
            self.assertFalse(PROOF.stale_editor_transport(PROOF.atspi.GateError(message)))
        self.assertTrue(PROOF.stale_editor_transport(PROOF.atspi.GateError(
            "org.freedesktop.DBus.Error.UnknownObject")))

    def test_recycled_text_heading_is_not_read_and_valid_editor_mismatch_is_not_retried(self):
        cached = {"bus": "chat-bus", "process": 123, "agent": "Tide", "detached": True,
                  "composer": "recycled-heading"}
        fresh = {**cached, "composer": "message-editor"}
        payload_paths = []
        class Client:
            def __init__(self, *_args):
                pass
            def property(self, path, _name):
                return "(<'Unrelated heading'>,)" if path == "recycled-heading" \
                    else "(<'Message composer'>,)"
            def interfaces(self, _path):
                return ["org.a11y.atspi.Text", "org.a11y.atspi.EditableText"]
            def call(self, path, _method, *_args):
                payload_paths.append(path)
                if path == "recycled-heading":
                    raise AssertionError("unrelated private payload must never be consumed")
                return "('nihao',)"
        with patch.object(PROOF.atspi, "atspi_address", return_value="private-address"), \
                patch.object(PROOF.atspi, "AtspiClient", Client), \
                patch.object(PROOF, "process_program", return_value="chat"), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=fresh.copy()) as resolve:
            access = cached.copy()
            self.assertTrue(PROOF.private_editor_value_matches(access, "nihao"))
            self.assertEqual(payload_paths, ["message-editor"])
            self.assertEqual(access, fresh)
            resolve.assert_called_once_with("Tide", detached=True, stale_only=True)
            resolve.reset_mock()
            self.assertFalse(PROOF.private_editor_value_matches(access, "PRIVATE-CANARY"))
            resolve.assert_not_called()
            self.assertEqual(payload_paths, ["message-editor", "message-editor"])
            # A full discovery may itself retire before the contract query.
            # Discard that candidate without reading it or changing ownership.
            with patch.object(PROOF, "agent_conversation_accessibility",
                              side_effect=[cached.copy(), fresh.copy()]) as rediscover, \
                    patch.object(PROOF.time, "sleep"):
                access = cached.copy()
                self.assertTrue(PROOF.private_editor_value_matches(access, "nihao"))
                self.assertEqual(rediscover.call_count, 2)
                self.assertEqual(access, fresh)
                self.assertNotIn("recycled-heading", payload_paths)
            with patch.object(PROOF, "agent_conversation_accessibility", return_value=cached.copy()), \
                    patch.object(PROOF.time, "sleep"), \
                    patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 31]):
                access = cached.copy()
                with self.assertRaises(PROOF.ProofTimeout):
                    PROOF.private_editor_value_matches(access, "nihao")
                self.assertEqual(access, cached)
                self.assertNotIn("recycled-heading", payload_paths)

    def return_editor_continuity_subjects(self):
        primary = {"app_id": "so.tilo.chat", "surface_id": 9973, "frame_seq": 40,
                   "presented": True, "pixels_ready": True, "last_error": None,
                   "keyboard_focused": False}
        child = {"app_id": "so.tilo.chat", "surface_id": 9974, "frame_seq": 22,
                 "floating_id": "private-native-child", "presented": False,
                 "keyboard_focused": True, "suspended": True, "transferring": True,
                 "last_error": None}
        access = {"agent": "Tide", "process": 9975, "detached": False}
        before = {"draft_matches": True, "caret": 5, "selection": [4, 5],
                  "accessibility_focused": True}
        expected = {"access": {**access, "detached": True},
                    "draft": "draft-secret", "observation": before}
        return primary, child, access, before, expected

    def physical_send_control_subjects(self):
        primary, _, access, before, _ = self.return_editor_continuity_subjects()
        primary = {**primary, "keyboard_focused": True}
        access = {**access, "bus": "private-bus", "window": "private-window",
                  "composer": "private-composer", "send_controls": ["private-send"]}
        return primary, access, before

    def test_physical_send_control_reobserves_known_stale_target_from_fresh_tree(self):
        primary, access, before = self.physical_send_control_subjects()
        fresh = {**access, "send_controls": ["fresh-private-send"]}
        stale = PROOF.atspi.GateError("org.freedesktop.DBus.Error.UnknownObject private-send")
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "call", return_value={"answer": {"frames": [primary]}}), \
                patch.object(PROOF, "agent_conversation_accessibility",
                             side_effect=[access, fresh]) as discover, \
                patch.object(PROOF, "chat_editor_contract", return_value=True), \
                patch.object(PROOF, "_private_editor_observation", return_value=before), \
                patch.object(PROOF, "accessibility_control_enabled",
                             side_effect=[stale, True]) as enabled, \
                patch.object(PROOF.time, "sleep"):
            output = pathlib.Path(directory)
            result = PROOF.wait_for_owned_physical_send_control(
                "socket", output, access, "draft-secret", primary, None)
            self.assertEqual(result, (fresh, "fresh-private-send", primary))
            self.assertEqual(discover.call_args_list,
                             [call("Tide", stale_only=True, include_send=True)] * 2)
            self.assertEqual(enabled.call_args_list,
                             [call(access, "private-send"), call(fresh, "fresh-private-send")])
            payload = (output / "physical-send-control-observations.json").read_text()
            receipt = json.loads(payload)
            self.assertEqual(receipt["observation_count"], 2)
            self.assertTrue(receipt["initial"]["known_stale_transport"])
            self.assertFalse(receipt["initial"]["accepted"])
            self.assertTrue(receipt["last"]["accepted"])
            for forbidden in ("private-send", "private-bus", "private-window",
                              "draft-secret", "9973", "9975", "UnknownObject"):
                self.assertNotIn(forbidden, payload)
            self.assertTrue(all(type(flag) is bool for row in
                                (receipt["initial"], receipt["last"]) for flag in row.values()))

    def test_physical_send_control_persistent_stale_transport_times_out(self):
        primary, access, before = self.physical_send_control_subjects()
        stale = PROOF.atspi.GateError(
            "org.freedesktop.DBus.Error.UnknownInterface org.a11y.atspi.Text")
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "call", return_value={"answer": {"frames": [primary]}}), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "chat_editor_contract", return_value=True), \
                patch.object(PROOF, "_private_editor_observation", side_effect=stale), \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 31]):
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_owned_physical_send_control(
                    "socket", pathlib.Path(directory), access, "draft-secret", primary, None)

    def test_physical_send_control_nonstale_transport_fails_immediately(self):
        primary, access, before = self.physical_send_control_subjects()
        error = PROOF.atspi.GateError(
            "org.freedesktop.DBus.Error.UnknownInterface org.a11y.atspi.Accessible")
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "call", return_value={"answer": {"frames": [primary]}}), \
                patch.object(PROOF, "agent_conversation_accessibility", side_effect=error), \
                patch.object(PROOF.time, "sleep") as sleep:
            with self.assertRaises(PROOF.atspi.GateError):
                PROOF.wait_for_owned_physical_send_control(
                    "socket", pathlib.Path(directory), access, "draft-secret", primary, None)
            sleep.assert_not_called()
            receipt = json.loads((pathlib.Path(directory) /
                                  "physical-send-control-observations.json").read_text())
            self.assertTrue(receipt["last"]["other_transport_failure"])

    def test_physical_send_control_foreign_owner_or_changed_draft_fails_immediately(self):
        primary, access, before = self.physical_send_control_subjects()
        cases = [("process", {**access, "process": 9976}, before),
                 ("agent", {**access, "agent": "Foreign"}, before),
                 ("route", {**access, "detached": True}, before),
                 ("draft", access, {**before, "draft_matches": False})]
        for label, discovered, observed in cases:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "call", return_value={"answer": {"frames": [primary]}}), \
                    patch.object(PROOF, "agent_conversation_accessibility", return_value=discovered), \
                    patch.object(PROOF, "chat_editor_contract", return_value=True), \
                    patch.object(PROOF, "_private_editor_observation", return_value=observed), \
                    patch.object(PROOF, "accessibility_control_enabled") as enabled, \
                    patch.object(PROOF.time, "sleep") as sleep:
                with self.assertRaises(RuntimeError):
                    PROOF.wait_for_owned_physical_send_control(
                        "socket", pathlib.Path(directory), access, "draft-secret", primary, None)
                sleep.assert_not_called()
                enabled.assert_not_called()

    def test_physical_send_control_missing_native_owner_or_control_never_passes(self):
        primary, access, before = self.physical_send_control_subjects()
        cases = [("missing-primary", [], access, True),
                 ("foreign-seat", [{**primary, "input_route": "foreign"}], access, True),
                 ("unfocused", [{**primary, "keyboard_focused": False}], access, True),
                 ("missing-send", [primary], {**access, "send_controls": []}, True),
                 ("ambiguous-send", [primary], {**access, "send_controls": ["a", "b"]}, True),
                 ("disabled-send", [primary], access, False)]
        for label, rows, discovered, enabled in cases:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "call", return_value={"answer": {"frames": rows}}), \
                    patch.object(PROOF, "agent_conversation_accessibility", return_value=discovered), \
                    patch.object(PROOF, "chat_editor_contract", return_value=True), \
                    patch.object(PROOF, "_private_editor_observation", return_value=before), \
                    patch.object(PROOF, "accessibility_control_enabled", return_value=enabled), \
                    patch.object(PROOF.time, "sleep") as sleep:
                with self.assertRaises(RuntimeError):
                    PROOF.wait_for_owned_physical_send_control(
                        "socket", pathlib.Path(directory), access, "draft-secret", primary, None)
                sleep.assert_not_called()

    def test_physical_send_observer_uses_single_fresh_tree_without_repair_inputs(self):
        source = inspect.getsource(PROOF.wait_for_owned_physical_send_control)
        for forbidden in ("accessible_subtree", "GrabFocus", '"op": "key"',
                          '"op": "type"', "SetText", "DoAction", "invoke("):
            self.assertNotIn(forbidden, source)
        self.assertIn("stale_only=True", source)
        self.assertIn("include_send=True", source)
        discovery = inspect.getsource(PROOF.agent_conversation_accessibility)
        self.assertIn('matches[key]["send_controls"] = client.named(tree, "Send message")', discovery)
        journey = inspect.getsource(PROOF.physical_send_after_stop)
        self.assertLess(journey.index("wait_for_owned_physical_send_control("),
                        journey.index("focus_agent_conversation_control("))
        after_observer = journey.split("wait_for_owned_physical_send_control(", 1)[1]
        after_arguments = after_observer.split("    )\n", 1)[1]
        self.assertTrue(after_arguments.startswith(
            '    focus_agent_conversation_control(access, send_control, "Send message")\n'))
        self.assertLess(journey.index("focus_agent_conversation_control("),
                        journey.index('"before-physical-send-'))
        self.assertLess(journey.index("send_log_offset = log.stat().st_size"),
                        journey.index('"op": "key", "key": "Enter"'))

    def test_return_editor_wait_observes_adoption_then_real_focus_publication(self):
        primary, child, access, before, expected = self.return_editor_continuity_subjects()
        ready = {**primary, "keyboard_focused": True}
        retired = {**child, "keyboard_focused": False, "transferring": False}
        response = lambda rows: {"answer": {"frames": rows}}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "private_editor_observation", side_effect=[
                    {**before, "accessibility_focused": False},
                    {**before, "accessibility_focused": False}, before.copy()]), \
                patch.object(PROOF, "call", side_effect=[response([primary, child]),
                    response([ready, retired]), response([ready, retired])]) as observe_native:
            output = pathlib.Path(directory)
            observed = PROOF.wait_for_return_editor_continuity(
                "socket", output, primary, child, expected, None)
            self.assertEqual(observed, (ready, access, before))
            self.assertEqual(observe_native.call_args_list,
                             [call("socket", {"op": "foreign_frames"})] * 3)
            receipt = json.loads((output / "return-editor-adoption-observations.json").read_text())
            self.assertEqual(receipt["observation_count"], 3)
            self.assertTrue(receipt["initial"]["pending_adoption"])
            self.assertFalse(receipt["initial"]["accessibility_focus_equal"])
            self.assertTrue(receipt["last"]["accepted"])
            self.assertTrue(receipt["last"]["accessibility_focus_equal"])
            for sample in (receipt["initial"], receipt["last"]):
                self.assertTrue(all(type(value) is bool for value in sample.values()))
                self.assertTrue(sample["content_matches"] and sample["caret_equal"]
                                and sample["selection_equal"])
            for secret in ("draft-secret", "private-native-child", "9973", "9974", "9975"):
                self.assertNotIn(secret, json.dumps(receipt))
        source = inspect.getsource(PROOF.native_float_return)
        wait = source.index("wait_for_return_editor_continuity(")
        assertion = source.index("if attached != before_return_editor:", wait)
        repair = source.index("physically_click_editor(", assertion)
        self.assertLess(wait, assertion)
        self.assertLess(assertion, repair)

    def test_return_editor_wait_refuses_mutation_and_foreign_or_lost_native_seats(self):
        primary, child, access, before, expected = self.return_editor_continuity_subjects()
        response = lambda rows: {"answer": {"frames": rows}}
        cases = [
            ({**before, "draft_matches": False}, access, primary, child),
            ({**before, "caret": 4}, access, primary, child),
            ({**before, "selection": None}, access, primary, child),
            (before, {**access, "process": 9976}, primary, child),
            (before, {**access, "detached": True}, primary, child),
            (before, access, {**primary, "surface_id": 9976}, child),
            (before, access, {**primary, "frame_seq": 39}, child),
            (before, access, primary, {**child, "floating_id": "foreign"}),
            (before, access, primary, {**child, "frame_seq": 21}),
            (before, access, primary, {**child, "keyboard_focused": False}),
            (before, access, primary, {**child, "suspended": False}),
            (before, access, primary, {**child, "presented": True}),
            (before, access, {**primary, "input_route": "floating-preedit-child"}, child),
            (before, access, primary, {**child, "keyboard_focused": None}),
            (before, access, {**primary, "keyboard_focused": True}, child),
        ]
        with tempfile.TemporaryDirectory() as directory:
            for observation, current_access, current_primary, current_child in cases:
                with self.subTest(case=(observation, current_access, current_primary, current_child)), \
                        patch.object(PROOF, "agent_conversation_accessibility", return_value=current_access), \
                        patch.object(PROOF, "private_editor_observation", return_value=observation), \
                        patch.object(PROOF, "call", return_value=response([current_primary, current_child])), \
                        patch.object(PROOF.time, "sleep") as sleep:
                    with self.assertRaises(RuntimeError):
                        PROOF.wait_for_return_editor_continuity(
                            "socket", pathlib.Path(directory), primary, child, expected, None)
                    sleep.assert_not_called()

    def test_keyboard_return_uses_same_strict_continuity_after_exact_primary_repaint(self):
        primary, child, access, before, expected = self.return_editor_continuity_subjects()
        ready = {**primary, "keyboard_focused": True}
        retired = {**child, "keyboard_focused": False, "transferring": False}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "private_editor_observation", side_effect=[
                    {**before, "accessibility_focused": False}, before.copy()]), \
                patch.object(PROOF, "call", side_effect=[
                    {"answer": {"frames": [primary, child]}},
                    {"answer": {"frames": [ready, retired]}}]):
            output = pathlib.Path(directory)
            self.assertEqual(PROOF.wait_for_return_editor_continuity(
                "socket", output, primary, child, expected, None, keyboard=True),
                (ready, access, before))
            receipt = json.loads((output / "keyboard-return-editor-adoption-observations.json").read_text())
            self.assertTrue(receipt["initial"]["pending_adoption"])
            self.assertTrue(receipt["last"]["accepted"])
            self.assertFalse((output / "return-editor-adoption-observations.json").exists())
            self.assertNotIn("draft-secret", json.dumps(receipt))
        source = inspect.getsource(PROOF.native_float_return)
        baseline = source.index("keyboard_before_return = current_same_primary_presentation(")
        enter = source.index('{"op": "key", "key": "Enter"}', baseline)
        repaint = source.index("keyboard_primary = wait_for(", enter)
        continuity = source.index("wait_for_return_editor_continuity(", repaint)
        strict = source.index("if keyboard_attached != keyboard_detached:", continuity)
        next_phase = source.index("native_semantic_float_return(", strict)
        self.assertLess(baseline, enter)
        self.assertLess(enter, repaint)
        self.assertLess(repaint, continuity)
        self.assertLess(continuity, strict)
        between = source[continuity:next_phase]
        self.assertNotIn("physically_click_editor(", between)
        self.assertNotIn("focus_agent_conversation_control(", between)
        self.assertNotIn('"op": "key"', between)
        self.assertIn("keyboard=True", between)

    def test_return_editor_wait_bounds_transient_accessibility_publication_by_exact_native_route(self):
        primary, child, access, before, expected = self.return_editor_continuity_subjects()
        ready = {**primary, "keyboard_focused": True}
        retired = {**child, "keyboard_focused": False, "transferring": False}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF, "agent_conversation_accessibility", side_effect=[None, access]), \
                patch.object(PROOF, "private_editor_observation", return_value=before) as editor, \
                patch.object(PROOF, "call", side_effect=[
                    {"answer": {"frames": [primary, child]}},
                    {"answer": {"frames": [ready, retired]}}]):
            output = pathlib.Path(directory)
            self.assertEqual(PROOF.wait_for_return_editor_continuity(
                "socket", output, primary, child, expected, None), (ready, access, before))
            editor.assert_called_once_with(access, "draft-secret")
            receipt = json.loads((output / "return-editor-adoption-observations.json").read_text())
            self.assertTrue(receipt["initial"]["pending_accessibility_publication"])
            self.assertTrue(receipt["initial"]["exact_primary_present"])
            self.assertTrue(receipt["initial"]["exact_child_exists"])
            self.assertFalse(receipt["initial"]["accepted"])
            self.assertTrue(receipt["last"]["accepted"])
            self.assertTrue(receipt["last"]["accessibility_observable"])
            self.assertNotIn("draft-secret", json.dumps(receipt))
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 31]), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=None), \
                patch.object(PROOF, "private_editor_observation") as editor, \
                patch.object(PROOF, "call", return_value={"answer": {"frames": [ready, retired]}}):
            output = pathlib.Path(directory)
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_return_editor_continuity(
                    "socket", output, primary, child, expected, None)
            editor.assert_not_called()
            receipt = json.loads((output / "return-editor-adoption-observations.json").read_text())
            self.assertTrue(receipt["last"]["pending_accessibility_publication"])
            self.assertFalse(receipt["last"]["accepted"])
        for rows in ([ready], [{**ready, "surface_id": 9976}, retired],
                     [primary, {**child, "keyboard_focused": False}],
                     [ready, {**retired, "suspended": None}],
                     [{**ready, "input_route": "foreign"}, retired]):
            with self.subTest(rows=rows), tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF.time, "sleep") as sleep, \
                    patch.object(PROOF, "agent_conversation_accessibility", return_value=None) as accessibility, \
                    patch.object(PROOF, "private_editor_observation") as editor, \
                    patch.object(PROOF, "call", return_value={"answer": {"frames": rows}}):
                with self.assertRaises(RuntimeError):
                    PROOF.wait_for_return_editor_continuity(
                        "socket", pathlib.Path(directory), primary, child, expected, None)
                accessibility.assert_not_called()
                editor.assert_not_called()
                sleep.assert_not_called()

    def test_return_editor_adoption_diagnostics_classify_pixel_and_error_refusals_without_details(self):
        primary, child, _, _, expected = self.return_editor_continuity_subjects()
        ready = {**primary, "keyboard_focused": True}
        retired = {**child, "keyboard_focused": False, "transferring": False}
        for current, returned_child, pixels, child_error in (
                ({**ready, "pixels_ready": False}, retired, False, False),
                (ready, {**retired, "last_error": "private-renderer-detail"}, True, True)):
            with self.subTest(pixels=pixels, child_error=child_error), \
                    tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "call", return_value={
                        "answer": {"frames": [current, returned_child]}}), \
                    patch.object(PROOF, "agent_conversation_accessibility") as accessibility, \
                    patch.object(PROOF.time, "sleep") as sleep:
                output = pathlib.Path(directory)
                with self.assertRaisesRegex(RuntimeError, "ordinary native adoption route"):
                    PROOF.wait_for_return_editor_continuity(
                        "socket", output, primary, child, expected, None)
                accessibility.assert_not_called()
                sleep.assert_not_called()
                receipt = json.loads((output / "return-editor-adoption-observations.json").read_text())
                self.assertTrue(receipt["last"]["primary_presented"])
                self.assertEqual(receipt["last"]["primary_pixels_ready"], pixels)
                self.assertFalse(receipt["last"]["primary_error_present"])
                self.assertEqual(receipt["last"]["child_error_present"], child_error)
                self.assertFalse(receipt["last"]["accepted"])
                self.assertNotIn("private-renderer-detail", json.dumps(receipt))

    def test_return_editor_waits_for_explicit_exact_primary_capture_without_accepting_pending_pixels(self):
        primary, child, access, before, expected = self.return_editor_continuity_subjects()
        pending = {**primary, "keyboard_focused": True, "pixels_ready": False,
                   "capture_pending": True}
        ready = {**primary, "keyboard_focused": True, "capture_pending": False}
        retired = {**child, "keyboard_focused": False, "transferring": False}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "private_editor_observation", return_value=before) as editor, \
                patch.object(PROOF, "call", side_effect=[
                    {"answer": {"frames": [pending, retired]}},
                    {"answer": {"frames": [ready, retired]}}]):
            output = pathlib.Path(directory)
            self.assertEqual(PROOF.wait_for_return_editor_continuity(
                "socket", output, primary, child, expected, None, semantic=True),
                (ready, access, before))
            self.assertEqual(editor.call_count, 2)
            receipt = json.loads((output / "semantic-return-editor-adoption-observations.json").read_text())
            self.assertTrue(receipt["initial"]["primary_capture_pending"])
            self.assertTrue(receipt["initial"]["pending_primary_pixels"])
            self.assertFalse(receipt["initial"]["accepted"])
            self.assertTrue(receipt["initial"]["content_matches"])
            self.assertTrue(receipt["initial"]["caret_equal"])
            self.assertTrue(receipt["initial"]["selection_equal"])
            self.assertTrue(receipt["last"]["primary_pixels_ready"])
            self.assertTrue(receipt["last"]["accepted"])
            self.assertNotIn("draft-secret", json.dumps(receipt))

    def test_return_editor_pending_capture_never_masks_mutation_or_extends_deadline(self):
        primary, child, access, before, expected = self.return_editor_continuity_subjects()
        pending = {**primary, "keyboard_focused": True, "pixels_ready": False,
                   "capture_pending": True}
        retired = {**child, "keyboard_focused": False, "transferring": False}
        response = {"answer": {"frames": [pending, retired]}}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "private_editor_observation", side_effect=[
                    before.copy(), {**before, "caret": 4}]) as editor, \
                patch.object(PROOF, "call", return_value=response):
            with self.assertRaisesRegex(RuntimeError, "content, caret or selection"):
                PROOF.wait_for_return_editor_continuity(
                    "socket", pathlib.Path(directory), primary, child, expected, None)
            self.assertEqual(editor.call_count, 2)
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 31]), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "private_editor_observation", return_value=before) as editor, \
                patch.object(PROOF, "call", return_value=response):
            output = pathlib.Path(directory)
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_return_editor_continuity(
                    "socket", output, primary, child, expected, None)
            editor.assert_called_once_with(access, "draft-secret")
            receipt = json.loads((output / "return-editor-adoption-observations.json").read_text())
            self.assertTrue(receipt["last"]["pending_primary_pixels"])
            self.assertFalse(receipt["last"]["accepted"])

    def test_return_editor_missing_pixels_without_exact_pending_capture_still_fail_immediately(self):
        primary, child, _, _, expected = self.return_editor_continuity_subjects()
        pending = {**primary, "keyboard_focused": True, "pixels_ready": False}
        retired = {**child, "keyboard_focused": False, "transferring": False}
        for current, current_child in (
                (pending, retired), ({**pending, "capture_pending": False}, retired),
                ({**pending, "capture_pending": 1}, retired),
                ({**pending, "capture_pending": None}, retired),
                ({**pending, "capture_pending": True, "pixels_ready": True}, retired),
                ({**pending, "capture_pending": True, "surface_id": 9976}, retired),
                ({**pending, "capture_pending": True}, {**retired, "keyboard_focused": True}),
                ({**pending, "capture_pending": True}, {**retired, "last_error": "private-error"})):
            with self.subTest(current=current, child=current_child), tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF.time, "sleep") as sleep, \
                    patch.object(PROOF, "agent_conversation_accessibility") as accessibility, \
                    patch.object(PROOF, "call", return_value={
                        "answer": {"frames": [current, current_child]}}):
                with self.assertRaises(RuntimeError):
                    PROOF.wait_for_return_editor_continuity(
                        "socket", pathlib.Path(directory), primary, child, expected, None)
                sleep.assert_not_called()
                accessibility.assert_not_called()

    def test_return_editor_wait_never_accepts_unpublished_focus_or_masks_later_mutation(self):
        primary, child, access, before, expected = self.return_editor_continuity_subjects()
        ready = {**primary, "keyboard_focused": True}
        retired = {**child, "keyboard_focused": False, "transferring": False}
        response = {"answer": {"frames": [ready, retired]}}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "call", return_value=response), \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF, "private_editor_observation", return_value={
                    **before, "accessibility_focused": False}), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 31]):
            output = pathlib.Path(directory)
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_return_editor_continuity(
                    "socket", output, primary, child, expected, None)
            receipt = json.loads((output / "return-editor-adoption-observations.json").read_text())
            self.assertTrue(receipt["last"]["pending_focus_publication"])
            self.assertFalse(receipt["last"]["accepted"])
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "call", return_value=response), \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF, "private_editor_observation", side_effect=[
                    {**before, "accessibility_focused": False}, {**before, "caret": 4}]):
            with self.assertRaisesRegex(RuntimeError, "content, caret or selection"):
                PROOF.wait_for_return_editor_continuity(
                    "socket", pathlib.Path(directory), primary, child, expected, None)

    def test_keyboard_return_wait_requires_exact_ordinary_child_seat_before_enter(self):
        child = {"app_id": "so.tilo.chat", "surface_id": 11, "floating_id": "private",
                 "frame_seq": 22, "presented": True, "pixels_ready": True,
                 "keyboard_focused": False, "suspended": False, "transferring": False,
                 "last_error": None}
        focused = {**child, "frame_seq": 23, "keyboard_focused": True}
        response = lambda row: {"answer": {"frames": [row]}}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF, "call", side_effect=[response(child), response(focused)]):
            output = pathlib.Path(directory)
            self.assertEqual(PROOF.wait_for_floating_keyboard_seat(
                "socket", child, output, None), focused)
            receipt = json.loads((output / "keyboard-return-child-seat.json").read_text())
            self.assertEqual(receipt["observation_count"], 2)
            self.assertFalse(receipt["initial"]["accepted"])
            self.assertTrue(receipt["last"]["accepted"])
            self.assertNotIn("private", json.dumps(receipt))
            for changes in ({"surface_id": 99}, {"floating_id": "other"},
                            {"frame_seq": 21}, {"keyboard_focused": None},
                            {"input_route": "floating-preedit-source"}, {"suspended": True}):
                with self.subTest(changes=changes), patch.object(PROOF, "call", return_value=response({**focused, **changes})):
                    with self.assertRaises(RuntimeError):
                        PROOF.wait_for_floating_keyboard_seat("socket", child, output, None)
            with patch.object(PROOF, "call", return_value=response(child)), \
                    patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 10, 20, 30]):
                with self.assertRaises(PROOF.ProofTimeout):
                    PROOF.wait_for_floating_keyboard_seat("socket", child, output, None)
        for fixture in (PROOF.native_float_return, PROOF.native_ime_float_return):
            source = inspect.getsource(fixture)
            focus = source.index('focus_agent_conversation_control(keyboard_access, keyboard_access["control"], "Return")')
            seat = source.index("wait_for_floating_keyboard_seat(", focus)
            enter = source.index('{"op": "key", "key": "Enter"}', focus)
            self.assertLess(focus, seat)
            self.assertLess(seat, enter)

    def test_return_editor_classification_distinguishes_retired_nodes_without_private_values(self):
        access = {"bus": "private-bus", "composer": "private-path"}
        with patch.object(PROOF.atspi, "atspi_address", return_value="private-address"), \
                patch.object(PROOF.atspi, "AtspiClient") as constructor:
            client = constructor.return_value
            client.property.return_value = "(<'Message composer'>,)"
            for value, expected in (("nihao", "baseline_matches"), ("nihao你好", "chinese_matches"),
                                    ("nihaonihao", "direct_latin_matches"), ("nihao ", "literal_space_matches"), ("", "empty"),
                                    ("PRIVATE-CANARY", "other")):
                client.call.return_value = repr((value,))
                observed = PROOF.closed_return_editor_observation(access, "nihao", "nihao你好", True)
                self.assertTrue(observed[expected])
                self.assertEqual(observed["character_count"], len(value))
                self.assertTrue(observed["transport_success"])
                self.assertNotIn("PRIVATE-CANARY", json.dumps(observed))
                self.assertNotIn("private-path", json.dumps(observed))
            client.call.return_value = "malformed PRIVATE-CANARY"
            self.assertTrue(PROOF.closed_return_editor_observation(
                access, "nihao", "nihao你好", True)["unparseable"])
            client.call.side_effect = PROOF.atspi.GateError(
                "org.freedesktop.DBus.Error.UnknownObject PRIVATE-CANARY private-path")
            retired = PROOF.closed_return_editor_observation(access, "nihao", "nihao你好", True)
            self.assertTrue(retired["unknown_object"])
            self.assertFalse(retired["transport_success"])
            self.assertIsNone(retired["character_count"])
            self.assertNotIn("PRIVATE-CANARY", json.dumps(retired))
        missing = PROOF.closed_return_editor_observation(None, "nihao", "nihao你好", False)
        self.assertFalse(missing["editor_observed"])
        source = inspect.getsource(PROOF.native_ime_float_return)
        receipt = source.index('"ime-returned-editor-observations.json"')
        guard = source.index('"Return committed or deactivated the live Rime preedit"')
        self.assertLess(receipt, guard)
        self.assertIn("private_editor_value_matches(returned_access, committed_before)", source[receipt:guard])

    def test_float_focus_native_diagnostics_do_not_replace_actual_control_focus(self):
        owner = {"process": 7, "bus": "PRIVATE", "agent": "Tide", "detached": False, "drag": "/old"}
        current = {**owner, "drag": "/fresh"}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=current), \
                patch.object(PROOF, "_editor_accessibility_focus", side_effect=[False, True]) as focus, \
                patch.object(PROOF, "closed_native_custody_observation", return_value={"observable": False}) as custody, \
                patch.object(PROOF.time, "sleep"):
            self.assertEqual(PROOF.wait_for_conversation_control_focus(
                owner, pathlib.Path(directory), None, detached=False, control="drag", socket="socket"), current)
            receipt = json.loads((pathlib.Path(directory) / "keyboard-float-control-focus.json").read_text())
        self.assertEqual(focus.call_count, 2)
        self.assertEqual(custody.call_count, 2)
        self.assertFalse(receipt["initial"]["control_focused"])
        self.assertTrue(receipt["last"]["control_focused"])
        self.assertEqual(receipt["last"]["native_custody"], {"observable": False})
        self.assertNotIn("PRIVATE", json.dumps(receipt))

    def test_return_space_diagnostics_keep_cached_and_fresh_subjects_separate(self):
        owner = {"agent": "Tide", "process": 7, "bus": "PRIVATE-BUS", "composer": "PRIVATE-CACHED"}
        fresh = {**owner, "composer": "PRIVATE-FRESH"}
        with patch.object(PROOF, "agent_conversation_accessibility", return_value=fresh), \
                patch.object(PROOF, "closed_return_editor_observation", side_effect=[
                    {"unknown_object": True}, {"chinese_matches": True}]) as classify:
            result = PROOF.closed_return_space_content_observation(owner, "PRIVATE-BASELINE", "PRIVATE-COMMIT")
        self.assertTrue(result["cached"]["unknown_object"])
        self.assertTrue(result["fresh"]["chinese_matches"])
        self.assertIs(classify.call_args_list[0].args[0], owner)
        self.assertIs(classify.call_args_list[1].args[0], fresh)
        self.assertNotIn("PRIVATE", json.dumps(result))
        self.assertEqual(owner["composer"], "PRIVATE-CACHED")
        source = inspect.getsource(PROOF.native_ime_float_return)
        before = source.index('retain_space_observation("before_space")')
        after = source.index('retain_space_observation("after_space")', before)
        self.assertEqual(source[before:after].count('{"op": "key", "key": "Space"}'), 1)
        self.assertIn("private_editor_value_matches(returned_access, committed_after)", source[after:])
        self.assertIn('current.get("input_source_surface_id") is not None', source[after:])

    def test_native_custody_diagnostics_bind_exact_witness_and_export_only_closed_fields(self):
        primary = {"app_id": "so.tilo.chat", "surface_id": 7, "keyboard_focused": False,
                   "presented": True, "pixels_ready": True, "capture_pending": False}
        child = {"app_id": "so.tilo.chat", "surface_id": 11, "floating_id": "PRIVATE-ID",
                 "keyboard_focused": True, "suspended": True, "transferring": True,
                 "last_error": "PRIVATE-ERROR", "input_source_surface_id": 7}
        response = {"answer": {"frames": [primary, child, {**child, "surface_id": 99}]}}
        with patch.object(PROOF, "call", return_value=response):
            observed = PROOF.closed_native_custody_observation("socket", primary, child)
            ambiguous = PROOF.closed_native_custody_observation("socket")
        self.assertTrue(observed["observable"])
        self.assertEqual(observed["child_count"], 1)
        self.assertTrue(observed["child_transferring"])
        self.assertTrue(observed["child_error_present"])
        self.assertFalse(observed["child_ordinary_route"])
        self.assertFalse(observed["primary_keyboard_focused"])
        self.assertEqual(ambiguous["child_count"], 2)
        self.assertFalse(ambiguous["child_observable"])
        self.assertNotIn("PRIVATE", json.dumps(observed))
        with patch.object(PROOF, "call", side_effect=RuntimeError("PRIVATE-ERROR")):
            self.assertEqual(PROOF.closed_native_custody_observation("socket"), {"observable": False})

    def test_pending_return_wait_accepts_only_live_exact_transfer_then_strict_proxy(self):
        primary = {"app_id": "so.tilo.chat", "surface_id": 7, "frame_seq": 30,
                   "presented": True, "last_error": None, "keyboard_focused": False}
        child = {"app_id": "so.tilo.chat", "floating_id": "private", "surface_id": 11,
                 "frame_seq": 22, "presented": False, "keyboard_focused": True,
                 "suspended": True, "transferring": True}
        response = lambda p, c: {"answer": {"frames": [p, c]}}
        pending = response(primary, child)
        valid_primary = {**primary, "frame_seq": 31, "input_route": "floating-preedit-child",
                         "input_source_surface_id": 11}
        valid = response(valid_primary, {**child, "transferring": False})
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "call", side_effect=[pending, valid]), \
                patch.object(PROOF, "closed_atomic_input_method_observation", return_value={"observable": False}), \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF, "chat_text_input_active", return_value=True), \
                patch.object(PROOF, "chat_input_method_preedit_active", return_value=True), \
                patch.object(PROOF, "same_fcitx_process", return_value=True):
            output = pathlib.Path(directory)
            observed, route = PROOF.wait_for_return_input_route(
                "socket", output, primary, child, True, object(), None)
            self.assertEqual(observed, valid_primary)
            self.assertTrue(route["child_preedit_continuation"])
            path = output / "ime-returned-adoption-observations.json"
            receipt = json.loads(path.read_text())
            self.assertEqual(receipt["observation_count"], 2)
            self.assertTrue(receipt["initial"]["pending_adoption"])
            self.assertTrue(receipt["last"]["strict_route_accepted"])
            self.assertNotIn("surface_id", json.dumps(receipt))
            self.assertTrue(all(type(value) is bool for name in ("initial", "last")
                                for value in receipt[name].values()))
            with patch.object(PROOF, "call", return_value=pending), \
                    patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 10, 20, 30]):
                with self.assertRaises(PROOF.ProofTimeout):
                    PROOF.wait_for_return_input_route(
                        "socket", output, primary, child, True, object(), None)
            receipt = json.loads(path.read_text())
            self.assertEqual(receipt["observation_count"], 3)
            self.assertTrue(receipt["last"]["pending_adoption"])
            for invalid in (
                    response({**primary, "input_route": "foreign"}, child),
                    response({**primary, "input_source_surface_id": 99}, child),
                    response(primary, {**child, "surface_id": 99}),
                    response(primary, {**child, "keyboard_focused": False}),
                    response(primary, {**child, "frame_seq": 21}),
                    response(primary, {**child, "transferring": "true"}),
                    response(primary, {**child, "input_route": "foreign"}),
                    response(primary, {**child, "transferring": False})):
                with self.subTest(invalid=invalid), patch.object(PROOF, "call", return_value=invalid):
                    with self.assertRaises(RuntimeError):
                        PROOF.wait_for_return_input_route(
                            "socket", output, primary, child, True, object(), None)
            with patch.object(PROOF, "call", return_value=pending), \
                    patch.object(PROOF, "chat_input_method_preedit_active", return_value=False):
                with self.assertRaisesRegex(RuntimeError, "lost active composition"):
                    PROOF.wait_for_return_input_route(
                        "socket", output, primary, child, True, object(), None)

    def test_native_ime_stage_uses_real_rime_and_preserves_each_handoff_boundary(self):
        source = inspect.getsource(PROOF.native_ime_float_return)
        for forbidden in ("Ime::Preedit", "TrustedInputMethod", "SetText", "AddSelection"):
            self.assertNotIn(forbidden, source)
        for required in (
                '"text": "nihao"', '"key": "Space"',
                'committed_after = "nihao你好"',
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
        detached_preedit_gate = source.index(
            "or not chat_input_method_preedit_active(socket):", detached_ready)
        detached_snapshot = source.index('"ime-preedit-detached-', detached_ready)
        space = source.index('{"op": "key", "key": "Space"}', detached_ready)
        floating_commit = source.index('"Rime\'s exact detached commit and child seat"')
        refusal_capture = source.index("preserve_detached_commit_refusal", floating_commit)
        floating_select = source.index("wait_for_private_selection(", floating_commit)
        floating_undo = source.index('"detached Rime commit undo"', floating_select)
        second_type = source.index('{"op": "type", "text": "nihao"}', floating_undo)
        second_preedit = source.index('"genuine Rime preedit in the detached Chat frame"', second_type)
        returned = source.index('"Rime preedit\'s returned primary frame"', second_preedit)
        returned_commit = source.index('"Rime\'s exact post-Return commit and primary seat"', returned)
        self.assertLess(detached_ready, floating_commit)
        self.assertLess(detached_ready, detached_preedit_gate)
        self.assertLess(detached_preedit_gate, detached_snapshot)
        self.assertLess(detached_snapshot, space)
        self.assertLess(floating_commit, refusal_capture)
        self.assertLess(refusal_capture, floating_select)
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
            production_default = (
                PROOF.REPO / "os/usr/share/rime-data/default.custom.yaml"
            ).read_bytes()
            owned_default = (
                profile / "data/fcitx5/rime/default.custom.yaml"
            ).read_bytes()
            owned_files = sorted(
                item.relative_to(profile).as_posix()
                for item in profile.rglob("*") if item.is_file()
            )
        self.assertEqual(settings["appearance"], {"theme": "high-contrast"})
        self.assertEqual(settings["input"], {
            "repeat_delay": 270,
            "keyboard_layout": "ime/rime",
            "keyboard_layouts": ["us", "ime/rime"],
        })
        self.assertEqual(owned_default, production_default)
        self.assertEqual(
            re.findall(rb"(?m)^\s*-\s+schema:\s*([^\s#]+)", owned_default),
            [b"luna_pinyin_simp"],
        )
        self.assertEqual(owned_files, [
            "config/tilo/settings.json",
            "data/fcitx5/rime/default.custom.yaml",
        ])

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

    def test_return_control_uses_current_exact_child_geometry_and_fresh_accessibility(self):
        previous = {
            "app_id": "so.tilo.chat", "floating_id": "float-a",
            "surface_id": 7, "frame_seq": 1,
        }
        current = {
            **previous, "frame_seq": 9, "presented": True, "pixels_ready": True,
            "last_error": None,
            "rect": {"x": 700.0, "y": 100.0, "w": 600.0, "h": 500.0},
            "content_rect": {"x": 720.0, "y": 110.0, "w": 560.0, "h": 470.0},
            "content_origin": {"x": 710.0, "y": 100.0},
            "keyboard_focused": True,
        }
        response = {"answer": {"frames": [current]}}
        access = {"bus": "chat", "control": "/return"}
        target = {
            "client_rect": {"x": 40, "y": 20, "w": 30, "h": 10},
            "global_center": {"x": 765.0, "y": 125.0},
        }
        with patch.object(PROOF, "call", return_value=response), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "private_editor_observation", return_value={"selection": [1, 2]}), \
                patch.object(PROOF, "component_global_target", return_value=target) as component:
            observed = PROOF.current_detached_control_target(
                "socket", previous, "Tide", "private", {"selection": [1, 2]}
            )
        self.assertEqual(observed, (current, access, target))
        component.assert_called_once_with(
            access, "/return", PROOF.presented_content_geometry(current),
            "Return conversation control",
        )

        for changed in ({"surface_id": 8}, {"floating_id": "float-b"}):
            with self.subTest(changed=changed), \
                    patch.object(PROOF, "call", return_value={
                        "answer": {"frames": [{**current, **changed}]}
                    }), \
                    patch.object(PROOF, "agent_conversation_accessibility") as accessibility:
                self.assertIsNone(PROOF.current_detached_control_target(
                    "socket", previous, "Tide", "private", {"selection": [1, 2]}
                ))
                accessibility.assert_not_called()

        moved = {
            **current,
            "frame_seq": current["frame_seq"] + 1,
            "rect": {**current["rect"], "x": current["rect"]["x"] + 24.0},
            "content_rect": {
                **current["content_rect"],
                "x": current["content_rect"]["x"] + 24.0,
            },
            "content_origin": {
                **current["content_origin"],
                "x": current["content_origin"]["x"] + 24.0,
            },
        }
        with patch.object(PROOF, "call", side_effect=[
                    response, {"answer": {"frames": [moved]}},
                ]), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value=access), \
                patch.object(PROOF, "private_editor_observation", return_value={"selection": [1, 2]}), \
                patch.object(PROOF, "component_global_target") as component:
            self.assertIsNone(PROOF.current_detached_control_target(
                "socket", previous, "Tide", "private", {"selection": [1, 2]}
            ))
            component.assert_not_called()

    def test_return_continuity_diagnostic_classifies_without_weakening_refusal(self):
        previous = {"app_id": "so.tilo.chat", "floating_id": "private-surface",
                    "surface_id": 7, "frame_seq": 1}
        current = {**previous, "frame_seq": 2, "presented": True,
                   "pixels_ready": True, "last_error": None,
                   "keyboard_focused": True, "suspended": False, "transferring": False}
        before = {"draft_matches": True, "caret": 9, "selection": [8, 9],
                  "accessibility_focused": False}
        for change in ({"accessibility_focused": True}, {"caret": 8}, {"selection": None}):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory, \
                    patch.object(PROOF, "call", return_value={"answer": {"frames": [current]}}) as reads, \
                    patch.object(PROOF, "agent_conversation_accessibility", return_value={
                        "bus": "private-bus", "control": "/private-return"}), \
                    patch.object(PROOF, "private_editor_observation", return_value={**before, **change}), \
                    patch.object(PROOF, "component_global_target") as target:
                path = pathlib.Path(directory) / "continuity.json"
                with self.assertRaisesRegex(RuntimeError, "refreshing Return changed private editor state"):
                    PROOF.current_detached_control_target(
                        "socket", previous, "Tide", "private-draft", before,
                        diagnostic_path=path)
                observed = json.loads(path.read_text())
                self.assertEqual(observed["accessibility_focus_equal"],
                                 "accessibility_focused" not in change)
                self.assertEqual(observed["caret_equal"], "caret" not in change)
                self.assertEqual(observed["selection_equal"], "selection" not in change)
                self.assertTrue(observed["content_matches"])
                self.assertTrue(observed["same_native_child"])
                self.assertFalse(observed["private_content_recorded"])
                self.assertTrue(all(type(value) is bool for value in observed.values()))
                self.assertNotIn("private", path.read_text().replace("private_content_recorded", ""))
                reads.assert_called_once_with("socket", {"op": "foreign_frames"})
                target.assert_not_called()

    def test_return_diagnostic_write_failure_preserves_original_refusal(self):
        child = {"app_id": "so.tilo.chat", "floating_id": "float", "surface_id": 7,
                 "frame_seq": 1, "presented": True, "pixels_ready": True, "last_error": None}
        with patch.object(PROOF, "call", return_value={"answer": {"frames": [child]}}), \
                patch.object(PROOF, "agent_conversation_accessibility", return_value={}), \
                patch.object(PROOF, "private_editor_observation", return_value={"caret": 2}), \
                patch.object(pathlib.Path, "write_text", side_effect=PermissionError("private-error")):
            with self.assertRaisesRegex(RuntimeError, "refreshing Return changed private editor state"):
                PROOF.current_detached_control_target(
                    "socket", child, "Tide", "private-draft", {"caret": 1},
                    diagnostic_path=pathlib.Path("unused"))

    def test_ime_return_refreshes_target_only_while_exact_real_composition_survives(self):
        primary, child, access, editor, _ = self.return_editor_continuity_subjects()
        child = {**child, "presented": True, "suspended": False, "transferring": False}
        access = {**access, "detached": True}
        target = {"global_center": {"x": 60, "y": 200}}
        with patch.object(PROOF, "current_detached_control_target",
                          return_value=(child, access, target)) as fresh, \
                patch.object(PROOF, "chat_text_input_active", return_value=True), \
                patch.object(PROOF, "chat_input_method_preedit_active", return_value=True), \
                patch.object(PROOF, "same_fcitx_process", return_value=True):
            self.assertEqual(PROOF.current_ime_return_target(
                "socket", child, access, "draft-secret", editor, {}), (child, access, target))
            fresh.assert_called_once_with("socket", child, "Tide", "draft-secret", editor)
        for changed in ({"process": 9976}, {"detached": False}):
            with self.subTest(changed=changed), \
                    patch.object(PROOF, "current_detached_control_target",
                                 return_value=(child, {**access, **changed}, target)):
                with self.assertRaises(RuntimeError):
                    PROOF.current_ime_return_target("socket", child, access, "draft-secret", editor, {})
        for lost in ("chat_text_input_active", "chat_input_method_preedit_active", "same_fcitx_process"):
            with self.subTest(lost=lost), \
                    patch.object(PROOF, "current_detached_control_target", return_value=(child, access, target)), \
                    patch.object(PROOF, "chat_text_input_active", return_value=True), \
                    patch.object(PROOF, "chat_input_method_preedit_active", return_value=True), \
                    patch.object(PROOF, "same_fcitx_process", return_value=True), \
                    patch.object(PROOF, lost, return_value=False):
                with self.assertRaises(RuntimeError):
                    PROOF.current_ime_return_target("socket", child, access, "draft-secret", editor, {})

    def test_ime_return_withdrawal_records_only_closed_same_native_custody(self):
        primary, child, _, _, _ = self.return_editor_continuity_subjects()
        pending = {**child, "presented": True, "suspended": False}
        retired = {**child, "presented": False, "suspended": True}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "call", side_effect=[
                    {"answer": {"frames": [primary, pending]}},
                    {"answer": {"frames": [primary, retired]}}]), \
                patch.object(PROOF, "chat_text_input_active", return_value=True), \
                patch.object(PROOF, "chat_input_method_preedit_active", return_value=True), \
                patch.object(PROOF, "same_fcitx_process", return_value=True), \
                patch.object(PROOF.time, "sleep"):
            output = pathlib.Path(directory)
            self.assertTrue(PROOF.wait_for_ime_return_withdrawal(
                "socket", output, child, primary, {}, None))
            payload = (output / "ime-return-withdrawal-observations.json").read_text()
            receipt = json.loads(payload)
            self.assertEqual(receipt["observation_count"], 2)
            self.assertFalse(receipt["initial"]["withdrawn"])
            self.assertTrue(receipt["last"]["withdrawn"])
            self.assertTrue(all(type(v) is bool for row in
                                (receipt["initial"], receipt["last"]) for v in row.values()))
            for forbidden in ("9973", "9974", "private-native-child", "draft-secret"):
                self.assertNotIn(forbidden, payload)
        for rows in ([{**child, "surface_id": 9976}], [child, child],
                     [{**child, "presented": None}]):
            with self.subTest(rows=rows), self.assertRaises(RuntimeError):
                PROOF.ime_return_withdrawal_observation({"answer": {"frames": rows}}, child, primary)

    def test_ime_return_withdrawal_does_not_replay_click_or_extend_timeout(self):
        primary, child, _, _, _ = self.return_editor_continuity_subjects()
        pending = {**child, "presented": True}
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PROOF, "call", return_value={"answer": {"frames": [primary, pending]}}), \
                patch.object(PROOF, "chat_text_input_active", return_value=True), \
                patch.object(PROOF, "chat_input_method_preedit_active", return_value=True), \
                patch.object(PROOF, "same_fcitx_process", return_value=True), \
                patch.object(PROOF.time, "sleep"), \
                patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 31]):
            with self.assertRaises(PROOF.ProofTimeout):
                PROOF.wait_for_ime_return_withdrawal(
                    "socket", pathlib.Path(directory), child, primary, {}, None)
        source = inspect.getsource(PROOF.wait_for_ime_return_withdrawal)
        self.assertNotIn("click_at", source)
        self.assertNotIn("timeout=", source)
        fixture = inspect.getsource(PROOF.native_ime_float_return)
        self.assertIn("current_ime_return_target(", fixture)
        self.assertIn('"ime-return-target.json"', fixture)

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

    def test_floating_numeric_geometry_exports_only_closed_finite_measurements(self):
        rect = {"x": -10, "y": 20, "w": 512, "h": 360}
        extent = {"w": 512, "h": 360}
        geometry = {
            "raw_buffer": extent, "content_crop": rect,
            "configured_extent": extent, "min_size": {"w": 0, "h": 0},
            "max_size": {"w": 0, "h": 0},
            "window_geometry": {**rect, "viewport_destination": extent, "buffer_scale": 1},
        }
        expected = {"rect": rect, "content_rect": rect, "target_rect": rect,
                    "frame_seq": 17, "device_scale": 1.25, "frame_geometry": geometry}
        untrusted = json.loads(json.dumps(expected))
        untrusted.update({"surface_id": 987, "app_id": "private", "text": "private"})
        untrusted["rect"]["opaque_id"] = 123
        untrusted["frame_geometry"]["window_geometry"]["private_text"] = "private"
        observed = PROOF.floating_numeric_geometry(untrusted)
        self.assertEqual(observed, expected)
        self.assertNotIn("private", json.dumps(observed, allow_nan=False))
        self.assertNotIn("opaque_id", json.dumps(observed))
        for nullable in (
                {**expected, "frame_geometry": None},
                {**expected, "frame_geometry": {**geometry, "configured_extent": None,
                                                "window_geometry": None}},
                {**expected, "frame_geometry": {**geometry, "window_geometry": {
                    **geometry["window_geometry"], "viewport_destination": None}}}):
            self.assertEqual(PROOF.floating_numeric_geometry(nullable), nullable)
        invalid = [
            {**expected, "frame_seq": True}, {**expected, "device_scale": 0},
            {**expected, "device_scale": float("inf")},
            {**expected, "target_rect": {**rect, "w": -1}},
            {**expected, "rect": {**rect, "x": True}},
            {**expected, "frame_geometry": {**geometry, "raw_buffer": {"w": float("nan"), "h": 4}}},
            {**expected, "frame_geometry": {**geometry, "window_geometry": {
                **geometry["window_geometry"], "buffer_scale": False}}},
            {key: value for key, value in expected.items() if key != "frame_geometry"},
        ]
        for value in invalid:
            with self.subTest(value=value), self.assertRaises(RuntimeError):
                PROOF.floating_numeric_geometry(value)

    def test_detached_geometry_receipts_bracket_real_space_and_precede_selection(self):
        source = inspect.getsource(PROOF.native_ime_float_return)
        before = source.index('"ime-detached-before-commit-geometry.json"')
        space = source.index('{"op": "key", "key": "Space"}', before)
        fresh = source.index('current["frame_seq"] <= frame["frame_seq"]', space)
        committed = source.index('"Rime\'s exact detached commit and child seat"', fresh)
        after = source.index('"ime-detached-after-commit-geometry.json"', committed)
        selection = source.index("with hold_physical_modifiers(socket, shift=True)", after)
        self.assertLess(before, space)
        self.assertLess(space, fresh)
        self.assertLess(fresh, committed)
        self.assertLess(committed, after)
        self.assertLess(after, selection)
        self.assertEqual(source.count("json.dumps(floating_numeric_geometry(frame),"), 2)
        self.assertIn("stable_floating_commit_geometry(before_commit_frame, current)", source)

    def test_real_ime_geometry_refuses_observed_transfer_height_rebound(self):
        rect = {"x": 100, "y": 100, "w": 512, "h": 360}
        extent = {"w": 512, "h": 360}
        before = {"rect": rect, "content_rect": rect, "target_rect": rect,
                  "frame_seq": 10, "device_scale": 1,
                  "frame_geometry": {
                      "raw_buffer": extent, "content_crop": {**rect, "x": 0, "y": 0},
                      "configured_extent": extent, "min_size": extent, "max_size": extent,
                      "window_geometry": None}}
        after = json.loads(json.dumps(before))
        after["frame_seq"] = 11
        self.assertTrue(PROOF.stable_floating_commit_geometry(before, after))
        rebound = json.loads(json.dumps(after))
        rebound["frame_geometry"]["raw_buffer"]["h"] = 772
        self.assertFalse(PROOF.stable_floating_commit_geometry(before, rebound))
        stale_before = json.loads(json.dumps(before))
        for name in ("configured_extent", "min_size", "max_size"):
            stale_before["frame_geometry"][name]["h"] = 772
        self.assertFalse(PROOF.stable_floating_commit_geometry(stale_before, after))
        for name in ("raw_buffer", "content_crop", "configured_extent", "min_size", "max_size"):
            wrong = json.loads(json.dumps(after))
            wrong["frame_geometry"][name]["w"] += 2
            with self.subTest(name=name):
                self.assertFalse(PROOF.stable_floating_commit_geometry(before, wrong))
        for name in ("rect", "content_rect", "target_rect"):
            wrong = json.loads(json.dumps(after))
            wrong[name]["h"] += 1
            with self.subTest(name=name):
                self.assertFalse(PROOF.stable_floating_commit_geometry(before, wrong))
        wrong = {**after, "device_scale": 2}
        self.assertFalse(PROOF.stable_floating_commit_geometry(before, wrong))
        scaled_before = json.loads(json.dumps(before))
        scaled_before["device_scale"] = 1.25
        for name in ("raw_buffer", "content_crop"):
            scaled_before["frame_geometry"][name].update({"w": 640, "h": 450})
        scaled_after = json.loads(json.dumps(scaled_before))
        scaled_after["frame_seq"] = 12
        self.assertTrue(PROOF.stable_floating_commit_geometry(scaled_before, scaled_after))

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

    def test_chat_frontier_resamples_real_input_until_the_exact_held_sequence(self):
        for sequences, expected in (([1, 3], True), ([1, 1, 1], False)):
            with self.subTest(persistent_old=not expected), \
                    tempfile.TemporaryDirectory() as directory, ExitStack() as stack:
                output = pathlib.Path(directory)
                observer = ('[app so.tilo.chat stderr] tilo-physical-input-observer '
                            '{"app_id":"so.tilo.chat","enabled":true}\n')
                log = output / "shell.log"
                log.write_text(observer)
                remaining = iter(sequences)

                def control(_socket, request):
                    if request == {"op": "key", "key": "ArrowLeft"}:
                        sequence = next(remaining)
                        row = {"after": {"agent_task": {
                            "task_id": "task", "run_epoch": 7,
                            "execution": {"run_epoch": 7, "phase": "planning",
                                          "sequence": sequence}}}}
                        log.write_text(observer + 'tilo-physical-input '
                                       + json.dumps(row) + '\n')
                    return {}

                access = {"window": "/window", "bus": "owned"}
                replacements = {
                    "presented_primary_frame": {"frame_seq": 9},
                    "presented_content_geometry": {},
                    "agent_conversation_accessibility": access,
                    "editor_accessibility_focus": True,
                    "task_phase_accessibility": True,
                }
                for name, value in replacements.items():
                    stack.enter_context(patch.object(PROOF, name, return_value=value))
                clicked = stack.enter_context(patch.object(PROOF, "physically_click_editor"))
                stack.enter_context(patch.object(PROOF, "require_chat_keyboard_focus_after_click"))
                stack.enter_context(patch.object(PROOF, "require_ok"))
                called = stack.enter_context(patch.object(PROOF, "call", side_effect=control))
                stack.enter_context(patch.object(
                    PROOF.time, "monotonic", side_effect=[0, 0, 10, 20, 30]))
                stack.enter_context(patch.object(PROOF.time, "sleep"))
                if expected:
                    frontier = PROOF.observe_chat_task_frontier(
                        "socket", output, "task", 7, "planning", None,
                        expected_sequence=3)
                    self.assertEqual(frontier["sequence"], 3)
                else:
                    with self.assertRaisesRegex(PROOF.ProofTimeout, "current-task frontier"):
                        PROOF.observe_chat_task_frontier(
                            "socket", output, "task", 7, "planning", None,
                            expected_sequence=3)
                self.assertEqual(clicked.call_count, len(sequences))
                samples = [entry for entry in called.call_args_list
                           if entry == call("socket", {"op": "key", "key": "ArrowLeft"})]
                self.assertEqual(len(samples), len(sequences))

    def test_primed_second_frontier_checks_real_sequence_without_dismissing_navigation(self):
        class Client(PROOF.atspi.AtspiClient):
            def __init__(self):
                self.observed = []

            def property(self, path, name):
                self.observed.append((path, name))
                return "(<'Planning: Agent marker proof'>,)"

        for sequences, success in (([1, 3], True), ([1, 1, 1], False)):
            with self.subTest(success=success), tempfile.TemporaryDirectory() as directory, \
                    ExitStack() as stack:
                output = pathlib.Path(directory)
                remaining = iter(sequences)
                client = Client()
                prepared = {"access": {"bus": "owned"}, "address": "owned",
                            "phase_control": "/phase", "phase_label": "Planning: Agent marker proof"}

                def control(_socket, request):
                    if request == {"op": "key", "key": "ArrowLeft"}:
                        row = {"after": {"agent_task": {
                            "task_id": "task", "run_epoch": 7,
                            "execution": {"run_epoch": 7, "phase": "planning",
                                          "sequence": next(remaining)}}}}
                        (output / "shell.log").write_text(
                            'tilo-physical-input ' + json.dumps(row) + '\n')
                    return {}

                stack.enter_context(patch.object(PROOF, "call", side_effect=control))
                stack.enter_context(patch.object(PROOF, "presented_primary_frame", return_value={}))
                stack.enter_context(patch.object(PROOF.atspi, "AtspiClient", return_value=client))
                stack.enter_context(patch.object(PROOF, "accessible_subtree",
                                                side_effect=AssertionError("full tree during pulse")))
                stack.enter_context(patch.object(PROOF, "physically_click_editor",
                                                side_effect=AssertionError("dismiss navigation")))
                focused = stack.enter_context(patch.object(PROOF, "require_chat_keyboard_focus_after_click"))
                stack.enter_context(patch.object(PROOF, "require_ok"))
                stack.enter_context(patch.object(PROOF.time, "monotonic", side_effect=[0, 0, 10, 20, 30]))
                stack.enter_context(patch.object(PROOF.time, "sleep"))
                if success:
                    result = PROOF.sample_primed_chat_frontier(
                        "socket", output, "task", {"run_epoch": 7, "sequence": 3}, prepared, None)
                    self.assertEqual(result["sequence"], 3)
                else:
                    with self.assertRaisesRegex(PROOF.ProofTimeout, "second held marker frontier"):
                        PROOF.sample_primed_chat_frontier(
                            "socket", output, "task", {"run_epoch": 7, "sequence": 3}, prepared, None)
                self.assertEqual(focused.call_count, len(sequences))
                self.assertEqual(client.observed, [("/phase", "Name")] * len(sequences))

    def test_task_phase_retries_only_stale_objects_within_the_original_deadline(self):
        class Client(PROOF.atspi.AtspiClient):
            def __init__(self, stale, labels):
                self.stale = stale
                self.labels = list(labels)
                self.walks = 0
                self.status_reads = 0

            def property(self, path, _name):
                if path == "/status":
                    self.status_reads += 1
                    return f"'{self.labels.pop(0)}'"
                return "'window'"

            def interfaces(self, _path):
                return "()"

            def call(self, path, interface):
                self.assert_interface = interface
                if path == "/window":
                    self.walks += 1
                    if self.stale:
                        self.stale -= 1
                        raise PROOF.atspi.GateError(
                            "gdbus call --address owned --dest failed: Error: "
                            "GDBus.Error:org.freedesktop.DBus.Error.UnknownObject: retired node")
                    return "([(':1.1', '/status')],)"
                return "([],)"

        access = {"bus": "owned", "window": "/window"}
        for stale, labels, expected in (
                (1, ["Planning another task", "Planning: Lifecycle task"], True),
                (10, [], False)):
            client = Client(stale, labels)
            with self.subTest(persistent=not expected), \
                    patch.object(PROOF.atspi, "atspi_address", return_value="owned"), \
                    patch.object(PROOF.atspi, "AtspiClient", return_value=client), \
                    patch.object(PROOF.time, "monotonic", side_effect=[0, 0, .1, .2, .3]), \
                    patch.object(PROOF.time, "sleep"):
                predicate = lambda: PROOF.task_phase_accessibility(
                    access, "Planning: Lifecycle task")
                if expected:
                    self.assertTrue(PROOF.wait_for("current task phase", predicate, timeout=.25))
                    self.assertEqual(client.status_reads, 2)
                else:
                    with self.assertRaisesRegex(PROOF.ProofTimeout, "current task phase"):
                        PROOF.wait_for("current task phase", predicate, timeout=.25)
                    self.assertEqual(client.status_reads, 0)
                self.assertEqual(client.walks, 3)
                self.assertEqual(client.assert_interface, "org.a11y.atspi.Accessible.GetChildren")

        for detail in (
                "the bounded application accessibility subtree is incomplete",
                "gdbus failed: Error: GDBus.Error:org.freedesktop.DBus.Error.UnknownMethod: absent",
                "gdbus failed: Error: GDBus.Error:org.freedesktop.DBus.Error.AccessDenied: refused"):
            with self.subTest(detail=detail), \
                    patch.object(PROOF.atspi, "atspi_address", return_value="owned"), \
                    patch.object(PROOF.atspi, "AtspiClient"), \
                    patch.object(PROOF, "accessible_subtree", side_effect=PROOF.atspi.GateError(detail)):
                with self.assertRaises(PROOF.atspi.GateError):
                    PROOF.task_phase_accessibility(access, "Planning: Lifecycle task")

    def test_control_enabled_reads_the_actual_atspi_state(self):
        class Client:
            def __init__(self, reply):
                self.reply = reply

            def call(self, path, interface):
                self.observed = (path, interface)
                return self.reply

        enabled = Client(f"([uint32 {PROOF.AT_SPI_ENABLED_STATE_BIT}, uint32 0],)")
        disabled = Client("([uint32 0, uint32 0],)")
        malformed = Client("PRIVATE")
        access = {"bus": "bus"}
        with patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF.atspi, "AtspiClient", return_value=enabled):
            self.assertTrue(PROOF.accessibility_control_enabled(access, "/steer"))
        self.assertEqual(enabled.observed, (
            "/steer", "org.a11y.atspi.Accessible.GetState"
        ))
        with patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF.atspi, "AtspiClient", return_value=disabled):
            self.assertFalse(PROOF.accessibility_control_enabled(access, "/steer"))
        with patch.object(PROOF.atspi, "atspi_address", return_value="address"), \
                patch.object(PROOF.atspi, "AtspiClient", return_value=malformed), \
                self.assertRaises(RuntimeError):
            PROOF.accessibility_control_enabled(access, "/steer")


class MarkerPrecedingInvokeResultTests(unittest.TestCase):
    @staticmethod
    def request(result):
        return {"messages": [
            {"role": "assistant", "tool_calls": [{
                "id": "proof-plan", "function": {"name": "invoke"},
            }]},
            {"role": "tool", "tool_call_id": "proof-plan",
             "content": json.dumps(result)},
        ]}

    def test_result_projection_never_retains_private_fields(self):
        for result, expected in [("succeeded", "succeeded"), ("failed", "failed"),
                                 ("not run", "not_run")]:
            row = PROOF.marker_preceding_invoke_result(self.request({
                "result": result, "error": "PRIVATE ERROR", "why": "PRIVATE WHY",
                "value": "PRIVATE VALUE", "resources": ["PRIVATE RESOURCE"],
                "changed_view": "PRIVATE VIEW", "newly_available": ["PRIVATE ACTION"],
            }))
            self.assertEqual(row, {"result": expected, "error_class": "unclassified",
                                   "private_content_recorded": False})
            self.assertNotIn("PRIVATE", json.dumps(row))

    def test_rpc_error_is_not_a_definite_refusal(self):
        for error in ["capnp error: PRIVATE ONE", "capnp error: PRIVATE TWO"]:
            row = PROOF.marker_preceding_invoke_result(self.request({
                "result": "failed", "error": error,
            }))
            self.assertEqual(row["result"], "failed")
            self.assertEqual(row["error_class"], "rpc_error")
            self.assertNotIn("PRIVATE", json.dumps(row))
            self.assertNotIn("refused", json.dumps(row))

    def test_exact_plain_marker_refusals_project_only_fixed_classes(self):
        reasons = {
            "private_view: A collaborator cannot move this participant's private focus or "
                "viewport; use Follow or Reveal explicitly": "private_view",
            "agent is not admitted to this workspace": "agent_membership_lost",
        }
        for reason, expected in reasons.items():
            with self.subTest(reason=reason):
                row = PROOF.marker_preceding_invoke_result(self.request({
                    "result": "failed", "error": reason,
                }))
                self.assertEqual(row, {
                    "result": "failed",
                    "error_class": expected,
                    "private_content_recorded": False,
                })
            for changed in (reason + " PRIVATE", reason.upper(), " " + reason):
                with self.subTest(changed=changed):
                    row = PROOF.marker_preceding_invoke_result(self.request({
                        "result": "failed", "error": changed,
                    }))
                    self.assertEqual(row["error_class"], "unclassified")
                    self.assertNotIn("PRIVATE", json.dumps(row))

    def test_exact_static_rpc_reasons_project_only_fixed_classes(self):
        prefix = "capnp error: Failed: remote exception: "
        reasons = {
            "the application no longer publishes that action": "target_unpublished",
            "the application has no invocation stream": "invocation_stream_missing",
            "caller no longer authorized for invocation": "permission_revoked",
            "only an agent may invoke a task action": "agent_admission_lost",
            "the task run lease is unknown, expired, or belongs to another agent":
                "task_run_stale",
            "canonical task run is stale": "task_run_stale",
            "new task direction must be applied before another action": "direction_pending",
            "task cancellation was requested before another action": "task_cancelled",
            "the task was cancelled; no further action may start": "task_cancelled",
            "task action confirmation evidence is not accepted; request human confirmation outside the run":
                "confirmation_unsupported",
            "durable task steps are not served by this host": "step_unsupported",
            "accepted task action has no broker state": "invocation_state_missing",
            "remote actions are unavailable: the application no longer publishes that action":
                "target_unpublished",
            "remote actions are unavailable: the application has no invocation stream":
                "invocation_stream_missing",
            "remote actions are unavailable: caller no longer authorized for invocation":
                "permission_revoked",
            "the invocation id was already used for a different request":
                "invocation_conflict",
            "no such invocation": "invocation_unknown",
            "remote actions are unavailable: the invocation is still being admitted; retry":
                "admission_in_flight",
        }
        for reason, expected in reasons.items():
            with self.subTest(reason=reason):
                row = PROOF.marker_preceding_invoke_result(self.request({
                    "result": "failed", "error": prefix + reason,
                }))
                self.assertEqual(row, {
                    "result": "failed", "error_class": expected,
                    "private_content_recorded": False,
                })
                local = PROOF.marker_preceding_invoke_result(self.request({
                    "result": "failed", "error": "capnp error: Failed: " + reason,
                }))
                self.assertEqual(local, row)

        for changed in (
                prefix + "the application has no invocation stream PRIVATE",
                prefix
                    + "remote actions are unavailable: the application has no invocation stream PRIVATE",
                prefix + "remote actions are unavailable: no such invocation",
                prefix + "no such invocation PRIVATE",
                prefix.lower() + "the application has no invocation stream",
                "capnp error: the application has no invocation stream"):
            with self.subTest(changed=changed):
                row = PROOF.marker_preceding_invoke_result(self.request({
                    "result": "failed", "error": changed,
                }))
                self.assertEqual(row["error_class"], "rpc_error")
                self.assertNotIn("PRIVATE", json.dumps(row))

    def test_exact_observe_unknown_is_distinct_from_rpc_and_never_a_refusal(self):
        reason = "the action host no longer knows that invocation"
        row = PROOF.marker_preceding_invoke_result(self.request({
            "result": "failed", "error": reason,
        }))
        self.assertEqual(row, {
            "result": "failed", "error_class": "invocation_unknown",
            "private_content_recorded": False,
        })
        for changed in (reason + " PRIVATE", "PRIVATE " + reason,
                        reason.upper(), "capnp error: Failed: " + reason):
            row = PROOF.marker_preceding_invoke_result(self.request({
                "result": "failed", "error": changed,
            }))
            self.assertIn(row["error_class"], ("unclassified", "rpc_error"))
            self.assertNotIn("PRIVATE", json.dumps(row))
            self.assertNotIn("refused", json.dumps(row))

    def test_only_exact_source_timeouts_mean_uncertain(self):
        for error in ["the broker did not answer the task action request",
                      "Task action did not reach a terminal state before its deadline",
                      "Task step did not settle before its deadline; its outcome is still unknown"]:
            row = PROOF.marker_preceding_invoke_result(self.request({
                "result": "failed", "error": error,
            }))
            self.assertEqual(row["error_class"], "uncertain")
            changed = PROOF.marker_preceding_invoke_result(self.request({
                "result": "failed", "error": error + " PRIVATE",
            }))
            self.assertEqual(changed["error_class"], "unclassified")

    def test_missing_malformed_ambiguous_and_foreign_results(self):
        self.assertEqual(PROOF.marker_preceding_invoke_result({"messages": []})["result"],
                         "missing")
        baseline = self.request({"result": "succeeded"})
        malformed = json.loads(json.dumps(baseline))
        malformed["messages"][1]["content"] = "PRIVATE NON JSON"
        duplicate = json.loads(json.dumps(baseline))
        duplicate["messages"].append(duplicate["messages"][1])
        wrong_call = json.loads(json.dumps(baseline))
        wrong_call["messages"][0]["tool_calls"][0]["id"] = "other-call"
        wrong_tool = json.loads(json.dumps(baseline))
        wrong_tool["messages"][0]["tool_calls"][0]["function"]["name"] = "plan"
        for request in [malformed, duplicate, wrong_call, wrong_tool,
                        {"messages": [None] * 257}, self.request({"result": ["PRIVATE"]})]:
            with self.subTest(request_kind=type(request).__name__):
                row = PROOF.marker_preceding_invoke_result(request)
                self.assertEqual(row["result"], "invalid")
                self.assertNotIn("PRIVATE", json.dumps(row))

    def test_live_marker_is_activated_before_costly_frontier_and_diagnostics(self):
        source = inspect.getsource(PROOF.managed_agent_marker_stage)
        first = source.index('expected_sequence=admission["sequence"]')
        release = source.index('provider.release_through(1)')
        prime = source.index('prime_chat_frontier_access(')
        second = source.index('sample_primed_chat_frontier(')
        marker = source.index('"actual compositor Agent marker at the renamed category"')
        activate = source.index('activate_shell_accessibility_control(marker, marker["label"])')
        reopen = source.index('reopen_marker_composer_while_live(')
        held = source.index('provider.marker_second_held')
        result = source.index('marker-preceding-invoke-result.json')
        snapshot = source.index('agent-marker-second-held-')
        self.assertLess(first, release)
        self.assertLess(prime, release)
        self.assertLess(release, held)
        self.assertLess(held, second)
        self.assertLess(second, marker)
        self.assertLess(release, marker)
        self.assertLess(marker, activate)
        self.assertLess(activate, reopen)
        self.assertLess(reopen, result)
        self.assertLess(held, result)
        self.assertLess(result, snapshot)


class OwnedRimeSchemaTests(unittest.TestCase):
    def test_missing_user_config_does_not_hide_fixed_deployment(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = pathlib.Path(directory)
            root = profile / "data/fcitx5/rime"
            (root / "build").mkdir(parents=True)
            (root / "build/luna_pinyin_simp.schema.yaml").write_text(
                '__build_info:\n  timestamps: {}\nkey_binder:\n  bindings:\n'
                '    - {accept: Control+Shift+at, toggle: ascii_mode, when: always}\n')
            env = {"XDG_DATA_HOME": str(profile / "data"),
                   "XDG_CONFIG_HOME": str(profile / "config")}
            for user in (None, "", "var:\n  previously_selected_schema: private-schema\n"):
                if user is not None:
                    (root / "user.yaml").write_text(user)
                observed = PROOF.owned_rime_schema_observation(env)
                self.assertTrue(observed["rime_data_directory_present"])
                self.assertTrue(observed["rime_deployed_directory_present"])
                self.assertTrue(observed["rime_schema_exists"])
                self.assertTrue(observed["rime_schema_deployed"])
                self.assertTrue(observed["rime_schema_default_keybinder"])
                self.assertFalse(observed["rime_selected_schema_is_luna_pinyin_simp"])
                self.assertEqual(observed["rime_selected_schema_observed"], user not in (None, ""))
                self.assertTrue(all(type(value) is bool for value in observed.values()))
                self.assertNotIn("private-schema", json.dumps(observed))

    def test_deployed_known_schema_exports_only_closed_booleans(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = pathlib.Path(directory)
            root = profile / "data/fcitx5/rime"
            (root / "build").mkdir(parents=True)
            (root / "user.yaml").write_text(
                'var:\n  previously_selected_schema: "luna_pinyin_simp"\nprivate: never-export\n')
            (root / "default.custom.yaml").write_bytes(PROOF.production_rime_default())
            (root / "build/luna_pinyin_simp.schema.yaml").write_text(
                '__build_info:\n  timestamps: {}\nkey_binder:\n  bindings:\n'
                '    - {accept: Control+Shift+2, toggle: ascii_mode, when: always}\n')
            observed = PROOF.owned_rime_schema_observation({
                "XDG_DATA_HOME": str(profile / "data"),
                "XDG_CONFIG_HOME": str(profile / "config"),
            })
            self.assertTrue(observed["rime_selected_schema_observed"])
            self.assertTrue(observed["rime_selected_schema_is_luna_pinyin_simp"])
            self.assertTrue(observed["rime_default_custom_matches_production"])
            self.assertTrue(observed["rime_schema_deployed"])
            self.assertTrue(observed["rime_schema_default_keybinder"])
            self.assertTrue(all(type(value) is bool for value in observed.values()))
            self.assertNotIn("never-export", json.dumps(observed))

    def test_missing_empty_unknown_duplicate_and_oversized_configs_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = pathlib.Path(directory)
            root = profile / "data/fcitx5/rime"
            root.mkdir(parents=True)
            env = {"XDG_DATA_HOME": str(profile / "data"),
                   "XDG_CONFIG_HOME": str(profile / "config")}
            self.assertFalse(PROOF.owned_rime_schema_observation(env)["rime_user_config_present"])
            for text, present, empty, selected in (
                    ("", True, True, False),
                    ("var:\n  previously_selected_schema: ../../private\n", True, False, True),
                    ("var:\n  previously_selected_schema: luna_pinyin\n"
                     "  previously_selected_schema: luna_pinyin\n", True, False, False),
                    ("x" * 65537, False, False, False)):
                (root / "user.yaml").write_text(text)
                observed = PROOF.owned_rime_schema_observation(env)
                self.assertEqual(observed["rime_user_config_present"], present)
                self.assertEqual(observed["rime_user_config_empty"], empty)
                self.assertEqual(observed["rime_selected_schema_observed"], selected)
                self.assertFalse(observed["rime_schema_exists"])

    def test_wrong_profile_and_symlink_escape_are_not_observed(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = pathlib.Path(directory)
            root = profile / "data/fcitx5/rime"
            root.mkdir(parents=True)
            outside = profile / "outside.yaml"
            outside.write_text("var:\n  previously_selected_schema: luna_pinyin\n")
            (root / "user.yaml").symlink_to(outside)
            env = {"XDG_DATA_HOME": str(profile / "data"),
                   "XDG_CONFIG_HOME": str(profile / "config")}
            observed = PROOF.owned_rime_schema_observation(env)
            self.assertFalse(observed["rime_user_config_present"])
            self.assertFalse(observed["rime_selected_schema_observed"])
            self.assertFalse(observed["rime_schema_exists"])
            self.assertFalse(any(PROOF.owned_rime_schema_observation({}).values()))


if __name__ == "__main__":
    unittest.main()
