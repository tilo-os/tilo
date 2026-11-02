#!/usr/bin/env python3
"""Every unit file under os/usr/lib/systemd/system is placed on purpose.

A unit nothing installs is a unit nothing starts, and systemd only says so when
a recipe enables it by name (the build then fails with "Unit does not exist").
Dropped from the recipe *and* from the line that enabled it, a hook leaves no
trace at all until the machine needs it. That is how the machine's first boot
went: os/image stopped copying the machine's first-boot unit, nothing ran the
first-boot setup any more, and the charge limit in Settings kept starting a
`tilo-battery-cap@` instance the image never shipped.

So, over every unit this repository keeps:

  * a recipe that ships unit files installs it, or PLACED_ELSEWHERE says which
    medium installs it instead;
  * a unit that declares [Install] is enabled by a recipe, or STARTED_BY says
    what starts it without one.
"""
from __future__ import annotations

from pathlib import Path
import re
import unittest

REPO = Path(__file__).resolve().parents[2]
UNIT_DIR = REPO / "os/usr/lib/systemd/system"
SUFFIXES = (".service", ".target", ".socket", ".timer", ".path", ".mount")

# Units another medium installs, and how. The live installer does not COPY each
# of its units: it stages the whole os/usr tree (os/dev/installer_overlay.py),
# so its units appear in no COPY line, and os/Containerfile.installer enables
# them there.
INSTALLER_MEDIUM = (
    "the live installer stages all of os/usr (os/dev/installer_overlay.py) "
    "and os/Containerfile.installer enables it"
)
PLACED_ELSEWHERE = {
    "tilo-installer-code-refresh.service": INSTALLER_MEDIUM,
    "tilo-installer-grow-root.service": INSTALLER_MEDIUM,
    "tilo-installer-setup.service": INSTALLER_MEDIUM,
    "tilo-live-recovery.service": INSTALLER_MEDIUM,
    "tilo-staging-handoff.service": INSTALLER_MEDIUM,
    "tilo-staging-reclaim.service": INSTALLER_MEDIUM
    + " (its timer starts it)",
    "tilo-staging-reclaim.timer": INSTALLER_MEDIUM,
}

# Units that ask to be started at boot but reach their start without a wants
# link of their own.
STARTED_BY = {
    "tilo-rollback-harden.service": "tilo-rollback-harden.timer starts it",
    "tilo-board-headless-session@.service": (
        "os/dev/disk-image.sh links the instance into multi-user.target.wants "
        "for the board's account"
    ),
    "tilo-node-relay@.service": (
        "a node's install enables the instance for its account, as the unit asks"
    ),
    "tilo-node-session@.path": (
        "a node's install enables the instance for its account, as the unit asks"
    ),
}


def recipes() -> list[Path]:
    """The files that put unit files on a disk.

    os/Containerfile is generated from os/image/** (tools/ci/os/pieces.py) and
    both are read, so an edit to either is seen.
    """
    paths = [REPO / "os/Containerfile", REPO / "os/components.toml"]
    paths += sorted(
        path
        for path in (REPO / "os").glob("Containerfile.*")
        if not path.name.endswith(".dockerignore")
    )
    paths += sorted((REPO / "os/image").rglob("*.Containerfile"))
    paths += sorted((REPO / "os/containers").glob("*/Containerfile"))
    return [path for path in paths if path.is_file()]


def units() -> list[str]:
    return sorted(
        path.name
        for path in UNIT_DIR.iterdir()
        if path.is_file() and path.name.endswith(SUFFIXES)
    )


def enabled(repo_text: str) -> set[str]:
    """The units `systemctl enable` names in one recipe."""
    # A RUN's commands continue across `\`-ended lines; joining them first keeps
    # an enable whose units are on the next line.
    joined = repo_text.replace("\\\n", " ")
    found: set[str] = set()
    for command in re.split(r"&&|\|\||;|\n", joined):
        for match in re.finditer(r"systemctl (?:--global )?enable\b(.*)", command):
            for word in match.group(1).split():
                word = word.strip("\"'")
                if word.endswith(SUFFIXES):
                    found.add(word)
    return found


class ShippedUnitsTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.texts = {path: path.read_text() for path in recipes()}
        cls.enabled = {
            unit: [str(path.relative_to(REPO)) for path, text in cls.texts.items()
                   if unit in enabled(text)]
            for unit in units()
        }

    def installed_by(self, unit: str) -> list[str]:
        return [
            str(path.relative_to(REPO))
            for path, text in self.texts.items()
            if f"systemd/system/{unit}" in text
        ]

    def asks_to_start(self, unit: str) -> bool:
        body = (UNIT_DIR / unit).read_text()
        return bool(re.search(r"^\[Install\]", body, re.MULTILINE))

    def test_every_unit_is_installed_by_a_recipe(self) -> None:
        missing = [
            unit
            for unit in units()
            if not self.installed_by(unit) and unit not in PLACED_ELSEWHERE
        ]
        self.assertEqual(
            missing,
            [],
            "units no recipe installs: name the medium that ships them in "
            "PLACED_ELSEWHERE, or install them",
        )

    def test_every_unit_that_asks_to_start_is_enabled(self) -> None:
        missing = [
            unit
            for unit in units()
            if self.asks_to_start(unit)
            and not self.enabled[unit]
            and unit not in STARTED_BY
        ]
        self.assertEqual(
            missing,
            [],
            "units that ask to start at boot and nothing enables: enable them, "
            "or name what starts them in STARTED_BY",
        )

    def test_the_lists_do_not_outlive_what_they_describe(self) -> None:
        """A unit that is installed or enabled again must leave its list, so the
        exception cannot quietly cover a later drop."""
        installed_again = sorted(
            unit for unit in PLACED_ELSEWHERE if self.installed_by(unit)
        )
        enabled_again = sorted(unit for unit in STARTED_BY if self.enabled[unit])
        self.assertEqual(installed_again, [], "a recipe installs these now")
        self.assertEqual(enabled_again, [], "a recipe enables these now")

    def test_every_listed_unit_exists(self) -> None:
        known = set(units())
        self.assertEqual(sorted(set(PLACED_ELSEWHERE) - known), [])
        self.assertEqual(sorted(set(STARTED_BY) - known), [])


if __name__ == "__main__":
    unittest.main()
