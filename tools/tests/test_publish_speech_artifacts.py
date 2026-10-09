"""Immutable publication must fail closed on conflicts and unavailable reads."""
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

SPEC = importlib.util.spec_from_file_location(
    "publish_speech_artifacts", Path(__file__).parents[1] / "release/publish_speech_artifacts.py"
)
publisher = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(publisher)


class SpeechArtifactPublication(unittest.TestCase):
    def fixture(self, directory):
        path = Path(directory) / "public-object"
        path.write_bytes(b"public-object")
        return path, "https://code.tilo.so/api/packages/tilo/generic/whisper-tiny/revision/weights", 13, publisher.hashlib.sha256(path.read_bytes()).hexdigest()

    def test_existing_receipt_conflict_never_uploads(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(publisher.urllib.request, "build_opener") as build:
            build.return_value.open.return_value = io.BytesIO(b"wrong")
            with self.assertRaises(ValueError):
                publisher.publish(*self.fixture(directory), "test-token")
            self.assertEqual(build.return_value.open.call_count, 1)

    def test_authentication_and_server_failure_never_turn_into_upload(self):
        for status in [401, 403, 429, 500]:
            with tempfile.TemporaryDirectory() as directory, patch.object(publisher.urllib.request, "build_opener") as build:
                build.return_value.open.side_effect = urllib.error.HTTPError("", status, "", {}, None)
                with self.assertRaises(ValueError):
                    publisher.publish(*self.fixture(directory), "test-token")
                self.assertEqual(build.return_value.open.call_count, 1)

    def test_missing_object_requires_credential_and_anonymous_exact_readback(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(publisher.urllib.request, "build_opener") as build:
            missing = urllib.error.HTTPError("", 404, "", {}, None)
            build.return_value.open.side_effect = missing
            with self.assertRaises(ValueError):
                publisher.publish(*self.fixture(directory), "")
            uploaded = io.BytesIO()
            uploaded.status = 201
            build.return_value.open.side_effect = [missing, uploaded, io.BytesIO(b"public-object")]
            publisher.publish(*self.fixture(directory), "test-token")
            calls = build.return_value.open.call_args_list
            self.assertEqual(calls[-2].args[0].method, "PUT")
            self.assertIsInstance(calls[-1].args[0], str)

    def test_redirects_and_foreign_destinations_are_rejected(self):
        with self.assertRaises(ValueError):
            publisher.NoRedirect().redirect_request(None, None, 302, "", {}, "https://other.invalid")
        with tempfile.TemporaryDirectory() as directory, patch.object(publisher.urllib.request, "build_opener") as build:
            path, _, size, digest = self.fixture(directory)
            with self.assertRaises(ValueError):
                publisher.publish(path, "https://other.invalid/weights", size, digest, "test-token")
            build.assert_not_called()


if __name__ == "__main__":
    unittest.main()
