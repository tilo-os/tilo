#!/usr/bin/env python3
"""The echo-cancelled microphone's PipeWire config is valid and does its job.

The desktop session runs ``pipewire -c tilo-echo-cancel.conf`` while
"Reduce echo in calls" is on. A syntax error or a wrong property there only
shows up as calls that still echo, so this parses the file with a SPA-JSON
reader (and with ``pw-config`` when the host has it) and asserts the contract:

* the echo-cancel module uses WebRTC in monitor mode, so every sound the
  speakers play is the echo reference and playback routing is untouched;
* the source advertises ``device.intended-roles = [ "Communication" ]``, the
  WirePlumber media-role policy that routes call capture to it, and ranks
  below every real microphone so it never becomes the default input;
* the capture and reference streams are passive (the microphone opens only
  while something records from the source) and name no target, so they follow
  the default devices;
* node names match what the session hides from device and app lists;
* the image installs the file where ``pipewire -c`` looks.
"""

import re
import shutil
import subprocess
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CONF = REPO / "os/usr/share/pipewire/tilo-echo-cancel.conf"
AUDIO_RS = REPO / "crates/platform/tilo-media/src/audio.rs"
RUNTIME_RS = REPO / "crates/platform/tilo-media/src/audio_runtime.rs"
CONTAINERFILE = REPO / "os/Containerfile"


class SpaJsonError(ValueError):
    pass


class SpaJson:
    """Reader for PipeWire's relaxed JSON: optional commas and colons/`=`,
    bare-word keys and values, and `#` comments."""

    BARE_END = set(' \t\r\n{}[]:=,"#')

    def __init__(self, text):
        self.text = text
        self.pos = 0

    def skip(self):
        while self.pos < len(self.text):
            ch = self.text[self.pos]
            if ch in " \t\r\n,":
                self.pos += 1
            elif ch == "#":
                end = self.text.find("\n", self.pos)
                self.pos = len(self.text) if end < 0 else end + 1
            else:
                return

    def peek(self):
        self.skip()
        return self.text[self.pos] if self.pos < len(self.text) else ""

    def error(self, message):
        line = self.text.count("\n", 0, self.pos) + 1
        raise SpaJsonError(f"line {line}: {message}")

    def value(self):
        ch = self.peek()
        if ch == "{":
            self.pos += 1
            return self.members("}")
        if ch == "[":
            self.pos += 1
            items = []
            while self.peek() != "]":
                if not self.peek():
                    self.error("unterminated array")
                items.append(self.value())
            self.pos += 1
            return items
        if ch == '"':
            return self.string()
        if not ch or ch in "}]:=":
            self.error(f"expected a value, found {ch!r}")
        start = self.pos
        while self.pos < len(self.text) and self.text[self.pos] not in self.BARE_END:
            self.pos += 1
        word = self.text[start : self.pos]
        return {"true": True, "false": False, "null": None}.get(word, word)

    def string(self):
        self.pos += 1
        out = []
        while self.pos < len(self.text):
            ch = self.text[self.pos]
            self.pos += 1
            if ch == "\\":
                out.append(self.text[self.pos])
                self.pos += 1
            elif ch == '"':
                return "".join(out)
            else:
                out.append(ch)
        self.error("unterminated string")

    def members(self, close):
        obj = {}
        while True:
            ch = self.peek()
            if ch == close:
                self.pos += 1
                return obj
            if not ch:
                if close:
                    self.error("unterminated object")
                return obj
            key = self.value()
            if not isinstance(key, str):
                self.error("object keys must be strings")
            self.skip()
            if self.pos < len(self.text) and self.text[self.pos] in ":=":
                self.pos += 1
            if key in obj:
                self.error(f"duplicate key {key!r}")
            obj[key] = self.value()

    def document(self):
        return self.members("")


def load_config():
    return SpaJson(CONF.read_text(encoding="utf-8")).document()


def rust_str_const(path, name):
    match = re.search(
        rf'pub const {name}: &str = "([^"]+)";', path.read_text(encoding="utf-8")
    )
    if not match:
        raise AssertionError(f"{path} no longer defines {name}")
    return match.group(1)


class EchoCancelConfigTest(unittest.TestCase):
    def setUp(self):
        self.config = load_config()
        modules = {m["name"]: m for m in self.config["context.modules"]}
        self.modules = modules
        self.args = modules["libpipewire-module-echo-cancel"]["args"]

    def test_reader_rejects_malformed_documents(self):
        for broken in ['a = { b = 1', 'a = [ 1 2', 'a = "open', "a = { = 1 }"]:
            with self.assertRaises(SpaJsonError, msg=broken):
                SpaJson(broken).document()

    def test_it_is_a_standalone_client_config(self):
        for required in (
            "libpipewire-module-protocol-native",
            "libpipewire-module-client-node",
            "libpipewire-module-adapter",
        ):
            self.assertIn(required, self.modules)
        self.assertIn("audio.convert.*", self.config["context.spa-libs"])

    def test_webrtc_cancels_everything_the_speakers_play(self):
        self.assertEqual(self.args["library.name"], "aec/libspa-aec-webrtc")
        self.assertIs(self.args["monitor.mode"], True)

    def test_calls_are_routed_by_role_and_nothing_else_is(self):
        source = self.args["source.props"]
        roles = source["device.intended-roles"]
        self.assertEqual(roles, ["Communication"])
        self.assertEqual(int(source["priority.session"]), 0)
        self.assertEqual(source["node.description"], "Echo-cancelled microphone")
        self.assertNotIn("target.object", source)

    def test_streams_follow_the_defaults_and_only_open_the_mic_on_demand(self):
        for side in ("capture.props", "sink.props"):
            props = self.args[side]
            self.assertIs(props.get("node.passive"), True, side)
            self.assertNotIn("target.object", props, side)
            self.assertNotIn("node.target", props, side)

    def test_node_names_match_what_the_session_hides(self):
        prefix = rust_str_const(AUDIO_RS, "ECHO_CANCEL_NODE_PREFIX")
        source = rust_str_const(AUDIO_RS, "ECHO_CANCEL_SOURCE_NAME")
        self.assertEqual(self.args["source.props"]["node.name"], source)
        for side in ("capture.props", "source.props", "sink.props", "playback.props"):
            self.assertTrue(self.args[side]["node.name"].startswith(prefix), side)

    def test_session_runs_the_file_the_image_installs(self):
        name = rust_str_const(RUNTIME_RS, "ECHO_CANCEL_CONFIG")
        self.assertEqual(name, CONF.name)
        text = CONTAINERFILE.read_text(encoding="utf-8")
        self.assertRegex(
            text,
            r"os/usr/share/pipewire/tilo-echo-cancel\.conf\s+/usr/share/pipewire/tilo-echo-cancel\.conf",
        )

    @unittest.skipUnless(shutil.which("pw-config"), "pw-config is not installed")
    def test_pipewire_parses_it_too(self):
        out = subprocess.run(
            ["pw-config", "--name", str(CONF), "list", "context.modules"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        self.assertIn("libpipewire-module-echo-cancel", out)


if __name__ == "__main__":
    unittest.main()
