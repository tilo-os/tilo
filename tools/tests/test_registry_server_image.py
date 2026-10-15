from pathlib import Path
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]


class RegistryServerImageTests(unittest.TestCase):
    def setUp(self) -> None:
        t = tomllib.loads((ROOT / "t.toml").read_text())
        self.checks = {check["name"]: check for check in t["check"]}
        self.containerfile = (
            ROOT / "crates/packaging/registry-server/container/Containerfile"
        ).read_text()
        self.smoke = (ROOT / "crates/packaging/registry-server/container/test-image.sh").read_text()

    def test_the_image_is_tested_before_main_publishes_it(self) -> None:
        build = self.checks["image-registry-server"]
        self.assertNotIn("id_token", build)
        self.assertNotIn("secrets", build)
        self.assertEqual(build["requires"], ["arch=x86_64"])
        self.assertIn("--platform linux/amd64", build["run"])
        test = build["run"].index("container/test-image.sh")
        self.assertGreater(test, build["run"].index("oci-build.sh"))
        publish = self.checks["publish-registry-server"]
        self.assertEqual(publish["needs"], ["image-registry-server"])
        self.assertEqual(publish["on"], ["main"])
        self.assertEqual(publish["id_token"], ["registry.tilo.so"])
        self.assertIn("registry.tilo.so/tilo/tilo-registry-server", publish["run"])
        self.assertIn("'sha-{commit}'", publish["run"])
        self.assertNotRegex(publish["run"], r"latest|:main")

    def test_runtime_is_distroless_and_nonroot(self) -> None:
        self.assertRegex(
            self.containerfile, r"FROM gcr\.io/distroless/cc-debian12:nonroot@sha256:[0-9a-f]{64}"
        )
        self.assertIn("USER 65532:65532", self.containerfile)
        self.assertIn("65532:65532", self.smoke)

    def test_smoke_requires_the_delegated_offline_root_cli(self) -> None:
        for command in (
            "authorize-channel",
            "collect-first-party",
            "publish-staged-first-party",
            "sign-delegated-snapshot",
        ):
            self.assertIn(command, self.smoke)
        self.assertIn("removed unsafe promote-first-party command is still present", self.smoke)
        self.assertIn("/health", self.smoke)


if __name__ == "__main__":
    unittest.main()
