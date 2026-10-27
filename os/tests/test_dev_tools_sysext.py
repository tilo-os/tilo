"""Contract for the tilo-dev-tools sysext tree built by os/Containerfile.dev.

The Rust toolchain and the GUI test clients are exportable as a sysext layer
(`--target dev-tools-sysext`). Only CI can build it; this pins what the layer
may contain and that it stays in step with what the dev image installs today.
"""

import re
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CONTAINERFILE = (ROOT / "os/Containerfile.dev").read_text(encoding="utf-8")
SCRIPT_PATH = ROOT / "os/scripts/install-dev-toolchain.sh"
SCRIPT = SCRIPT_PATH.read_text(encoding="utf-8")
LAYERS = (ROOT / "crates/dev/tilo-vmd/src/layers.rs").read_text(encoding="utf-8")
CLIENTS = "chromium gnome-text-editor featherpad qt6-qtwayland alacritty glx-utils xterm"


def stage(name: str) -> str:
    match = re.search(rf"^FROM .+ AS {name}\n(.*?)(?=^FROM |\Z)", CONTAINERFILE, re.M | re.S)
    assert match, name
    return match.group(1)


class DevToolsSysextTest(unittest.TestCase):
    def test_script_is_valid_shell(self):
        subprocess.run(["sh", "-n", str(SCRIPT_PATH)], check=True)

    def test_export_target_is_a_bare_scratch_tree(self):
        self.assertIn("FROM scratch AS dev-tools-sysext", CONTAINERFILE)
        lines = [l for l in stage("dev-tools-sysext").splitlines() if l.strip() and not l.startswith("#")]
        self.assertEqual(lines, ["COPY --from=dev-tools-tree /out-tools/ /"])

    def test_tree_is_computed_on_the_dev_base(self):
        self.assertIn("FROM dev-toolchain AS dev-tools-tree", CONTAINERFILE)
        self.assertIn("install-dev-toolchain.sh tools-tree /out-tools", stage("dev-tools-tree"))

    def test_client_set_matches_the_dev_image(self):
        self.assertIn(CLIENTS, " ".join(stage("dev").split()))
        self.assertIn(CLIENTS, " ".join(SCRIPT.split()))

    def test_only_usr_and_opt_travel(self):
        # tilo-vmd refuses any other top-level tree; the script must not copy one.
        self.assertIn('ALLOWED_TOP: [&str; 2] = ["usr", "opt"]', LAYERS)
        self.assertIn("grep -E '^/(usr|opt)/'", SCRIPT)
        self.assertNotIn("extension-release", SCRIPT)  # the importer writes it

    def test_toolchain_links_resolve_inside_the_overlay(self):
        # Relative to the merged /usr, never to the build's DEST.
        self.assertIn('ln -s "/usr/lib/rust-toolchain/bin/', SCRIPT)

    def test_base_dev_stage_unchanged_until_loader_lands(self):
        # Flipping the base is a separate change: finalize-dev-image.py still
        # needs rustc in the image and acceptance A08-A13 need the clients.
        self.assertIn("install-dev-toolchain.sh rust", CONTAINERFILE)
        self.assertIn("dnf install -y --setopt=reposdir=/repo", stage("dev"))


if __name__ == "__main__":
    unittest.main()
