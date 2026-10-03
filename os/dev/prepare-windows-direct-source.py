#!/usr/bin/env python3
"""Prepare a sparse VM disk matching a reviewed Windows direct-final plan.

The VM boots a separate CI installer medium. Its final disk has synthetic,
empty Windows extents and the exact Windows-assigned Tilo partition GUIDs.
Only the three Tilo payload extents are ever copied back to the physical disk.
`receipt` verifies a digest-pinned CI image signature and attestation, the
host-checked VM proof, and the source ESP target UKI before hashing the final
extents. The guest proof is produced by a separate VM install procedure.
"""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import ctypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import struct
import subprocess
import sys
import uuid
import zlib

_copy_path = Path(__file__).with_name("copy-windows-direct-final-install.py")
_spec = importlib.util.spec_from_file_location("tilo_windows_direct_copy", _copy_path)
direct = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(direct)
_provenance_path = Path(__file__).with_name("windows-direct-provenance.py")
_provenance_spec = importlib.util.spec_from_file_location("tilo_windows_direct_provenance", _provenance_path)
provenance = importlib.util.module_from_spec(_provenance_spec)
_provenance_spec.loader.exec_module(provenance)

SECTOR = 512
GPT_ENTRIES = 128
GPT_ENTRY_BYTES = 128
TABLE_BYTES = GPT_ENTRIES * GPT_ENTRY_BYTES
DIGEST_REF = re.compile(r"^.+@sha256:[0-9a-f]{64}$")
MANIFEST_SCHEMA = "tilo.windows-direct-vm-template.v1"
SAMPLE_BYTES = 64 * 1024
MAX_TEMPLATE_ALLOCATION = 8 * 1024 * 1024


def require(ok, message):
    if not ok:
        raise ValueError(message)


def plan_rows(plan):
    return direct.normalized(plan["preserve_partitions"] + plan["target_partitions"])


def gpt_header(current, backup, table_lba, sectors, disk_guid, table_crc):
    raw = bytearray(SECTOR)
    struct.pack_into("<8sIIIIQQQQ16sQIII", raw, 0, b"EFI PART", 0x10000,
                     92, 0, 0, current, backup, 34, sectors - 34,
                     disk_guid.bytes_le, table_lba, GPT_ENTRIES, GPT_ENTRY_BYTES, table_crc)
    struct.pack_into("<I", raw, 16, zlib.crc32(raw[:92]))
    return raw


def write_template(stream, plan, *, rows=None, disk_guid=None):
    """Write only MBR/GPT metadata; leave partition contents untouched."""
    disk_size = plan["windows_disk"]["size_bytes"]
    require(disk_size % SECTOR == 0 and disk_size >= 68 * SECTOR,
            "invalid VM source disk size")
    sectors = disk_size // SECTOR
    rows = plan_rows(plan) if rows is None else direct.normalized(rows)
    require(len(rows) in (4, 7) and len({r["number"] for r in rows}) == len(rows),
            "source template needs four or seven distinct partition slots")
    entries = bytearray(TABLE_BYTES)
    for row in rows:
        number = row["number"]
        start = row["offset_bytes"] // SECTOR
        end = (row["offset_bytes"] + row["size_bytes"]) // SECTOR - 1
        require(1 <= number <= GPT_ENTRIES and row["offset_bytes"] % SECTOR == 0
                and row["size_bytes"] % SECTOR == 0 and 34 <= start <= end <= sectors - 34,
                "source partition is outside GPT usable LBAs")
        struct.pack_into("<16s16sQQQ", entries, (number - 1) * GPT_ENTRY_BYTES,
                         uuid.UUID(row["type_guid"]).bytes_le,
                         uuid.UUID(row["partuuid"]).bytes_le, start, end, 0)
    table_crc = zlib.crc32(entries)
    identity = disk_guid or uuid.uuid4()
    mbr = bytearray(SECTOR)
    mbr[446 + 4] = 0xEE
    struct.pack_into("<I", mbr, 446 + 8, 1)
    struct.pack_into("<I", mbr, 446 + 12, min(sectors - 1, 0xFFFFFFFF))
    mbr[510:] = b"\x55\xaa"
    stream.seek(0)
    stream.write(mbr)
    stream.write(gpt_header(1, sectors - 1, 2, sectors, identity, table_crc))
    stream.write(entries)
    stream.seek((sectors - 33) * SECTOR)
    stream.write(entries)
    stream.write(gpt_header(sectors - 1, 1, sectors - 33, sectors, identity, table_crc))
    stream.flush()
    os.fsync(stream.fileno())
    return rows


