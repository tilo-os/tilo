#!/usr/bin/env python3
"""The early-boot units this repository ships close no ordering cycle.

systemd breaks a cycle by deleting one job in it and booting on, so the only
symptom is a unit that silently never ran. An early unit once sat
before systemd-sysext while PrivateTmp=yes ordered it after
systemd-tmpfiles-setup, which systemd-sysext itself precedes: the boot dropped
tmpfiles or sysext at random. The image build runs `systemd-analyze verify`
(os/scripts/verify-boot-ordering.sh); this is the same property checked
without an image, over the shipped units plus the stock early-boot units they
order against.
"""
from __future__ import annotations

import configparser
from collections import defaultdict
from pathlib import Path
import unittest

REPO = Path(__file__).resolve().parents[2]
UNIT_DIRS = (
    REPO / "os/usr/lib/systemd/system",
    REPO / "os/etc/systemd/system",
    REPO / "os/dev-image/usr/lib/systemd/system",
    REPO / "crates/dev/tilo-vmd/data",
)
UNIT_SUFFIXES = (".service", ".target", ".socket", ".timer", ".path", ".mount")

# Early-boot ordering of the stock Fedora units (systemd 259), as
# `systemctl show -p After -p Before` reports them on a booted dev image.
# The dev image enables systemd-sysext; release images mask it.
STOCK: dict[str, dict] = {
    "systemd-sysext.service": {
        "dd": False, "after": ["local-fs.target"],
        "before": ["sysinit.target", "systemd-tmpfiles-setup.service"]},
    "systemd-tmpfiles-setup.service": {
        "dd": False,
        "after": ["local-fs.target", "systemd-sysusers.service", "systemd-journald.service"],
        "before": ["sysinit.target"]},
    "systemd-sysusers.service": {
        "dd": False,
        "after": ["systemd-remount-fs.service", "systemd-tmpfiles-setup-dev-early.service"],
        "before": ["sysinit.target", "systemd-tmpfiles-setup.service",
                   "systemd-tmpfiles-setup-dev.service"]},
    "systemd-journald.service": {
        "dd": False, "after": [],
        "before": ["sysinit.target", "systemd-tmpfiles-setup.service"]},
    "systemd-remount-fs.service": {
        "dd": False, "after": [],
        "before": ["local-fs.target", "systemd-sysusers.service"]},
    "systemd-tmpfiles-setup-dev-early.service": {
        "dd": False, "after": [],
        "before": ["systemd-sysusers.service", "local-fs-pre.target",
                   "systemd-tmpfiles-setup-dev.service", "sysinit.target"]},
    "systemd-tmpfiles-setup-dev.service": {
        "dd": False,
        "after": ["systemd-sysusers.service", "systemd-tmpfiles-setup-dev-early.service"],
        "before": ["sysinit.target", "local-fs-pre.target"]},
    "local-fs-pre.target": {"dd": False, "after": [], "before": ["local-fs.target"]},
    "local-fs.target": {"dd": False, "after": ["local-fs-pre.target"], "before": ["sysinit.target"]},
    "sysinit.target": {"dd": False, "after": ["local-fs.target"], "before": []},
    "sockets.target": {"dd": False, "after": [], "before": ["basic.target"]},
    "timers.target": {"dd": False, "after": [], "before": ["basic.target"]},
    "paths.target": {"dd": False, "after": [], "before": ["basic.target"]},
    "basic.target": {"dd": False, "after": ["sysinit.target"], "before": []},
}


def words(section: dict[str, list[str]], key: str) -> list[str]:
    return [word for value in section.get(key, []) for word in value.split()]


