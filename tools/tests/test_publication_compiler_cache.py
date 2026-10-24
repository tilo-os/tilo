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
KERNEL_BUILD = ROOT / "tools/ci/publish/kernel.sh"
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


class KernelPublicationTests(unittest.TestCase):
    """t's image-kernel-<arch> (tools/ci/publish/kernel.sh build) compiles the kernel t signs."""

    def setUp(self):
        self.script = KERNEL_BUILD.read_text()
        self.build = self.script.split("  build)\n", 1)[1].split("\n    ;;", 1)[0]

    def test_published_kernels_never_read_the_cache(self):
        with self.subTest(phase='every kernel build compiles with the cache off'):
            self.assertIn("--build-arg TILO_COMPILER_CACHE=off --build-arg TILO_REQUIRE_COMPILER_CACHE=0)",
                          self.build)
            builds = re.findall(r'^    b build .*', self.build.replace("\\\n", " "), re.M)
            self.assertEqual(len(builds), 2, builds)
            for build in builds:
                self.assertIn('"${args[@]}"', build)
        with self.subTest(phase='populate compiles every object before writing it'):
            text = (ROOT / "os/kernel/Containerfile").read_text()
            self.assertIn("populate:0) export SCCACHE_IGNORE_SERVER_IO_ERROR=1 SCCACHE_RECACHE=1 ;;", text)
        with self.subTest(phase='kernel builds pass no cache credentials'):
            for forbidden in ("--secret", "ACTIONS_CACHE_URL", "ACTIONS_RUNTIME_TOKEN", "SCCACHE_AWS",
                              "SCCACHE_BUCKET", "aws_access_key_id", "compiler_cache_env.sh",
                              "TILO_COMPILER_CACHE=shared", "TILO_COMPILER_CACHE=populate"):
                self.assertNotIn(forbidden, self.script)
        with self.subTest(phase='a published kernel imports no layer cache'):
            self.assertNotIn("--cache-from", self.script)


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
        sign_job = DESKTOP_WORKFLOW.read_text().split("  sign-rpm:\n", 1)[1].split("  publish-os-payload:\n", 1)[0]
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
        text = (ROOT / "os/kernel/Containerfile").read_text()
        self.assertIn("ARG TILO_COMPILER_CACHE=shared", text)
        self.assertRegex(text, r'shared:0\|off:0(\|publication:0)?\)')
        self.assertIn('if [ "$TILO_COMPILER_CACHE" = off ]; then', text)
        # Refuses the runner's cache URL and token.
        self.assertRegex(text, r'\$\{ACTIONS_CACHE_URL\}\$\{ACTIONS_RUNTIME_TOKEN\}')
        # The OS image compiles outside any image build, in os/image/compile.sh.
        compile_sh = (ROOT / "os/image/compile.sh").read_text()
        off = compile_sh.split("if [[ $TILO_COMPILER_CACHE == off ]]; then", 1)[1].split("elif", 1)[0]
        self.assertRegex(off, r'\$\{ACTIONS_CACHE_URL:-\}\$\{ACTIONS_RUNTIME_TOKEN:-\}')
        self.assertIn("RUSTC_WRAPPER:-", off)
        self.assertIn("exit 1", off)
        self.assertIn("export RUSTC_WRAPPER=\n", off)

if __name__ == "__main__":
    unittest.main()
