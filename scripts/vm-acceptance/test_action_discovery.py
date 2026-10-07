"""Actual driver parsing must enforce the canonical discovery envelope."""
import copy
import unittest
from acceptance.action_discovery import discover

APP = "so.tilo.notes"
ROOM = "5ba25f73-87c5-447f-a5a6-880dc4249a81"
RECORD = "derived:default@tab:f276000000000044"
FAMILY = "notes.notebook"
BOUND = (APP, ROOM, RECORD)


def reply():
    return dict(query="", scope=dict(application=APP, room=ROOM, record=RECORD,
                semantic_family=FAMILY), count=1, actions=[dict(provider="app-actions",
                application=APP, room=ROOM, record=RECORD, operation="read",
                semantic_identity=dict(kind=FAMILY, key=FAMILY))])


class DiscoverySafety(unittest.TestCase):
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