def parse(text: str) -> dict[str, dict[str, list[str]]]:
    """Unit sections to key -> every value, in order (keys repeat in units)."""
    sections: dict[str, dict[str, list[str]]] = defaultdict(lambda: defaultdict(list))
    current = None
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line[0] in "#;":
            continue
        if line.startswith("[") and line.endswith("]"):
            current = line[1:-1]
            continue
        if current is None or "=" not in line:
            continue
        key, value = (part.strip() for part in line.split("=", 1))
        if value == "":
            sections[current][key] = []
        else:
            sections[current][key].append(value)
    return sections


def shipped_units() -> dict[str, str]:
    """Unit name -> its text with every shipped drop-in appended."""
    units: dict[str, str] = {}
    for directory in UNIT_DIRS:
        if not directory.is_dir():
            continue
        for path in sorted(directory.iterdir()):
            if path.is_file() and path.name.endswith(UNIT_SUFFIXES) and "@" not in path.name:
                units[path.name] = path.read_text(encoding="utf-8")
        for dropins in sorted(directory.glob("*.d")):
            name = dropins.name[:-2]
            for conf in sorted(dropins.glob("*.conf")):
                units[name] = units.get(name, "") + "\n" + conf.read_text(encoding="utf-8")
    return units


def after_graph(units: dict[str, str]) -> dict[str, set[str]]:
    """unit -> the units it starts after, with systemd's implicit orderings."""
    after: dict[str, set[str]] = defaultdict(set)
    parsed = {name: parse(text) for name, text in units.items()}
    for name, spec in STOCK.items():
        after[name].update(spec["after"])
        for later in spec["before"]:
            after[later].add(name)

    def default_deps(name: str) -> bool:
        values = parsed.get(name, {}).get("Unit", {}).get("DefaultDependencies")
        if not values:
            return STOCK.get(name, {}).get("dd", True)
        return values[-1].lower() not in ("no", "false", "0", "off")

    wanted_by: list[tuple[str, str]] = []
    for name, sections in parsed.items():
        unit = sections.get("Unit", {})
        service = sections.get("Service", {})
        after[name].update(words(unit, "After"))
        for later in words(unit, "Before"):
            after[later].add(name)
        # systemd.exec(5): PrivateTmp=yes orders the unit after
        # systemd-tmpfiles-setup.service; "disconnected" adds no ordering.
        if (service.get("PrivateTmp") or ["no"])[-1].lower() in ("yes", "true", "1", "on"):
            after[name].add("systemd-tmpfiles-setup.service")
        if default_deps(name):
            kind = name.rsplit(".", 1)[-1]
            if kind == "service":
                after[name].update(("sysinit.target", "basic.target"))
            elif kind in ("socket", "timer", "path"):
                after[name].add("sysinit.target")
                after[f"{kind}s.target"].add(name)
            elif kind == "mount":
                after[name].add("local-fs-pre.target")
                after["local-fs.target"].add(name)
        install = sections.get("Install", {})
        for target in words(install, "WantedBy") + words(install, "RequiredBy"):
            wanted_by.append((target, name))
        if name.endswith(".target"):
            wanted_by.extend((name, other) for other in words(unit, "Wants") + words(unit, "Requires"))
    # systemd.target(5): a target with default dependencies starts after the
    # units it wants or requires, unless that unit has none itself or is
    # already ordered after the target (unit_add_default_target_dependency).
    for target, name in wanted_by:
        if (target.endswith(".target") and default_deps(target) and default_deps(name)
                and target not in after[name]):
            after[target].add(name)
    return after


