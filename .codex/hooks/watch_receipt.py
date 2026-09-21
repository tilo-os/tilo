"""Process-lifetime exclusion and bounded recovery of an interrupted CI watcher."""

import datetime as dt
import json
import os
from pathlib import Path

MAX_WATCH_ATTEMPTS = 3
REPORTED_RETENTION = dt.timedelta(days=7)
PRUNE_LIMIT = 32


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


def lock_nonblocking(owner) -> bool:
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
        return True
    except OSError:
        return False


def prune_reported(directory: Path, *, now: dt.datetime) -> None:
    """Retire old terminal receipts without touching active or ambiguous watches."""
    cutoff = now - REPORTED_RETENTION
    removed = 0
    for path in directory.glob("*.json"):
        if removed >= PRUNE_LIMIT:
            break
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
            acquired = dt.datetime.fromisoformat(data["acquired"])
            if data.get("state") != "reported" or acquired.tzinfo is None or acquired >= cutoff:
                continue
        except (OSError, ValueError, KeyError, TypeError):
            continue

        lock_path = path.with_suffix(".lock")
        owner = lock_path.open("a+b")
        tombstone = lock_path.with_suffix(f".{os.getpid()}.pruned")
        try:
            if not lock_nonblocking(owner):
                continue
            # Re-read under the receipt lock so a state transition cannot race cleanup.
            current = json.loads(path.read_text(encoding="utf-8"))
            acquired = dt.datetime.fromisoformat(current["acquired"])
            if current.get("state") != "reported" or acquired.tzinfo is None or acquired >= cutoff:
                continue
            # Renaming the held lock removes its public name before the receipt.
            # A concurrent opener either observes this lock or creates the next one.
            if os.name == "nt":
                # Windows does not permit renaming this open handle. The receipt
                # is terminal, and a racing acquire rejects that state.
                owner.close()
                owner = None
            os.replace(lock_path, tombstone)
            path.unlink()
            removed += 1
        except (OSError, ValueError, KeyError, TypeError):
            continue
        finally:
            if owner is not None:
                owner.close()
            try:
                tombstone.unlink()
            except FileNotFoundError:
                pass


def acquire(path: Path, *, cwd: Path, session_id: str, sha: str) -> WatchReceipt | None:
    path.parent.mkdir(parents=True, exist_ok=True)
    now_value = dt.datetime.now(dt.timezone.utc)
    prune_reported(path.parent, now=now_value)
    owner = path.with_suffix(".lock").open("a+b")
    try:
        if not lock_nonblocking(owner):
            owner.close()
            return None
        now = now_value.isoformat()
        data = {"session_id": session_id, "sha": sha, "cwd": str(cwd),
                "state": "watching", "started": now, "attempt": 0}
        if path.exists():
            try:
                previous = json.loads(path.read_text(encoding="utf-8"))
                safe = (previous["state"] == "watching"
                        and previous["session_id"] == session_id
                        and previous["sha"] == sha
                        and type(previous["attempt"]) is int
                        and 0 < previous["attempt"] < MAX_WATCH_ATTEMPTS
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
