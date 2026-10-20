#!/usr/bin/env python3
"""Bounded, numeric-only sccache snapshots; never reset or configure the cache.

Deltas bracket one command's server-counter window. Cache operation durations
can overlap; they are not wall-time overhead and must not be summed as such.
"""
import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import time

COUNTERS = ("cache_hits", "cache_misses", "cache_writes", "cache_read_errors",
            "cache_write_errors", "cache_timeouts", "compile_requests", "requests_executed")
DURATIONS = ("cache_read_hit", "cache_read_miss", "cache_write")
LIMIT = 65536
TIMEOUT = 1.0


def count(value):
    if isinstance(value, dict):
        values = value.get("counts")
        if not isinstance(values, dict):
            return None
        numbers = [count(item) for item in values.values()]
        return None if None in numbers else sum(numbers)
    return value if type(value) is int and 0 <= value < 2**63 else None


def duration(value):
    if not isinstance(value, dict):
        return None
    seconds, nanos = count(value.get("secs")), count(value.get("nanos"))
    if seconds is None or nanos is None or seconds >= 10**12 or nanos >= 10**9:
        return None
    return seconds + nanos / 1e9


def snapshot():
    wrapper = os.environ.get("RUSTC_WRAPPER", "")
    if Path(wrapper).name != "sccache":
        return {"observed": False, "reason": "disabled"}
    started = time.monotonic()
    process = None
    try:
        process = subprocess.Popen([wrapper, "--show-stats", "--stats-format=json"],
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        data = bytearray()
        with selectors.DefaultSelector() as ready:
            ready.register(process.stdout, selectors.EVENT_READ)
            while True:
                left = TIMEOUT - (time.monotonic() - started)
                if left <= 0 or not ready.select(left):
                    raise TimeoutError
                chunk = os.read(process.stdout.fileno(), min(4096, LIMIT + 1 - len(data)))
                if not chunk:
                    break
                data.extend(chunk)
                if len(data) > LIMIT:
                    raise ValueError
            process.wait(timeout=max(0.001, TIMEOUT - (time.monotonic() - started)))
        if process.returncode:
            raise ValueError
        stats = json.loads(data)["stats"]
        counters = {name: count(stats.get(name)) for name in COUNTERS}
        if any(counters[name] is None for name in ("cache_hits", "cache_misses")):
            raise ValueError
        durations = {name + "_seconds": duration(stats.get(name + "_duration")) for name in DURATIONS}
        return {"observed": True, **counters, **durations}
    except (OSError, ValueError, KeyError, TypeError, AttributeError, TimeoutError, subprocess.TimeoutExpired):
        return {"observed": False, "reason": "unavailable"}
    finally:
        if process is not None:
            if process.poll() is None:
                process.kill()  # This owned stats reader only; never stop the cache server.
                process.wait()
            process.stdout.close()


def delta(before, after):
    if not before.get("observed") or not after.get("observed"):
        return {"observed": False, "reason": "unavailable"}
    if any(count(value.get(name)) is None for value in (before, after)
           for name in ("cache_hits", "cache_misses")):
        return {"observed": False, "reason": "unavailable"}
    values = {}
    for name in COUNTERS:
        first, last = count(before.get(name)), count(after.get(name))
        if first is not None and last is not None:
            if last < first:
                return {"observed": False, "reason": "counter_reset"}
            values[name] = last - first
    for name in DURATIONS:
        key = name + "_seconds"
        first, last = before.get(key), after.get(key)
        if type(first) in (int, float) and type(last) in (int, float):
            if not 0 <= first <= last < 10**12:
                return {"observed": False, "reason": "counter_reset"}
            values[key] = round(last - first, 6)
    return {"observed": True, **values}


if __name__ == "__main__":
    if sys.argv[1:] == ["snapshot"]:
        result = snapshot()
    elif len(sys.argv) == 4 and sys.argv[1] == "delta":
        result = delta(json.loads(sys.argv[2]), json.loads(sys.argv[3]))
    else:
        raise SystemExit("usage: compiler_cache_metrics.py snapshot | delta BEFORE AFTER")
    print(json.dumps(result, separators=(",", ":")))
