"""Unit tests for the app checks' process attribution and first-frame wait.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance.checks.apps import (FLAT_STDDEV, AppLaunch, Proc, app_launch,  # noqa: E402
                                    first_textured, lingering, parse_procs)

USER = "0::/user.slice/user-1001.slice/user@1001.service"
COMPOSITOR = f"{USER}/session.slice/tilo-session.service"


def row(pid, ppid, sid, cmd, cgroup=COMPOSITOR):
    return f"{pid}\tS {ppid} {sid} {sid} 0 -1 4194560\t{cgroup} \t{cmd} "


# The session before an open: the user manager, the compositor and the shared session bus.
BEFORE = [
    row(900, 1, 900, "/usr/lib/systemd/systemd --user", f"{USER}/init.scope"),
    row(1000, 900, 1000, "/usr/bin/tilo-shell --session", COMPOSITOR),
    row(1010, 1000, 1000, "/usr/bin/xwayland-satellite :4152 -listenfd 111", COMPOSITOR),
    row(2142, 900, 2142, "/usr/bin/dbus-broker-launch --scope user", f"{USER}/session.slice/dbus-broker.service"),
]
# A Terminal open: the launcher the compositor started, its bwrap sandbox (a session of its
# own), and portals the app's D-Bus traffic woke up, which the user manager owns.
TERMINAL = BEFORE + [
    row(3600, 1000, 1000, "/usr/libexec/tilo-launcher run --manifest /x --app-id so.tilo.terminal"),
    row(3601, 3600, 1000, "/usr/bin/xdg-dbus-proxy unix:path=/run/user/1001/bus"),
    row(3602, 3600, 3602, "bwrap --new-session --unshare-pid"),
    row(3603, 3602, 3602, "/app/bin/terminal"),
    row(3604, 3603, 3604, "/bin/bash -l"),
    row(3732, 900, 3732, "/usr/libexec/xdg-desktop-portal", f"{USER}/app.slice/xdg-desktop-portal.service"),
    row(3738, 900, 3738, "/usr/libexec/xdg-permission-store", f"{USER}/app.slice/xdg-permission-store.service"),
    row(3743, 900, 3743, "/usr/libexec/xdg-document-portal", f"{USER}/app.slice/xdg-document-portal.service"),
    row(3750, 900, 3750, "/usr/bin/fs recipient-service --socket /run/user/1001/tilo/services/tilo-fs.sock",
        f"{USER}/app.slice/tilo-fs-recipient.service"),
]
WINE_SCOPE = f"{USER}/app.slice/app-tilo-wine\\x2dwinefile-4100.scope"
# A host launch: the launcher joined its own scope, `setsid`ed the app, and wineserver
# double-forked out of the tree into the user manager while staying in the app's session.
WINE = BEFORE + [
    row(4100, 1000, 1000, "/usr/libexec/tilo-launcher run-host --desktop-id wine-winefile", WINE_SCOPE),
    row(4101, 4100, 4101, "wine winefile.exe", WINE_SCOPE),
    row(4120, 900, 4101, "/usr/bin/wineserver", WINE_SCOPE),
    row(4130, 900, 4130, "C:\\windows\\system32\\services.exe", WINE_SCOPE),
]


def procs(rows):
    return parse_procs("\n".join(rows))


class ParseProcsTests(unittest.TestCase):
    def test_rows_keep_parent_session_cgroup_and_command(self):
        parsed = procs(TERMINAL)
        self.assertEqual(parsed[3603], Proc("/app/bin/terminal", 3602, 3602, COMPOSITOR))
        self.assertEqual(parsed[3600].exe, "tilo-launcher")

    def test_kernel_threads_and_vanished_processes_are_skipped(self):
        parsed = parse_procs("2\tS 0 0 0\t\t\n77\t\t\t\nnot a row\n" + row(5, 1, 5, "init"))
        self.assertEqual(list(parsed), [5])


class AttributionTests(unittest.TestCase):
    def test_session_services_an_app_woke_up_are_not_the_apps(self):
        before, now = procs(BEFORE), procs(TERMINAL)
        launch = app_launch(now, before, "so.tilo.terminal")
        self.assertEqual(launch.roots, {3600})
        self.assertEqual(sorted(launch.members(now)), [3600, 3601, 3602, 3603, 3604])

    def test_a_shell_the_app_started_still_counts_after_the_close(self):
        before, now = procs(BEFORE), procs(TERMINAL)
        launch = app_launch(now, before, "so.tilo.terminal")
        owned = launch.members(now)
        # The launcher and sandbox exited, the shell was reparented to the user manager.
        after = procs(BEFORE + [row(3604, 900, 3604, "/bin/bash -l"),
                                row(3738, 900, 3738, "/usr/libexec/xdg-permission-store")])
        self.assertEqual(list(lingering(after, launch, owned)), [3604])

    def test_a_clean_close_leaves_nothing_even_with_portals_running(self):
        before, now = procs(BEFORE), procs(TERMINAL)
        launch = app_launch(now, before, "so.tilo.terminal")
        after = procs([r for r in TERMINAL if r.split("\t")[0] in ("900", "1000", "1010", "2142", "3738", "3743")])
        self.assertEqual(lingering(after, launch, launch.members(now)), {})

    def test_a_reused_pid_with_another_command_is_not_the_app(self):
        before, now = procs(BEFORE), procs(TERMINAL)
        launch = app_launch(now, before, "so.tilo.terminal")
        after = procs(BEFORE + [row(3603, 900, 3603, "/usr/bin/sleep 5")])
        self.assertEqual(lingering(after, launch, launch.members(now)), {})

    def test_host_launch_owns_its_scope_and_session_beyond_its_tree(self):
        before, now = procs(BEFORE), procs(WINE)
        launch = app_launch(now, before, "wine-winefile")
        self.assertEqual(launch.roots, {4100})
        self.assertEqual(launch.sessions, {4101, 4130})
        self.assertEqual(sorted(launch.members(now)), [4100, 4101, 4120, 4130])
        # The supervisor is gone, only the scope still names the Wine prefix's services.
        after = procs(BEFORE + [WINE[-1]])
        self.assertEqual(list(lingering(after, launch, {})), [4130])

    def test_a_scope_of_another_launch_is_not_this_ones(self):
        launch = AppLaunch(frozenset({41}), frozenset())
        other = Proc("x", 900, 7, f"{USER}/app.slice/app-tilo-foot-4100.scope")
        self.assertFalse(launch.owns(7, other, {7: other}))

    def test_a_launch_of_another_package_during_the_open_is_not_the_apps(self):
        now = procs(TERMINAL + [
            row(3700, 1000, 1000, "/usr/libexec/tilo-launcher run --app-id so.tilo.input-method --service"),
            row(3701, 3700, 3701, "bwrap --new-session"),
        ])
        launch = app_launch(now, procs(BEFORE), "so.tilo.terminal")
        self.assertEqual(launch.roots, {3600})
        self.assertNotIn(3701, launch.members(now))

    def test_launchers_already_running_before_the_open_are_not_its_launch(self):
        before = procs(TERMINAL)
        self.assertEqual(app_launch(procs(TERMINAL), before, "so.tilo.terminal").roots, frozenset())

    def test_a_parent_cycle_ends_the_walk(self):
        table = {2: Proc("b", 3, 50), 3: Proc("c", 2, 50)}
        self.assertFalse(AppLaunch(frozenset({99}), frozenset()).owns(3, table[3], table))


class FirstTexturedTests(unittest.TestCase):
    def run_samples(self, values, timeout=5.0, interval=0.5):
        now = [0.0]
        taken = []

        def sample():
            taken.append(now[0])
            return {"stddev": values[min(len(taken), len(values)) - 1]}

        def sleep(s):
            now[0] += s

        stats, attempts = first_textured(sample, timeout, interval, clock=lambda: now[0], sleep=sleep)
        return stats["stddev"], attempts, taken

    def test_waits_past_a_flat_clear_for_the_first_real_buffer(self):
        stddev, attempts, _ = self.run_samples([0.009, 0.009, 0.03])
        self.assertEqual((stddev, attempts), (0.03, 3))

    def test_an_already_textured_frame_is_taken_at_once(self):
        self.assertEqual(self.run_samples([0.05])[:2], (0.05, 1))

    def test_a_blank_client_stays_flat_and_the_wait_is_bounded(self):
        stddev, attempts, taken = self.run_samples([0.0], timeout=2.0)
        self.assertLessEqual(stddev, FLAT_STDDEV)
        self.assertEqual(attempts, 5)
        self.assertEqual(taken[-1], 2.0)

    def test_the_threshold_itself_is_still_flat(self):
        self.assertEqual(self.run_samples([FLAT_STDDEV], timeout=0.0)[:2], (FLAT_STDDEV, 1))


if __name__ == "__main__":
    unittest.main()
