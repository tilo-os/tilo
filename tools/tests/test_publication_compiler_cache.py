"""Which publications compile through the shared compiler cache, and how.

A signed OS image or kernel compiled through a shared cache could carry planted
compiler output unless an independent uncached rebuild must agree with it.
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
    """What the runner gives every job for its compiler cache."""
    return {"ACTIONS_CACHE_URL": "http://runner-cache:8088/run/", "ACTIONS_RUNTIME_TOKEN": "runner-token",
            "SCCACHE_GHA_ENABLED": "on", "RUSTC_WRAPPER": "/ci-tools/sccache"}


class ImagePublicationTests(unittest.TestCase):
    def setUp(self):
        self.workflow = IMAGE_WORKFLOW.read_text()
        self.steps = steps(self.workflow)
        self.bind = self.steps["Bind the runner compiler cache"]

    def test_image_publication_binds_no_cache_while_integration_builds_keep_it(self):
        with self.subTest(phase="the bind passes the runner's Actions cache as secrets"):
            values, _ = run_script(self.bind, {"ACTIONS_CACHE_URL": "http://10.0.0.5:8088/x/",
                                               "ACTIONS_RUNTIME_TOKEN": "tok"})
            self.assertEqual(values["mode"], "shared")
            self.assertEqual(values["secret-envs"].splitlines(), [
                "actions_cache_url=ACTIONS_CACHE_URL",
                "actions_runtime_token=ACTIONS_RUNTIME_TOKEN",
            ])
        with self.subTest(phase='a slot without the cache compiles without it'):
            values, _ = run_script(self.bind, {})
            self.assertEqual(values["mode"], "off")
            self.assertEqual(values["secret-envs"], "")
        with self.subTest(phase='no image build carries S3 cache settings or credentials'):
            for forbidden in ("SCCACHE_BUCKET", "FALLBACK_AWS", "aws_access_key_id", "compiler_cache_env.sh",
                              "TILO_PUBLICATION_CACHE"):
                self.assertNotIn(forbidden, self.workflow)
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
                    self.assertIn("TILO_COMPILER_CACHE=${{ steps.compiler-cache.outputs.mode }}", step)
                    self.assertNotIn("TILO_REQUIRE_COMPILER_CACHE", step)
                    self.assertIn("${{ steps.compiler-cache.outputs.secret-envs }}", step)
                    if name != "Build and cache the compile stages":
                        self.assertIn("browser_runtime_identity=RUNTIME_BUNDLE_SIGNING_KEY", step)
                        self.assertNotIn("browser_app_identity", step)
        with self.subTest(phase='the independent rebuild never sees the cache'):
            repro = self.steps["Independently rebuild the release candidate"]
            self.assertIn("if: steps.release-mode.outputs.publish == 'true'", repro)
            clear = self.steps["Clear the builder for the independent rebuild"]
            self.assertIn('docker buildx prune --builder "$BUILDER_NAME" --all --force', clear)
            self.assertIn("TILO_COMPILER_CACHE=off\n", repro)
            self.assertIn("TILO_REQUIRE_COMPILER_CACHE=0\n", repro)
            self.assertIn("no-cache: true", repro)
            self.assertIn("browser_runtime_identity=RUNTIME_BUNDLE_SIGNING_KEY", repro)
            for forbidden in ("SCCACHE_", "AWS_", "aws_", "compiler-cache"):
                self.assertNotIn(forbidden, repro)


class KernelPublicationTests(unittest.TestCase):
    def setUp(self):
        self.workflow = KERNEL_WORKFLOW.read_text()
        self.steps = steps(self.workflow)
        self.bind = self.steps["Bind the runner compiler cache"]

    def test_published_kernels_never_read_the_cache(self):
        with self.subTest(phase='only pull requests read the shared cache; publications only write it'):
            self.assertIn("PULL_REQUEST: ${{ github.event_name == 'pull_request' }}", self.bind)
            values, _ = run_script(self.bind, {"PULL_REQUEST": "false", **runner_cache()})
            self.assertEqual(values["KERNEL_COMPILER_CACHE"], "populate")
            self.assertEqual(values["KERNEL_REQUIRE_COMPILER_CACHE"], "0")
            values, _ = run_script(self.bind, {"PULL_REQUEST": "true", **runner_cache()})
            self.assertEqual(values["KERNEL_COMPILER_CACHE"], "shared")
            self.assertEqual(values["KERNEL_REQUIRE_COMPILER_CACHE"], "1")
            self.assertEqual(values["KERNEL_COMPILER_CACHE_SECRETS"].splitlines(), [
                "actions_cache_url=ACTIONS_CACHE_URL",
                "actions_runtime_token=ACTIONS_RUNTIME_TOKEN",
            ])
            for pull_request in ("true", "false"):
                values, _ = run_script(self.bind, {"PULL_REQUEST": pull_request})
                self.assertEqual(values["KERNEL_COMPILER_CACHE"], "off")
                self.assertEqual(values["KERNEL_COMPILER_CACHE_SECRETS"], "")
        with self.subTest(phase='populate compiles every object before writing it'):
            text = (ROOT / "os/kernel/Containerfile").read_text()
            self.assertIn("populate:0) export SCCACHE_IGNORE_SERVER_IO_ERROR=1 SCCACHE_RECACHE=1 ;;", text)
        with self.subTest(phase='kernel builds pass credentials only through the bind'):
            self.assertNotIn("TILO_REQUIRE_COMPILER_CACHE=1", self.workflow)
            build = self.steps["Build the tilo-kernel image (push only on tags/dispatch)"]
            self.assertIn("TILO_COMPILER_CACHE=${{ env.KERNEL_COMPILER_CACHE }}", build)
            self.assertIn("secret-envs: ${{ env.KERNEL_COMPILER_CACHE_SECRETS }}", build)
            probe = self.steps["Prove firmware rejects unsigned EFI and executes the signed image"]
            self.assertIn('"TILO_COMPILER_CACHE=$KERNEL_COMPILER_CACHE"', probe)
            self.assertIn("if [[ $KERNEL_COMPILER_CACHE != off ]]; then", probe)
            outside = probe.replace(probe[probe.index("cache_secrets=(--secret"):probe.index("          fi\n")], "")
            self.assertNotIn("--secret id=actions_cache_url", outside)
            for forbidden in ("SCCACHE_AWS", "SCCACHE_BUCKET", "aws_access_key_id", "compiler_cache_env.sh"):
                self.assertNotIn(forbidden, self.workflow)
        with self.subTest(phase='a published kernel imports no layer cache'):
            build = self.steps["Build the tilo-kernel image (push only on tags/dispatch)"]
            self.assertIn(
                "cache-from: ${{ github.event_name == 'pull_request' && "
                "format('type=gha,scope=kernel-{0}', matrix.arch) || '' }}", build)
            self.assertEqual(self.workflow.count("cache-from:"), 1)


class AppCollectionPublicationTests(unittest.TestCase):
    """App collections compile through the runner's Actions cache.

    Pull request jobs write only their own scope, and whoever can write the
    shared scope can already read the signing secrets. What matters is that
    those secrets never reach the compile, where every build script runs.
    """

    def setUp(self):
        self.steps = steps(APPS_WORKFLOW.read_text())

    def test_app_collection_compiles_cached_and_signs_without_compiling(self):
        compile_step = self.steps["Build and verify selected tapps once for this architecture"]
        sign = self.steps["Package and sign selected tapps"]
        with self.subTest(phase='every run prepares the runner cache'):
            prepare = self.steps["Prepare compiler cache"]
            self.assertNotIn("if:", prepare)
            self.assertIn("run: bash tools/ci/sccache.sh prepare", prepare)
        with self.subTest(phase='the compile step holds no signing secret'):
            self.assertNotIn("secrets.", compile_step)
            self.assertIn('TILO_APP_COMPILE_ONLY: "1"', compile_step)
        with self.subTest(phase='signing reuses the compiled graph'):
            self.assertIn("secrets.TAPP_PUBLISHER_IDENTITY", sign)
            self.assertIn("secrets.RUNTIME_BUNDLE_SIGNING_KEY", sign)
            self.assertIn("unset TAPP_PUBLISHER_IDENTITY RUNTIME_BUNDLE_SIGNING_KEY", sign)
            self.assertNotIn("TILO_APP_COMPILE_ONLY", sign)
            order = list(self.steps)
            self.assertLess(order.index("Build and verify selected tapps once for this architecture"),
                            order.index("Package and sign selected tapps"))
        with self.subTest(phase='compile-only stops before any identity is needed'):
            script = (ROOT / "dist/apps/package-apps.sh").read_text()
            stop = script.index("[[ ${TILO_APP_COMPILE_ONLY:-} != 1 ]] || exit 0")
            self.assertLess(script.index("build-thin-apps.sh\"\n"), stop)
            self.assertLess(stop, script.index("key generate"))
            self.assertLess(stop, script.index("RUNTIME_RELEASE="))


class DesktopPayloadPublicationTests(unittest.TestCase):
    def test_only_an_unpublishable_branch_dispatch_uses_the_cache(self):
        workflow_steps = steps(DESKTOP_WORKFLOW.read_text())
        for name in ("Build and install-test unsigned candidate userspaces",):
            with self.subTest(step=name):
                build = workflow_steps[name]
                self.assertIn(
                    "TILO_ENABLE_REMOTE_CACHE: ${{ github.event_name == 'workflow_dispatch' && "
                    "github.ref != 'refs/heads/main' && !startsWith(github.ref, 'refs/tags/') "
                    "&& '1' || '0' }}", build)
        # One unchanged cache predicate now covers both tag and candidate builds.
        # Signing has a separate job and cannot start a second compilation graph.
        build_job = DESKTOP_WORKFLOW.read_text().split("  package:\n", 1)[1].split("  sign-rpm:\n", 1)[0]
        sign_job = DESKTOP_WORKFLOW.read_text().split("  sign-rpm:\n", 1)[1].split("  host-session-e2e:\n", 1)[0]
        self.assertNotIn("secrets.TILO_RPM_SIGNING", build_job)
        self.assertNotIn("run-package-job.sh", sign_job)
        self.assertNotIn("TILO_ENABLE_REMOTE_CACHE", sign_job)
        job = (ROOT / "dist/ci/run-package-job.sh").read_text()
        disabled = job.split("if [[ ${TILO_ENABLE_REMOTE_CACHE:-0} == 1 ]]; then", 1)[1]
        disabled = disabled.split("    else\n", 1)[1].split("    fi\n", 1)[0]
        self.assertNotIn("SCCACHE_AWS", DESKTOP_WORKFLOW.read_text())
        self.assertIn('unset "$name"', disabled)
        self.assertIn("grep -E '^(AWS_|SCCACHE_|ACTIONS_CACHE_URL$|ACTIONS_RUNTIME_TOKEN$|RUSTC_WRAPPER$)'", disabled)


class ContainerfileTests(unittest.TestCase):
    def test_off_refuses_cache_settings_and_runs_no_wrapper(self):
        for path in ("os/Containerfile", "os/kernel/Containerfile"):
            with self.subTest(path):
                text = (ROOT / path).read_text()
                self.assertIn("ARG TILO_COMPILER_CACHE=shared", text)
                self.assertRegex(text, r'shared:0\|off:0(\|publication:0)?\)')
                self.assertIn('if [ "$TILO_COMPILER_CACHE" = off ]; then', text)
                # Refuses the runner's cache URL and token.
                self.assertRegex(text, r'\$\{ACTIONS_CACHE_URL\}\$\{ACTIONS_RUNTIME_TOKEN\}')
        image = (ROOT / "os/Containerfile").read_text()
        off = image.split('if [ "$TILO_COMPILER_CACHE" = off ]; then', 1)[1].split("elif", 1)[0]
        self.assertIn("export RUSTC_WRAPPER=;", off)


if __name__ == "__main__":
    unittest.main()
