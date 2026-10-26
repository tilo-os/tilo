"""Unit tests for the app checks' process attribution, first-frame wait, startup timing,
folder listing and GPU adapter verdicts.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import core, gpu  # noqa: E402
from acceptance.vm import TiloVm, VmError  # noqa: E402
from acceptance.checks import apps, daily_apps  # noqa: E402
from acceptance.checks.apps import (FLAT_STDDEV, AppLaunch, Proc, app_launch,  # noqa: E402
                                    first_textured, lingering, parse_procs)
from acceptance.checks.startup import (ATSPI_CLIENT, ATSPI_PROBE, GUEST_ATSPI_CLIENT,  # noqa: E402
                                       first_frame_budget, judge_listing, kpi_p95_target, open_segments, p95, parse_launch_latencies,
                                       parse_launch_latency, parse_startup_marks, segment_series)
import gpu_check  # noqa: E402
import run  # noqa: E402

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


class ChromiumPlatformTests(unittest.TestCase):
    def probe(self, commands, flag):
        output = "\n".join(row(5000 + i, 1000, 5000 + i, cmd)
                           for i, cmd in enumerate(commands))
        ctx = SimpleNamespace(vm=SimpleNamespace(exec=lambda script: output))
        res = SimpleNamespace(metrics={})
        daily_apps._ozone_flag(ctx, res, flag)
        return res.metrics[f"ozone_{flag}_pids"]

    def test_probe_shell_and_unrelated_flags_cannot_pass_as_chromium(self):
        for cmd in (
            "sh -c pgrep -f -- '--ozone-platform=wayland' | head -3; true",
            "/usr/bin/not-a-browser --ozone-platform=wayland",
            "/usr/bin/chromium-browser --ozone-platform=x11",
            "/usr/bin/chromium-browser --ozone-platform=wayland-extra",
        ):
            with self.subTest(cmd=cmd), self.assertRaises(apps.CheckFailed):
                self.probe([cmd], "wayland")

    def test_browser_argv_reports_the_requested_platform(self):
        for flag in ("wayland", "x11"):
            with self.subTest(flag=flag):
                self.assertEqual(self.probe([
                    f"/usr/lib64/chromium-browser/chromium-browser --ozone-platform={flag}",
                ], flag), ["5000"])

    def test_process_query_failure_preserves_cause(self):
        def unavailable(script):
            raise VmError("slot exec unavailable")

        ctx = SimpleNamespace(vm=SimpleNamespace(exec=unavailable))
        with self.assertRaisesRegex(VmError, "slot exec unavailable"):
            daily_apps._ozone_flag(ctx, SimpleNamespace(metrics={}), "wayland")


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



def undo_toast(title, body="Select to undo, or press Ctrl+Shift+T later", **fields):
    return {"title": title, "body": body, "severity": "Info", "persistent": False, **fields}


class UndoCloseTests(unittest.TestCase):
    def test_only_captured_titles_match_independent_of_reopen_binding(self):
        titles = {"Wine File", "Wine Mono Installer"}
        for heading, body in [
            ("Closed Wine File", "Select to undo, or press Ctrl+Shift+T later"),
            ("Closed Wine Mono Installer", "Select to undo, or press Alt+R later"),
            ("Closed Wine Mono Installer", "Select to undo"),
            ("Wine Mono Installer geschlossen", "Zum Rückgängigmachen auswählen oder später Strg+Umschalt+T drücken"),
            ("Wine File geschlossen", "Zum Rückgängigmachen auswählen"),
        ]:
            with self.subTest(heading=heading, body=body):
                self.assertTrue(any(apps._is_undo_close(undo_toast(heading, body), title) for title in titles))

    def test_unrelated_titles_and_non_undo_messages_are_rejected(self):
        for heading, body in [
            ("Closed Notes", "Select to undo, or press Super+Shift+T later"),
            ("Closed Wine File backup", "Select to undo"),
            ("Wine File crashed", "Select to undo, or press Super+Shift+T later"),
            ("Closed Wine File", "Wine File crashed"),
            ("Wine File updated", "Select to undo"),
        ]:
            with self.subTest(heading=heading, body=body):
                self.assertFalse(apps._is_undo_close(undo_toast(heading, body), "Wine File"))

    def test_errors_and_persistent_toasts_are_rejected_even_for_closed_titles(self):
        for fields in [{"severity": "Error"}, {"severity": "Warning"}, {"persistent": True}]:
            with self.subTest(fields=fields):
                self.assertFalse(apps._is_undo_close(undo_toast("Closed Wine File", **fields), "Wine File"))

    def test_missing_or_empty_titles_cannot_match_a_reopen_hint(self):
        for title in [None, "", "   "]:
            with self.subTest(title=title):
                self.assertFalse(apps._is_undo_close(undo_toast("Closed ", "Select to undo, or press Super+Shift+T later"), title))
        for heading in [None, ""]:
            self.assertFalse(apps._is_undo_close(undo_toast(heading), "Wine File"))


class CloseFixture:
    """VM boundary fixture; run the production close and process/window checks."""
    def __init__(self, extra_toast=None, cascade=False, processes="", keep_main=False):
        self.groups = [{"id": 1, "tabs": [{"title": "Existing"}]},
                       {"id": 2, "tabs": [{"title": "Wine File", "tile": {"Foreign": {"app_id": "wineboot.exe"}}}]},
                       {"id": 3, "tabs": [{"title": "Wine Mono Installer", "tile": {"Foreign": {"app_id": "control.exe"}}}]}]
        self.toasts = [undo_toast("Old notification")]
        self.extra_toast = extra_toast
        self.cascade = cascade
        self.processes = processes
        self.keep_main = keep_main
        self.facts = {"groups_before:A07-x11": {1},
                      "launch:A07-x11": AppLaunch(frozenset({4100}), frozenset({4101}))}
        self.vm = self

    def state(self):
        return {"workspace_tree": {"workspaces": [{"strips": [{"window_groups": self.groups}]}]},
                "chrome": {"notifications": self.toasts}}

    def invoke(self, provider, action):
        gid = int(action.split(":")[1])
        closing = [g for g in self.groups if g["id"] == gid or self.cascade and g["id"] == 3]
        if self.keep_main and gid == 2:
            return
        self.groups = [g for g in self.groups if g not in closing]
        for group in closing:
            self.toasts.append(undo_toast("Closed " + group["tabs"][0]["title"]))
        if gid == 2 and self.extra_toast:
            self.toasts.append(self.extra_toast)

    def wait_state(self, predicate, timeout, why):
        apps.expect(predicate(self.state()), why)

    def exec(self, script):
        installer = any(g["id"] == 3 for g in self.groups)
        return self.processes + ("\n" + row(4131, 900, 4101, "C:\\windows\\system32\\control.exe appwiz.cpl install_mono")
                                 if installer else "")


class CloseNotificationRegressionTests(unittest.TestCase):
    def close(self, ctx):
        res = SimpleNamespace(metrics={})
        with patch.object(apps, "CLOSE_GRACE_S", 0):
            apps._close(ctx, res, "A07-x11", 2, {}, 1)
        return res.metrics

    def test_main_and_first_run_dialog_undo_toasts_pass(self):
        for cascade in [False, True]:
            with self.subTest(cascade=cascade):
                ctx = CloseFixture(cascade=cascade)
                try:
                    metrics = self.close(ctx)
                except apps.CheckFailed as exc:
                    self.fail(str(exc))
                self.assertEqual(len(metrics["undo_toasts"]), 2)
                self.assertEqual(metrics["toasts"], [])
                self.assertEqual(metrics["extra_windows"], 1)
                self.assertEqual([g["id"] for g in ctx.groups], [1])

    def test_unrelated_errors_persistent_and_crash_notifications_still_fail(self):
        for toast in [undo_toast("Closed Notes", "Select to undo, or press Super+Shift+T later"),
                      undo_toast("Closed Wine File", severity="Error"),
                      undo_toast("Closed Wine Mono Installer", persistent=True),
                      undo_toast("Wine File crashed", "Exited unexpectedly")]:
            with self.subTest(toast=toast):
                with self.assertRaisesRegex(apps.CheckFailed, "unexpected notifications"):
                    self.close(CloseFixture(extra_toast=toast))

    def test_lingering_app_process_still_fails(self):
        ctx = CloseFixture(processes=row(4101, 900, 4101, "wine winefile.exe"))
        with self.assertRaisesRegex(apps.CheckFailed, "processes still running"):
            self.close(ctx)

    def test_main_window_that_lingers_still_fails(self):
        with self.assertRaisesRegex(apps.CheckFailed, "window gone after close"):
            self.close(CloseFixture(keep_main=True))


class LateWindowCloseTests(unittest.TestCase):
    close = CloseNotificationRegressionTests.close
    def test_installer_appearing_at_main_close_guard_is_closed_and_classified(self):
        class LateInstaller(CloseFixture):
            def __init__(self):
                super().__init__()
                self.installer = self.groups.pop()
                self.reads = 0

            def state(self):
                self.reads += 1
                if self.reads == 2:
                    self.groups.append(self.installer)
                return super().state()

        ctx = LateInstaller()
        try:
            metrics = self.close(ctx)
        except apps.CheckFailed as exc:
            self.fail(str(exc))
        self.assertEqual([g["id"] for g in ctx.groups], [1])
        self.assertEqual(metrics["closed_titles"], ["Wine File", "Wine Mono Installer"])
        self.assertEqual([t["title"] for t in metrics["undo_toasts"]],
                         ["Closed Wine File", "Closed Wine Mono Installer"])
        self.assertEqual(metrics["lingering_after_close"], [])

    def test_new_unrelated_and_preexisting_same_app_windows_are_left_open(self):
        ctx = CloseFixture()
        ctx.groups[0]["tabs"][0]["tile"] = {"Foreign": {"app_id": "control.exe"}}
        ctx.groups.append({"id": 4, "tabs": [{"title": "Unrelated editor", "tile": {"Foreign": {"app_id": "gedit"}}}]})
        metrics = self.close(ctx)
        self.assertEqual([g["id"] for g in ctx.groups], [1, 4])
        self.assertEqual(metrics["closed_titles"], ["Wine File", "Wine Mono Installer"])

    def test_ambiguous_app_identity_is_not_closed(self):
        ctx = CloseFixture(processes=row(5001, 900, 5001, "C:\\windows\\system32\\control.exe unrelated-prefix"))
        with self.assertRaisesRegex(apps.CheckFailed, "processes still running"):
            self.close(ctx)
        self.assertEqual([g["id"] for g in ctx.groups], [1, 3])

    def test_group_containing_an_unrelated_tab_is_not_closed(self):
        ctx = CloseFixture()
        ctx.groups[2]["tabs"].append({"title": "Unrelated editor", "tile": {"Foreign": {"app_id": "gedit"}}})
        with self.assertRaisesRegex(apps.CheckFailed, "processes still running"):
            self.close(ctx)
        self.assertEqual([g["id"] for g in ctx.groups], [1, 3])

    def test_queued_installer_is_revalidated_at_its_close_guard(self):
        for transition in ["unrelated_tab", "ambiguous_process"]:
            class ChangedInstaller(CloseFixture):
                def __init__(self):
                    super().__init__()
                    self.reads = 0
                    self.invoked = []

                def state(self):
                    self.reads += 1
                    if self.reads == 4:
                        if transition == "unrelated_tab":
                            self.groups[-1]["tabs"].append({"title": "Editor", "tile": {"Foreign": {"app_id": "gedit"}}})
                        else:
                            self.processes = row(5001, 900, 5001, "C:\\windows\\system32\\control.exe unrelated-prefix")
                    return super().state()

                def invoke(self, provider, action):
                    self.invoked.append(action)
                    super().invoke(provider, action)

            with self.subTest(transition=transition):
                ctx = ChangedInstaller()
                with self.assertRaisesRegex(apps.CheckFailed, "processes still running"):
                    self.close(ctx)
                self.assertEqual(ctx.invoked, ["group.close:2"])
                self.assertEqual([g["id"] for g in ctx.groups], [1, 3])

    def test_invoked_close_keeps_title_while_foreign_group_removal_is_delayed(self):
        class DelayedRemoval(CloseFixture):
            def __init__(self):
                super().__init__()
                self.pending = None
                self.reads_after_close = 0

            def invoke(self, provider, action):
                if action == "group.close:3":
                    self.pending = self.groups[-1]
                    self.toasts.append(undo_toast("Closed Wine Mono Installer"))
                else:
                    super().invoke(provider, action)

            def state(self):
                if self.pending:
                    self.reads_after_close += 1
                    if self.reads_after_close == 2:
                        self.groups.remove(self.pending)
                        self.pending = None
                return super().state()

            def exec(self, script):
                return "" if self.pending else super().exec(script)

        metrics = self.close(DelayedRemoval())
        self.assertEqual(metrics["closed_titles"], ["Wine File", "Wine Mono Installer"])
        self.assertEqual(len(metrics["undo_toasts"]), 2)
        self.assertEqual(metrics["lingering_after_close"], [])

    def test_owned_multitab_group_uses_exact_localized_group_toast(self):
        for heading, body in [("Closed 2 tabs", "Select to undo"),
                              ("2 Tabs geschlossen", "Zum Rückgängigmachen auswählen oder später Strg+Umschalt+T drücken")]:
            class GroupToast(CloseFixture):
                def invoke(self, provider, action):
                    super().invoke(provider, action)
                    if action == "group.close:3":
                        self.toasts[-1] = undo_toast(heading, body)

            with self.subTest(heading=heading):
                ctx = GroupToast()
                ctx.groups[2]["tabs"].append({"title": "Second installer", "tile": {"Foreign": {"app_id": "control.exe"}}})
                metrics = self.close(ctx)
                self.assertEqual(len(metrics["undo_toasts"]), 2)
                self.assertEqual(metrics["closed_titles"], ["Wine File", "Wine Mono Installer", "Second installer"])
                self.assertEqual([g["id"] for g in ctx.groups], [1])

    def test_group_toasts_with_wrong_count_errors_or_persistence_fail(self):
        for toast in [undo_toast("Closed 3 tabs"), undo_toast("Closed 2 tabs", severity="Error"),
                      undo_toast("Closed 2 tabs", persistent=True)]:
            with self.subTest(toast=toast):
                with self.assertRaisesRegex(apps.CheckFailed, "unexpected notifications"):
                    self.close(CloseFixture(extra_toast=toast))

    def test_continually_created_app_windows_fail_within_the_close_budget(self):
        class Reopening(CloseFixture):
            def invoke(self, provider, action):
                super().invoke(provider, action)
                next_id = max(g["id"] for g in self.groups) + 1
                if next_id > 30:
                    raise AssertionError("close loop exceeded the fixture safety limit")
                self.groups.append({"id": next_id, "tabs": [{"title": "New installer", "tile": {"Foreign": {"app_id": "control.exe"}}}]})

            def exec(self, script):
                return row(4131, 900, 4101, "C:\\windows\\system32\\control.exe")

        tick = iter(range(0, 100, 2))
        with patch.object(apps.time, "monotonic", lambda: next(tick)):
            with self.assertRaisesRegex(apps.CheckFailed, "while closing A07-x11"):
                self.close(Reopening())


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


class FullscreenTests(unittest.TestCase):
    """Super+F on glxgears: its lone tile already spans 1856 of the output's 1920 px."""
    TILE = {"x": 32.0, "y": 80.0, "w": 1856.0, "h": 988.0}
    FULL = {"x": 0.0, "y": 0.0, "w": 1920.0, "h": 1080.0}

    def run_fullscreen(self, full_rect, restored_rect):
        gid, sid = 7, 70
        phase = {"fs": False, "rect": self.TILE}

        def frames():
            r = phase["rect"]
            return [{"surface_id": sid, "rect": r, "buffer": {"w": int(r["w"]), "h": int(r["h"])}}]

        def key(combo):
            phase["fs"] = not phase["fs"]
            phase["rect"] = full_rect if phase["fs"] else restored_rect

        def state():
            return {"chrome": {"displays": [{"id": 0, "groups": [gid]}],
                               "fullscreen_group": gid if phase["fs"] else None},
                    "workspace_tree": {"outputs": {"displays": [{"id": 0, "x": 0, "y": 0,
                                                                 "width": 1920, "height": 1080}]},
                                       "workspaces": []}}

        vm = SimpleNamespace(foreign_frames=frames, key=key, state=state)

        def wait_state(predicate, timeout, what):
            s = state()
            if not predicate(s):
                raise core.CheckFailed(f"{what} not reached")
            return s

        ctx = SimpleNamespace(vm=vm, state=state, wait_state=wait_state, shot=lambda label: label)
        res = SimpleNamespace(metrics={})
        with patch.object(daily_apps, "wait_until", lambda predicate, *_: predicate() or None), \
                patch.object(core, "load_image", lambda path: None), \
                patch.object(core, "region_stats", lambda img, rect: {"mean": 0.1, "stddev": 0.15}):
            daily_apps._fullscreen(ctx, res, daily_apps.GLXGEARS, sid, gid)
        return res.metrics

    def test_full_screen_and_back_to_a_tile_that_spans_most_of_the_output(self):
        metrics = self.run_fullscreen(self.FULL, self.TILE)
        self.assertEqual(metrics["restored_rect"], self.TILE)

    def test_a_window_left_at_its_tile_size_is_not_full_screen(self):
        with self.assertRaisesRegex(core.CheckFailed, "does not cover output"):
            self.run_fullscreen(self.TILE, self.TILE)

    def test_a_window_that_stays_full_screen_is_not_back_in_its_tile(self):
        with self.assertRaisesRegex(core.CheckFailed, "back in a tile"):
            self.run_fullscreen(self.FULL, self.FULL)

