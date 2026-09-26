"""Offline contract checks for the local-model release inputs. Nothing is downloaded."""

import importlib.util
import json
import pathlib
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("prepare_release", ROOT / "prepare-release.py")
prepare = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(prepare)

MODEL_DIRS = sorted(path.parent for path in ROOT.glob("*/release.json"))


class ReleaseInputs(unittest.TestCase):
    def test_every_model_is_pinned_and_names_a_pinned_runtime(self):
        self.assertTrue(MODEL_DIRS)
        for model_dir in MODEL_DIRS:
            with self.subTest(model=model_dir.name):
                release = prepare.load_release(model_dir)
                files = prepare.load_manifest(model_dir)
                runtime = prepare.load_runtime(release["runtime"])
                self.assertTrue(any(path.endswith(".safetensors") for _, path in files))
                self.assertIn(runtime["kind"], {"nvidia_cuda", "amd_rocm", "cpu"})
                self.assertNotIn("latest", json.dumps(release))

    def test_no_general_model_declares_face_recognition(self):
        for model_dir in MODEL_DIRS:
            release = prepare.load_release(model_dir)
            self.assertNotIn("face_recognition", release["capabilities"], model_dir.name)

    def test_the_review_model_fits_an_eight_gib_card_beside_the_desktop(self):
        release = prepare.load_release(ROOT / "qwen3-4b-awq")
        self.assertEqual(release["capabilities"], ["trace_review"])
        # Drivers report an 8 GiB card as a little under 8192 MiB.
        self.assertLess(release["required_accelerator_mib"], 8000)
        self.assertLessEqual(release["accelerator_memory_percent"], 75)

    def test_the_catalogue_source_points_every_file_at_its_vendor_resolve_url(self):
        models = [(prepare.load_release(path), prepare.load_manifest(path)) for path in MODEL_DIRS]
        runtimes = [prepare.load_runtime("vllm-cuda-0.10.1")]
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            for release, files in models:
                slug = release["id"].replace("/", "--")
                for _, path in files:
                    target = root / "models" / slug / path
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(b"x")
            source = prepare.catalog_source(
                models, runtimes, "dev-key", "qwen/qwen3-4b-awq", "sha256:" + "a" * 64, root
            )
        self.assertEqual(source["runtimes"][0]["oci_image"].rpartition("@sha256:")[2],
                         source["runtimes"][0]["artifact"]["sha256"])
        for model in source["models"]:
            snapshot = model["artifact"]["hf_snapshot"]["files"]
            for entry in snapshot:
                self.assertTrue(entry["url"].startswith("https://huggingface.co/"))
                self.assertTrue(entry["url"].endswith(f"/resolve/{model['revision']}/{entry['path']}"))
            self.assertIn(model["revision"], model["license_url"])
            self.assertEqual(model["installed_size_bytes"], len(snapshot))

    def test_unsafe_snapshot_paths_are_refused(self):
        for path in ["../escape", "/etc/passwd", "a//b", "a/./b", ""]:
            self.assertFalse(prepare.snapshot_path(path), path)


if __name__ == "__main__":
    unittest.main()
