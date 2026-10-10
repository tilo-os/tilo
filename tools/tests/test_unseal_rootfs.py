import importlib.util
import shutil
import struct
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
KVER = "7.2.0-1.tilo.x86_64"


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


unseal = load("unseal_rootfs_under_test", REPO / "os/scripts/unseal-rootfs.py")


def fake_uki(sections: dict[str, bytes]) -> bytes:
    """A minimal PE image: DOS stub, COFF header, no optional header, sections."""
    pe = 0x40
    table = pe + 24
    data_start = table + 40 * len(sections)
    header = bytearray(data_start)
    header[0:2] = b"MZ"
    struct.pack_into("<I", header, 0x3C, pe)
    header[pe:pe + 4] = b"PE\0\0"
    struct.pack_into("<H", header, pe + 6, len(sections))
    struct.pack_into("<H", header, pe + 20, 0)
    body = bytearray()
    for index, (name, payload) in enumerate(sections.items()):
        entry = table + 40 * index
        header[entry:entry + 8] = name.encode().ljust(8, b"\0")
        struct.pack_into("<IIII", header, entry + 8, len(payload), 0x1000 * (index + 1), len(payload),
                         data_start + len(body))
        body += payload
    return bytes(header + body)


class UnsealRootfsTests(unittest.TestCase):
    def root(self) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "usr/lib/modules" / KVER).mkdir(parents=True)
        (root / "usr/libexec").mkdir(parents=True)
        shutil.copy(REPO / "os/dev/uki-cmdline.py", root / "usr/libexec/tilo-uki-cmdline")
        return root

    def uki(self, root: Path) -> Path:
        path = root / "boot/EFI/Linux" / f"{KVER}.efi"
        path.parent.mkdir(parents=True)
        path.write_bytes(fake_uki({".osrel": b"ID=tilo\n", ".cmdline": b"composefs=00 rw",
                                   ".linux": b"KERNEL-FROM-UKI", ".initrd": b"INITRD-FROM-UKI"}))
        extra = path.with_name(path.name + ".extra.d")
        extra.mkdir()
        (extra / "dtb.addon.efi").write_bytes(b"addon")
        return path

    def test_unsealing_restores_the_kernel_or_fails_without_one(self):
        with self.subTest(phase='restores the kernel from the uki and removes it'):
            root = self.root()
            uki = self.uki(root)
            self.assertEqual(unseal.main(["unseal", str(root)]), 0)
            kdir = root / "usr/lib/modules" / KVER
            self.assertEqual((kdir / "vmlinuz").read_bytes(), b"KERNEL-FROM-UKI")
            self.assertEqual((kdir / "initramfs.img").read_bytes(), b"INITRD-FROM-UKI")
            self.assertFalse(uki.exists())
            self.assertFalse(uki.with_name(uki.name + ".extra.d").exists())
        with self.subTest(phase='prefers the split kernel directory'):
            root = self.root()
            self.uki(root)
            split = root / "kernel" / KVER
            split.mkdir(parents=True)
            (split / "vmlinuz").write_bytes(b"SPLIT-KERNEL")
            (split / "initramfs.img").write_bytes(b"SPLIT-INITRD")
            self.assertEqual(unseal.main(["unseal", str(root)]), 0)
            self.assertEqual((root / "usr/lib/modules" / KVER / "vmlinuz").read_bytes(), b"SPLIT-KERNEL")
            self.assertFalse((root / "kernel").exists())
            self.assertEqual(list((root / "boot").rglob("*.efi")), [])
        with self.subTest(phase='a split kernel without its initramfs takes the uki initrd'):
            # The production image since the change: /kernel keeps vmlinuz, the
            # initramfs ships only inside the UKI.
            root = self.root()
            uki = self.uki(root)
            split = root / "kernel" / KVER
            split.mkdir(parents=True)
            (split / "vmlinuz").write_bytes(b"SPLIT-KERNEL")
            self.assertEqual(unseal.main(["unseal", str(root)]), 0)
            kdir = root / "usr/lib/modules" / KVER
            self.assertEqual((kdir / "vmlinuz").read_bytes(), b"SPLIT-KERNEL")
            self.assertEqual((kdir / "initramfs.img").read_bytes(), b"INITRD-FROM-UKI")
            self.assertFalse(uki.exists())
            self.assertFalse((root / "kernel").exists())
        with self.subTest(phase='a split kernel without its initramfs and without a uki fails'):
            root = self.root()
            split = root / "kernel" / KVER
            split.mkdir(parents=True)
            (split / "vmlinuz").write_bytes(b"SPLIT-KERNEL")
            with self.assertRaisesRegex(SystemExit, "no kernel to restore"):
                unseal.main(["unseal", str(root)])
            self.assertFalse((root / "usr/lib/modules" / KVER / "vmlinuz").exists())
        with self.subTest(phase='an unsealed root passes through'):
            root = self.root()
            kdir = root / "usr/lib/modules" / KVER
            (kdir / "vmlinuz").write_bytes(b"K")
            (kdir / "initramfs.img").write_bytes(b"I")
            self.assertEqual(unseal.main(["unseal", str(root)]), 0)
            self.assertEqual((kdir / "vmlinuz").read_bytes(), b"K")
        with self.subTest(phase='no kernel anywhere fails'):
            with self.assertRaisesRegex(SystemExit, "no kernel to restore"):
                unseal.main(["unseal", str(self.root())])
        with self.subTest(phase='a uki without an initrd fails'):
            root = self.root()
            path = root / "boot/EFI/Linux" / f"{KVER}.efi"
            path.parent.mkdir(parents=True)
            path.write_bytes(fake_uki({".linux": b"K"}))
            with self.assertRaisesRegex(SystemExit, "no .initrd section"):
                unseal.main(["unseal", str(root)])


if __name__ == "__main__":
    unittest.main()
