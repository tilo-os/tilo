"""A check that fails while a consent prompt is on screen hands the next one a desktop whose
keyboard the prompt holds, and the prompts that are counted rather than named are exactly the
ones A01 meets.

In run r32ccbe0ded Terminal's environment could not start, A01 typed its probe anyway, Enter
took Terminal's offer of the device's own shell, and the shell queued the consent prompt for
it. The check then failed on the missing shell and returned, leaving the prompt up: the next
check's typing would have landed in it. These run A01 and the guard against a fake VM whose
shell keeps that prompt the way the compositor's state dump reports it (no running VM):

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import context as context_mod  # noqa: E402
from acceptance import core  # noqa: E402
from acceptance.checks import apps  # noqa: E402,F401 - registers A01
from acceptance.context import Context  # noqa: E402

from test_dialog_guard import DISPLAY, GuardTest, fast_wait_until  # noqa: E402

GROUP = {"id": 7, "tabs": [{"title": "Terminal", "tile": {"Foreign": {"app_id": "so.tilo.terminal"}}}]}


class PromptVm:
    """The parts of TiloVm A01 and the guard use, over a shell whose window opens with a
    prompt on screen: it holds the keyboard until it is answered, Escape answers it as
    dismissed, and a slot restart brings up a session without it."""

    def __init__(self):
        self.calls = []
        self.state_calls = 0
        self.group = dict(GROUP)
        self.prompt = False
        self.requested_restarts = 0

    def state(self):
        self.state_calls += 1
        groups = [self.group] if self.group else []
        return {
            "chrome": {
                "session_locked": False,
                "overlays": {},
                "palette": {"visible": False, "query": ""},
                "window_switcher": {"open": False},
                "peer_sync": {"invite_token": None, "connect_input": None, "status": None,
                              "invite_name": None, "invite_for": None},
                "portal_dialog": None,
                "pending_grants_count": 1 if self.prompt else 0,
                "input_capture": {"onboarding": False, "chrome": False,
                                  "inner_surface": False, "maximized_controls": False,
                                  "shortcut_help": False, "projection": False},
                "editing_workspace_name": None,
                "on_screen_keyboard": {"connected": False, "commits": 0,
                                       "serves_shell_field": False, "shown": None},
                "displays": [{"id": 0, "focused": True, "groups": [self.group["id"]] if self.group else []}],
            },
            "workspace_tree": {
                "outputs": {"displays": [{"id": 0, "x": DISPLAY[0], "y": DISPLAY[1],
                                          "width": DISPLAY[2], "height": DISPLAY[3]}]},
                "active": 1,
                "workspaces": [{"id": 1, "strips": [{"id": 2, "window_groups": groups}]}],
                "active_strip": {1: 2},
                "active_group": {2: self.group["id"]} if self.group else {},
            },
        }

    # -- the app A01 opens -----------------------------------------------------

    def invoke(self, provider, action):
        self.calls.append(("invoke", provider, action))
        return {}

    def control(self, *args):
        self.calls.append(("control",) + args)
        return ""

    def exec(self, script, timeout=None, check=True):
        # The shell runtime refused nothing it names: the environment simply has no runtime
        # installed, which is what the journal of r32ccbe0ded says.
        return ""

    def shot(self, path):
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        Path(path).write_bytes(b"\x89PNG\r\n\x1a\n")
        return path

    def restart_session(self, timeout=60):
        self.calls.append(("restart_session",))
        self.requested_restarts += 1
        self.prompt = False
        self.group = None

    def key(self, *combos):
        self.calls.append(("key",) + combos)
        if "Escape" in combos and self.prompt:
            self.prompt = False


class A01NoShellTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.out = Path(tmp.name)
        for target in ("acceptance.context.time.sleep", "acceptance.vm.time.sleep"):
            patcher = patch(target, lambda _s: None)
            patcher.start()
            self.addCleanup(patcher.stop)
        patcher = patch.object(context_mod, "wait_until", fast_wait_until)
        patcher.start()
        self.addCleanup(patcher.stop)

    def run_a01(self, prompt_on_failure):
        vm = PromptVm()
        ctx = Context(vm, self.out, {})
        # A01 leaves nothing open by itself: the prompt appears when it gives up, below.
        def lifecycle(c, res, *a, **kw):
            before = c.vm.state()["chrome"]
            c.vm.group = dict(GROUP)
            if prompt_on_failure:
                c.vm.prompt = True
            return GROUP["id"], {}, 0

        with patch.object(apps, "_lifecycle", side_effect=lifecycle), \
                patch.object(core, "terminal_shells", return_value=set()), \
                patch.object(core, "wait_new_terminal_shell", return_value=None), \
                patch.object(core, "ush_diagnostics", return_value=""), \
                patch.object(core, "ush_environment_refusal",
                             return_value="no environment can be built on this device: "
                                          "no env-runtime image is installed"):
            res = core.run_check(ctx, core.Check("A01", "apps", "Terminal opens with a live shell", apps.terminal))
        return vm, ctx, res

    def test_a_prompt_left_on_screen_fails_the_check_that_failed(self):
        vm, ctx, res = self.run_a01(prompt_on_failure=True)
        self.assertEqual(res.status, "fail")
        # A01's own failure stays first: what it could not do is the finding.
        self.assertTrue(res.detail.startswith("no shell ran in Terminal's environment within"),
                        res.detail)
        self.assertIn(core.LEFT_OPEN, res.detail)
        self.assertIn("the harness closed it with Escape", res.detail)
        self.assertEqual(ctx.facts["dialog_recovered_after"],
                         [{"check": "A01",
                           "open": ["a permission prompt"],
                           "route": "the harness closed it with Escape"}])
        self.assertFalse(vm.prompt, "the next check gets a desktop whose keyboard is free")

    def test_the_prompt_is_closed_the_way_a_person_denies_it(self):
        vm, _, _ = self.run_a01(prompt_on_failure=True)
        keys = [c for c in vm.calls if c[0] == "key"]
        self.assertEqual(keys, [("key", "Escape")])
        # Escape answers the prompt as denied: nothing is ever confirmed to clean up.
        self.assertFalse(any(c[0] == "control" for c in vm.calls))

    def test_a_failure_without_a_prompt_is_only_its_own(self):
        _, ctx, res = self.run_a01(prompt_on_failure=False)
        self.assertEqual(res.status, "fail")
        self.assertNotIn(core.LEFT_OPEN, res.detail, res.detail)
        self.assertNotIn("dialog_recovered_after", ctx.facts)


class PermissionPromptSurfaceTest(GuardTest):
    """A prompt the dump only counts (`pending_grants_count`) is a surface like any other: it
    takes the keyboard, so a check that leaves one open fails for it."""

    def leave_a_prompt(self, ctx, res):
        ctx.vm.prompt = True

    def test_a_counted_prompt_is_named_and_closed(self):
        vm = self.vm()
        clean = vm.state

        def state():
            st = clean()
            st["chrome"]["pending_grants_count"] = 1 if vm.prompt else 0
            return st

        # Escape answers a prompt as denied, the way the shell's ladder does.
        vm.key = lambda *combos: vm.prompt and "Escape" in combos and setattr(vm, "prompt", False)
        vm.prompt = False
        vm.state = state
        ctx = Context(vm, self.out, {})
        res = core.run_check(ctx, core.Check("X01", "window", "a check", self.leave_a_prompt))
        self.assertEqual(res.status, "fail")
        self.assertEqual(res.detail, "left a dialog open (a permission prompt); "
                                     "the harness closed it with Escape")
        self.assertEqual(ctx.facts["dialog_recovered_after"],
                         [{"check": "X01", "open": ["a permission prompt"],
                           "route": "the harness closed it with Escape"}])

    def test_no_prompt_on_screen_is_a_clean_desktop(self):
        vm = self.vm()
        ctx, res = self.run_check(vm, lambda c, r: None)
        self.assertEqual(res.status, "pass", res.detail)
        self.assertNotIn("dialog_recovered_after", ctx.facts)


if __name__ == "__main__":
    unittest.main()
