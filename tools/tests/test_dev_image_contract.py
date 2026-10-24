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
        for name in ("os/Containerfile", "os/Containerfile.dev", "os/image/compile.sh", "os/dev/resolve-rpm-snapshot.sh", "os/components.toml",
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

    def test_repository_and_fixture_pass_with_one_snapshot_pin_and_labelled_dev_image(self):
        with self.subTest(phase='repository passes'):
            self.assertEqual(self.findings(REPO), [])
        with self.subTest(phase='fixture passes'):
            self.assertEqual(self.findings(self.fixture()), [])
        with self.subTest(phase='snapshot pin has one home'):
            root = self.fixture()
            self.edit(root / "os/Containerfile.dev", "ARG RPMS_REF\n",
                      "ARG RPMS_REF=registry.tilo.so/tilo/rpms@sha256:0\n")
            self.assertIn("RPMS_REF has a default", "\n".join(self.findings(root)))
        with self.subTest(phase='rust toolchain is digest pinned'):
            root = self.fixture()
            dev = root / "os/Containerfile.dev"
            text = dev.read_text(encoding="utf-8")
            pinned = next(line for line in text.splitlines() if line.startswith("ARG RUST_TOOLCHAIN_IMAGE="))
            self.edit(dev, pinned, "ARG RUST_TOOLCHAIN_IMAGE=registry.tilo.so/tilo/tilo-builder:latest")
            self.assertIn("RUST_TOOLCHAIN_IMAGE is not a digest-pinned tilo-builder image",
                          "\n".join(self.findings(root)))
        with self.subTest(phase='builder packages leave out fedora rust'):
            listed = (REPO / "os/scripts/builder-packages.txt").read_text(encoding="utf-8").split()
            self.assertNotIn("rust", listed)
            self.assertNotIn("cargo", listed)
            self.assertIn("sccache", listed)
        with self.subTest(phase='builder package drift fails'):
            root = self.fixture()
            self.edit(root / "os/Containerfile", "        nasm \\\n", "        nasm mold \\\n")
            errors = "\n".join(self.findings(root))
            self.assertIn("os/scripts/builder-packages.txt: differs from the packages", errors)
            self.assertIn("missing: mold", errors)
            self.assertIn("builder-packages.py --write", errors)
        with self.subTest(phase='dev image must label and unseal'):
            root = self.fixture()
            dev = root / "os/Containerfile.dev"
            self.edit(dev, 'LABEL sh.tilo.image.flavour="dev"', "LABEL sh.tilo.image.other=x")
            self.edit(dev, "python3 /run/tilo-dev/os/scripts/unseal-rootfs.py /", "true")
            errors = "\n".join(self.findings(root))
            self.assertIn('does not label the image sh.tilo.image.flavour="dev"', errors)
            self.assertIn("never runs os/scripts/unseal-rootfs.py", errors)

    def test_missing_package_generator_cannot_skip_parity_verification(self):
        root = self.fixture()
        (root / "os/scripts/builder-packages.py").unlink()
        errors = self.findings(root)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("os/scripts/builder-packages.py: package generator is missing", errors[0])

    def test_unreadable_package_list_reports_the_input_error_not_package_drift(self):
        for state in ("missing", "directory"):
            with self.subTest(state=state):
                root = self.fixture()
                listing = root / "os/scripts/builder-packages.txt"
                listing.unlink()
                if state == "directory":
                    listing.mkdir()
                errors = self.findings(root)
                self.assertEqual(len(errors), 1, errors)
                self.assertIn("os/scripts/builder-packages.txt: cannot read package list:", errors[0])
                self.assertIn("No such file" if state == "missing" else "Is a directory", errors[0])
                self.assertNotIn("missing: ", errors[0])

    def test_dev_layers_features_and_images_never_reach_production(self):
        with self.subTest(phase='production copy of a dev layer names line why and fix'):
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
        with self.subTest(phase='production build of a dev only bin fails'):
            root = self.fixture()
            self.edit(root / "os/image/compile.sh", "--bin tilo-projector ", "--bin tilo-projector --bin tilo-vmd ")
            self.assertIn("bin 'tilo-vmd'", "\n".join(self.findings(root)))
        with self.subTest(phase='production dev feature fails'):
            root = self.fixture()
            self.edit(root / "os/image/compile.sh", "--bin tilo-projector ",
                      "--bin tilo-projector --features dev-overrides ")
            self.assertIn("dev feature 'dev-overrides'", "\n".join(self.findings(root)))
        with self.subTest(phase='production policy must reject dev repository'):
            root = self.fixture()
            path = root / "os/etc/containers/policy.json"
            policy = json.loads(path.read_text())
            del policy["transports"]["docker"]["registry.tilo.so/tilo/tilo-vm"]
            path.write_text(json.dumps(policy, indent=2))
            self.assertIn("does not reject registry.tilo.so/tilo/tilo-vm", "\n".join(self.findings(root)))
        with self.subTest(phase='production policy must reject integration repository'):
            root = self.fixture()
            path = root / "os/etc/containers/policy.json"
            policy = json.loads(path.read_text())
            policy["transports"]["docker"]["registry.tilo.so/tilo/tilo-ci"] = [{"type": "insecureAcceptAnything"}]
            path.write_text(json.dumps(policy, indent=2))
            self.assertIn("does not reject registry.tilo.so/tilo/tilo-ci", "\n".join(self.findings(root)))
        with self.subTest(phase='dev image tagged as production fails'):
            root = self.fixture()
            workflow = root / ".forgejo/workflows/bad.yml"
            workflow.write_text("jobs:\n  b:\n    steps:\n      - run: |\n"
                                "          podman build --file os/Containerfile.dev \\\n"
                                "            --tag registry.tilo.so/tilo/tilo:latest .\n")
            errors = "\n".join(self.findings(root))
            self.assertIn(".forgejo/workflows/bad.yml:5:", errors)
            self.assertIn("names the production image 'registry.tilo.so/tilo/tilo:latest'", errors)
        with self.subTest(phase='dev image pushed to production fails'):
            root = self.fixture()
            script = root / "os/push-dev.sh"
            script.write_text("#!/bin/sh\n# builds os/Containerfile.dev\n"
                              "podman push localhost/tilo-vm:x docker://registry.tilo.so/tilo/tilo:main\n")
            self.assertIn("os/push-dev.sh:3:", "\n".join(self.findings(root)))


class FinalizeDevImageTests(unittest.TestCase):
    finalize = load("finalize_dev_image_under_test", REPO / "os/scripts/finalize-dev-image.py")

    def test_dev_policy_admits_only_the_dev_repository_on_a_clean_base(self):
        with self.subTest(phase='dev policy admits only the dev repository'):
            production = json.loads((REPO / "os/etc/containers/policy.json").read_text())
            before = json.dumps(production["transports"]["docker"]["registry.tilo.so/tilo/tilo"], sort_keys=True)
            dev = self.finalize.dev_policy(json.loads(json.dumps(production)))
            docker = dev["transports"]["docker"]
            self.assertEqual(docker["registry.tilo.so/tilo/tilo-vm"], [{"type": "insecureAcceptAnything"}])
            self.assertEqual(json.dumps(docker["registry.tilo.so/tilo/tilo"], sort_keys=True), before)
            self.assertEqual(dev["default"], production["default"])
        with self.subTest(phase='a base that already admits dev images is refused'):
            policy = {"transports": {"docker": {"registry.tilo.so/tilo/tilo-vm": [{"type": "insecureAcceptAnything"}]}}}
            with self.assertRaises(SystemExit):
                self.finalize.dev_policy(policy)


if __name__ == "__main__":
    unittest.main()
