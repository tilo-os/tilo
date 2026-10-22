"""tools/with-test-environment.sh gives every test run the same isolated environment."""
from __future__ import annotations

import json
import re
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/with-test-environment.sh"
RUNNER = ROOT / "tools/cargo-runner.sh"
HELPER = ROOT / "tools/desktop-isolation.sh"
DUMP = "import json, os; print(json.dumps(dict(os.environ)))"


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


if __name__ == "__main__":
    unittest.main()
