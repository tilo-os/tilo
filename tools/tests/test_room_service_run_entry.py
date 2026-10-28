"""`run.py`'s `entry.json` is the image's, field for field.

The documented persistent-mode Lounge had no "Give feedback…" because the
local entry named no feedback channel: the room service kept the channel in
Chat, but the web entry never told the page where reports go.
"""

from __future__ import annotations

import argparse
import importlib.util
import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
RUN = ROOT / "crates/hosting/tilo-room-service/dev/run.py"
ENTRYPOINT = ROOT / "crates/browser/tilo-browser-runtime/container/entrypoint.sh"
RELAY_CHAT = ROOT / "crates/hosting/tilo-relay/src/room_chat.rs"


def load_run():
    spec = importlib.util.spec_from_file_location("tilo_room_service_run", RUN)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def args(**fields):
    parser = argparse.ArgumentParser()
    return argparse.Namespace(**{
        "name": "lounge",
        "shown_name": "Lounge",
        "home_app": None,
        "feedback_channel": None,
        **fields,
    })


class EntryJson(unittest.TestCase):
    def test_a_feedback_channel_names_where_reports_go(self):
        run = load_run()
        entry = run.entry_json(
            args(feedback_channel="feedback"), "http://localhost:9864", "http://localhost:8890"
        )
        self.assertEqual(entry["home"]["feedback"], {"channel": "c_feedback"})
        # The id is the one the relay keeps the channel under: `c_<name>`, with
        # `-` as `_` (crates/hosting/tilo-relay/src/room_chat.rs `channel_id`).
        body = re.search(r"fn channel_id\(name: &str\) -> String \{(.*?)\n\}",
                         RELAY_CHAT.read_text(), re.S).group(1)
        self.assertIn('format!("c_{}", name.replace(\'-\', "_"))', body)
        self.assertEqual(run.channel_id("show-and-tell"), "c_show_and_tell")

    def test_without_one_reports_are_not_offered(self):
        run = load_run()
        entry = run.entry_json(args(), "http://localhost:9864", "http://localhost:8890")
        self.assertNotIn("feedback", entry["home"])

    def test_the_entry_has_the_images_shape(self):
        # entrypoint.sh writes the same shape from TILO_WEB_HOME_FEEDBACK.
        text = ENTRYPOINT.read_text()
        self.assertIn('\\"feedback\\":{\\"channel\\":\\"$feedback\\"}', text)
        self.assertIn("feedback=${TILO_WEB_HOME_FEEDBACK:-}", text)


if __name__ == "__main__":
    unittest.main()
