"""Publications never compile through the shared compiler cache.

Every CI job and agent sandbox holds a credential that can write any object in
the sccache bucket. A signed OS image or kernel compiled through it could carry
planted compiler output, and the independent rebuild would agree with it.
"""
from pathlib import Path
import os
import re
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
IMAGE_WORKFLOW = ROOT / ".forgejo/workflows/build-image.yml"
KERNEL_WORKFLOW = ROOT / ".forgejo/workflows/build-kernel.yml"
CACHE_SETTINGS = (
    "SCCACHE_BUCKET", "SCCACHE_ENDPOINT", "SCCACHE_REGION", "SCCACHE_S3_USE_SSL",
    "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY",
)


def steps(workflow: str) -> dict[str, str]:
    found = {}
    for block in re.split(r"\n(?=      - (?:name|uses): )", workflow):
        match = re.match(r"      - name: (.+)\n", block)
        if match:
            found[match.group(1)] = block
    return found


def run_script(step: str, env: dict[str, str]) -> tuple[dict[str, str], str]:
    script = textwrap.dedent(step.split("        run: |\n", 1)[1])
    with tempfile.TemporaryDirectory() as temporary:
        output = Path(temporary) / "output"
        output.write_text("")
        clean = {"PATH": os.environ["PATH"], "GITHUB_OUTPUT": str(output),
                 "GITHUB_ENV": str(output), **env}
        result = subprocess.run(["bash", "-c", script], env=clean, text=True,
                                capture_output=True, check=False)
        if result.returncode:
            raise AssertionError(result.stderr)
        text = output.read_text()
    values = {}
    lines = iter(text.splitlines())
    for line in lines:
        if "<<" in line:
            name, delimiter = line.split("<<", 1)
            body = []
            for inner in lines:
                if inner == delimiter:
                    break
                body.append(inner)
            values[name] = "\n".join(body)
        else:
            name, _, value = line.partition("=")
            values[name] = value
    return values, text


def runner_cache() -> dict[str, str]:
    return {name: f"runner-{name.lower()}" for name in CACHE_SETTINGS}


class ImagePublicationTests(unittest.TestCase):
    def setUp(self):
        self.workflow = IMAGE_WORKFLOW.read_text()
        self.steps = steps(self.workflow)
        self.bind = self.steps["Bind the runner compiler cache"]

    def test_publication_binds_no_cache_even_when_the_runner_offers_one(self):
        values, text = run_script(self.bind, {"PUBLISH": "true", **runner_cache()})
        self.assertEqual(values["mode"], "off")
        self.assertEqual(values["required"], "0")
        self.assertEqual(values["secret-envs"], "")
        self.assertEqual(values["bucket"], "")
        self.assertEqual(values["endpoint"], "")
        self.assertNotIn("runner-", text)

    def test_integration_builds_keep_the_required_shared_cache(self):
        values, _ = run_script(self.bind, {"PUBLISH": "false", **runner_cache()})
        self.assertEqual(values["mode"], "shared")
        self.assertEqual(values["required"], "1")
        self.assertEqual(values["bucket"], "runner-sccache_bucket")
        self.assertEqual(values["secret-envs"].splitlines(), [
            "aws_access_key_id=AWS_ACCESS_KEY_ID",
            "aws_secret_access_key=AWS_SECRET_ACCESS_KEY",
        ])

    def test_the_bind_follows_the_publication_decision(self):
        self.assertIn("PUBLISH: ${{ steps.release-mode.outputs.publish }}", self.bind)
        order = list(self.steps)
        self.assertLess(order.index("Select build, validation, or publication mode"),
                        order.index("Bind the runner compiler cache"))

    def test_every_image_build_takes_its_cache_mode_and_credentials_from_the_bind(self):
        builds = {name: step for name, step in self.steps.items()
                  if "docker/build-push-action@" in step}
        self.assertGreaterEqual(len(builds), 5)
        for name, step in builds.items():
            with self.subTest(name):
                self.assertNotIn("TILO_REQUIRE_COMPILER_CACHE=1", step)
                self.assertNotIn("aws_access_key_id=AWS_ACCESS_KEY_ID", step)
                if name == "Independently rebuild the release candidate":
                    continue
                self.assertIn("TILO_COMPILER_CACHE=${{ steps.compiler-cache.outputs.mode }}", step)
                self.assertIn("secret-envs: ${{ steps.compiler-cache.outputs.secret-envs }}", step)

    def test_the_independent_rebuild_never_sees_the_cache(self):
        repro = self.steps["Independently rebuild the release candidate"]
        self.assertIn("if: steps.release-mode.outputs.publish == 'true'", repro)
        self.assertIn("TILO_COMPILER_CACHE=off\n", repro)
        self.assertIn("TILO_REQUIRE_COMPILER_CACHE=0\n", repro)
        self.assertIn("no-cache: true", repro)
        for forbidden in ("SCCACHE_", "secret-envs", "AWS_", "aws_", "compiler-cache"):
            self.assertNotIn(forbidden, repro)


