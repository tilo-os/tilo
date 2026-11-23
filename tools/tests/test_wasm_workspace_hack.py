"""The wasm32 workspace-hack check resolves what the page's families build.

A native dependency added through any member of that selection reaches a page's
build; resolving one root alone missed it, and the page's wasm32 clippy found it
instead.
"""
import importlib.util
import io
import os
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

TOOLS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "check_wasm_workspace_hack", TOOLS / "policy/check_wasm_workspace_hack.py")
hack = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hack)

WORKSPACE = '''[workspace]
members = ["page", "session", "helper", "workspace-hack", "wasm-bindgen"]
resolver = "2"
'''
CRATE = '''[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
'''
MEMBERS = ("page", "session", "helper", "workspace-hack", "wasm-bindgen")
PAGE = CRATE.format(name="page") + '\n[dependencies]\nwasm-bindgen = { path = "../wasm-bindgen" }\n'
SESSION = CRATE.format(name="session") + '\n[dependencies]\nwasm-bindgen = { path = "../wasm-bindgen" }\n'
SESSION_WITH_HELPER = SESSION + 'helper = { path = "../helper" }\n'
SESSION_GATED = SESSION + '''
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
helper = { path = "../helper" }
'''
HELPER = CRATE.format(name="helper") + '\n[dependencies]\nworkspace-hack = { path = "../workspace-hack" }\n'


class Fixture:
    """A workspace written for one test, removed with it."""

    def __init__(self, files):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.files = files

    def __enter__(self):
        root = self.root
        for name, text in self.files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        for name in MEMBERS:
            (root / name / "src").mkdir(parents=True, exist_ok=True)
            (root / name / "src/lib.rs").write_text("")
            if not (root / name / "Cargo.toml").exists():
                (root / name / "Cargo.toml").write_text(CRATE.format(name=name))
        # One resolve writes the lock the check's --locked calls read; path dependencies need
        # no net.
        subprocess.run(["cargo", "metadata", "--format-version", "1"], cwd=root, check=True,
                       capture_output=True, env={**os.environ, "CARGO_NET_OFFLINE": "true"})
        return root

    def __exit__(self, *_):
        self.directory.cleanup()
        return False


def verdict(root):
    original = hack.ROOT
    hack.ROOT = root
    out, err = io.StringIO(), io.StringIO()
    try:
        with redirect_stdout(out), redirect_stderr(err):
            code = hack.main()
    finally:
        hack.ROOT = original
    return code, out.getvalue(), err.getvalue()


class WasmWorkspaceHackTests(unittest.TestCase):
    def test_a_native_dependency_added_through_a_member_fails(self):
        # The page root alone is clean; the session, also in the selection, pulls the hack in
        # through a member's dependency. Resolving either root alone would pass.
        with Fixture({"Cargo.toml": WORKSPACE, "page/Cargo.toml": PAGE,
                      "session/Cargo.toml": SESSION_WITH_HELPER,
                      "helper/Cargo.toml": HELPER}) as root:
            code, out, err = verdict(root)
        self.assertEqual(code, 1, out + err)
        self.assertIn("session/Cargo.toml: session reaches workspace-hack", out)
        self.assertIn("helper", out)

    def test_the_same_dependency_gated_to_native_builds_passes(self):
        with Fixture({"Cargo.toml": WORKSPACE, "page/Cargo.toml": PAGE,
                      "session/Cargo.toml": SESSION_GATED,
                      "helper/Cargo.toml": HELPER}) as root:
            code, out, err = verdict(root)
        self.assertEqual(code, 0, out + err)
        self.assertIn("no workspace-hack", err)


if __name__ == "__main__":
    unittest.main()