ROOMS = "/home/tds-a/.local/share/tilo-rooms/personal/so.tilo.file-explorer/.local/state"


def marks_line(name, origin, events):
    doc = {"origin_epoch_ms": origin, "events": [{"name": n, "ns": ns} for n, ns in events]}
    return f"{ROOMS}/{name}\t{json.dumps(doc)}"


class StartupMarksTests(unittest.TestCase):
    def test_the_launch_after_the_open_is_read_in_seconds(self):
        text = "\n".join([
            marks_line("startup-1.json", 1_000, [("main-start", 0), ("first-frame-presented", 900_000_000)]),
            marks_line("startup-2.json", 5_000, [("main-start", 0), ("first-frame-presented", 2_100_000_000)]),
        ])
        got = parse_startup_marks(text, 4_000)
        self.assertEqual(got["path"], f"{ROOMS}/startup-2.json")
        self.assertEqual(got["events"]["first-frame-presented"], 2.1)

    def test_a_relaunch_after_it_does_not_replace_the_first_launch(self):
        text = "\n".join([
            marks_line("startup-3.json", 9_000, [("first-frame-presented", 100_000_000)]),
            marks_line("startup-2.json", 5_000, [("first-frame-presented", 400_000_000)]),
        ])
        self.assertEqual(parse_startup_marks(text, 4_000)["origin_epoch_ms"], 5_000)

    def test_launches_before_the_open_and_partial_files_are_ignored(self):
        text = "\n".join([marks_line("startup-1.json", 1_000, [("first-frame-presented", 1)]),
                          f'{ROOMS}/startup-2.json\t{{"origin_epoch_ms": 50', "receipt: ok"])
        self.assertIsNone(parse_startup_marks(text, 4_000))

    def test_a_launch_still_starting_has_no_first_frame_yet(self):
        got = parse_startup_marks(marks_line("startup-1.json", 5_000, [("main-start", 0)]), 4_000)
        self.assertNotIn("first-frame-presented", got["events"])

    def test_a_file_from_before_launcher_marks_reads_as_unsplit(self):
        got = parse_startup_marks(marks_line("startup-1.json", 5_000, [("first-frame-presented", 1)]), 4_000)
        self.assertEqual((got["origin_epoch_us"], got["launcher"], got["exec_to_main_us"]),
                         (5_000_000, [], None))
        self.assertIsNone(open_segments(4_000, got))