def windows_file_info(stream):
    """Read the native NTFS allocation and attributes from the same open handle."""
    import msvcrt

    class FileStandardInfo(ctypes.Structure):
        _fields_ = [("allocation_size", ctypes.c_int64),
                    ("end_of_file", ctypes.c_int64),
                    ("number_of_links", ctypes.c_uint32),
                    ("delete_pending", ctypes.c_ubyte),
                    ("directory", ctypes.c_ubyte)]

    class FileAttributeTagInfo(ctypes.Structure):
        _fields_ = [("file_attributes", ctypes.c_uint32),
                    ("reparse_tag", ctypes.c_uint32)]

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.GetFileInformationByHandleEx.argtypes = [ctypes.c_void_p, ctypes.c_int,
                                                     ctypes.c_void_p, ctypes.c_uint32]
    kernel.GetFileInformationByHandleEx.restype = ctypes.c_int
    handle = ctypes.c_void_p(msvcrt.get_osfhandle(stream.fileno()))
    standard = FileStandardInfo()
    attributes = FileAttributeTagInfo()
    for information_class, value in ((1, standard), (9, attributes)):
        if not kernel.GetFileInformationByHandleEx(handle, information_class,
                                                   ctypes.byref(value), ctypes.sizeof(value)):
            raise ctypes.WinError(ctypes.get_last_error())
    return standard.end_of_file, standard.allocation_size, attributes.file_attributes


def verify_sparse_windows(stream, expected_size):
    stream.flush()
    size, allocated, attributes = windows_file_info(stream)
    require(attributes & 0x200, "VM source lost its Windows sparse attribute")
    require(size == expected_size, "VM source has an unexpected logical size")
    require(0 <= allocated <= MAX_TEMPLATE_ALLOCATION,
            f"VM source unexpectedly allocated {allocated} bytes")


def make_sparse_windows(stream, size):
    import msvcrt
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    handle = ctypes.c_void_p(msvcrt.get_osfhandle(stream.fileno()))
    kernel.DeviceIoControl.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p,
                                       ctypes.c_uint32, ctypes.c_void_p, ctypes.c_uint32,
                                       ctypes.POINTER(ctypes.c_uint32), ctypes.c_void_p]
    kernel.DeviceIoControl.restype = ctypes.c_int
    kernel.SetFilePointerEx.argtypes = [ctypes.c_void_p, ctypes.c_int64,
                                        ctypes.c_void_p, ctypes.c_uint32]
    kernel.SetFilePointerEx.restype = ctypes.c_int
    kernel.SetEndOfFile.argtypes = [ctypes.c_void_p]
    kernel.SetEndOfFile.restype = ctypes.c_int
    returned = ctypes.c_uint32()
    if not kernel.DeviceIoControl(handle, 0x900C4, None, 0, None, 0,
                                  ctypes.byref(returned), None):
        raise ctypes.WinError(ctypes.get_last_error())
    verify_sparse_windows(stream, 0)
    # MSYS2's Python truncate() can zero-fill this terabyte-sized hole. Use
    # Windows' file pointer and EOF calls directly after FSCTL_SET_SPARSE.
    if not kernel.SetFilePointerEx(handle, size, None, 0):
        raise ctypes.WinError(ctypes.get_last_error())
    if not kernel.SetEndOfFile(handle):
        raise ctypes.WinError(ctypes.get_last_error())
    verify_sparse_windows(stream, size)
    stream.seek(0)


