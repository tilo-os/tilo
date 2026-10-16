"""Name-availability checks must distinguish absent crates from failed observation."""

import importlib.util
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch
import urllib.error


SCRIPT = Path(__file__).resolve().parents[1] / "sdk/publish_check.py"
SPEC = importlib.util.spec_from_file_location("sdk_publish_check_under_test", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(check)


class TakenNamesTests(unittest.TestCase):
    def test_registered_crate_is_taken(self):
        response = io.BytesIO(json.dumps({"crate": {"name": "tilo-example"}}).encode())
        with patch.object(check.urllib.request, "urlopen", return_value=response):
            self.assertEqual(check.taken_names(["tilo-example"]), {"tilo-example"})

    def test_only_not_found_means_available(self):
        error = urllib.error.HTTPError("https://crates.io", 404, "Not Found", {}, None)
        with patch.object(check.urllib.request, "urlopen", side_effect=error):
            self.assertEqual(check.taken_names(["tilo-example"]), set())

    def test_invalid_success_response_cannot_mark_a_name_available(self):
        for document in ({}, {"errors": [{"detail": "unavailable"}]},
                         {"crate": None}, {"crate": {}}, {"crate": []},
                         {"crate": {"name": ""}}, []):
            with self.subTest(document=document):
                response = io.BytesIO(json.dumps(document).encode())
                with patch.object(check.urllib.request, "urlopen", return_value=response):
                    with self.assertRaisesRegex(ValueError, "tilo-example.*cannot determine"):
                        check.taken_names(["tilo-example"])

    def test_service_and_transport_failures_propagate(self):
        for error in (urllib.error.HTTPError("https://crates.io", 503, "Unavailable", {}, None),
                      urllib.error.URLError("connection refused")):
            with self.subTest(error=error):
                with patch.object(check.urllib.request, "urlopen", side_effect=error):
                    with self.assertRaises(type(error)) as raised:
                        check.taken_names(["tilo-example"])
                    self.assertIs(raised.exception, error)

    def test_invalid_json_propagates(self):
        with patch.object(check.urllib.request, "urlopen", return_value=io.BytesIO(b"not json")):
            with self.assertRaises(json.JSONDecodeError):
                check.taken_names(["tilo-example"])


if __name__ == "__main__":
    unittest.main()
