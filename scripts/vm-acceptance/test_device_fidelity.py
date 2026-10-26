"""The agent-door (M01-M04) and input-fidelity (I01-I08) checks, without a VM.

The agent checks run for real against a fake `tilo-runtime-mcp` and a fake permission
prompt, through the same guest client the VM runs; the input checks' parsers read the
Yoga's own /proc/bus/input/devices. Expected failures and calibration are the harness's
(`core.ExpectedFailure`, `check(calibrating=...)`):

    python3 -m unittest scripts/vm-acceptance/test_device_fidelity.py
"""

import json
import os
import shlex
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'tools/tests'))
from hang import HANG_BOUND  # noqa: E402

from acceptance import core, report  # noqa: E402
from acceptance.checks import agent, input_devices  # noqa: E402

# What /proc/bus/input/devices says on the Yoga Slim 7x (83ED), trimmed to its I2C devices.
YOGA_DEVICES = """\
I: Bus=0018 Vendor=048d Product=8987 Version=0100
N: Name="hid-over-i2c 048D:8987"
P: Phys=1-003a
H: Handlers=kbd leds event1 rfkill
B: PROP=0
B: EV=12001f
B: REL=1040

I: Bus=0018 Vendor=0488 Product=1054 Version=0100
N: Name="hid-over-i2c 0488:1054 Touchpad"
P: Phys=1-002c
H: Handlers=mouse1 event3
B: PROP=5
B: EV=1b
B: ABS=2e0800000000003

I: Bus=0006 Vendor=7469 Product=0001 Version=0001
N: Name="tilo-vm keyboard (seat-acceptance)"
H: Handlers=sysrq kbd event4
B: PROP=0
"""

# A stand-in for tilo-runtime-mcp --connect behind the agent door: it asks the person on
# `initialize` (a file the fake VM answers), then serves the agent tools over a Notes notebook.
FAKE_MCP = r'''#!/usr/bin/env python3
import json, os, sys, time
state = os.environ["FAKE_STATE"]
def say(m): sys.stdout.write(json.dumps(m) + "\n"); sys.stdout.flush()
def tool(i, text, error=False):
    say({"jsonrpc": "2.0", "id": i, "result": {"isError": error, "content": [{"type": "text", "text": text}]}})
for line in sys.stdin:
    m = json.loads(line)
    if m.get("method") == "initialize":
        if os.path.exists(state + "/answer"): os.remove(state + "/answer")
        open(state + "/prompt", "w").write("1")
        for _ in range(50):
            if os.path.exists(state + "/answer"): break
            time.sleep(0.1)
        if open(state + "/answer").read() == "deny":
            say({"jsonrpc": "2.0", "id": m["id"], "error": {"code": -32000, "message": "the person refused"}})
            break
        say({"jsonrpc": "2.0", "id": m["id"], "result": {"serverInfo": {"name": "tilo"}}})
    elif m.get("method") == "tools/list":
        say({"jsonrpc": "2.0", "id": m["id"], "result": {"tools": [{"name": n} for n in
             ("tilo_ls", "tilo_read", "tilo_do", "tilo_find", "tilo_status")] + FAKE_EXTRA}})
    elif m.get("method") == "tools/call":
        p = m["params"]; a = p["arguments"]
        notes = state + "/notes"
        if p["name"] == "tilo_ls" and not a.get("path"):
            tool(m["id"], "\n".join(["apps"] + [f"{x.rsplit('.', 1)[-1]}  Notes  running" for x in APPS]))
        elif p["name"] == "tilo_ls" and a["path"] == "notes#new-note":
            tool(m["id"], "notes#new-note  New note\n  title  text")
        elif p["name"] == "tilo_do" and a["path"] == "notes#new-note":
            open(notes, "a").write(a["args"]["title"] + "\n")
            tool(m["id"], "ok note " + a["args"]["title"])
        elif p["name"] == "tilo_do":
            tool(m["id"], "notes has no action " + a["path"].split("#")[-1], error=True)
        elif p["name"] == "tilo_find":
            found = open(notes).read().splitlines() if os.path.exists(notes) else []
            tool(m["id"], "\n".join(f"notes/Notebook/{t}  {t}" for t in found if a["query"] in t))
'''