# One warm open: requested at 10 000 ms, the launcher ran 10 120–10 190 ms, bubblewrap
# until 10 200 ms, the Landlock shim exec'd the app at 10 215 ms, its main ran at
# 10 230.5 ms and its first frame 400 ms later.
SPLIT_OPEN = {
    "origin_epoch_ms": 10_230, "origin_epoch_us": 10_230_500,
    "launcher": [{"name": "launcher-start", "epoch_us": 10_120_000},
                 {"name": "package-verified", "epoch_us": 10_150_000},
                 {"name": "dbus-proxies-ready", "epoch_us": 10_175_000},
                 {"name": "sandbox-argv-ready", "epoch_us": 10_190_000},
                 {"name": "sandbox-entered", "epoch_us": 10_200_000},
                 {"name": "sandbox-exec", "epoch_us": 10_215_000}],
    "events": [{"name": "app-launch-start", "ns": 10_000}, {"name": "first-frame-presented", "ns": 400_000_000}],
}


class OpenSegmentTests(unittest.TestCase):
    def marks(self):
        return parse_startup_marks(f"{ROOMS}/startup-9.json\t{json.dumps(SPLIT_OPEN)}", 10_000)

    def test_the_segments_add_up_to_the_open(self):
        got = open_segments(10_000, self.marks())
        self.assertEqual(got, {"request_to_launcher": 120.0, "launcher": 70.0, "sandbox_to_main": 40.5,
                               "main_to_first_frame": 400.0, "exec_to_main": 15.5,
                               "launcher_phases": {"package-verified": 30.0, "dbus-proxies-ready": 25.0,
                                                   "sandbox-argv-ready": 15.0, "sandbox-entered": 10.0,
                                                   "sandbox-exec": 15.0}})
        whole = sum(got[name] for name in ("request_to_launcher", "launcher", "sandbox_to_main",
                                           "main_to_first_frame"))
        self.assertAlmostEqual(whole, 630.5)

    def test_series_name_each_segment_once_per_open(self):
        open_ = {"segments": open_segments(10_000, self.marks()),
                 "compositor": {"request_to_spawn": 9.0, "spawn_to_map": 500.0}}
        unsplit = {"segments": None, "compositor": {"request_to_spawn": 8.0}}
        series = segment_series([open_, unsplit])
        self.assertEqual(series["request_to_launcher"], [120.0])
        self.assertEqual(series["launcher_phase.dbus-proxies-ready"], [25.0])
        self.assertEqual(series["exec_to_main"], [15.5])
        self.assertEqual(series["request_to_spawn"], [9.0, 8.0])
        self.assertNotIn("main_to_first_frame", series)

    def test_the_gpu_target_is_the_kpi_documents(self):
        self.assertEqual(kpi_p95_target("app.open.warm_ms.settings"), 300.0)
        self.assertIsNone(kpi_p95_target("app.open.launcher_ms.settings"))
        self.assertEqual(p95([float(v) for v in range(1, 11)]), 10.0)
        self.assertEqual(p95([float(v) for v in range(1, 21)]), 19.0)


