"""No check may leave a modal, overlay, prompt or dialog open for the next one, and W20 never
opens the Invite composer.

In run 89618, W20's first touch (a fixed point meant only to plug the touchscreen in) landed
on the empty workspace's "Invite people" card. The composer it opened held the keyboard, so
W20's search never opened, and A01 later typed its Terminal command into the composer's name
field. These run `run_check`, `Context.reset` and W20 against a fake VM whose shell keeps
those surfaces the way the compositor's state dump reports them (no running VM):

    python3 -m unittest scripts/vm-acceptance/test_dialog_guard.py
"""

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import context as context_mod  # noqa: E402
from acceptance import core  # noqa: E402
from acceptance.checks import window  # noqa: E402,F401 - registers W20
from acceptance.context import Context  # noqa: E402
from acceptance.vm import VmError  # noqa: E402

DISPLAY = (0, 0, 1920, 1080)
# The launchpad as laid out on a 1920x1080 output in run 89618: two rows of three cards
# and "Open an app" under them. "Invite people" holds the point W20 used to tap (1152, 648).
CARDS = {
    "launchpad:chat": (652, 507, 292, 96), "launchpad:notes": (956, 507, 292, 96),
    "launchpad:tasks": (1260, 507, 292, 96), "launchpad:meet": (652, 613, 292, 96),
    "launchpad:invite": (956, 613, 292, 96), "launchpad:activity": (1260, 613, 292, 96),
    "launchpad:open-app": (1395, 720, 160, 32),
}
SEARCH_FIELD = (660, 200, 600, 48)
KEYBOARD = {"x": 480.0, "y": 680.0, "w": 960.0, "h": 380.0}


def inside(x, y, rect):
    rx, ry, rw, rh = rect
    return rx <= x < rx + rw and ry <= y < ry + rh


class ShellVm:
    """The parts of TiloVm the guard, the reset and W20 use, over a shell that keeps its
    surfaces like the compositor: a launchpad card activates on a touch, the Invite
    composer takes every key (so Super+Space opens nothing behind it), Escape closes the
    topmost surface, and a slot restart brings up a fresh, empty session."""

    def __init__(self, *, keyboard_for_search=True):
        self.keyboard_for_search = keyboard_for_search
        self.calls = []
        self.locked = False
        self.overlays = {"overview": False, "quick_settings": False, "ws_settings": False,
                         "collaborators": False, "room_activity": False, "power_menu": False}
        self.palette = False
        self.query = ""
        self.composer = False
        self.review = False
        self.portal = None
        self.portal_ignores_escape = False
        self.capture = {"onboarding": False, "chrome": False, "inner_surface": False,
                        "maximized_controls": False, "shortcut_help": False, "projection": False}
        self.osk_connected = False
        self.osk_shown = False
        self.commits = 0

    # -- what the session shows ------------------------------------------------

    def _captures(self):
        return (self.capture["chrome"] or self.palette or self.composer or self.portal is not None
                or any(self.overlays.values()))

    def state(self):
        capture = dict(self.capture, chrome=self._captures())
        return {"chrome": {
            "session_locked": self.locked,
            "overlays": dict(self.overlays),
            "palette": {"visible": self.palette, "query": self.query},
            "window_switcher": {"open": False},
            "peer_sync": {"invite_token": None, "connect_input": None, "status": None,
                          "invite_name": "" if self.composer else None, "invite_for": None},
            "portal_dialog": self.portal,
            "pending_grants_count": 0,
            "editing_workspace_name": None,
            "input_capture": capture,
            "on_screen_keyboard": {"connected": self.osk_connected, "commits": self.commits,
                                   "serves_shell_field": True,
                                   "shown": dict(KEYBOARD) if self.osk_shown else None},
        }, "workspace_tree": {"outputs": {"displays": [
            {"id": 0, "x": DISPLAY[0], "y": DISPLAY[1], "width": DISPLAY[2], "height": DISPLAY[3]}]},
            "workspaces": []}}

    def targets(self):
        out = [{"name": n, "rect": list(r)} for n, r in CARDS.items()]
        if self.palette:
            out.append({"name": "Palette/SearchField", "rect": list(SEARCH_FIELD)})
        return out

    def actions(self, query=""):
        return []

    # -- input -------------------------------------------------------------------

    def key(self, *combos):
        self.calls.append(("key", *combos))
        for combo in combos:
            if combo == "super+l":
                self.locked = True
            elif combo == "Escape":
                self._escape()
            elif combo == "super+space" and not self.composer:
                self.palette = True

    def _escape(self):
        if self.portal is not None:
            if not self.portal_ignores_escape:
                self.portal = None
        elif self.composer and self.review:
            self.review = False
        elif self.composer:
            self.composer = False
        elif self.palette:
            self.palette, self.query, self.osk_shown = False, "", False
        else:
            for name, on in self.overlays.items():
                if on:
                    self.overlays[name] = False
                    break

    def input(self, *args):
        self.calls.append(("input", *args))
        if args[:2] != ("touch", "tap"):
            return ""
        x, y = int(args[2]), int(args[3])
        self.osk_connected = True
        if self.osk_shown and inside(x, y, (KEYBOARD["x"], KEYBOARD["y"], KEYBOARD["w"], KEYBOARD["h"])):
            self.commits += 1
            self.query += "a"
        elif self.palette and inside(x, y, SEARCH_FIELD):
            self.osk_shown = self.keyboard_for_search
        elif not self.palette and not self.composer and inside(x, y, CARDS["launchpad:invite"]):
            self.composer = True
        return ""

    # -- pictures and the slot ---------------------------------------------------

    def shot(self, path):
        self.calls.append(("shot", str(path)))
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"\x89PNG")
        return path

    def restart_session(self):
        calls = [*self.calls, ("restart_session",)]
        self.__init__(keyboard_for_search=self.keyboard_for_search)
        self.calls = calls