def create_template(args):
    plan = direct.load_plan(args.plan)
    output = args.output.resolve(strict=False)
    manifest_path = Path(str(output) + ".manifest.json")
    require(not output.exists() and output.parent.is_dir(),
            "VM source output must be new in an existing directory")
    require(not manifest_path.exists(), "VM source manifest already exists")
    created = False
    try:
        with output.open("xb") as stream:
            created = True
            disk_size = plan["windows_disk"]["size_bytes"]
            if platform.system() == "Windows":
                make_sparse_windows(stream, disk_size)
            else:
                stream.truncate(disk_size)
            rows = write_template(stream, plan, rows=plan["preserve_partitions"])
            if platform.system() == "Windows":
                verify_sparse_windows(stream, disk_size)
        with output.open("rb", buffering=0) as source:
            disk_guid, actual = direct.read_gpt(source, plan["windows_disk"]["size_bytes"])
            samples = [preserved_sample(source, row)
                       for row in direct.normalized(plan["preserve_partitions"])]
        require(direct.same_gpt_content(actual, rows), "VM source GPT readback differs")
        manifest = {"schema": MANIFEST_SCHEMA, "source_disk_guid": disk_guid,
                    "disk_size_bytes": plan["windows_disk"]["size_bytes"],
                    "preserved_partitions": rows,
                    "target_partitions": [{key: row[key] for key in
                                           ("role", "type_guid", "offset_bytes", "size_bytes")}
                                          for row in plan["target_partitions"]],
                    "preserved_samples": samples}
        direct.stage.atomic_json(manifest_path, manifest)
    except Exception:
        if created:
            output.unlink(missing_ok=True)
        raise
    print(f"sparse VM source template: {output}")
    print(f"compact preservation manifest: {manifest_path}")


def preserved_sample(stream, row):
    length = min(SAMPLE_BYTES, row["size_bytes"])
    first = direct.hash_range(stream, row["offset_bytes"], length)
    last = direct.hash_range(stream, row["offset_bytes"] + row["size_bytes"] - length,
                             length)
    return {"partuuid": row["partuuid"], "sample_bytes": length,
            "first_sha256": first, "last_sha256": last}


def verify_template(args):
    manifest = direct.load_json(args.manifest)
    require(manifest.get("schema") == MANIFEST_SCHEMA,
            "unsupported VM source template manifest")
    raw = args.source_raw
    require(raw.is_file() and not raw.is_symlink()
            and raw.stat().st_size == manifest.get("disk_size_bytes"),
            "VM source size differs from template manifest")
    preserved = direct.normalized(manifest.get("preserved_partitions"))
    require(len(preserved) == 4 and len({row["partuuid"] for row in preserved}) == 4,
            "template preserved Windows entries are invalid")
    planned = manifest.get("target_partitions")
    require(type(planned) is list and [row.get("role") for row in planned] == list(direct.ROLES),
            "template final partition roles differ")
    with raw.open("rb", buffering=0) as source:
        disk_guid, current = direct.read_gpt(source, raw.stat().st_size)
        require(disk_guid == manifest.get("source_disk_guid"),
                "VM source GPT disk identity changed")
        by_id = {row["partuuid"]: row for row in current}
        require(len(by_id) == len(current) and all(
            by_id.get(row["partuuid"]) == row for row in preserved),
            "synthetic Windows GPT entries changed in VM")
        additions = [row for row in current if row["partuuid"] not in
                     {item["partuuid"] for item in preserved}]
        require(len(additions) in (0, 3), "VM source has partial or unexpected final partitions")
        if additions:
            for actual, wanted in zip(additions, planned, strict=True):
                require(all(actual[key] == wanted[key] for key in
                            ("type_guid", "offset_bytes", "size_bytes")),
                        "VM final partition geometry differs from template")
        samples = manifest.get("preserved_samples")
        require(type(samples) is list and len(samples) == 4,
                "template preserved content samples are incomplete")
        actual_samples = [preserved_sample(source, row) for row in preserved]
        require(actual_samples == samples,
                "synthetic Windows partition edge contents changed in VM")
    final_partuuids = (dict(zip(("efi", "boot", "root"),
                                (row["partuuid"] for row in additions), strict=True))
                       if additions else {})
    print(json.dumps({"schema": "tilo.windows-direct-vm-template-check.v1",
                      "source_raw": str(raw), "disk_guid": disk_guid,
                      "preserved_gpt_verified": True,
                      "preserved_edge_samples_verified": True,
                      "final_partition_count": len(additions),
                      "final_partuuids": final_partuuids}, indent=2))


