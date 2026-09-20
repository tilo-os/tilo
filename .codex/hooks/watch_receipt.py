"""Process-lifetime exclusion and bounded recovery of an interrupted CI watcher."""

import datetime as dt
import json
import os
from pathlib import Path


class WatchReceipt:
    def __init__(self, path: Path, owner, data: dict):
        self.path, self.owner, self.data = path, owner, data

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.owner.close()  # The kernel also releases ownership on process death.

    def save(self, **fields):
        self.data.update(fields)
        temporary = self.path.with_suffix(f".{os.getpid()}.tmp")
        temporary.write_text(json.dumps(self.data), encoding="utf-8")
        os.replace(temporary, self.path)

    @property
    def started(self):
        return dt.datetime.fromisoformat(self.data["started"])


def acquire(path: Path, *, cwd: Path, session_id: str, sha: str) -> WatchReceipt | None:
    path.parent.mkdir(parents=True, exist_ok=True)
    owner = path.with_suffix(".lock").open("a+b")
    try:
        owner.seek(0, os.SEEK_END)
        if owner.tell() == 0:
            owner.write(b"\0")
            owner.flush()
        owner.seek(0)
        try:
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(owner.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(owner.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            owner.close()
            return None
        now = dt.datetime.now(dt.timezone.utc).isoformat()
        data = {"session_id": session_id, "sha": sha, "cwd": str(cwd),
                "state": "watching", "started": now, "attempt": 0}
        if path.exists():
            try:
                previous = json.loads(path.read_text(encoding="utf-8"))
                safe = (previous["state"] == "watching"
                        and previous["session_id"] == session_id
                        and previous["sha"] == sha
                        and type(previous["attempt"]) is int
                        and 0 < previous["attempt"] < 3
                        and dt.datetime.fromisoformat(previous["started"]).tzinfo is not None)
            except (ValueError, KeyError, TypeError):
                safe = False
            if not safe:
                owner.close()
                return None
            # Holding the kernel lock proves the former owner cannot still watch.
            # Preserve discovery time so a resumed watcher finds the original runs.
            data["started"] = previous["started"]
            data["attempt"] = previous["attempt"]
        receipt = WatchReceipt(path, owner, data)
        receipt.save(pid=os.getpid(), attempt=data["attempt"] + 1, acquired=now)
        return receipt
    except BaseException:
        owner.close()
        raise
