#!/usr/bin/env python3
"""Small native-Windows disk adapter shared by the direct final install tools."""
from __future__ import annotations

import ctypes
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import uuid

ESP_GUID = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"
LINUX_GUID = "0fc63daf-8483-4772-8e79-3d69d8477de4"
TILO_ROOT_GUID = "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab"
PLAN_SCHEMA = "tilo.windows-internal-install-plan.v1"


def load_plan(_path):
    raise ValueError("legacy staged final copy is not available in this tool")


def integer(value, label, minimum=0):
    if type(value) is not int or value < minimum:
        raise ValueError(f"invalid {label}")
    return value


def guid(value):
    try:
        return str(uuid.UUID(str(value).strip().strip("{}")))
    except (ValueError, AttributeError) as error:
        raise ValueError(f"invalid GUID: {value!r}") from error


def ps_json(script):
    result = subprocess.run(["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script],
                            capture_output=True, text=True, timeout=180)
    if result.returncode:
        raise ValueError((result.stderr or result.stdout).strip() or "PowerShell storage command failed")
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ValueError("PowerShell returned invalid JSON") from error


def snapshot(disk_number):
    disk_number = integer(disk_number, "disk number")
    return ps_json(rf'''$ErrorActionPreference='Stop'
$d=Get-Disk -Number {disk_number} -ErrorAction Stop
$p=@(Get-Partition -DiskNumber {disk_number} -ErrorAction Stop | ForEach-Object {{
 [pscustomobject]@{{number=[int]$_.PartitionNumber;partuuid=([string]$_.Guid).Trim('{{','}}').ToLowerInvariant();type_guid=([string]$_.GptType).Trim('{{','}}').ToLowerInvariant();offset_bytes=[long]$_.Offset;size_bytes=[long]$_.Size}}
}})
[pscustomobject]@{{number=[int]$d.Number;unique_id=[string]$d.UniqueId;serial=[string]$d.SerialNumber;size_bytes=[long]$d.Size;logical_sector_size=[int]$d.LogicalSectorSize;partition_style=[string]$d.PartitionStyle;is_boot=[bool]$d.IsBoot;is_system=[bool]$d.IsSystem;is_offline=[bool]$d.IsOffline;is_read_only=[bool]$d.IsReadOnly;partitions=$p}}|ConvertTo-Json -Depth 5 -Compress''')


def normalized_rows(rows):
    if type(rows) is not list:
        raise ValueError("partition rows are not a list")
    return sorted([{"number": integer(row.get("number"), "partition number", 1),
                    "partuuid": guid(row.get("partuuid")), "type_guid": guid(row.get("type_guid")),
                    "offset_bytes": integer(row.get("offset_bytes"), "partition offset", 1),
                    "size_bytes": integer(row.get("size_bytes"), "partition size", 1)} for row in rows],
                  key=lambda row: row["offset_bytes"])


def disk_matches(plan, current):
    expected = plan["windows_disk"]
    return (current.get("number") == expected["number"] and current.get("unique_id") == expected["unique_id"]
            and str(current.get("serial", "")).strip() == expected["serial"]
            and current.get("size_bytes") == expected["size_bytes"]
            and current.get("logical_sector_size") == 512 and str(current.get("partition_style", "")).upper() == "GPT"
            and current.get("is_boot") is True and current.get("is_system") is True
            and current.get("is_offline") is False and current.get("is_read_only") is False)


def windows_install_gate(*, require_no_pagefile=True):
    state = ps_json(r'''$ErrorActionPreference='Stop'
$b=Get-BitLockerVolume -MountPoint C: -ErrorAction Stop
$pagefiles=@(Get-CimInstance Win32_PageFileUsage -ErrorAction Stop | Where-Object {$_.Name -like 'C:*'})
[pscustomobject]@{volume_status=[int]$b.VolumeStatus;protection_status=[int]$b.ProtectionStatus;encryption_percentage=[int]$b.EncryptionPercentage;lock_status=[int]$b.LockStatus;system_pagefile_count=$pagefiles.Count}|ConvertTo-Json -Compress''')
    if (state.get("volume_status") != 1 or state.get("protection_status") != 0
            or state.get("encryption_percentage") != 100 or state.get("lock_status") != 0):
        raise ValueError("BitLocker must remain fully encrypted, unlocked, and explicitly suspended")
    if require_no_pagefile and state.get("system_pagefile_count") != 0:
        raise ValueError("the C: pagefile is still active; reboot after disabling it before shrinking")
    return state


def open_physical_drive(number):
    if platform.system() != "Windows":
        raise ValueError("physical final copy requires native Windows")
    import msvcrt
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    create_file = kernel32.CreateFileW
    create_file.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p,
                            ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p]
    create_file.restype = ctypes.c_void_p
    handle = create_file(f"\\\\.\\PhysicalDrive{number}", 0xC0000000, 0x3, None, 3, 0x80000000, None)
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    descriptor = msvcrt.open_osfhandle(handle, os.O_BINARY | os.O_RDWR)
    return os.fdopen(descriptor, "r+b", buffering=0)


def atomic_json(path, value):
    path = path.resolve(strict=False)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write((json.dumps(value, indent=2, sort_keys=True) + "\n").encode())
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