class LaunchLatencyTests(unittest.TestCase):
    LINE = ("\x1b[2m2026-10-03T10:00:00Z\x1b[0m \x1b[32m INFO\x1b[0m tilo::launch_latency: "
            "application first frame presented package_id={pkg} surface_id=7 frame_seq=1 "
            "request_to_first_present_us=2140000 spawn_to_first_present_us=2001000")

    def test_reads_the_request_to_present_time_of_the_package(self):
        log = "\n".join([self.LINE.format(pkg="so.tilo.settings"), self.LINE.format(pkg="so.tilo.file-explorer")])
        self.assertEqual(parse_launch_latency(log, "so.tilo.file-explorer"), 2.14)

    def test_a_package_whose_id_extends_another_is_not_it(self):
        self.assertIsNone(parse_launch_latency(self.LINE.format(pkg="so.tilo.file-explorer2"),
                                               "so.tilo.file-explorer"))

    def test_absent_without_info_logging(self):
        self.assertIsNone(parse_launch_latency("", "so.tilo.file-explorer"))

    def test_every_compositor_segment_of_one_open(self):
        log = "\n".join([
            "INFO tilo::launch_latency: application launcher spawned package_id=so.tilo.notes pid=9 "
            "request_to_spawn_us=8400 dispatch_phases=preflight:0.3,identity:1.2,command:0.9,worker:3.0,"
            "spawned:1.5,registered:1.5",
            "INFO tilo::launch_latency: application toplevel mapped package_id=so.tilo.notes surface_id=3 "
            "request_to_map_us=610000 spawn_to_map_us=601600",
            self.LINE.format(pkg="so.tilo.notes"),
        ])
        self.assertEqual(parse_launch_latencies(log, "so.tilo.notes"),
                         {"request_to_spawn": 8.4, "spawn_to_map": 601.6,
                          "request_to_first_present": 2140.0,
                          "dispatch_phase.preflight": 0.3, "dispatch_phase.identity": 1.2,
                          "dispatch_phase.command": 0.9, "dispatch_phase.worker": 3.0,
                          "dispatch_phase.spawned": 1.5, "dispatch_phase.registered": 1.5})


