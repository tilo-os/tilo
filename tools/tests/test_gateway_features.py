"""Native verification must compile gateway server tests, not only its clients."""
import importlib.util
import json
import re
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "ci"))
import t_matrix  # noqa: E402
SPEC = importlib.util.spec_from_file_location("package_features", ROOT / "tools/ci/package_features.py")
features = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(features)


class GatewayFeaturesTests(unittest.TestCase):
    def test_recipe_covers_the_production_features_but_not_client_dependents(self):
        manifest = tomllib.loads((ROOT / "crates/agent/model-gateway/Cargo.toml").read_text())
        declared = manifest["features"]
        self.assertEqual(declared["default"], [])
        self.assertIn("server", declared["local-models"])
        self.assertIn("server", declared["host-backend"])
        self.assertEqual(features.arguments(["chat", "tilo-host", "tilo-agent-participant"]), [])
        argv = features.arguments(["tilo-model-gateway"])
        self.assertEqual(argv[0], "--features")
        self.assertEqual(set(argv[1].split(",")),
                         {"tilo-model-gateway/local-models", "tilo-model-gateway/host-backend",
                          "tilo-model-gateway/runtime-client"})
        for binary in manifest["bin"]:
            self.assertIn("server", binary["required-features"])

    def test_ci_and_t_wrapper_execute_the_gateway_recipe_without_workspace_features(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ci = root / "tools/ci"
            ci.mkdir(parents=True)
            for name in ("t-nextest.sh", "package_features.py"):
                shutil.copy(ROOT / "tools/ci" / name, ci / name)
            (root / "tools/test-env.sh").write_text(
                "#!/bin/sh\npython3 -c 'import json,sys; print(json.dumps(sys.argv[1:]))' \"$@\"\n")
            for package in ("tilo-model-gateway", "chat"):
                with self.subTest(package=package):
                    result = subprocess.run(
                        ["bash", str(ci / "t-nextest.sh"), "cargo", "nextest", "run",
                         "--locked", "-p", package, "-E", "all()"],
                        capture_output=True, text=True, check=True)
                    argv = json.loads(result.stdout)
                    self.assertEqual(argv[:5], ["cargo", "nextest", "run", "--locked", "-p"])
                    self.assertEqual(argv[5], package)
                    # Its tests' features come through workspace-unify, never as --features.
                    self.assertEqual(argv[8:10], ["-p", "workspace-unify"])
                    self.assertNotIn("--all-features", argv)
                    self.assertNotIn("--workspace", argv)
            command = ["cargo", "nextest", "run", "--locked", "-p", "chat",
                       "--package", "tilo-model-gateway", "-E", "all()"]
            result = subprocess.run(
                ["bash", str(ci / "t-nextest.sh"), *command],
                capture_output=True, text=True, check=True)
            self.assertEqual(json.loads(result.stdout)[:len(command) + 2],
                             command + ["-p", "workspace-unify"])
            anchor = tomllib.loads((ROOT / "crates/dev/workspace-unify/Cargo.toml").read_text())
            for table in anchor["target"].values():
                gateway = table.get("dependencies", {}).get("tilo-model-gateway")
                if gateway is not None:
                    self.assertLessEqual(set(features.NATIVE_TEST_FEATURES["tilo-model-gateway"]),
                                         set(gateway["features"]))
            command = ["cargo", "check", "--locked", "-p", "tilo-model-gateway"]
            result = subprocess.run(
                ["bash", str(ci / "t-nextest.sh"), *command],
                capture_output=True, text=True, check=True)
            self.assertEqual(json.loads(result.stdout), command)
        config = t_matrix.load(ROOT)
        test = next(family for family in config["cargo"] if family["name"] == "test")
        self.assertEqual(test["wrapper"], ["bash", "tools/ci/t-nextest.sh"])
        self.assertIn("tools/ci/package_features.py", test["inputs"])

    def test_os_release_builds_full_gateway_and_stages_http_binary(self):
        # The OS image's compile runs outside any image build.
        containerfile = (ROOT / "os/image/compile.sh").read_text()
        # The release build names every feature in one list (the relay's
        # web feature rides in it too); the gateway's two must both be there.
        enabled = {feature for listed in re.findall(r"--features (\S+)", containerfile)
                   for feature in listed.split(",")}
        self.assertLessEqual({"tilo-model-gateway/local-models",
                              "tilo-model-gateway/host-backend"}, enabled)
        self.assertIn("--bin tilo-model-gateway --bin tilo-model-http", containerfile)
        self.assertIn("target/release/tilo-model-http", containerfile)


if __name__ == "__main__":
    unittest.main()
