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
RUNTIMES = sorted(path.stem for path in (ROOT / "runtimes").glob("*.json"))


def load(model_dir):
    release = prepare.load_release(model_dir)
    return release, prepare.load_manifest(model_dir, release["format"])


class ReleaseInputs(unittest.TestCase):
    def test_every_model_is_pinned_and_names_a_pinned_runtime_that_loads_its_format(self):
        self.assertTrue(MODEL_DIRS)
        for model_dir in MODEL_DIRS:
            with self.subTest(model=model_dir.name):
                release, files = load(model_dir)
                runtime = prepare.load_runtime(release["runtime"])
                if release["format"] == "gguf":
                    self.assertEqual(runtime["engine"], "llama_cpp")
                    self.assertEqual(len(files), 1)
                else:
                    self.assertEqual(runtime["engine"], "vllm")
                    self.assertTrue(any(path.endswith(".safetensors") for _, path in files))
                self.assertNotIn("latest", json.dumps(release))

    def test_the_catalogue_stays_small_with_one_pack_per_engine(self):
        engines = [prepare.load_runtime(name)["engine"] for name in RUNTIMES]
        self.assertEqual(sorted(engines), ["llama_cpp", "vllm"])

    def test_the_llama_cpp_pack_runs_vulkan_and_the_cpu_from_its_vendor_registry(self):
        runtime = prepare.load_runtime("llama-cpp-vulkan-b11176")
        self.assertEqual(runtime["kinds"], ["vulkan", "cpu"])
        self.assertTrue(runtime["image"].startswith("ghcr.io/ggml-org/llama.cpp@sha256:"))

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

    def test_the_review_model_gguf_is_the_same_family_for_llama_cpp(self):
        awq = prepare.load_release(ROOT / "qwen3-4b-awq")
        gguf = prepare.load_release(ROOT / "qwen3-4b-gguf")
        self.assertEqual(gguf["capabilities"], awq["capabilities"])
        self.assertEqual(gguf["context_window"], awq["context_window"])
        self.assertEqual(gguf["license_id"], "Apache-2.0")
        self.assertTrue(gguf["repository"].startswith("Qwen/Qwen3-4B"))
        # Fits a 6 GiB card, and an iGPU with 8 GiB of RAM to spare.
        self.assertLess(gguf["required_accelerator_mib"], 6 * 1024)

    def test_a_gguf_release_is_exactly_one_gguf_file(self):
        with tempfile.TemporaryDirectory() as temp:
            model_dir = pathlib.Path(temp)
            (model_dir / "model-files.sha256").write_text(
                f"{'a' * 64}  model.gguf\n{'b' * 64}  config.json\n"
            )
            with self.assertRaises(ValueError):
                prepare.load_manifest(model_dir, "gguf")

    def test_the_catalogue_source_points_every_file_at_its_vendor_resolve_url(self):
        models = [load(path) for path in MODEL_DIRS]
        runtimes = [prepare.load_runtime(name) for name in RUNTIMES]
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            for release, files in models:
                slug = release["id"].replace("/", "--")
                for _, path in files:
                    target = root / "models" / slug / path
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(b"x")
            source = prepare.catalog_source(models, runtimes, "dev-key", "sha256:" + "a" * 64, root)
        self.assertNotIn("recommendation", source)
        for runtime in source["runtimes"]:
            self.assertEqual(runtime["oci_image"].rpartition("@sha256:")[2], runtime["artifact"]["sha256"])
        for model in source["models"]:
            artifact = model["artifact"]
            entries = artifact["hf_snapshot"]["files"] if "hf_snapshot" in artifact else [artifact["gguf"]]
            for entry in entries:
                self.assertTrue(entry["url"].startswith("https://huggingface.co/"))
                self.assertIn(f"/resolve/{model['revision']}/", entry["url"])
            self.assertIn(model["revision"], model["license_url"])
            self.assertEqual(model["installed_size_bytes"], len(entries))
        gguf = next(model for model in source["models"] if model["id"] == "qwen/qwen3-4b-gguf")
        self.assertEqual(gguf["engines"], ["llama_cpp"])
        self.assertEqual(gguf["allowed_runtimes"], ["vulkan", "cpu"])
        self.assertTrue(gguf["artifact"]["gguf"]["url"].endswith("/Qwen3-4B-Q4_K_M.gguf"))

    def test_unsafe_snapshot_paths_are_refused(self):
        for path in ["../escape", "/etc/passwd", "a//b", "a/./b", ""]:
            self.assertFalse(prepare.snapshot_path(path), path)


if __name__ == "__main__":
    unittest.main()