def fast_wait_until(predicate, timeout, interval=0.5):
    """wait_until without the wall-clock wait: the fake's state changes only on input."""
    for _ in range(3):
        value = predicate()
        if value:
            return value
    return None


class GuardTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.out = Path(tmp.name)
        for target in ("acceptance.context.time.sleep", "acceptance.vm.time.sleep"):
            p = patch(target, lambda _s: None)
            p.start()
            self.addCleanup(p.stop)
        p = patch.object(context_mod, "wait_until", fast_wait_until)
        p.start()
        self.addCleanup(p.stop)

    def vm(self, **kw):
        return ShellVm(**kw)

    def run_check(self, vm, func, cid="X01"):
        ctx = Context(vm, self.out, {})
        return ctx, core.run_check(ctx, core.Check(cid, "window", "a check", func))

    def escapes(self, vm):
        return vm.calls.count(("key", "Escape"))


class OpenSurfacesTest(GuardTest):
    def test_names_each_surface_the_state_dump_reports(self):
        vm = self.vm()
        self.assertEqual(core.open_surfaces(vm.state()), [])
        vm.composer = True
        self.assertEqual(core.open_surfaces(vm.state()), ["the Invite someone composer"])
        vm.composer, vm.palette = False, True
        self.assertEqual(core.open_surfaces(vm.state()), ["search"])
        vm.palette, vm.overlays["quick_settings"] = False, True
        self.assertEqual(core.open_surfaces(vm.state()), ["the quick settings overlay"])
        vm.overlays["quick_settings"], vm.portal = False, {"kind": "file-chooser"}
        self.assertEqual(core.open_surfaces(vm.state()), ["a portal dialog (file-chooser)"])

    def test_names_the_rest_of_the_dump(self):
        st = ShellVm().state()
        chrome = st["chrome"]
        chrome["peer_sync"]["invite_token"] = "tilo-invite:..."
        chrome["peer_sync"]["connect_input"] = ""
        chrome["pending_grants_count"] = 1
        chrome["editing_workspace_name"] = "Untitled"
        chrome["window_switcher"]["open"] = True
        chrome["input_capture"].update(shortcut_help=True, projection=True, onboarding=True)
        found = core.open_surfaces(st)
        for name in ("an invite card showing a live token", "the Join card", "a permission prompt",
                     "the workspace-name editor", "the window switcher", "the shortcut help",
                     "the projection picker", "first-login onboarding"):
            self.assertIn(name, found)

    def test_power_menu_is_named_once(self):
        st = ShellVm().state()
        st["chrome"]["overlays"]["power_menu"] = True
        st["chrome"]["input_capture"]["inner_surface"] = True
        self.assertEqual(core.open_surfaces(st), ["the power menu overlay"])

    def test_an_unnamed_surface_that_captures_input_is_caught(self):
        vm = self.vm()
        vm.capture["chrome"] = True  # e.g. the emoji picker, which has no field of its own
        self.assertEqual(core.open_surfaces(vm.state()), ["a shell surface that captures input"])

    def test_hovering_a_maximized_windows_controls_is_not_a_dialog(self):
        vm = self.vm()
        vm.capture.update(chrome=True, maximized_controls=True)
        self.assertEqual(core.open_surfaces(vm.state()), [])


