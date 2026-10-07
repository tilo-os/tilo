"""Read-only public SDK identity from kernel mapped-file objects, never namespace paths."""
import hashlib
import json
import os
from pathlib import Path
import stat
import sys


def start_time(process):
    return process.joinpath("stat").read_text().rsplit(")", 1)[1].split()[19]


def mappings(process):
    result = {}
    for line in process.joinpath("maps").read_text().splitlines():
        fields = line.split(maxsplit=5)
        if len(fields) == 6 and fields[5].startswith("/run/tilo-runtime/"):
            if fields[5].endswith(" (deleted)") or fields[4] == "0":
                raise RuntimeError("resident SDK mapping is deleted or lacks file identity")
            result[fields[0]] = (fields[2], fields[3], fields[4], fields[5])
    return result


def fingerprint(status):
    return status.st_dev, status.st_ino, status.st_size, status.st_mtime_ns, status.st_ctime_ns


def hash_backing(path, remaining=512 * 1024 * 1024):
    # Follow the kernel magic link itself. readlink's string and /proc/PID/root
    # cannot identify a resident object after namespace mounts have changed.
    with path.open("rb") as backing:
        before = os.fstat(backing.fileno())
        if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= min(remaining, 512 * 1024 * 1024):
            raise RuntimeError("mapped SDK backing exceeds the public file bound")
        digest = hashlib.sha256()
        count = 0
        while chunk := backing.read(1024 * 1024):
            count += len(chunk)
            if count > before.st_size:
                raise RuntimeError("mapped backing changed while hashing")
            digest.update(chunk)
        after = os.fstat(backing.fileno())
        if count != before.st_size or fingerprint(before) != fingerprint(after):
            raise RuntimeError("mapped backing changed while hashing")
    return {"sha256": digest.hexdigest(), "backing_device": [os.major(before.st_dev), os.minor(before.st_dev)],
            "backing_inode": before.st_ino, "bytes": count}


def probe(uid, name, proc=Path("/proc"), selection=None):
    if name not in ("store", "browser", "terminal", "notes", "settings", "calculator") or not isinstance(uid, int) or uid <= 0:
        raise RuntimeError("unsupported public SDK subject")
    processes = []
    unrelated = []
    if selection is not None:
        pid, start = selection
        if type(pid) is not int or pid <= 0 or not isinstance(start, str) or not start.isdecimal():
            raise RuntimeError("invalid authenticated worker selection")
    total_bytes = 0
    for process in proc.iterdir():
        if not process.name.isdigit():
            continue
        try:
            matched = process.stat().st_uid == uid and process.joinpath("comm").read_text().strip() == name
        except FileNotFoundError:
            continue  # This vanished process was never selected as the subject.
        if not matched:
            continue
        before_start = start_time(process)
        if selection is not None and int(process.name) != selection[0]:
            unrelated.append({"pid": int(process.name), "start_time_ticks": before_start,
                              "status": "unrelated_not_verified_against_selected_offer"})
            if len(unrelated) > 16:
                raise RuntimeError("unrelated worker observation exceeds bound")
            continue
        if selection is not None and before_start != selection[1]:
            raise RuntimeError("selected worker PID lifetime changed")
        before_maps = mappings(process)
        if not before_maps or len(before_maps) > 1024:
            raise RuntimeError("resident SDK mappings unobservable or exceed bound")
        executable = None
        executable_identity = None
        if name == "calculator":
            executable_identity = fingerprint((process / "exe").stat())
            executable = hash_backing(process / "exe", 2 * 1024 * 1024 * 1024 - total_bytes)
            total_bytes += executable["bytes"]
        files = {}
        for address, identity in before_maps.items():
            path = identity[3]
            if path in files:
                if files[path]["map_identity"][1:3] != list(identity[1:3]):
                    raise RuntimeError("same SDK path maps different resident files")
                continue
            if len(files) >= 256:
                raise RuntimeError("resident SDK file count exceeds bound")
            backing = hash_backing(process / "map_files" / address, 2 * 1024 * 1024 * 1024 - total_bytes)
            total_bytes += backing["bytes"]
            files[path] = {"path": path, "map_range": address, "map_identity": list(identity), **backing}
        if (executable_identity is not None
                and executable_identity != fingerprint((process / "exe").stat())):
            raise RuntimeError("app executable changed during backing observation")
        if before_start != start_time(process) or before_maps != mappings(process):
            raise RuntimeError("SDK process/mappings changed during backing observation")
        processes.append({"pid": int(process.name), "start_time_ticks": before_start, "runtime": list(files.values()),
                          **({"executable": executable} if executable is not None else {})})
        if len(processes) > 16:
            raise RuntimeError("resident SDK subject count exceeds bound")
    if not processes:
        raise RuntimeError("public SDK subject unobservable")
    return {"uid": uid, "reader_uid": os.getuid(), "method": "proc-map-files-fd", "processes": processes,
            "unrelated_processes": unrelated}


if __name__ == "__main__":
    if len(sys.argv) not in (3, 5):
        raise RuntimeError("expected exact public process name and subject UID")
    selection = (int(sys.argv[3]), sys.argv[4]) if len(sys.argv) == 5 else None
    print(json.dumps(probe(int(sys.argv[2]), sys.argv[1], selection=selection)))
