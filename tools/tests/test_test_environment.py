"""tools/with-test-environment.sh gives every test run the same isolated environment."""
from __future__ import annotations

from collections import Counter
import json
import re
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/with-test-environment.sh"
RUNNER = ROOT / "tools/cargo-runner.sh"
HELPER = ROOT / "tools/desktop-isolation.sh"
SETUP = ROOT / "tools/test-env-setup.sh"
TEST_ENV = ROOT / "tools/test-env.sh"
DUMP = "import json, os; print(json.dumps(dict(os.environ)))"
# Programs a wrapper could start to decide or to prepare something.
RECORDED = ("bash", "sh", "env", "dirname", "basename", "id", "uname", "awk", "sed", "sort",
            "cut", "getent", "stat", "find", "mktemp", "mkdir", "chmod", "chown", "rm",
            "setpriv", "flock")


def recorders(directory: Path, *more: str) -> tuple[Path, Path]:
    """A PATH entry whose programs each note their name in a file and then run the real one:
    RECORDED's, and any `more`."""
    programs, started = directory / "recorders", directory / "started"
    programs.mkdir()
    started.touch()
    for name in (*RECORDED, *more):
        real = shutil.which(name)
        if real is not None:
            (programs / name).write_text(f'#!/bin/sh\necho {name} >>"{started}"\nexec "{real}" "$@"\n')
            (programs / name).chmod(0o755)
    return programs, started


def old_bash() -> bool:
    """Whether bash cannot tell its own variables from the environment's (before 5)."""
    major = subprocess.run(["bash", "-c", "echo ${BASH_VERSINFO[0]}"], capture_output=True,
                           text=True, check=True).stdout
    return int(major) < 5


