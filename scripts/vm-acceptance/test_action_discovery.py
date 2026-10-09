"""Actual driver parsing must enforce the canonical discovery envelope."""
import copy
import unittest
from unittest.mock import patch
from acceptance.action_discovery import discover

APP = "so.tilo.notes"
ROOM = "5ba25f73-87c5-447f-a5a6-880dc4249a81"
RECORD = "derived:default@tab:f276000000000044"
FAMILY = "notes.notebook"
BOUND = (APP, ROOM, RECORD)


def reply():
    return dict(query="", completion=dict(complete=True, incomplete=[]), scope=dict(application=APP, room=ROOM, record=RECORD,
                semantic_family=FAMILY), count=1, actions=[dict(provider="app-actions",
                application=APP, room=ROOM, record=RECORD, operation="read",
                semantic_identity=dict(kind=FAMILY, key=FAMILY))])


class DiscoverySafety(unittest.TestCase):
    def test_pending_polls_identical_scope_and_returns_only_complete_rows(self):
        partial = reply()
        partial["actions"][0]["operation"] = "partial-only"
        partial["completion"] = dict(complete=False,
                                     incomplete=[dict(provider="app-actions", reason="pending")])
        responses = iter([partial, reply()])
        calls = []
        def control(arguments):
            calls.append(arguments)
            return next(responses)
        with patch("acceptance.action_discovery.time.sleep") as sleep:
            self.assertEqual(discover(control, APP, BOUND, FAMILY), reply()["actions"])
        self.assertEqual(calls[0], calls[1])
        sleep.assert_called_once_with(0.05)

    def test_persistent_pending_is_bounded_and_never_accepted(self):
        value = reply()
        value["completion"] = dict(complete=False,
                                   incomplete=[dict(provider="app-actions", reason="pending")])
        calls = []
        with patch("acceptance.action_discovery.time.sleep") as sleep:
            with self.assertRaisesRegex(RuntimeError, "timed out after 8"):
                discover(lambda args: calls.append(args) or value, APP, BOUND, FAMILY)
        self.assertEqual(len(calls), 8)
        self.assertTrue(all(args == calls[0] for args in calls))
        self.assertEqual(sleep.call_count, 7)

    def test_later_partial_response_is_revalidated_before_any_further_poll(self):
        first = reply()
        first["completion"] = dict(complete=False,
                                   incomplete=[dict(provider="app-actions", reason="pending")])
        foreign = copy.deepcopy(first)
        foreign["actions"][0]["record"] = "foreign"
        responses = iter([first, foreign, reply()])
        calls = []
        def control(arguments):
            calls.append(arguments)
            return next(responses)
        with patch("acceptance.action_discovery.time.sleep"):
            with self.assertRaisesRegex(RuntimeError, "foreign row"):
                discover(control, APP, BOUND, FAMILY)
        self.assertEqual(len(calls), 2)

    def test_stale_refused_timeout_and_unknown_failure_are_not_retried(self):
        for reason in ("stale_cursor", "refused", "timeout", "unknown"):
            value = reply()
            value["completion"] = dict(complete=False,
                                       incomplete=[dict(provider="app-actions", reason=reason)])
            calls = []
            with self.subTest(reason=reason), self.assertRaisesRegex(RuntimeError, reason):
                discover(lambda args: calls.append(args) or value, APP, BOUND, FAMILY)
            self.assertEqual(len(calls), 1)

    def test_partial_scope_count_foreign_metadata_and_size_fail_before_poll(self):
        for fault in ("scope", "count", "foreign", "size"):
            value = reply()
            value["completion"] = dict(complete=False,
                                       incomplete=[dict(provider="app-actions", reason="pending")])
            if fault == "scope": value["scope"]["record"] = "foreign"
            elif fault == "count": value["count"] = 0
            elif fault == "foreign": value["actions"][0]["room"] = "foreign"
            else: value["actions"][0]["title"] = "x" * (900 * 1024)
            calls = []
            with self.subTest(fault=fault), self.assertRaises(RuntimeError):
                discover(lambda args: calls.append(args) or value, APP, BOUND, FAMILY)
            self.assertEqual(len(calls), 1)

    def test_missing_malformed_and_contradictory_completion_fail(self):
        for completion in (None, {}, dict(complete=1, incomplete=[]),
                           dict(complete=True, incomplete=[dict(provider="app-actions", reason="pending")]),
                           dict(complete=False, incomplete=[]),
                           dict(complete=False, incomplete=[{}]),
                           dict(complete=False, incomplete="pending")):
            value = reply()
            value["completion"] = completion
            calls = []
            with self.subTest(completion=completion), self.assertRaises(RuntimeError):
                discover(lambda args: calls.append(args) or value, APP, BOUND, FAMILY)
            self.assertEqual(len(calls), 1)

    def test_exact_opaque_record_scope_has_no_text_query_and_includes_programmatic(self):
        calls = []
        def control(arguments):
            calls.append(arguments)
            return reply()
        self.assertEqual(discover(control, APP, BOUND, FAMILY), reply()["actions"])
        self.assertEqual(calls[0][:4], ["actions", "list", "--programmatic", "--json"])
        for flag, value in (("--application", APP), ("--room", ROOM),
                            ("--record", RECORD), ("--semantic-family", FAMILY)):
            self.assertEqual(calls[0][calls[0].index(flag)+1], value)

    def test_native_appended_unbound_diagnostics_are_not_app_candidates(self):
        value = reply()
        value["actions"] += [dict(provider="session-diagnostics", id="configure@session-diagnostics",
                                  resource="session-diagnostics", operation="configure"),
                              dict(provider="session-diagnostics", id="inspect@session-diagnostics",
                                   resource="session-diagnostics", operation="inspect")]
        value["count"] = 3
        self.assertEqual(discover(lambda arguments: value, APP, BOUND, FAMILY), reply()["actions"])
        value["actions"] = value["actions"][1:]
        value["count"] = 2
        self.assertEqual(discover(lambda arguments: value, APP, BOUND, FAMILY), [])

    def test_diagnostic_rows_with_foreign_metadata_are_refused(self):
        core = dict(provider="session-diagnostics", id="inspect@session-diagnostics",
                    resource="session-diagnostics", operation="inspect")
        for key in ("id", "resource", "operation", "application", "room", "record",
                    "semantic_identity"):
            value = reply()
            value["actions"].append(dict(core, **{key: "foreign"}))
            value["count"] = 2
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                discover(lambda arguments: value, APP, BOUND, FAMILY)

    def test_scoped_empty_is_not_retried_with_broader_scope(self):
        calls = []
        value = reply(); value.update(actions=[], count=0)
        def control(arguments):
            calls.append(arguments)
            return value
        self.assertEqual(discover(control, APP, BOUND, FAMILY), [])
        self.assertEqual(len(calls), 1)

    def test_foreign_scope_provider_record_and_family_are_refused(self):
        for fault in ("query", "scope", "count", "boolean-count", "provider", "application",
                      "room", "record", "family", "malformed-identity", "oversize"):
            value = copy.deepcopy(reply())
            if fault == "query": value["query"] = APP
            elif fault == "scope": value["scope"]["record"] = "other"
            elif fault == "count": value["count"] = 0
            elif fault == "boolean-count": value["count"] = True
            elif fault == "family": value["actions"][0]["semantic_identity"]["kind"] = "notes.page"
            elif fault == "malformed-identity": value["actions"][0]["semantic_identity"] = {"kind": FAMILY}
            elif fault == "oversize": value["actions"][0]["title"] = "x" * (900*1024)
            else: value["actions"][0][fault] = "foreign"
            with self.subTest(fault=fault), self.assertRaises(RuntimeError):
                discover(lambda arguments: value, APP, BOUND, FAMILY)

    def test_invalid_scope_is_refused_before_transport(self):
        for bound, family in (((APP, "not-a-room", RECORD), FAMILY),
                              ((APP, ROOM, RECORD+"\n"), FAMILY),
                              ((APP, ROOM, "x"*513), FAMILY), (BOUND, "notes/*"),
                              (None, FAMILY), (BOUND, None)):
            calls = []
            with self.subTest(bound=bound, family=family), self.assertRaises(RuntimeError):
                discover(lambda arguments: calls.append(arguments), APP, bound, family)
            self.assertEqual(calls, [])

    def test_bootstrap_filters_application_metadata_not_titles_or_other_providers(self):
        value = reply(); value["scope"] = None
        value["actions"][0]["title"] = "Notebook"
        value["actions"] += [dict(value["actions"][0], application="so.tilo.store"),
                              dict(value["actions"][0], provider="files")]
        value["count"] = 3
        self.assertEqual(discover(lambda arguments: value, APP), value["actions"][:1])


if __name__ == "__main__":
    unittest.main()