class FirstFrameBudgetTests(unittest.TestCase):
    def test_hardware_gets_the_hardware_budget_and_a_vm_the_generous_one(self):
        self.assertEqual(first_frame_budget(run.BUDGETS, True)[0], 0.3)
        self.assertEqual(first_frame_budget(run.BUDGETS, False)[0], 3.0)

    def test_the_field_report_launch_fails_on_hardware(self):
        # A window that fell back to a CPU rasterizer took 2.1 s to its first frame.
        self.assertGreater(2.1, first_frame_budget(run.BUDGETS, True)[0])

    def test_both_checks_run_in_the_main_pass(self):
        ids = {c.id for c in run.core.REGISTRY}
        self.assertLessEqual({"A14", "R05"}, ids)
        self.assertLess(run.ORDER.index("A14"), run.ORDER.index("R02"))
        # R05 reads the adapter line of every app the run opened. K01 (it restarts the session
        # with the clock set back) and R08 (it signs seat0's greeter in) open none in the slot,
        # so only they and P03 may follow R05.
        self.assertEqual(run.ORDER[-4:], ["R05", "K01", "R08", "P03"])
        self.assertEqual(sorted(run.ORDER), sorted(ids - set(run.DE_PASS)))


class ListingTests(unittest.TestCase):
    def test_entries_of_home_pass(self):
        verdict, why = judge_listing({"root": True, "entries": ["Documents", "acc-files-1"]}, "acc-files-1")
        self.assertEqual(verdict, "pass")
        self.assertIn("home", why)

    def test_a_frame_without_a_listing_fails(self):
        self.assertEqual(judge_listing({"root": True, "entries": None, "names": []}, "m")[0], "fail")
        self.assertEqual(judge_listing({"root": True, "entries": []}, "m")[0], "fail")
        self.assertEqual(judge_listing({"root": False, "entries": None}, "m")[0], "fail")

    def test_an_unreadable_tree_is_unobservable(self):
        self.assertEqual(judge_listing({"error": "no AT-SPI bus"}, "m")[0], "unobservable")


