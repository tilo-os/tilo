"""Immutable publication must fail closed on conflicts and unavailable reads."""
import importlib.util
import io
from pathlib import Path
import subprocess
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
            build.return_value.open.side_effect = [missing, io.BytesIO(b"public-object")]
            with patch.object(publisher, "upload") as upload:
                publisher.publish(*self.fixture(directory), "test-token")
            upload.assert_called_once()
            calls = build.return_value.open.call_args_list
            self.assertIsInstance(calls[-1].args[0], str)

    def test_redirects_and_foreign_destinations_are_rejected(self):
        with self.assertRaises(ValueError):
            publisher.NoRedirect().redirect_request(None, None, 302, "", {}, "https://other.invalid")
        with tempfile.TemporaryDirectory() as directory, patch.object(publisher.urllib.request, "build_opener") as build:
            path, _, size, digest = self.fixture(directory)
            with self.assertRaises(ValueError):
                publisher.publish(path, "https://other.invalid/weights", size, digest, "test-token")
            build.assert_not_called()


class UploadTransport(unittest.TestCase):
    def test_credential_is_passed_on_stdin_only(self):
        result = subprocess.CompletedProcess([], 0, stdout="201", stderr="")
        with patch.object(publisher.subprocess, "run", return_value=result) as run:
            publisher.upload(Path("/tmp/reviewed-model.bin"), "https://code.tilo.so/model", "private-token")
        args = run.call_args.args[0]
        self.assertIn("--upload-file", args)
        self.assertIn("--fail", args)
        self.assertIn("--config", args)
        self.assertNotIn("private-token", " ".join(args))
        self.assertEqual(run.call_args.kwargs["input"], 'header = "Authorization: token private-token"\n')

    def test_redirect_or_http_error_never_count_as_publication(self):
        for code, status in [(0, "302"), (22, "413"), (56, "000")]:
            with self.subTest(code=code, status=status):
                result = subprocess.CompletedProcess([], code, stdout=status, stderr="")
                with patch.object(publisher.subprocess, "run", return_value=result):
                    with self.assertRaisesRegex(ValueError, f"HTTP {status}"):
                        publisher.upload(Path("/tmp/reviewed-model.bin"), "https://code.tilo.so/model", "private-token")

    def test_credential_cannot_inject_curl_configuration(self):
        with patch.object(publisher.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "invalid package publication credential"):
                publisher.upload(Path("/tmp/reviewed-model.bin"), "https://code.tilo.so/model", "bad\nheader")
            run.assert_not_called()


class CheckCredentialContract(unittest.TestCase):
    def test_only_the_publishing_check_holds_the_package_credential(self):
        import tomllib
        config = tomllib.loads((Path(__file__).resolve().parents[2] / "t.toml").read_text())
        checks = {check["name"]: check for check in config["check"]}
        self.assertEqual(checks["publish-model-catalog"]["secrets"], ["REGISTRY_TOKEN"])
        self.assertEqual(checks["sign-model-catalog"]["secrets"], ["TILO_MODEL_CATALOG_SIGNING_KEY"])
        self.assertNotIn("secrets", checks["model-catalog-compiler"])
        for name in ("sign-model-catalog", "publish-model-catalog", "review-speech-catalog"):
            self.assertEqual(checks[name]["on"], ["dispatch"], name)

if __name__ == "__main__":
    unittest.main()
