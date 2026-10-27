#!/usr/bin/env python3
"""Reproduce ID-addressed replacement on Dropbox twin 0.0.4.
Requires Python 3 and an installed Rystic CLI/twin. Uses dummy credentials.
Creates and removes one instance. Exit 1 means the defect was reproduced.
"""
import json
import os
import signal
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


def emit(**fields):
    print(json.dumps(fields, sort_keys=True), flush=True)


def bound(_signum, _frame):
    raise TimeoutError("reproduction exceeded 30-second work budget")


def cli(*args, timeout=15):
    # Never print raw CLI output: it may contain license or lease information.
    result = subprocess.run(["rystic", *args], capture_output=True, text=True,
                            timeout=timeout)
    if result.returncode:
        raise RuntimeError("rystic command failed: " + args[0])
    return json.loads(result.stdout)


def main():
    name = "dropbox-id-repro-" + uuid.uuid4().hex
    folder = "/id-repro-" + uuid.uuid4().hex
    errors = []
    signal.signal(signal.SIGALRM, bound)
    signal.alarm(30)  # Work <=30s, cleanup <=30s: total <=60s.
    started = time.monotonic()
    cleanup_ok = False
    try:
        # Metadata-only preflight: do not pull, update, compile or launch a check.
        products = cli("list", "-json")["data"]["products"]
        product = next(p for p in products if p["product"] == "dropbox-twin")
        assert product.get("current") == "0.0.4", "install/select 0.0.4 first"
        assert "0.0.4" in product.get("installed", []), "0.0.4 must already be installed"
        launch = cli("run", "dropbox-twin", "--name", name, "--port", "0", "-d", "-json")
        os.environ["BASE_URL"] = launch["data"]["url"].rstrip("/")
        base = os.environ["BASE_URL"]
        assert urllib.parse.urlsplit(base).hostname in ("127.0.0.1", "localhost", "::1"), "local twin only"
        emit(date="2026-10-17", product="dropbox-twin", version="0.0.4", BASE_URL=base)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        def post(route, arg, body=None, label=None):
            headers = {"Authorization": "Bearer sl.tilo-test"}
            if body is None:
                headers["Content-Type"] = "application/json"
                data = json.dumps(arg).encode()
            else:
                headers["Content-Type"] = "application/octet-stream"
                headers["Dropbox-API-Arg"] = json.dumps(arg)
                data = body
            request = urllib.request.Request(base + "/2/files/" + route,
                                             data=data, headers=headers, method="POST")
            try:
                response = opener.open(request, timeout=15)
            except urllib.error.HTTPError as response_error:
                response = response_error
            with response:
                status = response.code
                raw = response.read()
                if route == "download" and status == 200:
                    obj = json.loads(response.headers.get("Dropbox-API-Result", "{}"))
                    selected = {k: obj[k] for k in ("id", "rev", "path_display", "path_lower") if k in obj}
                    selected["body"] = raw.decode("utf-8")
                else:
                    obj = json.loads(raw)
                    selected = {k: obj[k] for k in ("id", "rev", "path_display", "path_lower", "error_summary") if k in obj}
                # Deliberately omit session_id, authorization and all CLI/lease records.
                emit(step=label or route, method="POST", url=base + "/2/files/" + route,
                     status=status, response=selected)
                return status, obj, selected

        def check(condition, message):
            if not condition:
                errors.append(message)

        status, _, _ = post("create_folder_v2", {"path": folder})
        assert status == 200, "folder creation failed"

        def case(kind):
            path = folder + "/" + kind + ".txt"
            original, changed = b"original", b"changed"

            def upload(payload, commit):
                if kind == "direct":
                    return post("upload", commit, payload, label=kind + " upload")
                status, session, _ = post("upload_session/start", {}, payload,
                                          label=kind + " start")
                assert status == 200, "session start failed"
                return post("upload_session/finish", {
                    "cursor": {"session_id": session["session_id"], "offset": len(payload)},
                    "commit": commit,
                }, b"", label=kind + " finish (empty body)")

            status, old, _ = upload(original, {"path": path, "mode": "add"})
            assert status == 200 and old["id"].startswith("id:"), "initial upload failed"
            old_id, old_rev = old["id"], old["rev"]
            commit = {"path": old_id, "mode": {".tag": "update", "update": old_rev},
                      "strict_conflict": True}
            emit(step=kind + " replacement request", commit=commit, body=changed.decode(),
                 expected={"status": 200, "id": old_id, "path_display": path,
                           "body": changed.decode(), "rev": "different from " + old_rev})
            status, new, _ = upload(changed, commit)
            check(status == 200, kind + ": replacement HTTP status is not 200")
            check(new.get("id") == old_id, kind + ": replacement changed native ID")
            check(new.get("rev") != old_rev, kind + ": revision did not change")
            check(new.get("path_display") == path, kind + ": replacement changed path")
            for selector in (path, old_id):
                status, meta, _ = post("get_metadata", {"path": selector},
                                      label=kind + " metadata selector=" + selector)
                check(status == 200 and meta.get("id") == old_id,
                      kind + ": metadata no longer resolves original ID at " + selector)
                check(meta.get("rev") == new.get("rev"),
                      kind + ": original selector revision differs from replacement")
                status, _, downloaded = post("download", {"path": selector}, b"",
                                             label=kind + " download selector=" + selector)
                check(status == 200 and downloaded.get("body") == changed.decode(),
                      kind + ": original selector does not download changed bytes at " + selector)

        # The shorter, direct repro runs first, on a distinct file in this same instance.
        case("direct")
        case("session")
        emit(expected="ID and original path preserved; changed bytes and revision visible",
             failures=errors, result="FAIL" if errors else "PASS")
        assert not errors, "; ".join(errors)  # Strict failure, after collecting both cases.
    finally:
        signal.alarm(0)
        cleanup_start = time.monotonic()
        codes = []
        # Always attempt both operations, including after assertions or startup failure.
        for operation in ("stop", "rm"):
            try:
                args = ["rystic", operation]
                if operation == "stop":
                    args += ["-timeout", "2s"]
                args.append(name)
                result = subprocess.run(args, capture_output=True, timeout=14)
                codes.append(result.returncode)
            except (subprocess.TimeoutExpired, OSError):
                codes.append(-1)
        cleanup_ok = codes == [0, 0]
        emit(cleanup={"stop_exit": codes[0], "rm_exit": codes[1], "ok": cleanup_ok},
             cleanup_seconds=round(time.monotonic() - cleanup_start, 3),
             total_seconds=round(time.monotonic() - started, 3))
        if not cleanup_ok:
            raise RuntimeError("own-instance cleanup failed")


if __name__ == "__main__":
    main()