class GuestAtspiProbeTests(unittest.TestCase):
    def probe(self, client, no_commands):
        proc = subprocess.run(
            [sys.executable, "-", "file-explorer", "Files activity", "Files", "0"],
            input=ATSPI_PROBE, text=True, capture_output=True, timeout=10,
            env={**os.environ, "TILO_ATSPI_CLIENT": str(client), "PATH": str(no_commands)})
        self.assertEqual(proc.returncode, 0, proc.stderr)
        return json.loads(proc.stdout)

    def test_the_guest_probe_loads_the_real_client(self):
        with tempfile.TemporaryDirectory() as empty:
            probe = self.probe(ATSPI_CLIENT, empty)
        self.assertTrue(probe["error"].startswith("no AT-SPI bus:"), probe)
        self.assertIn("gdbus", probe["error"])
        self.assertEqual(judge_listing(probe, "m")[0], "unobservable")

    def test_a_missing_client_reports_its_path(self):
        with tempfile.TemporaryDirectory() as empty:
            client = Path(empty) / "missing.py"
            probe = self.probe(client, empty)
        self.assertTrue(probe["error"].startswith(f"no AT-SPI client at {client}:"), probe)

    def test_an_existing_client_import_error_is_a_load_failure(self):
        with tempfile.TemporaryDirectory() as empty:
            client = Path(empty) / "broken.py"
            client.write_text("raise RuntimeError('broken client')\n")
            probe = self.probe(client, empty)
        self.assertIn("failed to load: RuntimeError: broken client", probe["error"])
        self.assertNotIn("no AT-SPI client", probe["error"])

    def test_a_missing_import_dependency_is_a_load_failure(self):
        with tempfile.TemporaryDirectory() as empty:
            client = Path(empty) / "dependency.py"
            client.write_text(f"open({str(Path(empty) / 'missing-data')!r})\n")
            probe = self.probe(client, empty)
        self.assertIn("failed to load: FileNotFoundError:", probe["error"])
        self.assertNotIn("no AT-SPI client", probe["error"])