def finalize_source(args):
    """After VM guided install, replace only its GPT IDs with reviewed IDs."""
    plan = direct.load_plan(args.plan)
    raw = args.source_raw
    require(raw.is_file() and not raw.is_symlink()
            and raw.stat().st_size == plan["windows_disk"]["size_bytes"],
            "VM-installed source raw size differs from reviewed disk")
    expected = plan_rows(plan)
    preserved = direct.normalized(plan["preserve_partitions"])
    with raw.open("r+b", buffering=0) as stream:
        disk_guid, current = direct.read_gpt(stream, raw.stat().st_size)
        by_id = {row["partuuid"]: row for row in current}
        require(len(current) == 7 and len(by_id) == 7
                and all(by_id.get(row["partuuid"]) == row for row in preserved),
                "VM guided install changed synthetic Windows GPT entries")
        additions = [row for row in current if row["partuuid"] not in
                     {preserved_row["partuuid"] for preserved_row in preserved}]
        wanted_final = direct.normalized(plan["target_partitions"])
        for actual, wanted in zip(additions, wanted_final, strict=True):
            require(all(actual[key] == wanted[key]
                        for key in ("type_guid", "offset_bytes", "size_bytes")),
                    "VM guided install created a different final partition layout")
        # Keep the source disk GUID and every partition content byte. Only GPT
        # entry numbers and PARTUUIDs are rewritten to match Windows' plan.
        write_template(stream, plan, disk_guid=uuid.UUID(disk_guid))
        _, after = direct.read_gpt(stream, raw.stat().st_size)
        require(direct.same_gpt_content(after, expected),
                "VM source GPT final identity readback differs")
    print(f"VM source final GPT identities match Windows plan: {raw}")


@contextmanager
def exclusive_source(path):
    """Windows sharing rules refuse any open QEMU or other file handle."""
    require(platform.system() == 'Windows', 'reusing an allocated source requires native Windows')
    import msvcrt
    create = ctypes.windll.kernel32.CreateFileW
    create.argtypes = (ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p,
                       ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p)
    create.restype = ctypes.c_void_p
    handle = create(str(path), 0xC0000000, 0, None, 3, 0x80, None)
    if handle == ctypes.c_void_p(-1).value:
        raise OSError('VM source cannot be locked exclusively; stop its VM before resetting')
    try:
        fd = msvcrt.open_osfhandle(handle, os.O_RDWR | os.O_BINARY)
    except Exception:
        ctypes.windll.kernel32.CloseHandle(ctypes.c_void_p(handle))
        raise
    with os.fdopen(fd, 'r+b', buffering=0) as stream:
        yield stream


