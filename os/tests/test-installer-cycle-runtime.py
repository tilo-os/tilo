#!/usr/bin/env python3
import copy
import importlib.util
from pathlib import Path
import unittest
import tempfile

path = Path(__file__).resolve().parents[1] / "dev/verify-installer-cycle-runtime.py"
spec = importlib.util.spec_from_file_location("probe", path)
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


def fixture(mode="prepared-alongside"):
    e = dict(composefs_digest="a" * 128, target_uki_sha256="b" * 64,
             source_manifest_digest="sha256:" + "c" * 64,
             source_config_digest="sha256:" + "d" * 64,
             generation=1, mode=mode, installed_target="registry/test:development")
    cmd = "composefs=" + e["composefs_digest"] + " tilo.generation=1"
    mount = dict(target="/", fstype="overlay", source="composefs:" + e["composefs_digest"], options="ro,metacopy=on,verity=require")
    prepared = mode == "prepared-alongside"
    geometry = [(1, 2048 if prepared else 512), (2049 if prepared else 513, 512), (2561 if prepared else 1025, 512)]
    parts = [dict(type="part", path="/dev/vda"+str(i+1), start=start*2048, size=size*2**20,
                  partuuid=str(i+1)+"0000000-0000-4000-8000-00000000000"+str(i+1)) for i,(start,size) in enumerate(geometry)]
    if not prepared:
        parts.append(dict(type="part", path="/dev/vda4", start=1537*2048, size=2**31))
    parts.append(dict(type="part", path="/dev/vda4" if prepared else "/dev/vda5",
                      start=(3073 if prepared else 3585)*2048, size=2**30,
                      fstype="ext4", label="TILO_BOOT"))
    parts.append(dict(type="part", start=(4097 if prepared else 4609)*2048,
                      size=75492848*512 if prepared else 36350*2**20,
                      fstype="crypto_LUKS", children=[dict(type="crypt", fstype="btrfs", path="/dev/mapper/root")]))
    table = dict(blockdevices=[dict(type="disk", serial="tilo-install-target", children=parts)])
    return e, cmd, "1\n", mount, table


class RuntimeProof(unittest.TestCase):
    def test_accepts_real_boot_reports(self):
        with self.subTest(phase="selected_entry_binds_actual_bootloader_report"):
            with tempfile.TemporaryDirectory() as d:
                esp = Path(d); entries = esp / "loader/entries"; entries.mkdir(parents=True)
                entry = entries / "tilo.conf"
                entry.write_text("title Tilo\nefi /EFI/Linux/bootc/bootc_composefs-" + "a"*128 + ".efi\n")
                raw = bytes(4) + "tilo.conf\0".encode("utf-16-le")
                self.assertEqual(probe.selected_entry(esp, raw, "a"*128)["entry"], "tilo.conf")
                with self.assertRaises(ValueError): probe.selected_entry(esp, raw, "b"*128)
                entry.write_text(entry.read_text() + "options bypass=1\n")
                with self.assertRaises(ValueError): probe.selected_entry(esp, raw, "a"*128)
                with self.assertRaises(ValueError): probe.selected_entry(esp, bytes(4)+"../tilo.conf\0".encode("utf-16-le"), "a"*128)
        with self.subTest(phase="bootc_status_matches_retained_1164_schema"):
            e=fixture()[0]
            doc={"status":{"type":None,"booted":{"incompatible":False,"ostree":None,
                "composefs":{"verity":e["composefs_digest"],"missingVerityAllowed":False,"bootType":"Uki","bootloader":"systemd"},
                "image":{"imageDigest":e["source_manifest_digest"],"architecture":"arm64",
                         "image":{"image":e["installed_target"],"transport":"registry"}}}}}
            probe.check_bootc_status(doc,e)
            for field,value in (("verity","b"*128),("missingVerityAllowed",True),("bootType","uki"),("bootloader","grub")):
                bad=copy.deepcopy(doc);bad["status"]["booted"]["composefs"][field]=value
                with self.subTest(field=field),self.assertRaises(ValueError):probe.check_bootc_status(bad,e)
            bad=copy.deepcopy(doc);bad["status"]["booted"]["image"]["imageDigest"]="sha256:"+"f"*64
            with self.assertRaises(ValueError):probe.check_bootc_status(bad,e)
        with self.subTest(phase="both_real_fixture_layouts"):
            for mode in ("prepared-alongside", "guided-alongside"):
                with self.subTest(mode=mode):
                    probe.check_runtime(*fixture(mode))

    def test_rejects_wrong_boot_state(self):
        with self.subTest(phase="wrong_or_duplicate_cmdline_and_installer_rejected"):
            for extra in (" composefs="+"b"*128, " tilo.generation=1", " tilo.install", " tilo.install=0"):
                f=list(fixture());f[1]+=extra
                with self.subTest(extra=extra), self.assertRaises(ValueError):
                    probe.check_runtime(*f)
        with self.subTest(phase="writable_unverified_or_wrong_root_rejected"):
            for key,value in (("options","rw,verity=require"),("options","ro,verity=on"),("source","composefs:"+"b"*128),("fstype","btrfs")):
                f=fixture();f[3][key]=value
                with self.subTest(key=key,value=value),self.assertRaises(ValueError):
                    probe.check_runtime(*f)
        with self.subTest(phase="installer_medium_still_attached_rejected"):
            f=fixture();f[4]["blockdevices"].append(dict(type="disk",serial="tilo-installer"))
            with self.assertRaises(ValueError):probe.check_runtime(*f)
        with self.subTest(phase="unencrypted_target_rejected"):
            f=fixture();f[4]["blockdevices"][0]["children"][-1]["fstype"]="btrfs"
            with self.assertRaises(ValueError):probe.check_runtime(*f)

    def test_rejects_wrong_geometry_or_expectations(self):
        with self.subTest(phase="wrong_foreign_geometry_or_uuid_rejected"):
            for key,value in (("start",0),("size",1),("partuuid","bad")):
                f=fixture();f[4]["blockdevices"][0]["children"][0][key]=value
                with self.subTest(key=key),self.assertRaises(ValueError):probe.check_runtime(*f)
        with self.subTest(phase="fixture_64g_assumption_rejected"):
            f=fixture();f[4]["blockdevices"][0]["children"][-1]["size"]=64*2**30
            with self.assertRaises(ValueError):probe.check_runtime(*f)
        with self.subTest(phase="malformed_expected_seal_rejected"):
            f=fixture();f[0]["composefs_digest"]="../outside"
            with self.assertRaises(ValueError):probe.check_runtime(*f)

if __name__ == "__main__":unittest.main()