class LeaveNoDialogTest(GuardTest):
    def test_run_check_fails_a_check_that_leaves_the_invite_composer_open(self):
        vm = self.vm()

        def opens_the_composer(ctx, res):
            ctx.vm.input("touch", "tap", "1152", "648")

        ctx, res = self.run_check(vm, opens_the_composer)
        self.assertEqual(res.status, "fail")
        self.assertEqual(res.detail, "left a dialog open (the Invite someone composer); "
                                     "the harness closed it with Escape")
        self.assertFalse(vm.composer)
        self.assertNotIn(("restart_session",), vm.calls)
        self.assertEqual(ctx.facts["dialog_recovered_after"],
                         [{"check": "X01", "open": ["the Invite someone composer"],
                           "route": "the harness closed it with Escape"}])

    def test_the_next_checks_typing_reaches_its_subject(self):
        vm = self.vm()
        ctx = Context(vm, self.out, {})
        core.run_check(ctx, core.Check("W20", "window", "leaks",
                                       lambda c, r: c.vm.input("touch", "tap", "1152", "648")))

        def opens_search(c, r):
            c.vm.key("super+space")
            core.expect(c.state()["chrome"]["palette"]["visible"], "search open")
            c.vm.key("Escape")

        res = core.run_check(ctx, core.Check("A01", "apps", "types", opens_search))
        self.assertEqual(res.status, "pass", res.detail)

    def test_keeps_the_checks_own_failure_first(self):
        def opens_and_fails(ctx, res):
            ctx.vm.input("touch", "tap", "1152", "648")
            raise core.CheckFailed("search open not reached within 5s")

        _, res = self.run_check(self.vm(), opens_and_fails)
        self.assertTrue(res.detail.startswith("search open not reached within 5s; left a dialog open"),
                        res.detail)

    def test_the_review_step_takes_a_second_escape(self):
        vm = self.vm()

        def leaves_review_open(ctx, res):
            ctx.vm.composer, ctx.vm.review = True, True

        _, res = self.run_check(vm, leaves_review_open)
        self.assertEqual(res.status, "fail")
        self.assertIn("the harness closed it with Escape", res.detail)
        self.assertEqual(self.escapes(vm), 2)
        self.assertFalse(vm.composer)

    def test_a_dialog_escape_cannot_close_restarts_the_session(self):
        vm = self.vm()

        def leaves_a_stuck_sheet(ctx, res):
            ctx.vm.portal, ctx.vm.portal_ignores_escape = {"kind": "access"}, True

        ctx, res = self.run_check(vm, leaves_a_stuck_sheet)
        self.assertEqual(res.status, "fail")
        self.assertEqual(res.detail, "left a dialog open (a portal dialog (access)); "
                                     "Escape did not close it; the harness restarted the slot session")
        self.assertEqual(self.escapes(vm), core.DISMISS_ESCAPES)
        self.assertEqual(vm.calls.count(("restart_session",)), 1)
        self.assertIsNone(vm.portal)
        # Nothing is ever confirmed to clean up: Escape and the restart only.
        self.assertEqual({c[1] for c in vm.calls if c[0] == "key"}, {"Escape"})

    def test_reports_a_restart_that_fails(self):
        vm = self.vm()

        def broken_restart():
            vm.calls.append(("restart_session",))
            raise VmError("`tilo-vm slot restart acceptance` exited 1: error: the slot is gone")

        vm.restart_session = broken_restart

        def leaves_a_stuck_sheet(ctx, res):
            ctx.vm.portal, ctx.vm.portal_ignores_escape = {"kind": "access"}, True

        _, res = self.run_check(vm, leaves_a_stuck_sheet)
        self.assertEqual(res.status, "fail")
        self.assertIn("Escape did not close it, and restarting the slot session failed: ", res.detail)
        self.assertIn("error: the slot is gone", res.detail)

    def test_leaves_a_clean_desktop_alone(self):
        vm = self.vm()
        ctx, res = self.run_check(vm, lambda c, r: None)
        self.assertEqual(res.status, "pass", res.detail)
        self.assertEqual(vm.calls, [])
        self.assertNotIn("dialog_recovered_after", ctx.facts)

    def test_hover_over_maximized_controls_passes(self):
        vm = self.vm()

        def hovers(ctx, res):
            ctx.vm.capture.update(chrome=True, maximized_controls=True)

        _, res = self.run_check(vm, hovers)
        self.assertEqual(res.status, "pass", res.detail)
        self.assertEqual(self.escapes(vm), 0)

    def test_the_lock_guard_owns_a_locked_session(self):
        vm = self.vm()

        def leaves_both(ctx, res):
            ctx.vm.input("touch", "tap", "1152", "648")
            ctx.vm.key("super+l")

        ctx, res = self.run_check(vm, leaves_both)
        self.assertEqual(res.status, "fail")
        self.assertIn(core.LEFT_LOCKED, res.detail)
        # The restart that unlocked it also closed the composer; no Escape typed into the lock.
        self.assertNotIn(core.LEFT_OPEN, res.detail)
        self.assertEqual(vm.calls.count(("restart_session",)), 1)
        self.assertEqual(self.escapes(vm), 0)
        self.assertNotIn("dialog_recovered_after", ctx.facts)


