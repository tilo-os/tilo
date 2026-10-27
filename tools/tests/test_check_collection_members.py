#!/usr/bin/env python3
"""The one allowlist of app collection members, and the two readers that use it."""
from pathlib import Path
import subprocess
import unittest
from hang import HANG_BOUND

ROOT = Path(__file__).resolve().parents[2]
CHECK = ROOT / "tools/apps/check-collection-members.sh"
RUNTIME = "runtime/tilo-runtime/2026.07+native.07ebd4fdaafcf63c676f16737f3fb233"
VALID = ["tapp/", "tapp/files.tapp", "runtime/", RUNTIME + "/", RUNTIME + "/bundle.toml",
         "index.json", "SHA256SUMS"]


def check(members, *flags, label="app collection"):
    return subprocess.run(["bash", str(CHECK), *flags, label], input="".join(m + "\n" for m in members),
                          text=True, capture_output=True, timeout=HANG_BOUND)


class CollectionMembersTest(unittest.TestCase):
    def test_a_collection_with_variable_fonts_passes(self):
        # Run 90890 failed on the first of these: every current collection carries them.
        for name in ("AtkinsonHyperlegibleNext-Italic[wght].ttf", "Roboto[wdth,wght].ttf",
                     "Custom[GRAD].otf", "Regular.ttf"):
            with self.subTest(name=name):
                result = check(VALID + [RUNTIME + "/lib/fonts/" + name], "--require-tapp")
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_unsafe_or_malformed_members_are_refused(self):
        for member in (
            "../outside", "/tmp/outside", RUNTIME + "/../outside", "tapp/../x.tapp",
            RUNTIME + "//lib/fonts/Test[wght].ttf",
            RUNTIME + "/lib/fonts/Test[].ttf", RUNTIME + "/lib/fonts/Test[wght.ttf",
            RUNTIME + "/lib/fonts/Test[wght]].ttf", RUNTIME + "/lib/fonts/Test[wght,].ttf",
            RUNTIME + "/lib/fonts/Test[weight].ttf", RUNTIME + "/lib/fonts/Test[wght].so",
            RUNTIME + "/lib/Test[wght].ttf", RUNTIME + "/lib/fonts/sub/Test[wght].ttf",
            "tapp/Test[wght].tapp", "tapp/files.tar", "extra.json",
        ):
            with self.subTest(member=member):
                result = check(VALID + [member], label="prior collection")
                self.assertEqual(result.returncode, 1)
                self.assertIn(f"unexpected prior collection member: {member}", result.stderr)

    def test_require_tapp_refuses_a_collection_without_one(self):
        members = [m for m in VALID if not m.endswith(".tapp")]
        self.assertEqual(check(members).returncode, 0)
        result = check(members, "--require-tapp")
        self.assertEqual(result.returncode, 1)
        self.assertIn("holds no tapp", result.stderr)

    def test_prior_collection_reader_uses_the_allowlist(self):
        reuse = (ROOT / "tools/reuse_prior_app_collection.sh").read_text()
        self.assertIn('tar -tzf "$prior" | "$(dirname "${BASH_SOURCE[0]}")/apps/check-collection-members.sh" '
                      '"prior collection"', reuse)
        self.assertNotIn("while IFS= read -r member", reuse)


if __name__ == "__main__":
    unittest.main()