class GuestClientCopyTests(unittest.TestCase):
    def test_the_client_reaches_the_guest_whole_in_commands_the_kernel_accepts(self):
        # The image ships no test tooling: A14 copies the checkout's client into the guest.
        with tempfile.TemporaryDirectory() as home:
            scripts = []

            class Vm(TiloVm):
                def exec(self, script, timeout=120, check=True):
                    scripts.append(script)
                    return subprocess.run(["sh", "-c", script], env={**os.environ, "HOME": home},
                                          check=True, capture_output=True, text=True).stdout

            Vm("tilo-vm", "test-vm", "test-slot", Path(home) / "log").write_home_file(
                GUEST_ATSPI_CLIENT, ATSPI_CLIENT.read_bytes())
            self.assertEqual((Path(home) / GUEST_ATSPI_CLIENT).read_bytes(), ATSPI_CLIENT.read_bytes())
            self.assertFalse(list(Path(home).rglob("*.acc-tmp")))
        self.assertTrue(all(len(script.encode()) < 128 * 1024 for script in scripts))


def adapter_line(app, kind, name="llvmpipe (LLVM 19.1.7, 256 bits)", backend="Vulkan", driver="llvmpipe"):
    return (f'[app {app} stderr] tilo-gpu: window adapter "{name}" type={kind} '
            f'backend={backend} driver="{driver}"')


OK = '{"status": "ok", "problems": [], "render_nodes": ["/dev/dri/renderD128"]}'
GPU_LOG = "\n".join([
    "INFO shell: started",
    adapter_line("so.tilo.file-explorer", "VirtualGpu", "virgl (AMD Radeon)", "Gl", "virgl"),
    adapter_line("so.tilo.settings", "VirtualGpu", "virgl (AMD Radeon)", "Gl", "virgl"),
])


