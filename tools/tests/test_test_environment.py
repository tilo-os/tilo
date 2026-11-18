"""tools/with-test-environment.sh gives every test run the same isolated environment."""
from __future__ import annotations

from collections import Counter
import json
import re
import os
from pathlib import Path
import pwd
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


def handover_user() -> pwd.struct_passwd | None:
    """The image's test user, where this suite is itself a job that hands tests over to one.

    That is a job that starts as root in an image naming its test user (TILO_TEST_USER). An
    ordinary account hands nothing over, and neither does a root that may not change owners
    (a job sandbox with one user id), whose image names no test user for that reason.
    """
    name = os.environ.get("TILO_TEST_USER")
    if os.geteuid() != 0 or not name:
        return None
    return pwd.getpwnam(name)


def default_mode() -> str:
    """The mode of a directory made with no mode named, as `stat -c %a` prints it."""
    mask = os.umask(0)
    os.umask(mask)
    return f"{0o777 & ~mask:o}"


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
            "tilo-quick-settings:tilo-quick-settings",
            "tilolang-process:tilolang-process", "launcher:tilo-launcher",
        ])
        for profile in ("default", "ci"):
            rules = [rule for rule in config["profile"][profile]["scripts"]
                     if rule["setup"] == "session-siblings"]
            expected = ([("package(tilo-session) or package(tilo-quick-settings)",
                         {"host": 'cfg(target_os = "linux")'})]
                        if profile == "default" else [])
            self.assertEqual(
                [(rule["filter"], rule["platform"]) for rule in rules],
                expected,
                f"the {profile} profile builds the session's siblings for its tests",
            )
        # Chat's acceptance starts the app, the launcher that starts it and the
        # runtime host the app runs on, so the run that drives it builds the
        # three packages of other crates first; a branched device runs that
        # same acceptance, with its host away, and a window lifted across to
        # the person's phone lands in a Chat started the same way.
        chat = scripts.pop("chat-siblings")
        self.assertEqual(chat["command"][:2], ["bash", "tools/build-sibling-binaries.sh"])
        self.assertEqual(chat["command"][2:], ["chat:chat", "launcher:tilo-launcher",
                                               "tilo-runtime:tilo-runtime"])
        for profile in ("default", "ci"):
            rules = [rule for rule in config["profile"][profile]["scripts"]
                     if rule["setup"] == "chat-siblings"]
            self.assertEqual(
                [(rule["filter"], rule["platform"]) for rule in rules],
                [("package(tilo-session) and binary(it) and (test(chat_2x2::) or test(device_branch::)"
                  " or test(shared_input::a_window_lifted) or test(workspace_agents::))",
                  {"host": 'cfg(target_os = "linux")'})],
                f"the {profile} profile builds Chat and its launcher for the acceptance",
            )
        # A branched device's vault and provider keys are read through the apps
        # that keep them, so the run that drives it builds those two and the
        # launcher a desktop starts them from first.
        vault = scripts.pop("vault-siblings")
        self.assertEqual(vault["command"], ["bash", "tools/build-sibling-binaries.sh",
                                            "passwords:passwords", "model-providers:model-providers",
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
        # A closed Tasks starts its app and the launcher the session starts it
        # through, so the run that drives it builds both first.

        tasks = scripts.pop("tasks-siblings")
        self.assertEqual(tasks["command"], ["bash", "tools/build-sibling-binaries.sh",
                                            "tasks:tasks", "launcher:tilo-launcher"])
        for profile in ("default", "ci"):
            rules = [rule for rule in config["profile"][profile]["scripts"]
                     if rule["setup"] == "tasks-siblings"]
            self.assertEqual(
                [(rule["filter"], rule["platform"]) for rule in rules],
                [("package(tilo-session) and binary(it) and test(closed_tasks::)",
                  {"host": 'cfg(target_os = "linux")'})],
                f"the {profile} profile builds Tasks and its launcher for the closed app",
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
        # The server's coding-agent tests start the guest's own agent program in its
        # serving mode, as a workspace's machine starts it: a program of another
        # package, which Cargo builds for no test of the server's. So do the agent
        # session's tests of a coding agent on a person's server, which start it
        # through that server's own keeper of coding agents.
        machine = scripts.pop("machine-siblings")
        self.assertEqual(machine["command"], ["bash", "tools/build-sibling-binaries.sh",
                                              "linux-vm-agent:tilo-linux-vm-agent"])
        for profile in ("default", "ci"):
            rules = [rule for rule in config["profile"][profile]["scripts"]
                     if rule["setup"] == "machine-siblings"]
            self.assertEqual(
                [(rule["filter"], rule["platform"]) for rule in rules],
                [(filter_, {"host": 'cfg(target_os = "linux")'})
                 for filter_ in ("package(tilo-server) and test(coding_agents::)",
                                 "package(ush) and (test(served_runs::) or test(environment_programs::))",
                                 "package(tilo-agent-session) and test(hosted_coding_agents::)")],
                f"the {profile} profile builds the guest's agent for the tests that start a coding agent",
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
                [("package(tilo-session-client) and binary(it) and (test(passkey_root::) or "
                  "test(node_workspace::))",
                  {"host": 'cfg(target_os = "linux")'})],
                f"the {profile} profile builds the page bundle its page tests run against",
            )
        steps = {name: script["command"][2:] for name, script in scripts.items()}
        self.assertEqual(steps, {"test-runtime": ["runtime"], "font-catalog": ["fonts"],
                                 "headless-audio": ["audio"],
                                 "ntfs-fixtures": ["ntfs"], "as-root": ["root"],
                                 "swtpm": ["swtpm"], "pdf-sandbox": ["pdf-sandbox"],
                                 "video-render": ["video-render"],
                                 "pam-module": ["pam-module"],
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
                                 "package(tilo-session) and binary(it) and (test(chat_2x2::) or "
                                 "test(device_branch::) or test(shared_input::a_window_lifted) or "
                                 "test(workspace_agents::))":
                                     "chat-siblings",
                                 "package(tilo-session) and binary(it) and test(closed_tasks::)": "tasks-siblings",
                                 "package(tilo-session) and binary(it) and test(device_branch::)":
                                     "vault-siblings",
                                 "package(tilo-session) and binary(it) and test(lounge_e2e::)": "lounge-siblings",
                                 "package(tilo-session-client) and binary(it) and "
                                 "test(lounge_fresh_page::)":
                                     "lounge-siblings",
                                 "package(tilo-server) and test(coding_agents::)": "machine-siblings",
                                 "package(ush) and (test(served_runs::) or "
                                 "test(environment_programs::))": "machine-siblings",
                                 "package(tilo-agent-session) and test(hosted_coding_agents::)":
                                     "machine-siblings",
                                 "package(tilo-session-client) and binary(it) and "
                                 "(test(passkey_root::) or test(node_workspace::))": "page-bundle",
                                 "package(tilo-ntfs-helper)": "ntfs-fixtures",
                                 # Only the sandbox's tests that map host ids or
                                 # make cgroups run as root.
                                 "package(tilo-sandbox) and (test(/^host_ids::/) or "
                                 "test(/^limits::/))": "as-root",
                                 # The values only some packages' tests need, for those packages:
                                 # given to the run's environment, each would key every build
                                 # script's and proc macro's cache entry by it.
                                 "package(tilo-auth) or package(tilo-rollback) or "
                                 "package(tilo-test-env)": "swtpm",
                                 "package(tilo-pdf) or package(pdf-viewer) or "
                                 "package(tilo-thumbnail)": "pdf-sandbox",
                                 "package(tilo-agent-session)": "video-render",
                                 "package(pam-tilo)": "pam-module"})

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
                if handover_user() is None:
                    # Where the tests run as another user, looking that user up starts more.
                    self.assertEqual(started, expected)

    def mapped(self, manifest: str) -> tuple[list[str], list[str]]:
        """The capabilities the runtime step requires for MANIFEST as the shell reads it, and
        the ones tools/ci/capability_filter.py reads there."""
        driver = Path(f"/usr/share/vulkan/icd.d/lvp_icd.{os.uname().machine}.json")
        with tempfile.TemporaryDirectory() as tmp:
            tree = Path(tmp) / "tree"
            (tree / "tools/ci").mkdir(parents=True)
            (tree / ".config").mkdir()
            for name in ("test-env-setup.sh", "as-test-user.sh", "ci/capability_filter.py"):
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
                             (ROOT / "crates/sdk/ui-core/assets/fonts/manifest.toml").read_bytes())

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

    def test_a_handed_render_helper_is_named_to_the_tests_and_not_built(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            helper = work / "tilo-video-render"
            helper.write_text("#!/bin/sh\n")
            helper.chmod(0o755)
            result, provided, given = self.handed_step(
                work, "video-render",
                **{"NEXTEST_BIN_EXE_tilo-video-render": str(helper)})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(provided, {"TILO_VIDEO_RENDER": str(helper)})
            self.assertEqual(given, [], "the job's helper, not one built here")

    def test_a_render_helper_not_handed_is_built(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            result, provided, given = self.handed_step(work, "video-render")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(given, ["build --locked -p tilo-video-render --bin tilo-video-render"])
            self.assertEqual(provided,
                             {"TILO_VIDEO_RENDER": f"{work}/target/debug/tilo-video-render"})

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

    def test_a_handed_program_that_cannot_run_fails_instead_of_building(self) -> None:
        # Building here would hide a broken handoff and compile the program in every shard again.
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            result, provided, given = self.handed_step(
                work, "video-render",
                **{"NEXTEST_BIN_EXE_tilo-video-render": str(work / "not-executable")})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("NEXTEST_BIN_EXE_tilo-video-render", result.stderr)
            self.assertEqual((provided, given), ({}, []))


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
        # The test user reaches the tree through this directory.
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

    def test_started_as_root_a_test_runs_as_the_test_user_in_a_tree_that_is_its_own(self) -> None:
        user = handover_user()
        if user is None:
            self.skipTest("only a job that starts as root in an image naming its test user hands over")
        # What the test is: who it runs as, whose its environment is, and that it can write
        # beside the build's output and into the build directory's tmp, which root made.
        report = ('set -e; id -u; stat -c "%u %a" "$TILO_TEST_ENVIRONMENT" "$HOME" "$XDG_RUNTIME_DIR"; '
                  ': >"$1/debug/review/written"; : >"$1/tmp/written"; echo "$TILO_TEST_ENVIRONMENT"')
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            target, binary = self.build_tree(work, report)
            programs, log = recorders(work)
            path = f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}"
            mine = [str(user.pw_uid), f"{user.pw_uid} 700", f"{user.pw_uid} {default_mode()}",
                    f"{user.pw_uid} 700"]

            result = self.start(binary, str(target), PATH=path, TILO_TEST_USER=user.pw_name)
            self.assertEqual(result.returncode, 0, result.stderr)
            lines = result.stdout.splitlines()
            self.assertEqual(lines[:4], mine)
            self.assertFalse(Path(lines[4]).exists(), "the environment is removed")
            self.assertTrue((target / f".handed-to-{user.pw_name}").exists(), "the job's mark")
            for made in (target, target / "debug/review", target / "tmp", binary):
                self.assertEqual(made.stat().st_uid, user.pw_uid, made)
            # The first start of a job walks the tree, under the lock its other starts wait on.
            cold = self.started(log)
            self.assertEqual(cold, Counter(flock=1, chown=2, mktemp=1, mkdir=1, chmod=1, setpriv=1, rm=1))

            # Every later start finds the mark, and a tmp that is the test user's already: it
            # hands over the environment it made, and nothing else.
            result = self.start(binary, str(target), PATH=path, TILO_TEST_USER=user.pw_name)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(self.started(log), Counter(chown=1, mktemp=1, mkdir=1, chmod=1, setpriv=1, rm=1))

            # A build as root makes the tmp again after the job's walk: it goes over again.
            shutil.rmtree(target / "tmp")
            (target / "tmp").mkdir()
            self.assertEqual((target / "tmp").stat().st_uid, 0)
            result = self.start(binary, str(target), PATH=path, TILO_TEST_USER=user.pw_name)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((target / "tmp").stat().st_uid, user.pw_uid)
            self.assertEqual(self.started(log)["chown"], 2)

            # A shell told another user id by its environment still knows it started as root.
            result = self.start(binary, str(target), PATH=path, TILO_TEST_USER=user.pw_name,
                                EUID=str(user.pw_uid), UID=str(user.pw_uid))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.splitlines()[:4], mine)

    def test_a_test_that_says_it_needs_root_stays_root(self) -> None:
        user = handover_user()
        if user is None:
            self.skipTest("only a job that starts as root in an image naming its test user hands over")
        report = 'id -u; stat -c "%u %a" "$TILO_TEST_ENVIRONMENT" "$HOME" "$XDG_RUNTIME_DIR"'
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            target, binary = self.build_tree(work, report)
            result = self.start(binary, TILO_TEST_USER=user.pw_name, TILO_TEST_AS_ROOT="1")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.splitlines(), ["0", "0 700", f"0 {default_mode()}", "0 700"])
            self.assertEqual(target.stat().st_uid, 0, "the tree stays root's")
            self.assertEqual(list(target.glob(".handed-to-*")), [])


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


class TestUserTests(unittest.TestCase):
    """tools/as-test-user.sh gives the test user the build tree only in the image that has one."""

    def run_helper(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": os.environ.get("HOME", "/"),
        }
        return subprocess.run(["bash", str(ROOT / "tools/as-test-user.sh"), *arguments], env=env,
                              capture_output=True, text=True, check=False)

    def test_a_developers_own_tree_is_left_to_them(self) -> None:
        # A developer's account is the test user: handing the tree over must do nothing,
        # and must not chown it to an empty owner.
        with tempfile.TemporaryDirectory() as work:
            tree = Path(work) / "target"
            (tree / "debug").mkdir(parents=True)
            owner = (tree.stat().st_uid, tree.stat().st_gid)
            result = self.run_helper("--own-tree", str(tree))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((tree.stat().st_uid, tree.stat().st_gid), owner)
            result = self.run_helper("--own", str(tree))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((tree.stat().st_uid, tree.stat().st_gid), owner)

    def test_who_started_the_job_is_not_what_the_environment_says(self) -> None:
        # bash's EUID answers without an `id`, but only bash's own. One the environment brings
        # would make root of an ordinary account, or an ordinary account of root, whose tests
        # would then run as root.
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": os.environ.get("HOME", "/")}
        helper = ["bash", str(ROOT / "tools/as-test-user.sh"), "id", "-u"]
        if os.geteuid() != 0:
            # Told it is root, an ordinary account still runs the command as itself.
            result = subprocess.run(helper, env=env | {"TILO_TEST_USER": "no-such-test-user",
                                                       "EUID": "0", "UID": "0"},
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), str(os.geteuid()))
            return
        # Told it is an ordinary account, root still hands the command over: it runs as the
        # test user, or not at all where this root may not become another user.
        user = handover_user()
        name = user.pw_name if user else "nobody"
        result = subprocess.run(helper, env=env | {"TILO_TEST_USER": name, "EUID": "4242", "UID": "4242"},
                                capture_output=True, text=True, check=False)
        self.assertNotEqual(result.stdout.strip(), "0", "the command ran as root")
        if user is not None:
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), str(user.pw_uid))

    def test_the_helpers_read_into_a_shell_only_define_their_functions(self) -> None:
        # tools/cargo-runner.sh reads both into its own shell: they must start nothing there,
        # and leave the shell's arguments alone.
        script = ('set -euo pipefail; source "$0/tools/with-test-environment.sh"; '
                  'echo "$# $1 ${TILO_TEST_ENVIRONMENT:-none} [$tilo_test_user]"; '
                  'declare -F tilo_test_environment_enter tilo_test_user_exec tilo_test_user_own_tree')
        result = subprocess.run(["bash", "-c", script, str(ROOT), "--check"], capture_output=True,
                                text=True, check=False,
                                env={"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": "/"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [
            "1 --check none []", "tilo_test_environment_enter", "tilo_test_user_exec",
            "tilo_test_user_own_tree"])


if __name__ == "__main__":
    unittest.main()