class KernelPublicationTests(unittest.TestCase):
    def setUp(self):
        self.workflow = KERNEL_WORKFLOW.read_text()
        self.steps = steps(self.workflow)
        self.bind = self.steps["Bind the runner compiler cache"]

    def test_only_pull_requests_bind_the_shared_cache(self):
        self.assertIn("PULL_REQUEST: ${{ github.event_name == 'pull_request' }}", self.bind)
        values, text = run_script(self.bind, {"PULL_REQUEST": "false", **runner_cache()})
        self.assertEqual(values["KERNEL_COMPILER_CACHE"], "off")
        self.assertEqual(values["KERNEL_REQUIRE_COMPILER_CACHE"], "0")
        self.assertEqual(values["KERNEL_COMPILER_CACHE_SECRETS"], "")
        self.assertNotIn("runner-", text)
        values, _ = run_script(self.bind, {"PULL_REQUEST": "true", **runner_cache()})
        self.assertEqual(values["KERNEL_COMPILER_CACHE"], "shared")
        self.assertEqual(len(values["KERNEL_COMPILER_CACHE_SECRETS"].splitlines()), 2)

    def test_kernel_builds_pass_credentials_only_through_the_bind(self):
        self.assertNotIn("TILO_REQUIRE_COMPILER_CACHE=1", self.workflow)
        build = self.steps["Build the tilo-kernel image (push only on tags/dispatch)"]
        self.assertIn("TILO_COMPILER_CACHE=${{ env.KERNEL_COMPILER_CACHE }}", build)
        self.assertIn("secret-envs: ${{ env.KERNEL_COMPILER_CACHE_SECRETS }}", build)
        probe = self.steps["Prove firmware rejects unsigned EFI and executes the signed image"]
        self.assertIn('"TILO_COMPILER_CACHE=$KERNEL_COMPILER_CACHE"', probe)
        self.assertIn("if [[ $KERNEL_COMPILER_CACHE == shared ]]; then", probe)
        outside = probe.replace(probe[probe.index("cache_secrets=(--secret"):probe.index("          fi\n")], "")
        self.assertNotIn("--secret id=aws_access_key_id", outside)


class ContainerfileTests(unittest.TestCase):
    def test_off_refuses_cache_settings_and_runs_no_wrapper(self):
        for path in ("os/Containerfile", "os/kernel/Containerfile"):
            with self.subTest(path):
                text = (ROOT / path).read_text()
                self.assertIn("ARG TILO_COMPILER_CACHE=shared", text)
                self.assertIn('shared:0|off:0)', text)
                self.assertIn('if [ "$TILO_COMPILER_CACHE" = off ]; then', text)
                self.assertIn('${AWS_ACCESS_KEY_ID}${AWS_SECRET_ACCESS_KEY}${SCCACHE_BUCKET}${SCCACHE_ENDPOINT}', text)
        image = (ROOT / "os/Containerfile").read_text()
        off = image.split('if [ "$TILO_COMPILER_CACHE" = off ]; then', 1)[1].split("elif", 1)[0]
        self.assertIn("export RUSTC_WRAPPER=;", off)


if __name__ == "__main__":
    unittest.main()
