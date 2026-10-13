"""Offline checks for public qualification receipts, bounds and honest scoring."""
import importlib.util
import io
from pathlib import Path
import struct
import tempfile
import unittest
from unittest import mock
import wave


def load(name):
    path = Path(__file__).resolve().parents[1] / "release" / (name + ".py")
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


prepare = load("prepare_speech_benchmark")
score = load("summarize_speech_benchmark")


class SpeechBenchmarkTest(unittest.TestCase):
    def test_selected_baseline_does_not_stage_other_model_weights(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "public"
            with mock.patch("sys.argv", ["prepare", str(destination), "--model", "base", "--model", "base"]), \
                    mock.patch.object(prepare, "stage_language", return_value=[]), \
                    mock.patch.object(prepare, "stage_model", return_value={"file": "base.bin"}) as stage:
                prepare.main()
            stage.assert_called_once_with(destination, "base", None)
            import json
            self.assertEqual(set(json.loads((destination / "manifest.json").read_text())["models"]), {"base"})

    def test_insecure_transport_is_rejected_before_request(self):
        with self.assertRaises(ValueError):
            prepare.get("http://example.invalid")
        with self.assertRaises(ValueError):
            prepare.HttpsOnly().redirect_request(None, None, 302, "", {}, "http://example.invalid")

    def test_pcm16_conversion_preserves_real_samples(self):
        output = io.BytesIO()
        with wave.open(output, "wb") as target:
            target.setnchannels(1)
            target.setsampwidth(2)
            target.setframerate(16000)
            target.writeframes(struct.pack("<hhh", -32768, 0, 32767))
        converted, seconds = prepare.pcm16(output.getvalue())
        with wave.open(io.BytesIO(converted)) as source:
            self.assertEqual(source.readframes(3), struct.pack("<hhh", -32768, 0, 32767))
        self.assertEqual(seconds, 3 / 16000)
        with self.assertRaises(ValueError):
            prepare.pcm16(output.getvalue()[:-1])

    def test_compressed_source_bound_fails_closed(self):
        stream = prepare.BoundedArchive(io.BytesIO(b"abcd"))
        stream.remaining = 3
        self.assertEqual(stream.read(2), b"ab")
        with self.assertRaises(ValueError):
            stream.read(2)
        with self.assertRaises(ValueError):
            stream.read(-1)

    def test_scoring_retains_language_and_number_errors(self):
        self.assertEqual(score.units("Hello, WORLD!"), ["hello", "world"])
        self.assertEqual(score.distance(score.units("2011"), score.units("2021")), 1)
        self.assertEqual(score.distance(score.units("这", True), score.units("這", True)), 1)
        self.assertEqual(score.distance(["one", "two"], ["one", "three", "four"], True), 1)
        self.assertEqual(score.distance(["one"], ["one", "two"], True), 0)

    def test_unobserved_probes_fail(self):
        import tempfile
        with tempfile.NamedTemporaryFile(mode="w") as empty:
            empty.flush()
            with self.assertRaises(ValueError):
                score.summarize(empty.name)


if __name__ == "__main__":
    unittest.main()
