"""Which publications compile through a compiler cache, and how.

A signed OS image or kernel compiled through a shared cache could carry planted
compiler output unless an independent uncached rebuild must agree with it. The
kernel's cache is not shared: a check of the same run compiles it from empty.
"""
from pathlib import Path
import os
import re
import subprocess
import tempfile
import textwrap
import tomllib
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
    """t's image-kernel-<arch> (tools/ci/publish/kernel.sh build) compiles the kernel t signs.

    It compiles through one compiler cache only: the sccache directory image-kernel-cache-<arch>
    (kernel.sh compile-cache) filled from empty, handed over as that check's output. In a trusted
    run t serves both only from trusted runs, so nothing an untrusted build wrote reaches a signed
    kernel, and no remote cache, credential or other job's writes reach either.
    """

    def setUp(self):
        self.script = KERNEL_BUILD.read_text()
        self.build = self.script.split("  build)\n", 1)[1].split("\n    ;;", 1)[0]
        self.compile_cache = self.script.split("  compile-cache)\n", 1)[1].split("\n    ;;", 1)[0]
        checks = tomllib.loads((ROOT / "t.toml").read_text())["check"]
        self.checks = {c["name"]: c for c in checks}

    def test_kernel_builds_read_only_the_cache_their_run_compiled(self):
        with self.subTest(phase='the build reads the cache only as image-kernel-cache gives it'):
            kernel = self.checks["image-kernel-{arch}"]
            self.assertIn("image-kernel-cache-{arch}", kernel["needs"])
            self.assertTrue(kernel["run"].endswith(
                " /target/publish/kernel-cache/{arch}/compiler-cache.tar"), kernel["run"])
            self.assertIn('args+=(--build-arg TILO_COMPILER_CACHE=local --volume "$cache:/work/compiler-cache")',
                          self.build)
            self.assertIn("args+=(--build-arg TILO_COMPILER_CACHE=off)", self.build)
        with self.subTest(phase='the cache is compiled from empty by a check that takes no other'):
            cache = self.checks["image-kernel-cache-{arch}"]
            self.assertEqual(cache["run"],
                             "bash tools/ci/publish/kernel.sh compile-cache {arch} /target/publish/kernel-cache/{arch}")
            self.assertEqual(set(cache["needs"]), {"os-assets", "signing-tools"})
            for field in ("secrets", "id_token", "reads_pointers", "env"):
                self.assertNotIn(field, cache)
            self.assertIn('rm -rf "$cache" && mkdir -p "$cache"', self.compile_cache)
        with self.subTest(phase='the cache outlives patch and configuration changes, not the toolchain'):
            inputs = self.checks["image-kernel-cache-{arch}"]["inputs"]
            for kept in ("os/kernel-buildroot-snapshot.env", "os/kernel/cc-wrapper.sh",
                         "tools/ci/publish/kernel.sh"):
                self.assertIn(kept, inputs)
            self.assertFalse([i for i in inputs if "patches" in i or "config" in i or i == "os/kernel/**"],
                             inputs)
        with self.subTest(phase='no remote cache or credential reaches a kernel build'):
            for path in ("tools/ci/publish/kernel.sh", "os/kernel/Containerfile", "os/kernel/build-support.sh"):
                text = (ROOT / path).read_text()
                for forbidden in ("--secret", "ACTIONS_CACHE_URL", "ACTIONS_RUNTIME_TOKEN", "SCCACHE_AWS",
                                  "SCCACHE_BUCKET", "SCCACHE_GHA", "SCCACHE_REDIS", "SCCACHE_WEBDAV",
                                  "SCCACHE_MEMCACHED", "SCCACHE_GCS", "SCCACHE_AZURE", "aws_access_key_id",
                                  "compiler_cache_env.sh", "--cache-from"):
                    self.assertNotIn(forbidden, text, path)
        with self.subTest(phase='both share one compile stage and its arguments'):
            joined = (self.compile_cache + "\n" + self.build).replace("\\\n", " ")
            builds = re.findall(r'^    +b build .*', joined, re.M)
            self.assertEqual(len(builds), 5, builds)
            for build in builds:
                if "--target publish-cross" not in build:
                    self.assertIn('"${args[@]}"', build)


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
    def test_off_runs_no_wrapper_and_local_reads_only_its_directory(self):
        text = (ROOT / "os/kernel/Containerfile").read_text()
        self.assertIn("ARG TILO_COMPILER_CACHE=off", text)
        self.assertIn('off) echo "compiling the kernel without a compiler cache" >&2 ;;', text)
        self.assertIn("local) export TILO_KERNEL_COMPILER_CACHE=/work/compiler-cache ;;", text)
        self.assertIn('*) echo "TILO_COMPILER_CACHE must be off or local" >&2; exit 1 ;;', text)
        helper = (ROOT / "os/kernel/build-support.sh").read_text()
        # sccache's only storage is the directory it was given.
        self.assertIn('export SCCACHE_DIR="$cache"', helper)
        self.assertEqual(sorted(set(re.findall(r"SCCACHE_[A-Z_]+", helper))),
                         ["SCCACHE_CACHE_SIZE", "SCCACHE_DIR", "SCCACHE_DIRECT", "SCCACHE_IDLE_TIMEOUT"])
        # The OS image compiles outside any image build, in os/image/compile.sh.
        compile_sh = (ROOT / "os/image/compile.sh").read_text()
        off = compile_sh.split("if [[ $TILO_COMPILER_CACHE == off ]]; then", 1)[1].split("elif", 1)[0]
        self.assertRegex(off, r'\$\{ACTIONS_CACHE_URL:-\}\$\{ACTIONS_RUNTIME_TOKEN:-\}')
        self.assertIn("RUSTC_WRAPPER:-", off)
        self.assertIn("exit 1", off)
        self.assertIn("export RUSTC_WRAPPER=\n", off)

if __name__ == "__main__":
    unittest.main()
