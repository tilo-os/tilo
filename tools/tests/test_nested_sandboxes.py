"""tools/ci/nested-sandboxes.toml names workspace packages, and its reader filters by name."""
from __future__ import annotations

from pathlib import Path
import subprocess
import sys
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
READER = ROOT / "tools/ci/nested_sandboxes.py"


def workspace_packages() -> set[str]:
    members = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["members"]
    names = set()
    for pattern in members:
        for manifest in ROOT.glob(f"{pattern}/Cargo.toml"):
            package = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {})
            if "name" in package:
                names.add(package["name"])
    return names


def read(*args: str) -> list[str]:
    return subprocess.run([sys.executable, str(READER), *args], check=True,
                          capture_output=True, text=True).stdout.split()


class NestedSandboxes(unittest.TestCase):
    def test_every_listed_package_is_a_workspace_member(self) -> None:
        listed = read()
        self.assertTrue(listed)
        self.assertEqual(sorted(set(listed) - workspace_packages()), [])

    def test_names_filter_the_list(self) -> None:
        self.assertEqual(read("-p", "shell", "-p", "collab-types", "ush"), ["shell", "ush"])
        self.assertEqual(read("-p", "collab-types"), [])


if __name__ == "__main__":
    unittest.main()