class GpuTests(unittest.TestCase):
    def verdict(self, health, log):
        return gpu.judge(gpu.parse_health(health), gpu.parse_adapters(log), gpu.parse_fallbacks(log), health)

    def test_parses_every_adapter_line(self):
        got = gpu.parse_adapters(GPU_LOG)
        self.assertEqual([(a["app"], a["type"], a["backend"], a["driver"]) for a in got],
                         [("so.tilo.file-explorer", "VirtualGpu", "Gl", "virgl"),
                          ("so.tilo.settings", "VirtualGpu", "Gl", "virgl")])
        self.assertEqual(got[0]["name"], "virgl (AMD Radeon)")

    def test_windows_on_the_gpu_pass(self):
        self.assertEqual(self.verdict(OK, GPU_LOG)[0], "pass")

    def test_a_cpu_adapter_beside_a_gpu_fails(self):
        verdict, why = self.verdict(OK, GPU_LOG + "\n" + adapter_line("so.tilo.notes", "Cpu"))
        self.assertEqual(verdict, "fail")
        self.assertIn("so.tilo.notes", why)

    def test_a_reported_cpu_fallback_fails(self):
        log = GPU_LOG + ("\n[app so.tilo.notes stderr] tilo-gpu: window renders on a CPU adapter "
                         "although a GPU is present: llvmpipe")
        self.assertEqual(self.verdict(OK, log)[0], "fail")

    def test_degraded_health_fails_with_its_problems(self):
        verdict, why = self.verdict('{"status": "degraded", "problems": ["compositor offers no linux-dmabuf"]}',
                                    GPU_LOG)
        self.assertEqual(verdict, "fail")
        self.assertIn("linux-dmabuf", why)

    def test_a_machine_without_a_gpu_is_unobservable(self):
        log = adapter_line("so.tilo.file-explorer", "Cpu")
        self.assertEqual(self.verdict('{"status": "no-gpu", "problems": []}', log)[0], "unobservable")

    def test_no_report_or_no_adapter_line_is_unobservable(self):
        self.assertEqual(self.verdict("error: unknown flag --gpu", GPU_LOG)[0], "unobservable")
        self.assertEqual(self.verdict(OK, "")[0], "unobservable")
        self.assertEqual(self.verdict('{"status": "fine"}', GPU_LOG)[0], "unobservable")

    def test_health_is_found_after_a_warning_and_pretty_printed(self):
        text = 'warning: slow\n{\n  "status": "ok",\n  "problems": []\n}\n'
        self.assertEqual(gpu.parse_health(text)["status"], "ok")

    def test_the_probe_output_splits_into_health_and_log(self):
        health, log = gpu.split_probe(f"{OK}\n\n{gpu.LOG_SEPARATOR}\n{GPU_LOG}\n")
        self.assertEqual(gpu.parse_health(health)["status"], "ok")
        self.assertEqual(len(gpu.parse_adapters(log)), 2)
        self.assertEqual(gpu.parse_adapters(health), [])

    def test_the_gate_exits_by_verdict(self):
        def gate(stdin):
            out, old = io.StringIO(), sys.stdin
            sys.stdin = io.StringIO(stdin)
            try:
                with contextlib.redirect_stdout(out):
                    return gpu_check.main(["gpu_check.py"]), out.getvalue()
            finally:
                sys.stdin = old

        self.assertEqual(gate(f"{OK}\n{gpu.LOG_SEPARATOR}\n{GPU_LOG}")[0], 0)
        code, text = gate(f"{OK}\n{gpu.LOG_SEPARATOR}\n{adapter_line('so.tilo.settings', 'Cpu')}")
        self.assertEqual(code, 1)
        self.assertIn("::error::", text)
        code, text = gate(f'{{"status": "no-gpu"}}\n{gpu.LOG_SEPARATOR}\n')
        self.assertEqual(code, 0)
        self.assertIn("::notice::", text)
        self.assertEqual(gate(f"{OK}\n{gpu.LOG_SEPARATOR}\n")[0], 1)


class TerminalShellPerformanceTests(unittest.TestCase):
    def test_old_baseline_fails_even_within_readiness_timeout(self):
        from acceptance import core
        self.assertLess(39.6, core.TERMINAL_SHELL_S)
        with self.assertRaises(core.CheckFailed):
            core.assert_terminal_shell_performance(39.6)

    def test_measured_batched_import_passes(self):
        from acceptance import core
        core.assert_terminal_shell_performance(21.0)

    def test_missing_readiness_and_performance_boundary_fail(self):
        from acceptance import core
        for seconds in (None, core.TERMINAL_SHELL_PERFORMANCE_S):
            with self.subTest(seconds=seconds), self.assertRaises(core.CheckFailed):
                core.assert_terminal_shell_performance(seconds)


if __name__ == "__main__":
    unittest.main()
