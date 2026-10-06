import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
PRUNE = ROOT / "scripts" / "vm-acceptance" / "prune_disks.py"
BUILT_BY = "scripts/vm-acceptance/build-disk.sh"


@unittest.skipUnless(shutil.which("qemu-img"), "qemu-img is needed to read backing chains")
class PruneDisksTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.dir = self.root / "vm-images"
        self.dir.mkdir()
        self.config = self.root / "config"
        (self.config / "tilo-vm").mkdir(parents=True)

    def disk(self, name, hour, built_by: str | None = BUILT_BY, backing=None):
        path = self.dir / f"tilo-dev-{name}.qcow2"
        cmd = ["qemu-img", "create", "-q", "-f", "qcow2"]
        if backing:
            cmd += ["-b", str(backing), "-F", "qcow2"]
        subprocess.run([*cmd, str(path), "64M"], check=True)
        if built_by is not None:
            record = {"commit": "0" * 40, "built_at": f"2026-10-02T{hour:02d}:00:00+00:00",
                      "built_by": built_by}
            Path(f"{path}.provenance.json").write_text(json.dumps(record))
            Path(f"{path}.build.log").write_text("disk=…\n")
        return path

    def prune(self, *args):
        env = {**os.environ, "XDG_CONFIG_HOME": str(self.config)}
        env.pop("TILO_DISK_KEEP", None)
        return subprocess.run([sys.executable, str(PRUNE), *args, str(self.dir)],
                              capture_output=True, text=True, check=True, env=env)

    def left(self):
        return sorted(p.name for p in self.dir.iterdir())

    def test_keeps_the_newest_and_removes_sidecars(self):
        for hour, name in enumerate(["a", "b", "c", "d", "e"]):
            self.disk(name, hour)
        self.prune("--keep", "2")
        self.assertEqual(
            [n for n in self.left() if n.endswith(".qcow2")],
            ["tilo-dev-d.qcow2", "tilo-dev-e.qcow2"],
        )
        self.assertFalse((self.dir / "tilo-dev-a.qcow2.provenance.json").exists())
        self.assertFalse((self.dir / "tilo-dev-a.qcow2.build.log").exists())

    def test_never_touches_disks_it_cannot_prove_build_disk_wrote(self):
        self.disk("hand", 0, built_by=None)
        self.disk("other", 1, built_by="someone else")
        self.disk("new", 2)
        self.prune("--keep", "1")
        self.assertIn("tilo-dev-hand.qcow2", self.left())
        self.assertIn("tilo-dev-other.qcow2", self.left())

    def test_keeps_backing_files_protected_and_registered_disks(self):
        base = self.disk("base", 0)
        self.disk("overlay", 1, built_by=None, backing=base)
        registered = self.disk("registered", 2)
        protected = self.disk("protected", 3)
        child = self.root / "vm-child.qcow2"
        vm_image = self.disk("vm-image", 4)
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-b", str(vm_image),
                        "-F", "qcow2", str(child)], check=True)
        self.disk("old", 5)
        self.disk("newest", 6)
        (self.config / "tilo-vm" / "vms.toml").write_text(
            f'[vms.acc]\nimage = "{registered}"\n[vms.other]\ndisk = "{child}"\n')
        self.prune("--keep", "1", "--protect", str(protected))
        self.assertEqual(
            [n for n in self.left() if n.endswith(".qcow2")],
            ["tilo-dev-base.qcow2", "tilo-dev-newest.qcow2", "tilo-dev-overlay.qcow2",
             "tilo-dev-protected.qcow2", "tilo-dev-registered.qcow2", "tilo-dev-vm-image.qcow2"],
        )

    def test_keeps_open_disks(self):
        held = self.disk("held", 0)
        self.disk("newest", 1)
        with open(held, "rb"):
            self.prune("--keep", "1")
        self.assertIn("tilo-dev-held.qcow2", self.left())

    def test_dry_run_and_all_delete_nothing(self):
        for hour, name in enumerate(["a", "b", "c"]):
            self.disk(name, hour)
        before = self.left()
        out = self.prune("--keep", "1", "--dry-run").stdout
        self.assertIn("would remove", out)
        self.prune("--keep", "all")
        self.assertEqual(self.left(), before)


if __name__ == "__main__":
    unittest.main()