def fake_binary(directory: Path, apps: list[str], extra_tools=()) -> Path:
    path = directory / "tilo-runtime-mcp"
    path.write_text(FAKE_MCP.replace("APPS", repr(apps)).replace("FAKE_EXTRA", repr([{"name": n} for n in extra_tools])))
    path.chmod(0o755)
    return path


class FakeVm:
    """The parts of TiloVm the agent checks use, over a real guest client process."""

    def __init__(self, root: Path, binary: Path | None, door=True, answer_prompts=True):
        self.root, self.binary, self.door, self.answer_prompts = root, binary, door, answer_prompts
        self.home = root / "home"
        self.state = root / "state"
        self.home.mkdir()
        self.state.mkdir()
        self.invoked = []
        self.grants = []

    def exec(self, script, timeout=120, check=True):
        if script.startswith("test -S"):
            return ("door" if self.door else "no-door") + "\nmcp\npython\n"
        env = dict(os.environ, HOME=str(self.home), FAKE_STATE=str(self.state))
        if self.binary:
            env["ACC_MCP_BIN"] = str(self.binary)
        env["ACC_MCP_INITIALIZE_S"] = "20"
        return subprocess.run(["sh", "-c", script], env=env, capture_output=True, text=True,
                              timeout=timeout).stdout

    def write_home_file(self, rel, data):
        path = self.home / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def actions(self, query=""):
        if not (self.state / "prompt").exists() or (self.state / "answer").exists() or not self.answer_prompts:
            return []
        return [{"provider": "permission-prompts", "id": f"{d}@p1"} for d in ("allow-once", "deny")]

    def invoke(self, provider, action_id, args=None):
        self.invoked.append((provider, action_id, args))
        (self.state / "answer").write_text("deny" if action_id.startswith("deny") else "allow")
        return {}

    def run(self, *args, **kwargs):
        notes = self.state / "notes"
        if args[:2] == ("control", "read") and notes.exists():
            return json.dumps({"notes": notes.read_text().splitlines()})
        return ""

    def raw(self, op, timeout=60):
        self.grants.append(op)
        return {"ok": True}

    def control_json(self, *args):
        return {}

    def vm_actions(self, q=""):
        return []


def make_ctx(vm: FakeVm, out: Path):
    ctx = SimpleNamespace(vm=vm, facts={}, out=out, current=None)
    ctx.reset = lambda: None
    ctx.open_app = lambda *a, **k: ({"id": 1}, 0.0)
    ctx.settle = lambda *a: None
    ctx.shot = lambda name: None
    ctx.write = lambda name, text: (out / name).write_text(text)
    # An unlocked desktop with nothing open: what the harness's after-check guards
    # (`leave_unlocked`, `leave_no_dialog`) read between checks.
    ctx.state = lambda: {"chrome": {}}
    return ctx


def run_all(ids, ctx):
    by_id = {c.id: c for c in core.REGISTRY}
    return {i: core.run_check(ctx, by_id[i]) for i in ids}


