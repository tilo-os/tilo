#!/usr/bin/env python3
import copy
import importlib.util
from pathlib import Path
import unittest

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
    parts.append(dict(type="part", start=(3073 if prepared else 3585)*2048,
                      size=77590000*512 if prepared else 37374*2**20,
                      fstype="crypto_LUKS", children=[dict(type="crypt", fstype="btrfs", path="/dev/mapper/root")]))
    table = dict(blockdevices=[dict(type="disk", serial="tilo-install-target", children=parts)])
    return e, cmd, "1\n", mount, table


class RuntimeProof(unittest.TestCase):
    def test_both_real_fixture_layouts(self):
        for mode in ("prepared-alongside", "guided-alongside"):
            with self.subTest(mode=mode):
                probe.check_runtime(*fixture(mode))

    def test_wrong_or_duplicate_cmdline_and_installer_rejected(self):
        for extra in (" composefs="+"b"*128, " tilo.generation=1", " tilo.install", " tilo.install=0"):
            f=list(fixture());f[1]+=extra
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                probe.check_runtime(*f)

    def test_writable_unverified_or_wrong_root_rejected(self):
        for key,value in (("options","rw,verity=require"),("options","ro,verity=on"),("source","composefs:"+"b"*128),("fstype","btrfs")):
            f=fixture();f[3][key]=value
            with self.subTest(key=key,value=value),self.assertRaises(ValueError):
                probe.check_runtime(*f)

    def test_installer_medium_still_attached_rejected(self):
        f=fixture();f[4]["blockdevices"].append(dict(type="disk",serial="tilo-installer"))
        with self.assertRaises(ValueError):probe.check_runtime(*f)

    def test_wrong_foreign_geometry_or_uuid_rejected(self):
        for key,value in (("start",0),("size",1),("partuuid","bad")):
            f=fixture();f[4]["blockdevices"][0]["children"][0][key]=value
            with self.subTest(key=key),self.assertRaises(ValueError):probe.check_runtime(*f)

    def test_unencrypted_target_rejected(self):
        f=fixture();f[4]["blockdevices"][0]["children"][-1]["fstype"]="btrfs"
        with self.assertRaises(ValueError):probe.check_runtime(*f)

    def test_fixture_64g_assumption_rejected(self):
        f=fixture();f[4]["blockdevices"][0]["children"][-1]["size"]=64*2**30
        with self.assertRaises(ValueError):probe.check_runtime(*f)

    def test_malformed_expected_seal_rejected(self):
        f=fixture();f[0]["composefs_digest"]="../outside"
        with self.assertRaises(ValueError):probe.check_runtime(*f)

if __name__ == "__main__":unittest.main()