class ResetTest(GuardTest):
    def test_reset_closes_the_invite_composer(self):
        vm = self.vm()
        vm.composer = True
        Context(vm, self.out, {}).reset()
        self.assertFalse(vm.composer)


class W20Test(GuardTest):
    def run_w20(self, vm):
        ctx = Context(vm, self.out, {})
        w20 = next(c for c in core.REGISTRY if c.id == "W20")
        return core.run_check(ctx, w20)

    def touches(self, vm):
        return [(int(c[3]), int(c[4])) for c in vm.calls if c[:3] == ("input", "touch", "tap")]

    def test_the_old_fixed_point_is_on_the_invite_card(self):
        # What run 89618 tapped, so the tests below are about the card that bit.
        x, y = int(DISPLAY[2] * 0.6), int(DISPLAY[3] * 0.6)
        self.assertTrue(inside(x, y, CARDS["launchpad:invite"]))

    def test_w20_first_touch_lands_on_no_launchpad_card(self):
        vm = self.vm()
        res = self.run_w20(vm)
        self.assertEqual(res.status, "pass", res.detail)
        first = self.touches(vm)[0]
        self.assertFalse(any(inside(*first, r) for r in CARDS.values()), first)
        self.assertEqual(res.metrics["first_touch"], list(first))
        self.assertFalse(vm.composer)
        self.assertFalse(vm.palette, "W20 left search open")
        self.assertEqual(res.metrics["query"], "aaa")

    def test_w20_closes_search_when_it_fails_midway(self):
        vm = self.vm(keyboard_for_search=False)
        res = self.run_w20(vm)
        self.assertEqual(res.status, "fail")
        self.assertIn("the on-screen keyboard shown for the search field", res.detail)
        # W20 closed its own search: the guard found nothing to report.
        self.assertNotIn(core.LEFT_OPEN, res.detail)
        self.assertFalse(vm.palette)

    def test_w20_refuses_to_tap_blind(self):
        vm = self.vm()
        whole = {"launchpad:everything": (0, 0, DISPLAY[2], DISPLAY[3])}
        with patch.dict(CARDS, whole, clear=True):
            res = self.run_w20(vm)
        self.assertEqual(res.status, "fail")
        self.assertTrue(res.unobservable)
        self.assertIn("on a launchpad card", res.detail)
        self.assertEqual(self.touches(vm), [])


if __name__ == "__main__":
    unittest.main()