class AgentDoorChecks(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        agent.PROMPT_S = 15

    def run_agent(self, **kwargs):
        binary = fake_binary(self.root, kwargs.pop("apps", ["so.tilo.notes"]), kwargs.pop("extra", ()))
        vm = FakeVm(self.root, binary, **kwargs)
        ctx = make_ctx(vm, self.root)
        return vm, run_all(["M01", "M02", "M03", "M04"], ctx)

    def test_no_door_is_an_expected_failure_that_names_the_missing_socket(self):
        _, results = self.run_agent(door=False)
        for r in results.values():
            self.assertEqual(r.status, "xfail", r)
            self.assertIn("agent.sock", r.detail)
            self.assertIn("fix/agent-door", r.detail)
            self.assertFalse(r.unobservable)

    def test_an_admitted_agent_sees_the_desktop_and_makes_a_note(self):
        vm, results = self.run_agent()
        self.assertEqual({i: r.status for i, r in results.items()},
                         {"M01": "pass", "M02": "pass", "M03": "pass", "M04": "pass"},
                         {i: r.detail for i, r in results.items()})
        self.assertEqual([a[1] for a in vm.invoked], ["allow-once@p1", "deny@p1"][:len(vm.invoked)])
        self.assertTrue(any(i[1].startswith("deny@") for i in vm.invoked))
        # Notes is granted for the session and taken back after it.
        self.assertEqual([g["op"] for g in vm.grants], ["allow", "disallow"])

    def test_an_empty_standalone_runtime_fails_m02_not_m01(self):
        _, results = self.run_agent(apps=[])
        self.assertEqual(results["M01"].status, "pass")
        self.assertEqual(results["M02"].status, "fail")
        self.assertIn("second runtime", results["M02"].detail)

    def test_pixel_tools_on_the_door_fail_m01(self):
        _, results = self.run_agent(extra=("computer",))
        self.assertEqual(results["M01"].status, "fail")
        self.assertIn("pixel tools", results["M01"].detail)

    def test_a_person_who_is_never_asked_is_unobservable_not_a_pass(self):
        agent.PROMPT_S = 2
        _, results = self.run_agent(answer_prompts=False)
        self.assertEqual(results["M01"].status, "fail")
        self.assertTrue(results["M01"].unobservable)
        self.assertIn("never asked", results["M01"].detail)

    def test_missing_eof_observation_cannot_pass_refusal_check(self):
        ctx = make_ctx(None, self.root)
        transcript = {"refusal": {"result": {"isError": True, "content": [{"text": "unknown action"}]}}}
        denied = {"initialize": {"error": {"message": "denied"}}}
        with (patch.object(agent, "_session", return_value=transcript),
              patch.object(agent, "_run_client", return_value=denied)):
            result = run_all(["M04"], ctx)["M04"]
        self.assertEqual(result.status, "fail", result.detail)
        self.assertIn("door stayed open", result.detail)

    def test_non_boolean_is_error_cannot_prove_a_refusal(self):
        ctx = make_ctx(None, self.root)
        for value in ("false", "true", 1, [], None):
            with self.subTest(value=value), patch.object(agent, "_session", return_value={
                    "refusal": {"result": {"isError": value, "content": [{"text": "unknown action"}]}}}):
                with patch.object(agent, "_run_client") as denied:
                    result = run_all(["M04"], ctx)["M04"]
                self.assertEqual(result.status, "fail", result.detail)
                self.assertIn("not reported as isError", result.detail)
                denied.assert_not_called()

    def test_transcript_parser_and_helpers(self):
        line = "receipt: x\nACC-MCP " + json.dumps({"a": 1}) + "\n"
        self.assertEqual(agent.parse_transcript(line), {"a": 1})
        with self.assertRaises(core.CheckFailed):
            agent.parse_transcript("nothing here")
        rows = agent.decision_rows([{"provider": "permission-prompts", "id": "deny@7"},
                                    {"provider": "other", "id": "deny@8"}], "deny")
        self.assertEqual(rows, ["deny@7"])
        self.assertIs(agent.is_error({"result": {"isError": True}}), True)
        self.assertIsNone(agent.is_error("timeout"))


class GuestClientTransport(unittest.TestCase):
    def probe(self, server, assertions):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "fake-mcp"
            binary.write_text("#!/usr/bin/env python3\nimport os, sys\n" + textwrap.dedent(server))
            binary.chmod(0o755)
            code = ("import runpy\n"
                    f"HANG = {HANG_BOUND!r}\n"
                    f"module = runpy.run_path({str(agent.CLIENT)!r})\n"
                    "client = module['Client']()\n"
                    "try:\n" + textwrap.indent(textwrap.dedent(assertions), "    ") +
                    "\nfinally:\n    client.close()\n")
            # Bound regressions that previously blocked forever inside readline/close.
            proc = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True,
                                  env=dict(os.environ, ACC_MCP_BIN=str(binary)), timeout=HANG_BOUND)
            self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_coalesced_lines_do_not_disappear_in_text_buffering(self):
        self.probe('''
            os.write(1, b'{"id":1}\\n{"id":2}\\n')
            sys.stdin.read()
        ''', '''
            assert client.read(HANG) == {"id": 1}
            assert client.read(0.1) == {"id": 2}
        ''')

    def test_partial_line_respects_deadline_and_survives_next_read(self):
        self.probe('''
            os.write(1, b'{"id":')
            sys.stdin.readline()
            os.write(1, b'1}\\n')
        ''', '''
            assert client.read(0.1) == "timeout"
            client.send({"continue": True})
            assert client.read(HANG) == {"id": 1}
        ''')

    def test_stderr_backpressure_cannot_block_protocol_reply(self):
        self.probe('''
            os.write(2, b'x' * 262144)
            os.write(1, b'{"id":1}\\n')
        ''', '''
            assert client.read(HANG) == {"id": 1}
        ''')

    def test_shutdown_reaps_a_server_that_ignores_eof(self):
        self.probe('''
            import signal
            sys.stdin.read()
            signal.pause()
        ''', '''
            client.close()
            client.close = lambda: None
            assert client.proc.returncode == -9
            assert client.proc.stdout.closed
        ''')

    def test_shutdown_drains_final_notifications_and_reaps_child(self):
        self.probe('''
            sys.stdin.read()
            os.write(1, b'x' * 262144)
            os.write(2, b'final diagnostic')
        ''', '''
            assert client.close() == "final diagnostic"
            assert client.proc.returncode == 0
            assert client.proc.stdout.closed
            # The probe's finally must not close the same client twice.
            client.close = lambda: None
        ''')