def cycles(after: dict[str, set[str]]) -> list[list[str]]:
    """Strongly connected components with more than one unit, or a self-loop."""
    index: dict[str, int] = {}
    low: dict[str, int] = {}
    stack: list[str] = []
    on_stack: set[str] = set()
    found: list[list[str]] = []
    counter = 0
    nodes = set(after) | {n for targets in after.values() for n in targets}
    for root in sorted(nodes):
        if root in index:
            continue
        work = [(root, iter(sorted(after.get(root, ()))))]
        index[root] = low[root] = counter
        counter += 1
        stack.append(root)
        on_stack.add(root)
        while work:
            node, children = work[-1]
            child = next(children, None)
            if child is not None:
                if child not in index:
                    index[child] = low[child] = counter
                    counter += 1
                    stack.append(child)
                    on_stack.add(child)
                    work.append((child, iter(sorted(after.get(child, ())))))
                elif child in on_stack:
                    low[node] = min(low[node], index[child])
                continue
            work.pop()
            if work:
                low[work[-1][0]] = min(low[work[-1][0]], low[node])
            if low[node] == index[node]:
                component = []
                while True:
                    member = stack.pop()
                    on_stack.discard(member)
                    component.append(member)
                    if member == node:
                        break
                if len(component) > 1 or node in after.get(node, ()):
                    found.append(sorted(component))
    return found


class BootOrdering(unittest.TestCase):
    def test_the_shipped_units_close_no_ordering_cycle(self):
        units = shipped_units()
        self.assertIn("tilo-store-loop.service", units,
                      "the unit scan found none of the shipped early-boot units")
        self.assertEqual(cycles(after_graph(units)), [])

    def test_the_model_sees_the_cycle_private_tmp_once_closed(self):
        units = shipped_units()
        early = ("[Unit]\nDefaultDependencies=no\nAfter=local-fs.target\n"
                 "Before=systemd-sysext.service sysinit.target\n[Service]\n")
        units["early-before-sysext.service"] = early + "PrivateTmp=disconnected\n"
        self.assertEqual(cycles(after_graph(units)), [])
        units["early-before-sysext.service"] = early + "PrivateTmp=yes\n"
        found = cycles(after_graph(units))
        self.assertTrue(
            any({"early-before-sysext.service", "systemd-sysext.service",
                 "systemd-tmpfiles-setup.service"} <= set(c) for c in found), found)

    def test_extension_activation_is_not_an_early_boot_unit(self):
        # It rebuilds parts from /var and merges them on purpose, after sysinit;
        # the masked systemd-sysext.service never merges /var by itself.
        unit = parse(shipped_units()["tilo-extension-activate.service"])
        self.assertNotIn("DefaultDependencies", unit["Unit"])
        self.assertNotIn("systemd-sysext.service", words(unit["Unit"], "Before"))

    def test_extension_activation_starts_only_when_the_helper_has_work(self):
        # It sits before the greeter, and its Python start alone cost 0.4-1.4 s per
        # boot (TIL-767). The unit's triggering conditions are the helper's own
        # early return: nothing turned on and no part left in /var/lib/extensions.
        unit = parse(shipped_units()["tilo-extension-activate.service"])
        self.assertEqual(unit["Unit"]["ConditionPathExistsGlob"], ["|/var/lib/tilo/extensions/*/enabled"])
        self.assertEqual(unit["Unit"]["ConditionDirectoryNotEmpty"], ["|/var/lib/extensions"])
        helper = (REPO / "os/usr/libexec/tilo-extension").read_text()
        self.assertIn('return self.at("/var/lib/tilo/extensions")', helper)
        self.assertIn('return self.state(ident) / "enabled"', helper)
        self.assertIn('return self.at("/var/lib/extensions")', helper)
        self.assertIn("    if not any(layout.desired(c.id).exists() for c in catalogues) \\\n"
                      "            and not any((layout.extensions / name).exists() for name in ours):\n"
                      "        return 0\n", helper)

    def test_nothing_before_tmpfiles_setup_is_ordered_after_it(self):
        after = after_graph(shipped_units())
        seen: set[str] = set()
        pending = ["systemd-tmpfiles-setup.service"]
        while pending:
            unit = pending.pop()
            for dependency in after.get(unit, ()):
                if dependency not in seen:
                    seen.add(dependency)
                    pending.append(dependency)
        self.assertIn("systemd-sysext.service", seen)
        self.assertNotIn("systemd-tmpfiles-setup.service", seen)


if __name__ == "__main__":
    unittest.main()
