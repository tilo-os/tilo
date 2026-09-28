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
APPS_WORKFLOW = ROOT / ".forgejo/workflows/release-apps.yml"
DESKTOP_WORKFLOW = ROOT / ".forgejo/workflows/release-desktop.yml"
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


def run_script(step: str, env: dict[str, str], files: dict[str, str] | None = None) -> tuple[dict[str, str], str]:
    script = textwrap.dedent(step.split("        run: |\n", 1)[1])
    with tempfile.TemporaryDirectory() as temporary:
        output = Path(temporary) / "output"
        output.write_text("")
        for name, body in (files or {}).items():
            path = Path(temporary) / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(body)
            path.chmod(0o755)
        selector = ROOT / "tools/ci/compiler_cache_env.sh"
        destination = Path(temporary) / "tools/ci/compiler_cache_env.sh"
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(selector.read_text())
        destination.chmod(0o755)
        clean = {"PATH": os.environ["PATH"], "GITHUB_OUTPUT": str(output),
                 "GITHUB_ENV": str(output), **env}
        result = subprocess.run(["bash", "-c", script], env=clean, text=True, cwd=temporary,
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

    def test_image_publication_binds_no_cache_while_integration_builds_keep_it(self):
        with self.subTest(phase='publication compiles through the shared cache without requiring it'):
            values, _ = run_script(self.bind, {"PUBLISH": "true", **runner_cache()})
            self.assertEqual(values["mode"], "shared")
            self.assertEqual(values["required"], "0")
            self.assertEqual(values["bucket"], "runner-sccache_bucket")
            self.assertEqual(values["secret-envs"].splitlines(), [
                "aws_access_key_id=AWS_ACCESS_KEY_ID",
                "aws_secret_access_key=AWS_SECRET_ACCESS_KEY",
            ])
        with self.subTest(phase='publication uses only its own role scoped cache'):
            fake = ('#!/bin/sh\n[ "$1" = arn:aws:iam::123456789012:role/pub ] || exit 9\n'
                    'echo \'{"AccessKeyId":"ASIAPUB","SecretAccessKey":"s3cr3t","SessionToken":"tok"}\'\n')
            values, text = run_script(self.bind, {
                "PUBLISH": "true", **runner_cache(),
                "PUBLICATION_CACHE_ROLE_ARN": "arn:aws:iam::123456789012:role/pub",
                "PUBLICATION_CACHE_BUCKET": "tilo-publication-sccache-123456789012-us-west-2",
            }, files={"os/dev/publication-cache-credentials.sh": fake})
            self.assertEqual(values["mode"], "publication")
            self.assertEqual(values["bucket"], "tilo-publication-sccache-123456789012-us-west-2")
            self.assertEqual(values["region"], "us-west-2")
            self.assertEqual(values["endpoint"], "https://s3.us-west-2.amazonaws.com")
            self.assertEqual(values["prefix"], "tilo/publication/v1")
            self.assertEqual(values["secret-envs"].splitlines(), [
                "aws_access_key_id=TILO_PUBLICATION_AWS_ACCESS_KEY_ID",
                "aws_secret_access_key=TILO_PUBLICATION_AWS_SECRET_ACCESS_KEY",
                "aws_session_token=TILO_PUBLICATION_AWS_SESSION_TOKEN",
            ])
            self.assertEqual(values["TILO_PUBLICATION_AWS_ACCESS_KEY_ID"], "ASIAPUB")
            # Nothing of the runner's shared cache reaches a publication.
            self.assertNotIn("runner-", text)
        with self.subTest(phase='publication credentials come from the oidc role alone'):
            script = (ROOT / "os/dev/publication-cache-credentials.sh").read_text()
            self.assertIn("audience=tilo-publication-cache", script)
            self.assertIn("Action=AssumeRoleWithWebIdentity", script)
            self.assertNotIn("AWS_ACCESS_KEY_ID", script)
            self.assertNotIn("SCCACHE_", script)
        with self.subTest(phase='integration builds keep the required shared cache'):
            values, _ = run_script(self.bind, {"PUBLISH": "false", **runner_cache()})
            self.assertEqual(values["mode"], "shared")
            self.assertEqual(values["required"], "1")
            self.assertEqual(values["bucket"], "runner-sccache_bucket")
            self.assertEqual(values["secret-envs"].splitlines(), [
                "aws_access_key_id=AWS_ACCESS_KEY_ID",
                "aws_secret_access_key=AWS_SECRET_ACCESS_KEY",
            ])
        with self.subTest(phase='the bind follows the publication decision'):
            self.assertIn("PUBLISH: ${{ steps.release-mode.outputs.publish }}", self.bind)
            order = list(self.steps)
            self.assertLess(order.index("Select build, validation, or publication mode"),
                            order.index("Bind the runner compiler cache"))
        with self.subTest(phase='every image build takes its cache mode and credentials from the bind'):
            builds = {name: step for name, step in self.steps.items()
                      if "docker/build-push-action@" in step}
            self.assertGreaterEqual(len(builds), 5)
            for name, step in builds.items():
                with self.subTest(name):
                    self.assertNotIn("TILO_REQUIRE_COMPILER_CACHE=1", step)
                    self.assertNotIn("aws_access_key_id=AWS_ACCESS_KEY_ID", step)
                    if name == "Independently rebuild the release candidate":
                        continue
                    if "file: os/Containerfile.wpe" in step:
                        # The engine recipe compiles no Rust and takes no cache settings.
                        for forbidden in ("SCCACHE_", "AWS_", "aws_", "compiler-cache"):
                            self.assertNotIn(forbidden, step)
                        continue
                    self.assertIn("TILO_COMPILER_CACHE=${{ steps.compiler-cache.outputs.mode }}", step)
                    self.assertIn("${{ steps.compiler-cache.outputs.secret-envs }}", step)
                    if name != "Build and cache the compile stages":
                        self.assertIn("browser_runtime_identity=RUNTIME_BUNDLE_SIGNING_KEY", step)
                        self.assertIn("browser_app_identity=TAPP_PUBLISHER_IDENTITY", step)
        with self.subTest(phase='the independent rebuild never sees the cache'):
            repro = self.steps["Independently rebuild the release candidate"]
            self.assertIn("if: matrix.role == 'reproduce'", repro)
            self.assertIn("TILO_COMPILER_CACHE=off\n", repro)
            self.assertIn("TILO_REQUIRE_COMPILER_CACHE=0\n", repro)
            self.assertIn("no-cache: true", repro)
            self.assertIn("browser_runtime_identity=RUNTIME_BUNDLE_SIGNING_KEY", repro)
            self.assertIn("browser_app_identity=TAPP_PUBLISHER_IDENTITY", repro)
            for forbidden in ("SCCACHE_", "AWS_", "aws_", "compiler-cache"):
                self.assertNotIn(forbidden, repro)


class KernelPublicationTests(unittest.TestCase):
    def setUp(self):
        self.workflow = KERNEL_WORKFLOW.read_text()
        self.steps = steps(self.workflow)
        self.bind = self.steps["Bind the runner compiler cache"]

    def test_kernel_builds_use_the_cache_only_for_pull_requests_through_the_bind(self):
        with self.subTest(phase='only pull requests bind the shared cache'):
            self.assertIn("PULL_REQUEST: ${{ github.event_name == 'pull_request' }}", self.bind)
            values, text = run_script(self.bind, {"PULL_REQUEST": "false", **runner_cache()})
            self.assertEqual(values["KERNEL_COMPILER_CACHE"], "off")
            self.assertEqual(values["KERNEL_REQUIRE_COMPILER_CACHE"], "0")
            self.assertEqual(values["KERNEL_COMPILER_CACHE_SECRETS"], "")
            self.assertNotIn("runner-", text)
            values, _ = run_script(self.bind, {"PULL_REQUEST": "true", **runner_cache()})
            self.assertEqual(values["KERNEL_COMPILER_CACHE"], "shared")
            self.assertEqual(len(values["KERNEL_COMPILER_CACHE_SECRETS"].splitlines()), 2)
        with self.subTest(phase='kernel builds pass credentials only through the bind'):
            self.assertNotIn("TILO_REQUIRE_COMPILER_CACHE=1", self.workflow)
            build = self.steps["Build the tilo-kernel image (push only on tags/dispatch)"]
            self.assertIn("TILO_COMPILER_CACHE=${{ env.KERNEL_COMPILER_CACHE }}", build)
            self.assertIn("secret-envs: ${{ env.KERNEL_COMPILER_CACHE_SECRETS }}", build)
            probe = self.steps["Prove firmware rejects unsigned EFI and executes the signed image"]
            self.assertIn('"TILO_COMPILER_CACHE=$KERNEL_COMPILER_CACHE"', probe)
            self.assertIn("if [[ $KERNEL_COMPILER_CACHE == shared ]]; then", probe)
            outside = probe.replace(probe[probe.index("cache_secrets=(--secret"):probe.index("          fi\n")], "")
            self.assertNotIn("--secret id=aws_access_key_id", outside)
        with self.subTest(phase='a published kernel imports no layer cache'):
            build = self.steps["Build the tilo-kernel image (push only on tags/dispatch)"]
            self.assertIn(
                "cache-from: ${{ github.event_name == 'pull_request' && format('type=registry,"
                "ref={0}/tilo/tilo-kernel-buildcache:{1}', env.TILO_LAYER_CACHE_REGISTRY, matrix.arch)"
                " || '' }}", build)
            self.assertEqual(self.workflow.count("cache-from:"), 1)


class AppCollectionPublicationTests(unittest.TestCase):
    def setUp(self):
        self.steps = steps(APPS_WORKFLOW.read_text())

    def test_app_collection_publication_compiles_without_the_shared_cache(self):
        with self.subTest(phase='only unpublished runs prepare the shared cache'):
            prepare = self.steps["Prepare compiler cache"]
            self.assertIn("if: needs.plan.outputs.publish != 'true'", prepare)
            self.assertIn("bash tools/ci/sccache.sh prepare", prepare)
            self.assertIn("source tools/ci/compiler_cache_env.sh", prepare)
        with self.subTest(phase='a publication compiles without cache credentials or wrapper'):
            build = self.steps["Build and verify selected tapps once for this architecture"]
            script = textwrap.dedent(build.split("        run: |\n", 1)[1])
            guard = script.split("if [[ -n ${TAPP_PUBLISHER_IDENTITY:-} ]]; then", 1)[0]
            probe = guard.replace("${{ needs.plan.outputs.publish }}", "true") + (
                'env | grep -E "^(AWS_|SCCACHE_)" && exit 1\n'
                '[[ -z ${RUSTC_WRAPPER:-} ]] || exit 1\n')
            env = {"PATH": os.environ["PATH"], "RUSTC_WRAPPER": "/usr/local/bin/sccache",
                   "SCCACHE_S3_KEY_PREFIX": "tilo/apps", **runner_cache()}
            result = subprocess.run(["bash", "-c", probe], env=env, text=True,
                                    capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertLess(script.index(guard), script.index("dist/apps/package-apps.sh"))


class DesktopPayloadPublicationTests(unittest.TestCase):
    def test_only_an_unpublishable_branch_dispatch_uses_the_cache(self):
        build = steps(DESKTOP_WORKFLOW.read_text())["Build and install-test all five target userspaces"]
        self.assertIn(
            "TILO_ENABLE_REMOTE_CACHE: ${{ github.event_name == 'workflow_dispatch' && "
            "github.ref != 'refs/heads/main' && !startsWith(github.ref, 'refs/tags/') "
            "&& '1' || '0' }}", build)
        job = (ROOT / "dist/ci/run-package-job.sh").read_text()
        disabled = job.split("if [[ ${TILO_ENABLE_REMOTE_CACHE:-0} == 1 ]]; then", 1)[1]
        disabled = disabled.split("    else\n", 1)[1].split("    fi\n", 1)[0]
        self.assertIn('unset "$name"', disabled)
        self.assertIn("grep -E '^(AWS_|SCCACHE_)'", disabled)


class ContainerfileTests(unittest.TestCase):
    def test_off_refuses_cache_settings_and_runs_no_wrapper(self):
        for path in ("os/Containerfile", "os/kernel/Containerfile"):
            with self.subTest(path):
                text = (ROOT / path).read_text()
                self.assertIn("ARG TILO_COMPILER_CACHE=shared", text)
                self.assertRegex(text, r'shared:0\|off:0(\|publication:0)?\)')
                self.assertIn('if [ "$TILO_COMPILER_CACHE" = off ]; then', text)
                self.assertIn('${AWS_ACCESS_KEY_ID}${AWS_SECRET_ACCESS_KEY}${SCCACHE_BUCKET}${SCCACHE_ENDPOINT}', text)
        image = (ROOT / "os/Containerfile").read_text()
        off = image.split('if [ "$TILO_COMPILER_CACHE" = off ]; then', 1)[1].split("elif", 1)[0]
        self.assertIn("export RUSTC_WRAPPER=;", off)


if __name__ == "__main__":
    unittest.main()