class CalibrationAndReport(unittest.TestCase):
    def check_result(self, func, **kwargs):
        chk = core.Check("T01", "input", "t", core._calibrated(func, "why") if kwargs.get("cal") else func)
        ctx = SimpleNamespace(facts={}, out=Path(tempfile.mkdtemp()), current=None,
                              state=lambda: {"chrome": {}}, vm=None)
        return core.run_check(ctx, chk)

    def test_a_calibrating_check_fails_as_xfail_and_a_pass_stays_a_pass(self):
        def fails(ctx, res):
            raise core.CheckFailed("the pad did not move")

        def breaks(ctx, res):
            return {}["missing"]

        for func in (fails, breaks):
            r = self.check_result(func, cal=True)
            self.assertEqual(r.status, "xfail")
            self.assertIn("still calibrating: why", r.detail)
        self.assertEqual(self.check_result(lambda c, r: None, cal=True).status, "pass")
        self.assertEqual(self.check_result(fails).status, "fail")

    def test_xfail_does_not_fail_the_run_but_stays_visible(self):
        results = [core.Result("M01", "agent", "door", status="xfail", detail="expected failure: no door"),
                   core.Result("I01", "input", "identity", status="pass")]
        out = Path(tempfile.mkdtemp())
        doc = report.write(out, {"commit_short": "abc"}, results, report.owners_from(None))
        self.assertEqual(doc["overall"], "pass")
        self.assertEqual(doc["summary"]["agent"]["xfail"], 1)
        self.assertEqual(doc["summary"]["agent"]["fail"], 0)
        text = (out / "report.md").read_text()
        self.assertIn("expected failure", text)
        self.assertIn("xfail", text)
        results.append(core.Result("I02", "input", "keys", status="fail"))
        self.assertEqual(report.write(out, {}, results, report.owners_from(None))["overall"], "fail")


