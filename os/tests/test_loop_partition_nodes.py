#!/usr/bin/env python3
"""loop_partition_nodes on hosts with and without udev.

Without udev (Kata, minimal containers) it creates each partition node itself,
root:root 0600, and removes it again. With udev the nodes already exist as
udev makes them (root:disk 0660 on Fedora): those are used and left alone,
while anything another user could open, or that is not the kernel's device,
is still refused.
"""

from __future__ import annotations

import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "dev"))
import private_disk_image  # noqa: E402


class LoopPartitionNodeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.work = tempfile.TemporaryDirectory()
        root = Path(self.work.name)
        self.devices = root / "dev"
        self.sys_class = root / "sys"
        self.devices.mkdir()
        # As in sysfs: /sys/class/block/<name> links into the device tree, where
        # each partition sits below its disk.
        disk = root / "devices" / "loop3"
        disk.mkdir(parents=True)
        self.sys_class.mkdir()
        (self.sys_class / "loop3").symlink_to(disk)
        for number in (1, 2):
            entry = disk / f"loop3p{number}"
            entry.mkdir()
            (entry / "partition").write_text(f"{number}\n")
            (entry / "dev").write_text(f"259:{number}\n")
            (entry / "uevent").write_text(f"DEVNAME=loop3p{number}\nPARTN={number}\n")
            (self.sys_class / f"loop3p{number}").symlink_to(entry)

    def tearDown(self) -> None:
        self.work.cleanup()

    def run_with(self, metadata: dict | None) -> tuple[list[Path], list, list]:
        """Enter loop_partition_nodes with every existing node reporting `metadata`
        (None: no node exists yet)."""
        created = []

        def lstat(path: Path):
            number = int(path.name[-1])
            if metadata is None and path not in created:
                raise FileNotFoundError(path)
            mode = stat.S_IFBLK | 0o600 if path in created else metadata["mode"]
            return SimpleNamespace(
                st_mode=mode, st_uid=0 if path in created else metadata["uid"],
                st_gid=0 if path in created else metadata["gid"],
                st_rdev=os.makedev(259, number), st_nlink=1, st_dev=1, st_ino=number)

        def mknod(path, mode, device):
            created.append(Path(path))

        unlinked = []
        with patch.object(Path, "lstat", lstat), patch.object(os, "mknod", mknod), \
                patch.object(Path, "unlink", lambda path: unlinked.append(path)):
            with private_disk_image.loop_partition_nodes(
                    "/dev/loop3", devices=self.devices, sys_class=self.sys_class,
                    sleep=lambda _: None) as nodes:
                pass
        return nodes, created, unlinked

    def test_nodes_are_created_without_udev_and_reused_with_it(self) -> None:
        with self.subTest(phase="no_udev_creates_and_removes_private_nodes"):
            nodes, created, unlinked = self.run_with(None)
            self.assertEqual([p.name for p in nodes], ["loop3p1", "loop3p2"])
            self.assertEqual(created, nodes)
            self.assertEqual(sorted(unlinked), sorted(nodes))
        disk = private_disk_image._disk_group()
        with self.subTest(phase="udev_nodes_are_used_and_kept"):
            for gid, mode in ((0, 0o600), (0, 0o660), (disk if disk is not None else 0, 0o660)):
                nodes, created, unlinked = self.run_with(
                    {"mode": stat.S_IFBLK | mode, "uid": 0, "gid": gid})
                self.assertEqual(created, [])
                self.assertEqual(unlinked, [])
        with self.subTest(phase="nodes_others_could_use_are_refused"):
            for metadata in (
                {"mode": stat.S_IFBLK | 0o666, "uid": 0, "gid": 0},
                {"mode": stat.S_IFBLK | 0o660, "uid": 1000, "gid": 0},
                {"mode": stat.S_IFBLK | 0o660, "uid": 0, "gid": 12345},
                {"mode": stat.S_IFREG | 0o600, "uid": 0, "gid": 0},
                {"mode": stat.S_IFBLK | 0o4600, "uid": 0, "gid": 0},
            ):
                with self.assertRaisesRegex(ValueError, "differs from kernel identity"):
                    self.run_with(metadata)


if __name__ == "__main__":
    unittest.main()
