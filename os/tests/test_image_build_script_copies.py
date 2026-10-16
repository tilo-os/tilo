"""The image build only sees what the Containerfile COPYs into the builder.

A script that a build step runs, or that a copied script runs beside itself,
but that no COPY brings in, fails the image build late with "No such file or
directory". This pins both directions statically.
"""
import re
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
CONTAINERFILE = ROOT / "os/Containerfile"
SCRIPT = r"[A-Za-z0-9_.-]+\.(?:sh|py)"


def logical_lines(text):
    """Containerfile instructions with line continuations joined."""
    joined = re.sub(r"\\\n", " ", text)
    return [line.strip() for line in joined.splitlines()]


def copied_files():
    """Destination path -> source path, for every COPY of a plain file."""
    copied = {}
    for line in logical_lines(CONTAINERFILE.read_text()):
        if not line.startswith("COPY "):
            continue
        words = [w for w in line.split()[1:] if not w.startswith("--")]
        if len(words) < 2:
            continue
        *sources, destination = words
        for source in sources:
            name = source.rsplit("/", 1)[-1]
            target = destination + name if destination.endswith("/") else destination
            copied[target] = source
    return copied


class ImageBuildScriptCopies(unittest.TestCase):
    def test_scripts_run_by_the_image_build_are_copied(self):
        copied = copied_files()
        text = CONTAINERFILE.read_text()
        called = set(re.findall(rf"/build/os/scripts/{SCRIPT}", text))
        self.assertTrue(called, "no build script call found: the pattern is stale")
        missing = sorted(path for path in called if path not in copied)
        self.assertEqual(missing, [], f"called by the build but never COPYed: {missing}")

    def test_scripts_a_copied_script_runs_beside_itself_are_copied(self):
        copied = copied_files()
        beside = re.compile(rf'BASH_SOURCE\[0\]\}}"\)/({SCRIPT})')
        checked = 0
        for destination, source in sorted(copied.items()):
            if not destination.startswith("/build/os/scripts/"):
                continue
            path = ROOT / source
            if not path.is_file():
                continue
            for name in beside.findall(path.read_text()):
                checked += 1
                sibling = destination.rsplit("/", 1)[0] + "/" + name
                self.assertTrue(
                    sibling in copied,
                    f"{source} runs {name} beside itself, but {sibling} is never COPYed",
                )
        self.assertGreater(checked, 0, "no sibling script call found: the pattern is stale")


if __name__ == "__main__":
    unittest.main()