class InputDeviceParsers(unittest.TestCase):
    def test_the_yoga_devices_are_found_by_identity_not_by_name_alone(self):
        devices = input_devices.parse_input_devices(YOGA_DEVICES)
        keyboard = input_devices.find_device(devices, input_devices.KEYBOARD_ID)
        touchpad = input_devices.find_device(devices, input_devices.TOUCHPAD_ID)
        self.assertEqual(input_devices.event_node(keyboard), "event1")
        self.assertEqual(input_devices.event_node(touchpad), "event3")
        self.assertEqual(touchpad["bits"]["PROP"], "5")
        # The default virtual keyboard is a different bus and vendor: it is not the 83ED's.
        fake = [d for d in devices if d["name"].startswith("tilo-vm")][0]
        self.assertEqual((fake["bus"], fake["vendor"]), (6, 0x7469))
        wrong_bus = dict(input_devices.KEYBOARD_ID, bus=0x06)
        self.assertIsNone(input_devices.find_device(devices, wrong_bus))

    def test_the_kernel_state_says_which_outputs_scan_out(self):
        state = (
            "plane[31]: plane-0\n\tcrtc=crtc-0\n\tfb=44\n\t\tallocated by = tilo\n\t\trefcount=2\n"
            "plane[32]: plane-1\n\tcrtc=(null)\n\tfb=0\n"
            "plane[33]: plane-2\n\tcrtc=crtc-1\n\tfb=0\n"
            "crtc[34]: crtc-0\n\tenable=1\n\tactive=1\n\tself_refresh_active=0\n"
            "crtc[35]: crtc-1\n\tenable=1\n\tactive=1\n"
            "crtc[36]: crtc-2\n\tenable=0\n\tactive=0\n"
            "connector[37]: Virtual-1\n\tcrtc=crtc-0\n\tself_refresh_aware=0\n"
            "connector[38]: Virtual-2\n\tcrtc=crtc-1\n"
            "connector[39]: Virtual-3\n\tcrtc=crtc-2\n"
            "connector[40]: Virtual-4\n\tcrtc=(null)\n"
        )
        blocks = input_devices.parse_kms_state(state)
        self.assertEqual(blocks[0], {"kind": "plane", "name": "plane-0", "crtc": "crtc-0", "fb": "44"})
        self.assertEqual(
            input_devices.scanning_out(blocks, ["Virtual-1", "Virtual-2", "Virtual-3", "Virtual-4"]),
            {"Virtual-1": "crtc-0", "Virtual-2": None, "Virtual-3": None, "Virtual-4": None},
            "only an active CRTC with a framebuffer on a plane scans out",
        )

    def test_udev_properties(self):
        props = input_devices.parse_properties("ID_INPUT=1\nID_INPUT_TOUCHPAD=1\nID_INPUT_WIDTH_MM=130\n")
        self.assertEqual(props["ID_INPUT_WIDTH_MM"], "130")

    def test_a_lost_super_space_names_the_keyboards_the_compositor_opened(self):
        written = {}
        state = {"devices": [{"name": "tilo-vm keyboard (seat-acceptance)"}],
                 "active_keyboard": "tilo-vm keyboard"}
        vm = SimpleNamespace(
            exec=lambda script, **k: json.dumps(state),
            root_exec=lambda script, **k: YOGA_DEVICES if "proc/bus" in script
            else "ID_INPUT_KEYBOARD=1\nID_SEAT=seat-acceptance\nDEVNAME=/dev/input/event1\n")
        ctx = SimpleNamespace(vm=vm, current=SimpleNamespace(id="I02"),
                              write=lambda name, text: written.update({name: text}))
        why = input_devices.keyboard_diagnosis(ctx)
        self.assertIn("has not opened the profile keyboard", why)
        self.assertIn("'tilo-vm keyboard'", why)
        self.assertIn("ID_SEAT", why)
        self.assertNotIn("DEVNAME", why)
        self.assertIn("I02-keyboards.json", written)

    def test_every_check_has_a_registered_order_and_area(self):
        from run import ORDER  # noqa: PLC0415
        ids = {c.id for c in core.REGISTRY if c.area in ("agent", "input")}
        self.assertEqual(ids, {f"M0{i}" for i in range(1, 5)} | {f"I0{i}" for i in range(1, 9)})
        self.assertTrue(ids <= set(ORDER), ids - set(ORDER))
        calibrating = {c.id for c in core.REGISTRY if c.area == "input" and c.func.__name__ in
                       ("touchpad_tap_click", "touchpad_scroll", "disable_while_typing",
                        "wake_from_sleep")}
        self.assertEqual(calibrating, {"I05", "I06", "I07", "I08"})


if __name__ == "__main__":
    unittest.main()