class TestEnvironmentTests(unittest.TestCase):
    def run_wrapped(self, *command: str, **extra: str) -> subprocess.CompletedProcess[str]:
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": "/nonexistent-developer-home",
            "LANG": "de_DE.UTF-8",
            "LC_ALL": "de_DE.UTF-8",
            "TZ": "Europe/Berlin",
            "DBUS_SESSION_BUS_ADDRESS": "unix:path=/run/user/1000/bus",
            "WAYLAND_DISPLAY": "wayland-0",
            "NOTIFY_SOCKET": "/run/user/1000/systemd/notify",
            "XDG_DATA_DIRS": "/home/me/.local/share/flatpak/exports/share",
            **extra,
        }
        return subprocess.run(["bash", str(SCRIPT), *command], env=env,
                              capture_output=True, text=True, check=False)

    def test_locale_timezone_home_and_session_are_replaced(self) -> None:
        result = self.run_wrapped("python3", "-c", DUMP)
        self.assertEqual(result.returncode, 0, result.stderr)
        env = json.loads(result.stdout)
        self.assertEqual(env["LANG"], "C.UTF-8")
        self.assertEqual(env["TZ"], "UTC")
        for name in ("LC_ALL", "WAYLAND_DISPLAY", "XDG_DATA_DIRS", "NOTIFY_SOCKET"):
            self.assertNotIn(name, env)
        root = Path(env["HOME"]).parent
        self.assertTrue(str(root).startswith("/tmp/tilo-test-env."))
        self.assertEqual(env["TILO_TEST_ENVIRONMENT"], str(root))
        # Both buses point at a socket nothing listens on.
        self.assertEqual(env["DBUS_SESSION_BUS_ADDRESS"], f"unix:path={root}/no-session-bus")
        self.assertEqual(env["DBUS_SYSTEM_BUS_ADDRESS"], f"unix:path={root}/no-system-bus")
        for name, leaf in (("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
                           ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache"),
                           ("XDG_RUNTIME_DIR", "runtime")):
            self.assertEqual(Path(env[name]), root / leaf)
        # The toolchain stays where the caller's HOME put it.
        self.assertEqual(env["CARGO_HOME"], "/nonexistent-developer-home/.cargo")
        self.assertEqual(env["RUSTUP_HOME"], "/nonexistent-developer-home/.rustup")
        self.assertFalse(root.exists(), "the temporary tree is removed afterwards")

    def test_explicit_toolchain_homes_are_kept(self) -> None:
        result = self.run_wrapped("python3", "-c", DUMP,
                                  CARGO_HOME="/cargo-home", RUSTUP_HOME="/usr/local/rustup")
        env = json.loads(result.stdout)
        self.assertEqual(env["CARGO_HOME"], "/cargo-home")
        self.assertEqual(env["RUSTUP_HOME"], "/usr/local/rustup")

    def test_exit_status_passes_through_and_read_only_trees_are_removed(self) -> None:
        result = self.run_wrapped(
            "sh", "-c", 'mkdir "$HOME/ro" && touch "$HOME/ro/f" && chmod 500 "$HOME/ro" '
            '&& echo "$HOME" && exit 7')
        self.assertEqual(result.returncode, 7)
        self.assertFalse(Path(result.stdout.strip()).parent.exists())

    def test_runtime_directory_is_private(self) -> None:
        result = self.run_wrapped("sh", "-c", 'stat -c %a "$XDG_RUNTIME_DIR"')
        self.assertEqual(result.stdout.strip(), "700")

    def test_an_intact_test_environment_is_kept(self) -> None:
        inner = f"bash {SCRIPT} sh -c 'echo \"$HOME $PULSE_SERVER\"'"
        result = self.run_wrapped(
            "sh", "-c", f'export PULSE_SERVER=unix:$XDG_RUNTIME_DIR/audio; echo "$HOME"; {inner}')
        self.assertEqual(result.returncode, 0, result.stderr)
        outer, inner_line = result.stdout.splitlines()
        home, pulse = inner_line.split(" ")
        self.assertEqual(home, outer)
        self.assertTrue(pulse.startswith("unix:/tmp/tilo-test-env."))

    def test_a_broken_test_environment_is_replaced(self) -> None:
        inner = f"WAYLAND_DISPLAY=wayland-0 bash {SCRIPT} sh -c 'echo \"$HOME ${{WAYLAND_DISPLAY:-none}}\"'"
        result = self.run_wrapped("sh", "-c", f'echo "$HOME"; {inner}')
        self.assertEqual(result.returncode, 0, result.stderr)
        outer, inner_line = result.stdout.splitlines()
        home, display = inner_line.split(" ")
        self.assertNotEqual(home, outer)
        self.assertEqual(display, "none")

    def test_the_runs_private_audio_endpoint_is_kept_and_a_desktops_is_not(self) -> None:
        private = {"TILO_HEADLESS_AUDIO_PID": "1234", "PULSE_RUNTIME_PATH": "/tmp/tilo-audio.x",
                   "PULSE_SERVER": "unix:/tmp/tilo-audio.x/native"}
        env = json.loads(self.run_wrapped("python3", "-c", DUMP, **private).stdout)
        self.assertEqual(env["PULSE_SERVER"], "unix:/tmp/tilo-audio.x/native")
        for desktop in ({"PULSE_SERVER": "unix:/run/user/1000/pulse/native"},
                        {**private, "PULSE_SERVER": "unix:/run/user/1000/pulse/native"}):
            env = json.loads(self.run_wrapped("python3", "-c", DUMP, **desktop).stdout)
            self.assertNotIn("PULSE_SERVER", env)

    def test_the_kernel_is_not_what_the_environment_says_it_is(self) -> None:
        # bash's OSTYPE answers without a `uname`, but only bash's own: an environment that
        # brings one names another machine's kernel, and would take lavapipe from a Linux run
        # or leave a macOS run with the login Keychain.
        linux = os.uname().sysname == "Linux"
        result = self.run_wrapped("python3", "-c", DUMP,
                                  OSTYPE="darwin25" if linux else "linux-gnu")
        self.assertEqual(result.returncode, 0, result.stderr)
        env = json.loads(result.stdout)
        if linux:
            self.assertIn("VK_DRIVER_FILES", env)
            self.assertNotIn("TILO_IDENTITY_FILE_SECRETS", env)
        else:
            self.assertEqual(env["TILO_IDENTITY_FILE_SECRETS"], "1")
            self.assertNotIn("VK_DRIVER_FILES", env)


class TestEnvironmentSetupTests(unittest.TestCase):
    """tools/test-env-setup.sh: the steps nextest's ci profile and tools/test-env.sh run."""

    def setup(self, *steps: str, **extra: str) -> tuple[subprocess.CompletedProcess[str], dict[str, str]]:
        with tempfile.TemporaryDirectory() as directory:
            env_file = Path(directory) / "env"
            env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": directory,
                   "NEXTEST_ENV": str(env_file), **extra}
            result = subprocess.run(["bash", str(SETUP), *steps], env=env,
                                    capture_output=True, text=True, check=False)
            lines = env_file.read_text().splitlines() if env_file.exists() else []
            return result, dict(line.split("=", 1) for line in lines)

    def test_the_ci_profile_runs_the_setup_where_its_tests_run(self) -> None:
        config = tomllib.loads((ROOT / ".config/nextest.toml").read_text())
        self.assertIn("setup-scripts", config["experimental"])
        scripts = dict(config["scripts"]["setup"])
        # The session's sibling binaries are built before its tests in every
        # profile: a test that started Cargo itself would build the workspace
        # again inside the run.
        siblings = scripts.pop("session-siblings")
        self.assertEqual(siblings["command"][:2], ["bash", "tools/build-sibling-binaries.sh"])
        self.assertEqual(siblings["command"][2:], [
            "tilo-lock-screen:tilo-lock-screen", "osk:osk",
            "tilolang-process:tilolang-process", "launcher:tilo-launcher",
        ])
        for profile in ("default", "ci"):
            rules = [rule for rule in config["profile"][profile]["scripts"]
                     if rule["setup"] == "session-siblings"]
            expected = ([("package(tilo-session)",
                         {"host": 'cfg(target_os = "linux")'})]
                        if profile == "default" else [])
            self.assertEqual(
                [(rule["filter"], rule["platform"]) for rule in rules],
                expected,
                f"the {profile} profile builds the session's siblings for its tests",
            )
        # A branched device's vault and provider keys are read through the apps
        # that keep them, so the run that drives it builds those two and the
        # launcher a desktop starts them from first.
        vault = scripts.pop("vault-siblings")
        self.assertEqual(vault["command"], ["bash", "tools/build-sibling-binaries.sh",
                                            "passwords:passwords",
                                            "launcher:tilo-launcher"])
        for profile in ("default", "ci"):
            rules = [rule for rule in config["profile"][profile]["scripts"]
                     if rule["setup"] == "vault-siblings"]
            self.assertEqual(
                [(rule["filter"], rule["platform"]) for rule in rules],
                [("package(tilo-session) and binary(it) and test(device_branch::)",
                  {"host": 'cfg(target_os = "linux")'})],
                f"the {profile} profile builds the vault apps the branched device's tests read",
            )
        # The lounge's end-to-end tests start a persistent server workspace, a program
        # of another package that only its `agents` feature builds: the
        # session's workspace-service test and the browser peer's fresh-page test.
        lounge = scripts.pop("lounge-siblings")
        self.assertEqual(lounge["command"], ["bash", "tools/build-sibling-binaries.sh",
                                             "tilo-server:tilo-server=tilo-server/agents"])
        for profile in ("default", "ci"):
            rules = [rule for rule in config["profile"][profile]["scripts"]
                     if rule["setup"] == "lounge-siblings"]
            self.assertEqual(
                [(rule["filter"], rule["platform"]) for rule in rules],
                [("package(tilo-session) and binary(it) and test(lounge_e2e::)",
                  {"host": 'cfg(target_os = "linux")'}),
                 ("package(tilo-session-client) and binary(it) and test(lounge_fresh_page::)",
                  {"host": 'cfg(target_os = "linux")'})],
                f"the {profile} profile builds the server for the lounge's tests",
            )
        # The browser peer's page tests drive the page's modules beside a
        # runtime module the harness builds once, before they start.
        page = scripts.pop("page-bundle")
        self.assertEqual(page["command"], ["bash", "tools/build-page-bundle.sh"])
        for profile in ("default", "ci"):
            rules = [rule for rule in config["profile"][profile]["scripts"]
                     if rule["setup"] == "page-bundle"]
            self.assertEqual(
                [(rule["filter"], rule["platform"]) for rule in rules],
                [("package(tilo-session-client) and binary(it) and test(node_workspace::)",
                  {"host": 'cfg(target_os = "linux")'})],
                f"the {profile} profile builds the page bundle its page tests run against",
            )
        # pam-tilo's tests load the module as libpam does, and no test binary links a cdylib:
        # the run's setup builds it first, in every profile, and hands the tests its path.
        module = scripts.pop("pam-module")
        self.assertEqual(module["command"], ["bash", "tools/test-env-setup.sh", "pam-module"])
        def rules(profile: str, setup: str) -> list[dict]:
            return [rule for rule in config["profile"][profile]["scripts"]
                    if rule["setup"] == setup]
        (pam,) = rules("default", "pam-module")
        self.assertEqual(pam["filter"], "package(pam-tilo)")
        (siblings_rule,) = rules("default", "session-siblings")
        self.assertEqual(pam["platform"], siblings_rule["platform"],
                         "the PAM module is built on the platform its sibling setups are")
        self.assertEqual(rules("ci", "pam-module"), [],
                         "ci inherits the default profile's PAM module")
        # The native NTFS images are generated once for the tests that read them; a test
        # started without them fails saying so, so every profile serves them too.
        (ntfs,) = rules("default", "ntfs-fixtures")
        self.assertEqual(ntfs["filter"], "package(tilo-ntfs-helper) and binary(it)")
        self.assertEqual(ntfs["platform"], siblings_rule["platform"])
        self.assertEqual(rules("ci", "ntfs-fixtures"), [],
                         "ci inherits the default profile's NTFS fixtures")
        # The session's app-open tests start the installed apps and the launcher
        # itself, and time the session's opening against their manifests' splash
        # budgets: the run builds each of them first.
        opened = scripts.pop("app-open-siblings")
        self.assertEqual(opened["command"], ["bash", "tools/build-sibling-binaries.sh",
                                             "tilo-runtime:tilo-runtime",
                                             "passwords:passwords",
                                             "launcher:tilo-launcher"])
        for profile in ("default", "ci"):
            rules = [rule for rule in config["profile"][profile]["scripts"]
                     if rule["setup"] == "app-open-siblings"]
            self.assertEqual(
                [(rule["filter"], rule["platform"]) for rule in rules],
                [("package(tilo-session) and binary(it) and test(app_open_budget::)",
                  {"host": 'cfg(target_os = "linux")'})],
                f"the {profile} profile builds the apps the session opens and times",
            )
        steps = {name: script["command"][2:] for name, script in scripts.items()}
        self.assertEqual(steps, {"test-runtime": ["runtime"], "font-catalog": ["fonts"],
                                 "headless-audio": ["audio"],
                                 "ntfs-fixtures": ["ntfs"],
                                 "swtpm": ["swtpm"], "pdf-sandbox": ["pdf-sandbox"],
                                 "throughput": ["throughput"],
                                 "property-cases": ["property-cases"]})
        for script in scripts.values():
            self.assertEqual(script["command"][:2], ["bash", "tools/test-env-setup.sh"])
        rules = {rule["filter"]: rule["setup"] for rule in config["profile"]["ci"]["scripts"]}
        # The two steps that take work run for the packages whose code can use them, never
        # for every test: a setup script runs once per nextest run, and a suite is over a
        # thousand of those. tools/tests/test_setup_filters.py holds each filter to the code.
        costly = {setup: filter_ for filter_, setup in rules.items()
                  if setup in ("font-catalog", "headless-audio")}
        self.assertEqual(set(costly), {"font-catalog", "headless-audio"})
        for filter_ in costly.values():
            self.assertNotIn("all()", filter_)
            del rules[filter_]
        # `ci` inherits `default`'s rules, so it names only what it adds.
        self.assertEqual(rules, {"all()": "test-runtime",
                                 "package(tilo-session) and binary(it) and "
                                 "test(app_open_budget::)": "app-open-siblings",
                                 "package(tilo-session) and binary(it) and test(device_branch::)":
                                     "vault-siblings",
                                 "package(tilo-session) and binary(it) and test(lounge_e2e::)": "lounge-siblings",
                                 "package(tilo-session-client) and binary(it) and "
                                 "test(lounge_fresh_page::)":
                                     "lounge-siblings",
                                 "package(tilo-session-client) and binary(it) and "
                                 "test(node_workspace::)": "page-bundle",
                                 # The values only some packages' tests need, for those packages:
                                 # given to the run's environment, each would key every build
                                 # script's and proc macro's cache entry by it.
                                 "package(tilo-auth) or package(tilo-rollback) or "
                                 "package(tilo-test-env)": "swtpm",
                                 "package(tilo-pdf) or "
                                 "package(tilo-thumbnail)": "pdf-sandbox",
                                 "package(tilo-agent-session) and "
                                 "test(workspace_actions::throughput::)": "throughput"})

    def test_the_deep_profile_gives_its_tests_more_property_cases(self) -> None:
        # t.toml's `test-deep` tests its packages with more proptest cases, through a profile of
        # its own: in the family's environment the count would key every build script's and proc
        # macro's cache entry by it, and no other family's build of the same packages would be
        # shared. The profile inherits `ci`, so the tests keep its setup, its hang bound and the
        # failure output their results are read by.
        config = tomllib.loads((ROOT / ".config/nextest.toml").read_text())
        self.assertEqual(config["profile"]["deep"]["inherits"], "ci")
        rules = [(rule["filter"], rule["setup"]) for rule in config["profile"]["deep"]["scripts"]]
        self.assertEqual(rules, [("all()", "property-cases")])
        result, env = self.setup("property-cases")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(env, {"PROPTEST_CASES": "256"})
        families = tomllib.loads((ROOT / "t.toml").read_text())["cargo"]
        family = next(family for family in families if family["name"] == "test-deep")
        self.assertIn(["--profile", "deep"], [family["args"][i:i + 2]
                                              for i in range(len(family["args"]) - 1)])
        # A nextest family's values are its profile's setup steps, never the job's environment:
        # there, each keys every build script's and proc macro's cache entry, so jobs that test
        # different packages share no build.
        for family in families:
            if family.get("tool") == "nextest":
                with self.subTest(family=family["name"]):
                    self.assertNotIn("env", family)

    def test_runtime_provides_the_renderer_and_the_promised_capabilities(self) -> None:
        driver = Path(f"/usr/share/vulkan/icd.d/lvp_icd.{os.uname().machine}.json")
        result, env = self.setup("runtime", TILO_REQUIRE_GPU="0" if not driver.exists() else "1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(env["TILO_REQUIRE_MEDIA"], "1")
        self.assertTrue(env["TILO_TEST_CACHE_DIR"].endswith("/.cache/tilo/test-env"))
        # The font catalog the frames are set in, where the runs that render find it, and the
        # audio endpoint: promised to every test here, and prepared by steps of their own for
        # the packages that can use them, so a test that needs one where it was not prepared
        # fails and does not skip.
        self.assertTrue(env["TILO_FONT_ASSET_CACHE"].endswith("/font-artifacts"))
        self.assertEqual(env["TILO_REQUIRE_FONTS"], "1")
        self.assertEqual(env["TILO_REQUIRE_AUDIO"], "1")
        if driver.exists():
            self.assertEqual(env["VK_DRIVER_FILES"], str(driver))
            self.assertEqual(env["WGPU_ADAPTER_NAME"], "llvmpipe")
            # One shader cache for the run's test processes, kept with the
            # fixtures: each has a fresh HOME, and would compile every shader
            # again there.
            self.assertEqual(env["MESA_SHADER_CACHE_DIR"],
                             env["TILO_TEST_CACHE_DIR"] + "/mesa-shader-cache")
        else:
            self.assertNotIn("VK_DRIVER_FILES", env)
            self.assertNotIn("MESA_SHADER_CACHE_DIR", env)
            self.assertEqual(env["TILO_REQUIRE_GPU"], "0")

    def test_runtime_starts_no_interpreter_and_no_other_shell(self) -> None:
        # Every job of a run pays for this step, and a suite is well over a thousand jobs (a
        # package's test binary, or a shard of one), each on a core of its own: a Python start
        # here was a quarter of a second of every one of them. The step reads and decides in
        # its own shell, and starts a program only for the driver's architecture and, once,
        # for the fixture cache's directory.
        driver = Path(f"/usr/share/vulkan/icd.d/lvp_icd.{os.uname().machine}.json")
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            programs, log = recorders(work, "python3")
            env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
                   "HOME": str(work / "home"), "NEXTEST_ENV": str(work / "env"),
                   "TILO_REQUIRE_GPU": "1" if driver.exists() else "0"}
            for expected in (Counter(uname=1, mkdir=1), Counter(uname=1)):
                # The shell is named, so the recorders see only what the step itself starts.
                result = subprocess.run([shutil.which("bash"), str(SETUP), "runtime"], env=env,
                                        capture_output=True, text=True, check=False)
                self.assertEqual(result.returncode, 0, result.stderr)
                started = Counter(log.read_text().split())
                log.write_text("")
                for program in ("python3", "bash", "sh", "env"):
                    self.assertEqual(started[program], 0, started)
                self.assertEqual(started, expected)

    def mapped(self, manifest: str) -> tuple[list[str], list[str]]:
        """The capabilities the runtime step requires for MANIFEST as the shell reads it, and
        the ones tools/ci/capability_filter.py reads there."""
        driver = Path(f"/usr/share/vulkan/icd.d/lvp_icd.{os.uname().machine}.json")
        with tempfile.TemporaryDirectory() as tmp:
            tree = Path(tmp) / "tree"
            (tree / "tools/ci").mkdir(parents=True)
            (tree / ".config").mkdir()
            for name in ("test-env-setup.sh", "ci/capability_filter.py"):
                shutil.copy(ROOT / "tools" / name, tree / "tools" / name)
            (tree / ".config/nextest-capabilities.toml").write_text(manifest)
            env_file = Path(tmp) / "env"
            env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": tmp,
                   "NEXTEST_ENV": str(env_file), "TILO_REQUIRE_GPU": "1" if driver.exists() else "0"}
            result = subprocess.run(["bash", str(tree / "tools/test-env-setup.sh"), "runtime"],
                                    env=env, capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            promised = {"TILO_REQUIRE_GPU", "TILO_REQUIRE_MEDIA", "TILO_REQUIRE_FONTS",
                        "TILO_REQUIRE_AUDIO"}
            shell = [line.split("=", 1)[0] for line in env_file.read_text().splitlines()
                     if line.startswith("TILO_REQUIRE_") and line.split("=", 1)[0] not in promised]
            tool = subprocess.run(
                [sys.executable, str(tree / "tools/ci/capability_filter.py"), "--require-variables"],
                capture_output=True, text=True, check=True).stdout
            return shell, [line.split("=", 1)[0] for line in tool.splitlines()]

    def test_the_shell_reads_the_mapped_capabilities_as_the_manifest_tool_does(self) -> None:
        # The runtime step reads the manifest itself, where it used to ask the tool. The two
        # readings are one list: of this tree's manifest, and of one written to tell them
        # apart (a capability not mapped, keys in either order, a key of the same name in a
        # test group's table and in a table that is no capability, a last line without its
        # newline).
        shell, tool = self.mapped((ROOT / ".config/nextest-capabilities.toml").read_text())
        self.assertEqual(shell, tool)
        self.assertTrue(tool, "the manifest maps a capability")
        shell, tool = self.mapped(
            '# Capabilities.\n'
            '[capabilities.One]\n'
            'variable = "TILO_REQUIRE_ONE"  # the first\n'
            'mapped = true\n'
            '\n'
            '[[capabilities.One.tests]]\n'
            'probe = "one"\n'
            'variable = "TILO_REQUIRE_A_GROUPS_KEY"\n'
            'mapped = true\n'
            'filter = "package(one)"\n'
            '\n'
            '[capabilities.Two]\n'
            'variable = "TILO_REQUIRE_TWO"\n'
            'mapped = false\n'
            '\n'
            '  [capabilities.Three]\n'
            '  mapped   =   true\n'
            '  variable="TILO_REQUIRE_THREE"\n'
            '\n'
            '[capabilities.Four]\n'
            'variable = "TILO_REQUIRE_FOUR"\n'
            '\n'
            '[elsewhere]\n'
            'variable = "TILO_REQUIRE_ELSEWHERE"\n'
            'mapped = true\n'
            '\n'
            '[capabilities.Five]\n'
            'variable = "TILO_REQUIRE_FIVE"\n'
            'mapped = true')
        self.assertEqual(tool, ["TILO_REQUIRE_ONE", "TILO_REQUIRE_THREE", "TILO_REQUIRE_FIVE"])
        self.assertEqual(shell, tool)

    def test_fonts_prepares_the_catalog_where_the_runtime_says_it_is(self) -> None:
        # The catalog is a step of its own, which nextest runs for the packages that can
        # read one; the runtime step every test gets names the same place and promises it.
        driver = Path(f"/usr/share/vulkan/icd.d/lvp_icd.{os.uname().machine}.json")
        with tempfile.TemporaryDirectory() as directory:
            provided = {}
            for step in ("runtime", "fonts"):
                env_file = Path(directory) / f"env-{step}"
                env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": directory,
                       "NEXTEST_ENV": str(env_file),
                       "TILO_REQUIRE_GPU": "1" if driver.exists() else "0"}
                result = subprocess.run(["bash", str(SETUP), step], env=env,
                                        capture_output=True, text=True, check=False)
                self.assertEqual(result.returncode, 0, result.stderr)
                provided[step] = dict(line.split("=", 1) for line in env_file.read_text().splitlines())
                cache = Path(provided[step]["TILO_FONT_ASSET_CACHE"])
                if step == "runtime":
                    self.assertFalse((cache / "catalogs").exists(), "the runtime prepares none")
            self.assertEqual(provided["fonts"], {"TILO_FONT_ASSET_CACHE": str(cache)})
            self.assertEqual(provided["runtime"]["TILO_FONT_ASSET_CACHE"], str(cache))
            (catalog,) = (cache / "catalogs").glob("*-fixtures")
            self.assertEqual((catalog / "manifest.toml").read_bytes(),
                             (ROOT / "crates/ui/ui-core/assets/fonts/manifest.toml").read_bytes())

    def test_a_missing_renderer_fails_where_it_is_promised(self) -> None:
        if Path(f"/usr/share/vulkan/icd.d/lvp_icd.{os.uname().machine}.json").exists():
            self.skipTest("this machine has lavapipe")
        result, _ = self.setup("runtime")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("lavapipe", result.stderr)

    def test_existing_settings_and_endpoints_are_kept(self) -> None:
        result, env = self.setup("audio", "ntfs", TILO_REQUIRE_AUDIO="0",
                                 TILO_NTFS_FIXTURES="/fixtures")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(env, {"TILO_NTFS_FIXTURES": "/fixtures"})
        result, _ = self.setup("bogus")
        self.assertEqual(result.returncode, 2)

    def test_the_tpm_and_pdf_sandbox_steps_promise_what_the_node_has(self) -> None:
        result, env = self.setup("swtpm")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(env, {"TILO_REQUIRE_SWTPM": "1"})
        # The sandbox is promised only where the kernel can enforce it
        # (tools/ci/require-pdf-sandbox.sh prints 1 there and nothing elsewhere).
        required = subprocess.run(
            ["bash", str(ROOT / "tools/ci/require-pdf-sandbox.sh")],
            capture_output=True, text=True, check=True).stdout.strip()
        result, env = self.setup("pdf-sandbox")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(env, {"TILO_REQUIRE_PDF_SANDBOX": required} if required else {})

    def handed_step(self, work: Path, step: str, **extra: str):
        """STEP as a shard gets it: the result, the variables it provided, and the Cargo
        invocations it made."""
        programs, ran = work / "bin", work / "cargo-ran"
        programs.mkdir()
        (programs / "cargo").write_text(f'#!/bin/sh\nprintf "%s\\n" "$*" >>"{ran}"\n')
        (programs / "cargo").chmod(0o755)
        env_file = work / "env"
        env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
               "HOME": str(work), "NEXTEST_ENV": str(env_file),
               "CARGO_TARGET_DIR": str(work / "target"), **extra}
        result = subprocess.run(["bash", str(SETUP), step], env=env,
                                capture_output=True, text=True, check=False)
        provided = (dict(line.split("=", 1) for line in env_file.read_text().splitlines())
                    if env_file.exists() else {})
        given = ran.read_text().splitlines() if ran.exists() else []
        return result, provided, given

    def test_a_handed_pam_module_is_named_to_the_tests_and_not_built(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            module = work / "libpam_tilo.so"
            module.write_text("")
            result, provided, given = self.handed_step(
                work, "pam-module", T_STARTED_CDYLIB_pam_tilo=str(module))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(provided, {"TILO_PAM_TILO_MODULE": str(module)})
            self.assertEqual(given, [], "the job's module, not one built here")

    def test_a_pam_module_not_handed_is_built(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            result, provided, given = self.handed_step(work, "pam-module")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(given, ["build --locked -p pam-tilo --lib"])
            self.assertEqual(provided,
                             {"TILO_PAM_TILO_MODULE": f"{work}/target/debug/libpam_tilo.so"})


class SiblingBinaryTests(unittest.TestCase):
    """tools/build-sibling-binaries.sh builds only what the run was not handed."""

    def run_script(self, work: Path, cargo: str, *binaries: str, **handed: str):
        fake = work / "cargo"
        fake.write_text(f"#!/bin/sh\necho \"$*\" >>{work}/cargo-ran\n{cargo}\n")
        fake.chmod(0o755)
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "CARGO": str(fake),
               "NEXTEST_ENV": str(work / "env"), **handed}
        result = subprocess.run(["bash", str(ROOT / "tools/build-sibling-binaries.sh"), *binaries],
                                env=env, capture_output=True, text=True, check=False)
        ran = (work / "cargo-ran").read_text() if (work / "cargo-ran").exists() else ""
        told = (work / "env").read_text().splitlines() if (work / "env").exists() else []
        return result, ran, told

    def test_handed_binaries_are_told_and_never_built(self) -> None:
        # t's archive holds them and names each as NEXTEST_BIN_EXE_<bin>: no Cargo runs.
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            for name in ("osk", "tilo-lock-screen"):
                (work / name).write_text("#!/bin/sh\n")
                (work / name).chmod(0o755)
            result, ran, told = self.run_script(
                work, "exit 9", "tilo-lock-screen:tilo-lock-screen", "osk:osk",
                **{"NEXTEST_BIN_EXE_tilo-lock-screen": str(work / "tilo-lock-screen"),
                   "NEXTEST_BIN_EXE_osk": str(work / "osk")})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(ran, "")
            self.assertEqual(told, [f"TILO_SIBLING_BIN_tilo_lock_screen={work}/tilo-lock-screen",
                                    f"TILO_SIBLING_BIN_osk={work}/osk"])

    def test_a_binary_not_handed_is_built_alone(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            for name in ("osk", "tilo-lock-screen"):
                (work / name).write_text("#!/bin/sh\n")
                (work / name).chmod(0o755)
            built = f'echo \'{{"executable":"{work}/osk"}}\''
            result, ran, told = self.run_script(
                work, built, "tilo-lock-screen:tilo-lock-screen", "osk:osk",
                **{"NEXTEST_BIN_EXE_tilo-lock-screen": str(work / "tilo-lock-screen")})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(ran, "build --locked --message-format=json-render-diagnostics "
                                  "--package osk --bin osk\n")
            self.assertEqual(told[-1], f"TILO_SIBLING_BIN_osk={work}/osk")

    def test_a_handed_library_is_told_and_never_built(self) -> None:
        # A cdylib no test binary links, which t's archive does not carry and its family
        # build hands as a file: pam-tilo's module, named to the tests as
        # TILO_PAM_TILO_MODULE.
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            module = work / "libpam_tilo.so"
            module.write_text("")
            result, ran, told = self.run_script(
                work, "exit 9", "pam-tilo:pam_tilo",
                **{"T_STARTED_CDYLIB_pam_tilo": str(module)})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(ran, "")
            self.assertEqual(told, [f"TILO_PAM_TILO_MODULE={module}"])

    def test_a_library_not_handed_is_built_and_told(self) -> None:
        # A developer's nextest invocation has no T_STARTED_CDYLIB_pam_tilo: it builds
        # the package's library, and hands the file Cargo uplifted beside the binaries.
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            built = f'echo \'{{"filenames":["{work}/deps/libpam_tilo.so", "{work}/libpam_tilo.so"]}}\''
            result, ran, told = self.run_script(work, built, "pam-tilo:pam_tilo")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(ran, "build --locked --message-format=json-render-diagnostics "
                                  "--package pam-tilo --lib\n")
            self.assertEqual(told, [f"TILO_PAM_TILO_MODULE={work}/libpam_tilo.so"])

    def test_a_handed_library_that_is_no_file_fails_instead_of_building(self) -> None:
        # Building here would hide a broken handoff and compile the library in every shard.
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            result, ran, told = self.run_script(
                work, "exit 9", "pam-tilo:pam_tilo",
                **{"T_STARTED_CDYLIB_pam_tilo": str(work / "not-a-file.so")})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("T_STARTED_CDYLIB_pam_tilo", result.stderr)
            self.assertEqual(ran, "")
            self.assertEqual(told, [])


class CargoRunnerTests(unittest.TestCase):
    """tools/cargo-runner.sh isolates test harnesses and leaves `cargo run` alone."""

    def run_binary(self, relative: str) -> dict[str, str]:
        with tempfile.TemporaryDirectory() as target:
            binary = Path(target) / relative
            binary.parent.mkdir(parents=True)
            binary.write_text(f"#!/bin/sh\nexec python3 -c '{DUMP}'\n")
            binary.chmod(0o755)
            env = {
                "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                "HOME": "/nonexistent-developer-home",
                "DBUS_SESSION_BUS_ADDRESS": "unix:path=/run/user/1000/bus",
                "XDG_RUNTIME_DIR": "/run/user/1000",
                "WAYLAND_DISPLAY": "wayland-0",
            }
            result = subprocess.run([str(RUNNER), str(binary)], env=env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)

    def test_test_harnesses_run_in_the_test_environment(self) -> None:
        for relative in ("debug/deps/shell-0123456789abcdef", "debug/deps/portal_test-0123"):
            env = self.run_binary(relative)
            root = env["TILO_TEST_ENVIRONMENT"]
            self.assertEqual(env["XDG_RUNTIME_DIR"], f"{root}/runtime")
            self.assertEqual(env["DBUS_SESSION_BUS_ADDRESS"], f"unix:path={root}/no-session-bus")
            self.assertNotIn("WAYLAND_DISPLAY", env)

    def test_cargo_run_targets_keep_the_callers_session(self) -> None:
        for relative in ("debug/tilo-shell", "debug/examples/native-file-picker"):
            env = self.run_binary(relative)
            self.assertNotIn("TILO_TEST_ENVIRONMENT", env)
            self.assertEqual(env["WAYLAND_DISPLAY"], "wayland-0")

    def test_a_wrapped_jobs_skips_reach_only_its_packages_test_binaries(self) -> None:
        # tools/test-env.sh leaves the tests this machine cannot run beside the
        # environment, per package: a test binary of that package is started
        # without them, as a nextest run leaves them out, and nothing else is.
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            target, binary = self.build_tree(work, 'printf "%s\\n" "$@"')
            (target / "test-env-vars").write_text(f"pid={os.getpid()}\n")
            skips = target / "test-env-skips"
            skips.write_text(f"pid={os.getpid()}\n"
                             "tilo-device-identity\tpersist::tests::a\n"
                             "other-package\tsome::test\n")
            result = self.start(binary, "user-filter",
                                CARGO_PKG_NAME="tilo-device-identity")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.splitlines(),
                             ["user-filter", "--skip", "persist::tests::a"])
            # Another package gets its own tests left out, never this one's.
            result = self.start(binary, CARGO_PKG_NAME="other-package")
            self.assertEqual(result.stdout.splitlines(), ["--skip", "some::test"])
            # A package with no gated test is started as given.
            result = self.start(binary, "user-filter", CARGO_PKG_NAME="elsewhere")
            self.assertEqual(result.stdout.splitlines(), ["user-filter"])
            # A program that is no test harness reads none of it.
            program = target / "debug" / "tool"
            program.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
            program.chmod(0o755)
            result = self.start(program, CARGO_PKG_NAME="tilo-device-identity")
            self.assertEqual(result.stdout.splitlines(), [""], "no argument was added")
            # A file a killed job left behind is not read.
            skips.write_text("pid=999999999\ntilo-device-identity\tpersist::tests::a\n")
            result = self.start(binary, CARGO_PKG_NAME="tilo-device-identity")
            self.assertEqual(result.stdout.splitlines(), [""], "no argument was added")

    def test_a_wrapped_jobs_environment_reaches_its_programs_but_never_a_stale_file(self) -> None:
        # tools/test-env.sh writes the test environment beside the build directory, so a build's
        # scripts never carry it (it is keyed into every build script's cache entry there). Every
        # program such a job starts gets it, and the isolation a test harness gets.
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            (work / ".rustc_info.json").write_text("{}")
            example = work / "debug" / "examples" / "probe"
            example.parent.mkdir(parents=True)
            example.write_text(f"#!/bin/sh\nexec python3 -c '{DUMP}'\n")
            example.chmod(0o755)
            environment = work / "test-env-vars"
            environment.write_text(f"pid={os.getpid()}\nTILO_PROBE=handed\nTILO_REQUIRE_MEDIA=1\n")
            base = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                    "HOME": "/nonexistent-developer-home",
                    "VK_DRIVER_FILES": "/nonexistent-driver.json"}
            result = subprocess.run([str(RUNNER), str(example)], env=base,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            env = json.loads(result.stdout)
            self.assertEqual(env["TILO_PROBE"], "handed")
            self.assertEqual(env["TILO_REQUIRE_MEDIA"], "1")
            self.assertEqual(env["XDG_RUNTIME_DIR"], f"{env['TILO_TEST_ENVIRONMENT']}/runtime")
            # A file a killed job left behind is not read.
            environment.write_text("pid=999999999\nTILO_PROBE=stale\n")
            result = subprocess.run([str(RUNNER), str(example)], env=base,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            env = json.loads(result.stdout)
            self.assertNotIn("TILO_PROBE", env)
            self.assertNotIn("TILO_TEST_ENVIRONMENT", env, "the caller's session is kept")

    def test_a_jobs_test_environment_goes_beside_the_build_directory_not_to_cargo(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            programs, dumped, stage = work / "bin", work / "cargo-env", work / "vars"
            programs.mkdir()
            (programs / "cargo").write_text(
                f'#!/bin/sh\npython3 -c \'{DUMP}\' >"{dumped}"\n'
                f'cp -- "$CARGO_TARGET_DIR/test-env-vars" "{stage}"\n')
            (programs / "cargo").chmod(0o755)
            env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
                   "HOME": str(work / "home"), "CARGO_TARGET_DIR": str(work / "target"),
                   "TILO_REQUIRE_GPU": "0", "TILO_REQUIRE_AUDIO": "0",
                   "TILO_TEST_NTFS_FIXTURES": "0"}
            result = subprocess.run(["bash", str(TEST_ENV), "cargo", "test", "--no-run"], env=env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            cargo_env = json.loads(dumped.read_text())
            self.assertEqual(cargo_env["HOME"], str(work / "home"),
                             "the build gets the caller's HOME")
            for name in ("PULSE_SERVER", "PULSE_RUNTIME_PATH", "MESA_SHADER_CACHE_DIR",
                         "TILO_TEST_CACHE_DIR", "TILO_REQUIRE_MEDIA", "TILO_TEST_ENVIRONMENT"):
                self.assertNotIn(name, cargo_env, "the build carries no test environment")
            lines = stage.read_text().splitlines()
            self.assertRegex(lines[0], r"^pid=\d+$")
            provided = dict(line.split("=", 1) for line in lines[1:])
            self.assertTrue(provided["TILO_TEST_CACHE_DIR"].endswith("/.cache/tilo/test-env"))
            self.assertFalse((work / "target" / "test-env-vars").exists(),
                             "the handoff is removed afterwards")

    def test_a_cargo_run_targets_environment_goes_beside_the_build_directory_too(self) -> None:
        # An example a job runs is a program the job starts, like its tests: the build gets the
        # caller's environment, and the target finds the test environment where the runner reads
        # it (tools/cargo-runner.sh).
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            programs, dumped, stage = work / "bin", work / "cargo-env", work / "vars"
            programs.mkdir()
            (programs / "cargo").write_text(
                f'#!/bin/sh\npython3 -c \'{DUMP}\' >"{dumped}"\n'
                f'cp -- "$CARGO_TARGET_DIR/test-env-vars" "{stage}"\n')
            (programs / "cargo").chmod(0o755)
            env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
                   "HOME": str(work / "home"), "CARGO_TARGET_DIR": str(work / "target"),
                   "TILO_REQUIRE_GPU": "0", "TILO_REQUIRE_AUDIO": "0",
                   "TILO_TEST_NTFS_FIXTURES": "0"}
            result = subprocess.run(
                ["bash", str(TEST_ENV), "cargo", "run", "--example", "window_share_shots"],
                env=env, capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            cargo_env = json.loads(dumped.read_text())
            self.assertEqual(cargo_env["HOME"], str(work / "home"),
                             "the build gets the caller's HOME")
            for name in ("PULSE_SERVER", "PULSE_RUNTIME_PATH", "TILO_HEADLESS_AUDIO_PID",
                         "TILO_TEST_CACHE_DIR", "TILO_TEST_ENVIRONMENT"):
                self.assertNotIn(name, cargo_env, "the build carries no test environment")
            provided = dict(line.split("=", 1) for line in stage.read_text().splitlines()[1:])
            self.assertIn("TILO_TEST_CACHE_DIR", provided)
            self.assertFalse((work / "target" / "test-env-vars").exists(),
                             "the handoff is removed afterwards")

    def test_a_build_runs_in_the_callers_environment_with_the_build_inputs(self) -> None:
        # tools/test-env.sh --build: for a command that builds and then runs what it built
        # (tools/ci/layout-check.sh, tools/ci/example-scenarios.sh) the build is the one every
        # other job's build shares, and nothing of the test environment reaches it.
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            programs, dumped = work / "bin", work / "cargo-env"
            programs.mkdir()
            (programs / "cargo").write_text(
                f'#!/bin/sh\npython3 -c \'{DUMP}\' >"{dumped}"\n')
            (programs / "cargo").chmod(0o755)
            env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
                   "HOME": str(work / "home"), "CARGO_TARGET_DIR": str(work / "target"),
                   "TILO_TEST_NTFS_FIXTURES": "0"}
            result = subprocess.run(
                ["bash", str(TEST_ENV), "--build", "cargo", "build", "--locked"],
                env=env, capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            cargo_env = json.loads(dumped.read_text())
            self.assertEqual(cargo_env["HOME"], str(work / "home"))
            self.assertEqual(cargo_env["TILO_FONT_ASSET_CACHE"],
                             str(work / "target" / "font-artifacts"),
                             "the build inputs every job's build shares are exported")
            for name in ("PULSE_SERVER", "PULSE_RUNTIME_PATH", "MESA_SHADER_CACHE_DIR",
                         "TILO_TEST_CACHE_DIR", "TILO_REQUIRE_MEDIA", "TILO_TEST_ENVIRONMENT",
                         "TILO_TEST_NTFS_FIXTURES"):
                self.assertNotIn(name, cargo_env, "the build carries no test environment")
            self.assertFalse((work / "target").exists(), "no environment was prepared")

    def test_a_command_started_in_the_environment_finds_the_builds_environment(self) -> None:
        # A command started in the environment that builds the app it then starts builds as
        # --build builds: tools/test-env.sh writes the environment it was given, before its
        # steps add theirs, beside the fresh HOME.
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            programs, dumped = work / "bin", work / "probe-env"
            programs.mkdir()
            (programs / "probe").write_text(
                f'#!/bin/sh\npython3 -c \'{DUMP}\' >"{dumped}"\n'
                f'cp -- "$TILO_TEST_ENVIRONMENT/build-environment" "{work}/environment"\n')
            (programs / "probe").chmod(0o755)
            env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
                   "HOME": str(work / "home"), "CARGO_TARGET_DIR": str(work / "target"),
                   "TILO_REQUIRE_GPU": "0", "TILO_REQUIRE_AUDIO": "0",
                   "TILO_TEST_NTFS_FIXTURES": "0"}
            result = subprocess.run(["bash", str(TEST_ENV), "probe"], env=env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            started = json.loads(dumped.read_text())
            self.assertEqual(started["XDG_RUNTIME_DIR"],
                             f"{started['TILO_TEST_ENVIRONMENT']}/runtime",
                             "the command itself gets the test environment")
            written = Path(work, "environment").read_bytes().decode("utf-8", "surrogateescape")
            build = dict(entry.split("=", 1) for entry in written.split("\0") if "=" in entry)
            self.assertEqual(build["HOME"], str(work / "home"),
                             "the build environment is the caller's")
            self.assertEqual(build["TILO_FONT_ASSET_CACHE"],
                             str(work / "target" / "font-artifacts"),
                             "the build inputs every job's build shares are in it")
            for name in ("PULSE_SERVER", "TILO_TEST_CACHE_DIR", "TILO_TEST_ENVIRONMENT",
                         "MESA_SHADER_CACHE_DIR", "TILO_HEADLESS_AUDIO_PID"):
                self.assertNotIn(name, build, "the build environment carries no test environment")

    def test_a_nextest_command_passes_unchanged(self) -> None:

        # Its ci profile sets the test environment up itself, where its tests execute.
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            programs, dumped = work / "bin", work / "cargo-env"
            programs.mkdir()
            (programs / "cargo").write_text(
                f'#!/bin/sh\npython3 -c \'{DUMP}\' >"{dumped}"\n')
            (programs / "cargo").chmod(0o755)
            env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
                   "HOME": str(work / "home"), "CARGO_TARGET_DIR": str(work / "target")}
            result = subprocess.run(
                ["bash", str(TEST_ENV), "cargo", "nextest", "run", "--profile", "ci"],
                env=env, capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            cargo_env = json.loads(dumped.read_text())
            self.assertEqual(cargo_env["HOME"], str(work / "home"))
            for name in ("TILO_TEST_CACHE_DIR", "TILO_TEST_ENVIRONMENT", "PULSE_SERVER"):
                self.assertNotIn(name, cargo_env)
            self.assertFalse((work / "target").exists(), "no environment was prepared")

    def build_tree(self, work: Path, test: str) -> tuple[Path, Path]:
        """A build directory as cargo leaves one, with TEST as a test binary in it."""
        work.chmod(0o755)
        target = work / "target"
        (target / "debug/deps").mkdir(parents=True)
        (target / "debug/review").mkdir()
        (target / "tmp").mkdir()
        (target / ".rustc_info.json").write_text("{}")
        binary = target / "debug/deps/suite-0123456789abcdef"
        binary.write_text(f"#!/bin/sh\n{test}\n")
        binary.chmod(0o755)
        return target, binary

    def start(self, binary: Path, *arguments: str, **extra: str) -> subprocess.CompletedProcess[str]:
        """Start BINARY as cargo does, in a run that named its Vulkan driver as CI's profile does."""
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": "/nonexistent-developer-home",
               "VK_DRIVER_FILES": "/nonexistent-driver.json", **extra}
        # The shell is named, so the recorders see only what the runner itself starts.
        return subprocess.run([shutil.which("bash"), str(RUNNER), str(binary), *arguments], env=env,
                              capture_output=True, text=True, check=False)

    def started(self, log: Path) -> Counter[str]:
        programs = Counter(log.read_text().split())
        log.write_text("")
        return programs

    def test_a_test_start_is_one_shell_and_what_makes_and_removes_its_environment(self) -> None:
        # A suite starts a process per test: every program a start runs, tens of thousands
        # of runs pay. The runner decides in its own shell and starts only what changes the
        # machine: the environment's directories, their mode and their removal.
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            _, binary = self.build_tree(work, 'echo "$TILO_TEST_ENVIRONMENT"')
            programs, log = recorders(work)
            path = f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}"
            result = self.start(binary, PATH=path)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(Path(result.stdout.strip()).exists(), "the environment is removed")
            expected = Counter(mktemp=1, mkdir=1, chmod=1, rm=1)
            if old_bash():
                # A bash that cannot vouch for its own OSTYPE asks, once.
                expected["uname"] = 1
            self.assertEqual(self.started(log), expected)

    def test_a_start_inside_an_intact_environment_starts_nothing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            _, binary = self.build_tree(work, 'echo "$HOME"')
            programs, log = recorders(work)
            root = work / "environment"
            (root / "runtime").mkdir(parents=True)
            result = self.start(
                binary, PATH=f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
                TILO_TEST_ENVIRONMENT=str(root), HOME=f"{root}/home",
                XDG_RUNTIME_DIR=f"{root}/runtime",
                DBUS_SESSION_BUS_ADDRESS=f"unix:path={root}/no-session-bus",
                DBUS_SYSTEM_BUS_ADDRESS=f"unix:path={root}/no-system-bus")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), f"{root}/home", "the environment is kept")
            self.assertEqual(self.started(log), Counter())
            self.assertTrue(root.exists(), "an environment the start did not make is not its to remove")

    def test_the_build_directory_is_found_from_a_relative_path_and_from_none(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            target, binary = self.build_tree(work, 'echo "$TILO_TEST_ENVIRONMENT"')
            result = subprocess.run(
                [str(RUNNER), "debug/deps/" + binary.name], cwd=target, capture_output=True,
                text=True, check=False,
                env={"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": "/nonexistent"})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(result.stdout.startswith("/tmp/tilo-test-env."), result.stdout)
            # Test binaries run away from their build directory (an archive of them) too.
            (target / ".rustc_info.json").unlink()
            result = self.start(binary)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(result.stdout.startswith("/tmp/tilo-test-env."), result.stdout)


class CapabilitySkipTests(unittest.TestCase):
    """A plain `cargo test` leaves out the tests whose capability this machine lacks, as a
    nextest run does, and says so: tools/test-env.sh writes the skips beside the environment,
    and tools/cargo-runner.sh hands them to the package's test binaries."""

    def wrapped(self, work: Path, probe: str, *arguments: str, uname: str | None = None):
        """`cargo test ARGUMENTS` through tools/test-env.sh, with PROBE's report: the run's
        result, the command the fake cargo was given, the variables it found, and the skips."""
        programs = work / "bin"
        programs.mkdir(exist_ok=True)
        (programs / "cargo").write_text(
            f'#!/bin/sh\nprintf "%s\\n" "$*" >"{work}/argv"\n'
            f'cp -- "$CARGO_TARGET_DIR/test-env-vars" "{work}/vars" 2>/dev/null || true\n'
            f'cp -- "$CARGO_TARGET_DIR/test-env-skips" "{work}/skips" 2>/dev/null || true\n')
        (programs / "cargo").chmod(0o755)
        if uname is not None:
            # The kernel test-env.sh asks for: `-s` answers it, everything else is the host's.
            (programs / "uname").write_text(
                f'#!/bin/sh\ncase $1 in -s) echo {uname} ;; *) exec /usr/bin/uname "$@" ;; esac\n')
            (programs / "uname").chmod(0o755)
        report = work / "probe"
        report.write_text(f"#!/bin/sh\n{probe}\n")
        report.chmod(0o755)
        env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
               "HOME": str(work / "home"), "CARGO_TARGET_DIR": str(work / "target"),
               "TILO_REQUIRE_GPU": "0", "TILO_REQUIRE_AUDIO": "0",
               "TILO_TEST_NTFS_FIXTURES": "0",
               "TILO_FONT_ASSET_CACHE": str(work / "fonts"),
               "NEXTEST_BIN_EXE_tilo-capability-probe": str(report)}
        result = subprocess.run(["bash", str(TEST_ENV), "cargo", "test", *arguments],
                                env=env, capture_output=True, text=True, check=False)
        argv = (work / "argv").read_text().strip() if (work / "argv").exists() else ""
        provided = (dict(line.split("=", 1)
                         for line in (work / "vars").read_text().splitlines()[1:])
                    if (work / "vars").exists() else {})
        skips = (work / "skips").read_text().splitlines() if (work / "skips").exists() else []
        return result, argv, provided, skips

    def test_a_gated_test_is_left_out_and_the_run_says_which_check_runs_it(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            result, argv, provided, skips = self.wrapped(
                work,
                "echo 'tilo-capability-probe non-root-identity unavailable: this process "
                "executes as uid 0'",
                "--locked", "-p", "tilo-device-identity")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(argv, "test --locked -p tilo-device-identity")
            # The run says what it left out, why, and where it does run.
            self.assertIn("not run:", result.stderr)
            self.assertIn("NonRoot", result.stderr)
            self.assertIn("this process executes as uid 0", result.stderr)
            self.assertIn("run by check identity-accounts", result.stderr)
            # The runner's file: the command that wrote it, and the test the group names.
            self.assertTrue(skips[0].startswith("pid="), skips)
            self.assertIn("tilo-device-identity\tpersist::tests::"
                          "an_unreachable_device_key_is_an_error_not_a_fresh_machine",
                          skips[1:])
            # The capability stays promised: the tests that do run still fail loudly
            # where a capability they need is missing.
            self.assertEqual(provided["TILO_REQUIRE_NONROOT"], "1")
            self.assertFalse((work / "target" / "test-env-skips").exists(),
                             "the skips are removed afterwards")

    def test_a_capability_this_machine_has_leaves_nothing_out(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            result, _, _, skips = self.wrapped(
                work, "echo 'tilo-capability-probe non-root-identity available'",
                "-p", "tilo-device-identity")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("not run:", result.stderr)
            self.assertEqual(skips, [])

    def test_a_run_of_no_package_the_manifest_names_probes_nothing(self) -> None:
        # The probe named here cannot run: asking it would fail the command.
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            result, _, _, skips = self.wrapped(work, "exit 9", "-p", "no-such-package")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(skips, [])

    def test_a_run_that_starts_no_test_probes_nothing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            result, _, _, skips = self.wrapped(work, "exit 9", "-p", "tilo-device-identity",
                                               "--no-run")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(skips, [])

    def test_a_machine_without_the_probes_leaves_nothing_out(self) -> None:
        # The probes are Linux's: a macOS run of the same package starts none,
        # and its tests state their own premises.
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            result, _, _, skips = self.wrapped(work, "exit 9", "-p", "tilo-device-identity",
                                               uname="Darwin")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("not run:", result.stderr)
            self.assertEqual(skips, [])


class DesktopIsolationHelperTests(unittest.TestCase):
    """tools/desktop-isolation.sh stages packaged scripts away from the host."""

    def stage(self, source: str, *assignments: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as work:
            src = Path(work) / "source"
            src.write_text(source)
            script = f'. {HELPER}; tilo_stage_script "$1" "$2" "${{@:3}}" && cat "$2"'
            return subprocess.run(["bash", "-c", script, "_", str(src), f"{work}/staged", *assignments],
                                  capture_output=True, text=True, check=False)

    def test_named_commands_are_rewritten(self) -> None:
        result = self.stage("#!/bin/sh\n# runs /usr/bin/systemctl in production\n"
                            "readonly SYSTEMCTL=/usr/bin/systemctl\nRUNNER=/usr/libexec/tilo-session-runner\n",
                            "SYSTEMCTL=/fake/systemctl", "RUNNER=/tree/runner")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("readonly SYSTEMCTL=/fake/systemctl\n", result.stdout)
        self.assertIn("RUNNER=/tree/runner\n", result.stdout)

    def test_a_remaining_host_command_fails(self) -> None:
        for leftover in ("SYSTEMCTL=/usr/bin/systemctl", "exec /usr/libexec/tilo-session-runner",
                         "SHELL_BIN=/usr/bin/tilo-shell", "/usr/bin/loginctl terminate-session"):
            result = self.stage(f"#!/bin/sh\nRUNNER=/usr/libexec/tilo-x\n{leftover}\n", "RUNNER=/tree/x")
            self.assertNotEqual(result.returncode, 0, leftover)
            self.assertIn("installed host commands", result.stderr)

    def test_a_variable_the_source_does_not_assign_fails(self) -> None:
        result = self.stage("#!/bin/sh\nSYSTEMCTL=/usr/bin/systemctl\n", "SYSTEMCTL=/fake", "RUNER=/typo")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("assigns no RUNER", result.stderr)

    def test_session_tests_stage_through_the_helper(self) -> None:
        tracked = subprocess.run(["git", "ls-files", "-z", "dist/*/tests/*", "os/tests/*", "tools/tests/*"],
                                 cwd=ROOT, check=True, capture_output=True).stdout.decode().split("\0")
        rewrite = re.compile(r"s[|#](readonly )?[A-Z_]+=/usr/")
        # A file the change deletes is still in the index when the change is uncommitted.
        for path in (path for path in tracked if path and (ROOT / path).exists()):
            text = (ROOT / path).read_text(errors="replace")
            self.assertIsNone(rewrite.search(text),
                              f"{path} rewrites a packaged command by hand; use tilo_stage_script")
            if "tilo_stage_script" in text and path != "tools/tests/test_test_environment.py":
                self.assertIn("tilo_test_isolate", text,
                              f"{path} stages session code without tilo_test_isolate")


class TestEnvironmentHelperTests(unittest.TestCase):
    """tools/with-test-environment.sh is read into the runner's own shell."""

    def test_the_helpers_read_into_a_shell_only_define_their_functions(self) -> None:
        # tools/cargo-runner.sh reads it into its own shell: it must start nothing there, and
        # leave the shell's arguments alone.
        script = ('set -euo pipefail; source "$0/tools/with-test-environment.sh"; '
                  'echo "$# $1 ${TILO_TEST_ENVIRONMENT:-none}"; '
                  'declare -F tilo_test_environment_enter')
        result = subprocess.run(["bash", "-c", script, str(ROOT), "--check"], capture_output=True,
                                text=True, check=False,
                                env={"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": "/"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["1 --check none", "tilo_test_environment_enter"])


if __name__ == "__main__":
    unittest.main()
