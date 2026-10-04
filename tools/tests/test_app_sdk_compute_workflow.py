"""The release worker crosses SDK jobs unchanged into the existing Chrome lane."""

from pathlib import Path
import json
import re
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".forgejo/workflows/app-sdk.yml"
ASSETS = (
    "compute-worker.mjs", "compute-domains.mjs", "compute-protocol.mjs",
    "pkg/compute/tilo_browser_compute.js", "pkg/compute/tilo_browser_compute_bg.wasm",
)


def job(source: str, name: str) -> str:
    match = re.search(rf"^  {name}:\n(.*?)(?=^  [\w-]+:|\Z)", source, re.M | re.S)
    if match is None:
        raise AssertionError(f"missing job {name}")
    return match[1]


def python_handoff(source: str) -> str:
    match = re.search(r'python3 - "\$RUNNER_TEMP/release-compute" <<\'PY\'\n(.*?)\n          PY', source, re.S)
    if match is None:
        raise AssertionError("missing release asset handoff")
    return "\n".join(line[10:] for line in match[1].splitlines()) + "\n"


class AppSdkComputeWorkflowTests(unittest.TestCase):
    def test_same_run_release_job_feeds_existing_chrome_lane(self):
        source = WORKFLOW.read_text()
        web, release = job(source, "web"), job(source, "web-size")
        self.assertIn("needs: [builder, web-size]", web)
        condition = "if: needs.builder.outputs.affected == 'true'"
        self.assertIn(condition, web)
        self.assertIn(condition, release)
        self.assertEqual(source.count("--bundle desktop --release"), 1)
        self.assertNotIn("--bundle desktop --release", web)
        self.assertIn("--bundle desktop --release", release)
        self.assertEqual(source.count("@puppeteer/browsers@2 install"), 1)
        self.assertIn("@puppeteer/browsers@2 install", web)
        self.assertIn("name: app-sdk-release-compute", release)
        self.assertIn("if-no-files-found: error", release)
        self.assertIn("actions/download-artifact@", web)
        self.assertIn("name: app-sdk-release-compute", web)
        self.assertIn("path: ${{ runner.temp }}/release-compute", web)
        self.assertNotIn("continue-on-error", web)
        self.assertIn('"$RUNNER_TEMP/release-compute" "$RUNNER_TEMP/compute-qualification" --no-sandbox', web)

    def test_exact_layout_hash_binding_and_product_preservation(self):
        source = WORKFLOW.read_text()
        emit = python_handoff(job(source, "web-size"))
        verify = python_handoff(job(source, "web"))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bundle = root / "target/tilo-app/web/calculator/bundle"
            originals = {name: (name + " emitted release bytes").encode() for name in ASSETS}
            for name, data in originals.items():
                path = bundle / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
            (bundle / "index.html").write_text("product index")
            before = {str(path.relative_to(bundle)): path.read_bytes() for path in bundle.rglob("*") if path.is_file()}
            artifact = root / "release-compute"
            def run(script):
                return subprocess.run([sys.executable, "-", str(artifact)], input=script,
                                      text=True, cwd=root, capture_output=True)
            emitted = run(emit)
            self.assertEqual(emitted.returncode, 0, emitted.stderr)
            hashes = json.loads((artifact / "sha256.json").read_text())
            self.assertEqual(set(hashes), set(ASSETS))
            self.assertEqual({name: (artifact / name).read_bytes() for name in ASSETS}, originals)
            self.assertEqual(set(str(p.relative_to(artifact)) for p in artifact.rglob("*") if p.is_file()), set(ASSETS) | {"sha256.json"})
            verified = run(verify)
            self.assertEqual(verified.returncode, 0, verified.stderr)
            for name in ASSETS:
                with self.subTest(corrupt=name):
                    path = artifact / name
                    path.write_bytes(b"corrupt")
                    self.assertNotEqual(run(verify).returncode, 0)
                    path.write_bytes(originals[name])
                with self.subTest(missing=name):
                    path.unlink()
                    self.assertNotEqual(run(verify).returncode, 0)
                    path.write_bytes(originals[name])
            self.assertEqual({str(path.relative_to(bundle)): path.read_bytes() for path in bundle.rglob("*") if path.is_file()}, before)


if __name__ == "__main__":
    unittest.main()
