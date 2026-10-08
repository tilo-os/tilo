#!/usr/bin/env python3
"""Check the native text proof's existing packaging boundary, without building an app."""
from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/headless-examples"))
SPEC = importlib.util.spec_from_file_location(
    "native_text_proof", ROOT / "scripts/headless-examples/code-editor-text-proof.py")
assert SPEC and SPEC.loader
PROOF = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROOF)
XDG = ("XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME")


class PackagingReached(Exception):
    """Stop before any compiler, signed package, runtime or native process starts."""


class BuildEnvironmentTests(unittest.TestCase):
    def run_packaging_boundary(self, caller):
        session = {key: f"/private-session/{key}" for key in XDG}
        session.update({"CARGO": "/selected/cargo-compatible-builder",
                        "TILO_IDENTITY_ROOT": "/private-session/identity",
                        "TILO_APPS_DATA_DIR": "/private-session/apps"})
        original = dict(session)
        calls = []

        def invoke(binary, args, environment):
            calls.append((binary, args, dict(environment)))
            if len(calls) == 2:
                raise PackagingReached()

        with patch.dict(os.environ, caller, clear=True), patch.object(PROOF.bootstrap, "run", invoke):
            with self.assertRaises(PackagingReached):
                PROOF.package(Path("/binaries"), Path("/private-session"), session)
        self.assertEqual(session, original, "packaging must not rewrite native runtime configuration")
        self.assertEqual(calls[0][0], Path("/binaries/tilo-app-package"))
        self.assertEqual(calls[0][2], session, "key generation keeps the private identity environment")
        self.assertEqual(calls[1][0], Path("/binaries/tilo-app"))
        self.assertEqual(calls[1][1][0], "package")
        self.assertIn("thin-app", calls[1][1])
        self.assertEqual(calls[1][2]["CARGO"], session["CARGO"])
        self.assertEqual(calls[1][2]["TILO_IDENTITY_ROOT"], session["TILO_IDENTITY_ROOT"])
        self.assertEqual(calls[1][2]["TILO_APPS_DATA_DIR"], session["TILO_APPS_DATA_DIR"])
        return calls[1][2]

    def test_only_packaging_uses_callers_existing_xdg_build_configuration(self):
        caller = {key: f"/caller-build/{key}" for key in XDG}
        build = self.run_packaging_boundary(caller)
        for key in XDG:
            self.assertEqual(build[key], caller[key])

    def test_unset_caller_xdg_uses_builder_defaults_without_private_session_fallback(self):
        build = self.run_packaging_boundary({})
        for key in XDG:
            self.assertNotIn(key, build)

    def test_partial_caller_configuration_preserves_only_actual_present_fields(self):
        caller = {"XDG_CONFIG_HOME": "/caller-config"}
        build = self.run_packaging_boundary(caller)
        self.assertEqual(build["XDG_CONFIG_HOME"], "/caller-config")
        for key in XDG[1:]:
            self.assertNotIn(key, build)


if __name__ == "__main__":
    unittest.main()
