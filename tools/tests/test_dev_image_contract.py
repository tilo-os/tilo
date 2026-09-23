import importlib.util
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


check = load("check_dev_image_contract_under_test", REPO / "tools/policy/check_dev_image_contract.py")


class DevImageContractTests(unittest.TestCase):
    def fixture(self) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for name in ("os/Containerfile", "os/Containerfile.dev", "os/rpms-snapshot.env", "os/components.toml",
                     "os/scripts/install-components.py", "os/etc/containers/policy.json",
                     "os/scripts/builder-packages.py", "os/scripts/builder-packages.txt",
                     "os/dev/derive-rpm-package-set.py"):
            (root / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(REPO / name, root / name)
        (root / ".forgejo/workflows").mkdir(parents=True)
        (root / "tools").mkdir()
        return root

    def edit(self, path: Path, old: str, new: str) -> None:
        text = path.read_text(encoding="utf-8")
        self.assertIn(old, text)
        path.write_text(text.replace(old, new, 1), encoding="utf-8")

    def findings(self, root: Path) -> list[str]:
        return [str(f) for f in check.audit(root)]

    def line_of(self, path: Path, needle: str) -> int:
        return next(n for n, t in enumerate(path.read_text().splitlines(), 1) if needle in t)

    def test_repository_passes(self):
        self.assertEqual(self.findings(REPO), [])

    def test_fixture_passes(self):
        self.assertEqual(self.findings(self.fixture()), [])

    def test_production_copy_of_a_dev_layer_names_line_why_and_fix(self):
        root = self.fixture()
        containerfile = root / "os/Containerfile"
        self.edit(containerfile, "COPY --from=component-files /image/windows/ /\n",
                  "COPY --from=component-files /image/windows/ /\nCOPY --from=component-files /image/dev-tools/ /\n")
        errors = self.findings(root)
        self.assertEqual(len(errors), 1, errors)
        line = self.line_of(containerfile, "/image/dev-tools/")
        self.assertIn(f"os/Containerfile:{line}: production image build references dev-only layer", errors[0])
        self.assertIn("why:", errors[0])
        self.assertIn("fix: remove it", errors[0])

    def test_production_build_of_a_dev_only_bin_fails(self):
        root = self.fixture()
        self.edit(root / "os/Containerfile", "--bin tilo-projector ", "--bin tilo-projector --bin tilo-control ")
        self.assertIn("bin 'tilo-control'", "\n".join(self.findings(root)))

    def test_production_dev_feature_fails(self):
        root = self.fixture()
        self.edit(root / "os/Containerfile", "--bin tilo-projector ",
                  "--bin tilo-projector --features shell/dev-overrides ")
        self.assertIn("dev feature 'shell/dev-overrides'", "\n".join(self.findings(root)))

    def test_production_policy_must_reject_dev_repository(self):
        root = self.fixture()
        path = root / "os/etc/containers/policy.json"
        policy = json.loads(path.read_text())
        del policy["transports"]["docker"]["code.sandbox.example/qos/tilo-vm"]
        path.write_text(json.dumps(policy, indent=2))
        self.assertIn("does not reject code.sandbox.example/qos/tilo-vm", "\n".join(self.findings(root)))

    def test_dev_image_tagged_as_production_fails(self):
        root = self.fixture()
        workflow = root / ".forgejo/workflows/bad.yml"
        workflow.write_text("jobs:\n  b:\n    steps:\n      - run: |\n"
                            "          podman build --file os/Containerfile.dev \\\n"
                            "            --tag code.sandbox.example/qos/tilo:latest .\n")
        errors = "\n".join(self.findings(root))
        self.assertIn(".forgejo/workflows/bad.yml:5:", errors)
        self.assertIn("names the production image 'code.sandbox.example/qos/tilo:latest'", errors)

    def test_dev_image_pushed_to_production_fails(self):
        root = self.fixture()
        script = root / "os/push-dev.sh"
        script.write_text("#!/bin/sh\n# builds os/Containerfile.dev\n"
                          "podman push localhost/tilo-vm:x docker://code.sandbox.example/qos/tilo:main\n")
        self.assertIn("os/push-dev.sh:3:", "\n".join(self.findings(root)))

    def test_snapshot_default_must_match_the_pin(self):
        root = self.fixture()
        self.edit(root / "os/rpms-snapshot.env", "RPMS_REF=code.sandbox.example/qos/rpms@sha256:",
                  "RPMS_REF=code.sandbox.example/qos/rpms@sha256:0")
        self.assertIn("but os/rpms-snapshot.env pins", "\n".join(self.findings(root)))

    def test_builder_package_drift_fails(self):
        root = self.fixture()
        self.edit(root / "os/Containerfile", "        nasm \\\n", "        nasm mold \\\n")
        errors = "\n".join(self.findings(root))
        self.assertIn("os/scripts/builder-packages.txt: differs from the packages", errors)
        self.assertIn("missing: mold", errors)
        self.assertIn("builder-packages.py --write", errors)

    def test_dev_image_must_label_and_unseal(self):
        root = self.fixture()
        dev = root / "os/Containerfile.dev"
        self.edit(dev, 'LABEL sh.tilo.image.flavour="dev"', "LABEL sh.tilo.image.other=x")
        self.edit(dev, "python3 /run/tilo-dev/os/scripts/unseal-rootfs.py /", "true")
        errors = "\n".join(self.findings(root))
        self.assertIn('does not label the image sh.tilo.image.flavour="dev"', errors)
        self.assertIn("never runs os/scripts/unseal-rootfs.py", errors)


class FinalizeDevImageTests(unittest.TestCase):
    finalize = load("finalize_dev_image_under_test", REPO / "os/scripts/finalize-dev-image.py")

    def test_dev_policy_admits_only_the_dev_repository(self):
        production = json.loads((REPO / "os/etc/containers/policy.json").read_text())
        before = json.dumps(production["transports"]["docker"]["code.sandbox.example/qos/tilo"], sort_keys=True)
        dev = self.finalize.dev_policy(json.loads(json.dumps(production)))
        docker = dev["transports"]["docker"]
        self.assertEqual(docker["code.sandbox.example/qos/tilo-vm"], [{"type": "insecureAcceptAnything"}])
        self.assertEqual(json.dumps(docker["code.sandbox.example/qos/tilo"], sort_keys=True), before)
        self.assertEqual(dev["default"], production["default"])

    def test_a_base_that_already_admits_dev_images_is_refused(self):
        policy = {"transports": {"docker": {"code.sandbox.example/qos/tilo-vm": [{"type": "insecureAcceptAnything"}]}}}
        with self.assertRaises(SystemExit):
            self.finalize.dev_policy(policy)


if __name__ == "__main__":
    unittest.main()