def reset_template(args):
    """Reuse an owned VM disk's allocation; invalidate its old Tilo headers and GPT."""
    plan = direct.load_plan(args.plan)
    manifest = direct.load_json(args.manifest)
    require(args.confirm_source_disk_guid == manifest.get('source_disk_guid'),
            'reset confirmation differs from the owned VM disk GUID')
    require(args.source_raw.is_file() and not args.source_raw.is_symlink()
            and args.source_raw.stat().st_size == plan['windows_disk']['size_bytes'],
            'reset target must be the regular VM raw file matching the reviewed plan')
    preserved = direct.normalized(manifest.get('preserved_partitions'))
    wanted = direct.normalized(plan['preserve_partitions'])
    require({row['partuuid']: {k: v for k, v in row.items() if k != 'number'} for row in preserved}
            == {row['partuuid']: {k: v for k, v in row.items() if k != 'number'} for row in wanted},
            'reset manifest differs from the reviewed preserved Windows identities')
    require(manifest.get('target_partitions') == [
        {k: row[k] for k in ('offset_bytes', 'role', 'size_bytes', 'type_guid')}
        for row in plan['target_partitions']], 'reset target geometry differs from the reviewed plan')
    # Verify before acquiring an exclusive writable handle. A second check on
    # that handle binds the exact GPT that is about to be changed.
    verify_template(args)
    with exclusive_source(args.source_raw) as stream:
        disk_guid, current = direct.read_gpt(stream, args.source_raw.stat().st_size)
        require(disk_guid == manifest['source_disk_guid'], 'source GUID changed before reset')
        require(all(row in current for row in preserved), 'preserved GPT changed before reset')
        final = [row for row in current if row not in preserved]
        require(not final or sorted(
            (row['offset_bytes'], row['size_bytes'], row['type_guid']) for row in final)
            == sorted((row['offset_bytes'], row['size_bytes'], row['type_guid'])
                      for row in plan['target_partitions']),
            'installed GPT geometry changed before reset')
        require([preserved_sample(stream, row) for row in preserved] == manifest['preserved_samples'],
                'preserved source content changed before reset')
        # The guided installer correctly refuses stale filesystem signatures.
        # Invalidate only the reviewed disposable Tilo extents' edges; retain
        # the bulk allocation and never touch synthetic Windows content.
        for row in plan['target_partitions']:
            length = min(1024 * 1024, row['size_bytes'] // 2)
            for offset in (row['offset_bytes'], row['offset_bytes'] + row['size_bytes'] - length):
                stream.seek(offset)
                stream.write(bytes(length))
                stream.seek(offset)
                require(stream.read(length) == bytes(length), 'Tilo header reset readback differs')
        write_template(stream, plan, rows=preserved, disk_guid=uuid.UUID(disk_guid))
        _, after = direct.read_gpt(stream, args.source_raw.stat().st_size)
        require(direct.same_gpt_content(after, preserved), 'reset GPT readback differs')
    verify_template(args)
    print('Owned VM source reset; Tilo headers invalidated, allocation reused, old proof obsolete')


def verified_ci_image(image, key, children, kind, command_json=None, key_arg=None):
    return provenance.verify_ci_image(image, key, children, kind, command_json, key_arg)


def make_receipt(args):
    plan = direct.load_plan(args.plan)
    rows = plan_rows(plan)
    final = direct.expected_rows(plan, None, rows)[1]
    raw = args.source_raw
    require(raw.is_file() and not raw.is_symlink()
            and raw.stat().st_size == plan["windows_disk"]["size_bytes"],
            "VM-installed source raw size differs from reviewed disk")
    if getattr(args, "development", False):
        ci_provenance, checked, dual_gate = direct.development.verify(args)
        index_digest = child_digest = args.ci_image.split("@", 1)[1]
    else:
        require(args.verify_key is not None and args.ci_index_manifest is not None,
                "signed source receipt requires CI verification key and index manifest")
        index_digest, child_digest, children, kind = provenance.signed_ci_child(
            args.ci_image, args.ci_index_manifest, args.ci_child_manifest)
        ci_provenance = verified_ci_image(args.ci_image, args.verify_key, children, kind,
                                          getattr(args, "cosign_command_json", None),
                                          getattr(args, "cosign_key_arg", None))
        checked = provenance.checked_guest_proof(
            args.vm_proof_checked, child_digest, args.ci_image, ci_provenance)
        if kind == "lenovo-83ed":
            require(checked["schema"] == provenance.PROOF_SCHEMA_V2,
                    "Lenovo physical source requires backend-derived signed target proof v2")
            dual_gate = provenance.dual_ci_medium_gate(
                args, checked, child_digest, ci_provenance["source_revision"])
        else:
            dual_gate = {}
    with raw.open("rb", buffering=0) as stream:
        disk_guid, actual = direct.read_gpt(stream, raw.stat().st_size)
        require(direct.same_gpt_content(actual, rows),
                "VM-installed source GPT differs from reviewed final map")
        require(disk_guid == checked["guest_proof"]["disk_guid"],
                "VM-installed source GPT disk identity differs from guest proof")
        uki_hash = provenance.installed_uki_sha256(
            stream, final[0], checked["guest_proof"]["composefs_digest"])
        require(uki_hash == checked["guest_proof"]["target_uki_sha256"],
                "VM-installed source ESP UKI differs from guest proof")
        loader_path, loader_hash = provenance.installed_loader(stream, final[0])
        hashes = {role: direct.hash_range(stream, row["offset_bytes"], row["size_bytes"])
                  for role, row in zip(direct.ROLES, final, strict=True)}
    receipt = {"schema": direct.SCHEMA, "raw_size_bytes": raw.stat().st_size,
               "partitions": rows, "final_sha256": hashes,
               "ci_image": args.ci_image, "ci_index_digest": index_digest,
               "ci_child_digest": child_digest,
               "ci_verification_key_sha256": ci_provenance["key_sha256"],
               "ci_source_revision": ci_provenance["source_revision"],
               "ci_invocation_id": ci_provenance["invocation_id"],
               "ci_generation": ci_provenance["generation"],
               "ci_image_kind": ci_provenance["image_kind"],
               "ci_firmware_candidate": ci_provenance["firmware_candidate"],
               "ci_firmware_manifest_sha256": ci_provenance["firmware_manifest_sha256"],
               "ci_slsa_statement_sha256": ci_provenance["statement_sha256"],
               "final_esp_partuuid": final[0]["partuuid"],
               "loader_path": loader_path, "loader_sha256": loader_hash,
               "vm_proof_checked_sha256": hashlib.sha256(
                   args.vm_proof_checked.read_bytes()).hexdigest(),
               "installed_uki_sha256": uki_hash}
    receipt.update(dual_gate)
    direct.stage.atomic_json(args.output, receipt)
    authority = "live development CI" if getattr(args, "development", False) else "CI signature"
    print(f"VM source readback and {authority} receipt: {args.output}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    template = commands.add_parser("template", help="create sparse VM disk with reviewed partition identities")
    template.add_argument("--plan", type=Path, required=True)
    template.add_argument("--output", type=Path, required=True)
    template.set_defaults(handler=create_template)
    verify = commands.add_parser("verify-template", help="check preserved GPT identities and compact Windows samples")
    verify.add_argument("--manifest", type=Path, required=True)
    verify.add_argument("--source-raw", type=Path, required=True)
    verify.set_defaults(handler=verify_template)
    reset = commands.add_parser('reset-template',
        help='discard an owned VM install and reuse its existing allocated blocks; never a physical drive')
    reset.add_argument('--plan', type=Path, required=True)
    reset.add_argument('--manifest', type=Path, required=True)
    reset.add_argument('--source-raw', type=Path, required=True)
    reset.add_argument('--confirm-source-disk-guid', required=True)
    reset.set_defaults(handler=reset_template)
    finalize = commands.add_parser("finalize", help="bind VM-installed GPT identities to Windows' reviewed final entries")
    finalize.add_argument("--plan", type=Path, required=True)
    finalize.add_argument("--source-raw", type=Path, required=True)
    finalize.set_defaults(handler=finalize_source)
    receipt = commands.add_parser("receipt", help="verify CI signature and record VM source extent hashes")
    receipt.add_argument("--plan", type=Path, required=True)
    receipt.add_argument("--source-raw", type=Path, required=True)
    receipt.add_argument("--ci-image", required=True)
    receipt.add_argument("--verify-key", type=Path)
    receipt.add_argument("--cosign-command-json", type=Path)
    receipt.add_argument("--cosign-key-arg")
    receipt.add_argument("--ci-index-manifest", type=Path)
    receipt.add_argument("--ci-child-manifest", type=Path, required=True)
    receipt.add_argument("--vm-proof-checked", type=Path, required=True)
    direct.development.add_arguments(receipt)
    for name in ("generic-ci-image", "generic-index-manifest",
                 "generic-child-manifest", "generic-vm-proof-checked",
                 "generic-medium-identity", "generic-ci-guest-proof",
                 "board-medium-identity", "board-ci-guest-proof"):
        receipt.add_argument("--" + name, type=str if name == "generic-ci-image" else Path)
    receipt.add_argument("--output", type=Path, required=True)
    receipt.set_defaults(handler=make_receipt)
    args = parser.parse_args(argv)
    try:
        args.handler(args)
        return 0
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError,
            subprocess.TimeoutExpired) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
