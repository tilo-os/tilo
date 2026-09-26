"""registry.tilo.so is pulled anonymously and pushed with the job's ID token."""
from pathlib import Path
import json
import os
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
ACTION = ROOT / ".forgejo/actions/tilo-registry-login"
WORKFLOWS = sorted((ROOT / ".forgejo/workflows").glob("*.y*ml"))


def jobs(text):
    """Yield (name, body) for each top-level job of a workflow."""
    body = text.split("\njobs:\n", 1)[1]
    parts = re.split(r"(?m)^  ([A-Za-z0-9_-]+):\n", body)
    for index in range(1, len(parts), 2):
        yield parts[index], parts[index + 1]


class LoginScriptTests(unittest.TestCase):
    """login.sh installs a credential helper that fetches an ID token per request."""

    def run_login(self, temp, token_env=True, config=None):
        (temp / "fake").mkdir()
        (temp / "fake/curl").write_text(
            '#!/usr/bin/env bash\nprintf "%s\\n" "$@" >> "$FAKE_DIR/curl-args"\n'
            'printf \'{"value":"id-token-sentinel"}\'\n')
        (temp / "fake/curl").chmod(0o755)
        (temp / "docker").mkdir()
        if config is not None:
            (temp / "docker/config.json").write_text(json.dumps(config))
        env = {key: value for key, value in os.environ.items()
               if not key.startswith("ACTIONS_ID_TOKEN_REQUEST_")}
        env.update(PATH=f"{temp / 'fake'}:{env['PATH']}", FAKE_DIR=str(temp),
                   DOCKER_CONFIG=str(temp / "docker"), RUNNER_TEMP=str(temp / "runner"),
                   GITHUB_PATH=str(temp / "github-path"))
        if token_env:
            env.update(ACTIONS_ID_TOKEN_REQUEST_URL="https://forge.invalid/token?x=1",
                       ACTIONS_ID_TOKEN_REQUEST_TOKEN="request-token-sentinel")
        result = subprocess.run(["bash", str(ACTION / "login.sh")], env=env,
                                capture_output=True, text=True, check=False)
        return result, env

    def test_clients_get_a_fresh_id_token_as_the_password_of_user_oidc(self):
        with tempfile.TemporaryDirectory() as temporary:
            temp = Path(temporary)
            result, env = self.run_login(temp, config={
                "auths": {"registry.tilo.so": {"auth": "c3RhbGU="}, "other.example": {"auth": "eA=="}}})
            self.assertEqual(result.returncode, 0, result.stderr)
            config = json.loads((temp / "docker/config.json").read_text())
            self.assertEqual(config["credHelpers"], {"registry.tilo.so": "tilo-registry"})
            self.assertEqual(list(config["auths"]), ["other.example"])
            helper_dir = Path((temp / "github-path").read_text().strip())
            helper = helper_dir / "docker-credential-tilo-registry"
            for _ in range(2):
                answer = subprocess.run([str(helper), "get"], input="registry.tilo.so", env=env,
                                        capture_output=True, text=True, check=True)
                self.assertEqual(json.loads(answer.stdout), {
                    "ServerURL": "registry.tilo.so", "Username": "oidc", "Secret": "id-token-sentinel"})
            requests = (temp / "curl-args").read_text()
            self.assertEqual(requests.count("https://forge.invalid/token?x=1&audience=registry.tilo.so"), 3)
            self.assertNotIn("id-token-sentinel", result.stdout)

    def test_missing_id_token_endpoint_names_the_job_setting(self):
        with tempfile.TemporaryDirectory() as temporary:
            temp = Path(temporary)
            result, _ = self.run_login(temp, token_env=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("enable-openid-connect: true", result.stderr)
            self.assertFalse((temp / "docker/config.json").exists())


class WorkflowRegistryTests(unittest.TestCase):
    def test_every_login_runs_in_a_job_that_receives_an_id_token(self):
        for workflow in WORKFLOWS:
            for name, body in jobs(workflow.read_text()):
                if "tilo-registry-login" in body:
                    with self.subTest(workflow=workflow.name, job=name):
                        self.assertIn("\n    enable-openid-connect: true\n", body)

    def test_the_login_action_is_checked_out_before_use(self):
        for workflow in WORKFLOWS:
            for name, body in jobs(workflow.read_text()):
                if "tilo-registry-login" not in body:
                    continue
                with self.subTest(workflow=workflow.name, job=name):
                    checkout = body.split("actions/checkout@", 1)
                    self.assertEqual(len(checkout), 2, "no checkout before the login action")
                    sparse = re.search(r"sparse-checkout: *\|?\n?((?:\s+\S.*\n)+?)\s+sparse-checkout-cone-mode",
                                       checkout[1])
                    if sparse:
                        self.assertIn(".forgejo/actions/tilo-registry-login", sparse.group(1))

    def test_registry_tilo_so_never_receives_forge_credentials(self):
        # A Forgejo token is not a registry.tilo.so credential: the token
        # service answers it with 401, which fails even an anonymous pull.
        for workflow in WORKFLOWS:
            text = workflow.read_text()
            with self.subTest(workflow=workflow.name):
                # Job containers come from registry.tilo.so (or a builder
                # resolved there); only a private forge image needs a login.
                for match in re.finditer(r"(?m)^( +)image: (.+)\n\1credentials:", text):
                    self.assertTrue(match.group(2).startswith("code.tilo.so/"), match.group(0))
                self.assertNotRegex(text, r"registry: registry\.tilo\.so")
                self.assertNotRegex(text, r"login registry\.tilo\.so")

    def test_private_board_images_stay_on_the_private_registry(self):
        build = (ROOT / ".forgejo/workflows/build-image.yml").read_text()
        self.assertIn("TILO_OS_REPOSITORY: ${{ needs.plan.outputs.hardware-profile == 'lenovo-83ed' "
                      "&& 'code.tilo.so/tilo/tilo' || 'registry.tilo.so/tilo/tilo' }}", build)
        self.assertIn("echo 'TILO_LAYER_CACHE_REGISTRY=code.tilo.so'", build)
        self.assertNotRegex(build, r"registry\.tilo\.so/tilo/tilo[:@][^ ]*lenovo")
        for path in ROOT.glob(".forgejo/workflows/*.y*ml"):
            with self.subTest(workflow=path.name):
                self.assertNotRegex(path.read_text(), r"registry\.tilo\.so/tilo/[a-z0-9._-]*lenovo")

    def test_kernel_job_keeps_an_exact_shallow_checkout(self):
        workflow = (ROOT / ".forgejo/workflows/build-kernel.yml").read_text()
        build = workflow[workflow.index("  build-and-push-kernel:"):workflow.index("  promote-native-kernels:")]
        self.assertIn("ref: ${{ github.event.pull_request.head.sha || github.sha }}", build)
        self.assertIn("fetch-depth: 1", build)
        self.assertIn("fetch-tags: false", build)
        self.assertNotIn("fetch-depth: 0", build)
        self.assertIn("uses: ./.forgejo/actions/tilo-registry-login", build)
        self.assertIn("docker logout registry.tilo.so", build)


if __name__ == "__main__":
    unittest.main()
