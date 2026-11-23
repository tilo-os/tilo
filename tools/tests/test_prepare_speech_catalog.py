"""Checks for the offline portions of review-only speech release staging."""
import importlib.util
import io
import os
from pathlib import Path
import tempfile
import unittest
import urllib.error
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "prepare_speech_catalog", Path(__file__).parents[1] / "release/prepare_speech_catalog.py"
)
staging = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(staging)


def http_error(url: str, code: int) -> urllib.error.HTTPError:
    return urllib.error.HTTPError(url, code, f"HTTP {code}", {}, None)


class SpeechCatalogPreparation(unittest.TestCase):
    def setUp(self):
        # The verified artifacts of one test do not stand in for another's.
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.cache = Path(temporary.name)
        environment = patch.dict(os.environ, {staging.CACHE_ENV: str(self.cache)})
        environment.start()
        self.addCleanup(environment.stop)
        sleeping = patch.object(staging.time, "sleep", lambda _seconds: None)
        sleeping.start()
        self.addCleanup(sleeping.stop)

    def test_transport_refuses_plaintext_and_plaintext_redirects(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                staging.stage(Path(directory) / "weights", "http://example.invalid", 3, "bad")
        with self.assertRaises(ValueError):
            staging.HttpsOnly().redirect_request(None, None, 302, "", {}, "http://example.invalid")

    def test_exact_receipt_promotes_and_bad_or_oversized_bytes_do_not(self):
        data = b"public-fixture"
        digest = staging.hashlib.sha256(data).hexdigest()
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "weights"
            for payload, receipt in [(data, "bad"), (data + b"extra", digest), (data[:-1], digest)]:
                with patch.object(staging.urllib.request, "build_opener") as opener:
                    opener.return_value.open.return_value = io.BytesIO(payload)
                    with self.assertRaises(ValueError):
                        staging.stage(destination, "https://example.invalid", len(data), receipt)
                self.assertEqual(list(Path(directory).iterdir()), [])
            with patch.object(staging.urllib.request, "build_opener") as opener:
                opener.return_value.open.return_value = io.BytesIO(data)
                staging.stage(destination, "https://example.invalid", len(data), digest)
            self.assertEqual(destination.read_bytes(), data)

    def test_base_candidate_uses_exact_receipt_and_complete_existing_notice(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "review"
            def stage_without_network(path, url, size, sha):
                path.parent.mkdir(parents=True, exist_ok=True)
            with patch("sys.argv", ["prepare", str(destination)]), patch.object(staging, "stage", side_effect=stage_without_network) as transfer:
                staging.main()
            import json
            source = json.loads((destination / "source.json").read_text())
            base = next(model for model in source["models"] if model["id"] == "whisper-base-multilingual")
            self.assertEqual(base["artifact"]["sha256"], "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe")
            self.assertEqual(base["artifact"]["size_bytes"], 147951465)
            self.assertIn("https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.bin", [call.args[1] for call in transfer.call_args_list])
            notice = (destination / "artifacts/whisper-base-LICENSE").read_bytes()
            self.assertEqual(len(notice), 1063)
            self.assertEqual(staging.hashlib.sha256(notice).hexdigest(), "b5d65a59060e68c4ff940e1eddfa6f94b2d68fdf58ed7f4dd57721c997e35e9d")

    def test_a_flaky_host_is_retried_and_its_artifact_is_kept(self):
        data = b"public-fixture"
        digest = staging.hashlib.sha256(data).hexdigest()
        url = "https://example.invalid/weights"
        answers = [http_error(url, 502), http_error(url, 503), io.BytesIO(data)]
        with patch.object(staging.urllib.request, "build_opener") as opener:
            opener.return_value.open.side_effect = answers
            with tempfile.TemporaryDirectory() as directory:
                destination = Path(directory) / "weights"
                staging.stage(destination, url, len(data), digest)
                self.assertEqual(destination.read_bytes(), data)
            self.assertEqual(opener.return_value.open.call_count, 3)
            # A later run of the same artifact fetches nothing.
            with tempfile.TemporaryDirectory() as directory:
                destination = Path(directory) / "weights"
                staging.stage(destination, url, len(data), digest)
                self.assertEqual(destination.read_bytes(), data)
            self.assertEqual(opener.return_value.open.call_count, 3)

    def test_a_host_that_keeps_failing_names_the_url_and_the_status(self):
        url = "https://example.invalid/weights"
        with patch.object(staging.urllib.request, "build_opener") as opener:
            opener.return_value.open.side_effect = http_error(url, 502)
            with tempfile.TemporaryDirectory() as directory:
                with self.assertRaises(staging.DownloadError) as raised:
                    staging.stage(Path(directory) / "weights", url, 3,
                                  staging.hashlib.sha256(b"bad").hexdigest())
            self.assertIn(url, str(raised.exception))
            self.assertIn("HTTP 502", str(raised.exception))
            self.assertEqual(opener.return_value.open.call_count, staging.TRIES)

    def test_a_refused_answer_is_not_retried(self):
        url = "https://example.invalid/weights"
        with patch.object(staging.urllib.request, "build_opener") as opener:
            opener.return_value.open.side_effect = http_error(url, 404)
            with tempfile.TemporaryDirectory() as directory:
                with self.assertRaises(urllib.error.HTTPError):
                    staging.stage(Path(directory) / "weights", url, 3,
                                  staging.hashlib.sha256(b"bad").hexdigest())
            self.assertEqual(opener.return_value.open.call_count, 1)

    def test_preparation_never_overwrites_an_existing_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "preserve"
            marker.write_text("existing work")
            with patch("sys.argv", ["prepare", directory]):
                with self.assertRaises(FileExistsError):
                    staging.main()
            self.assertEqual(marker.read_text(), "existing work")


if __name__ == "__main__":
    unittest.main()
