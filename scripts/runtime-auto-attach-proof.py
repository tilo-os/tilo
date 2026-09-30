#!/usr/bin/env python3
"""Prove ordinary native app launches auto-attach to one persistent runtime."""

import argparse
import hashlib
import json
import os
import pathlib
import queue
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
import traceback


class ProofError(RuntimeError):
    pass


class StaleDiscovery(ProofError):
    pass


def launch_commands(fixture):
    return [
        ("word-processor", ["cargo", "run", "-p", "word-processor"]),
        ("file-explorer", ["cargo", "run", "-p", "file-explorer"]),
        ("photos", ["cargo", "run", "-p", "photos", "--", "--open", str(fixture)]),
        ("calculator", ["cargo", "run", "-p", "calculator"]),
    ]


def decode_native_status(frame):
    try:
        value = json.loads(frame)
    except (json.JSONDecodeError, UnicodeDecodeError) as error:
        raise ProofError(f"invalid native status JSON: {error}") from error
    if (
        not isinstance(value, dict)
        or set(value) != {"version", "pid", "room"}
        or value["version"] != 1
        or not isinstance(value["pid"], int)
        or isinstance(value["pid"], bool)
        or value["pid"] <= 0
        or not isinstance(value["room"], str)
        or not value["room"]
    ):
        raise ProofError(f"invalid native status: {value!r}")
    return value


def index_expected_apps(apps, expected):
    selected = {}
    for app in apps:
        if app.get("app_id") in expected:
            if app["app_id"] in selected:
                raise ProofError(f"duplicate expected app registration: {app['app_id']}")
            selected[app["app_id"]] = app
    if set(selected) != set(expected):
        raise ProofError(f"expected {sorted(expected)}, observed {apps}")
    peers = [app.get("peer_id") for app in selected.values()]
    if any(not isinstance(peer, int) or peer <= 0 for peer in peers) or len(set(peers)) != len(peers):
        raise ProofError(f"expected distinct positive peer IDs: {selected}")
    return selected


def changed_sources(before, after):
    return sorted(path for path in set(before) | set(after) if before.get(path) != after.get(path))


def save(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def source_snapshot(repo):
    roots = [
        repo / "Cargo.toml",
        repo / "Cargo.lock",
        repo / "crates/runtime/runtime-portal",
        repo / "shell",
        repo / "ui",
        repo / "apps/word-processor",
        repo / "apps/file-explorer",
        repo / "apps/photos",
        repo / "apps/calculator",
    ]
    suffixes = {".rs", ".toml", ".lock", ".capnp"}
    files = []
    for root in roots:
        if root.is_file():
            files.append(root)
        elif root.is_dir():
            files.extend(path for path in root.rglob("*") if path.is_file() and path.suffix in suffixes)
    return {str(path.relative_to(repo)): sha256(path) for path in sorted(set(files))}


def process_snapshot():
    lines = subprocess.check_output(
        ["ps", "-axo", "pid=,ppid=,pgid=,stat=,command="], text=True
    ).splitlines()
    result = []
    for line in lines:
        fields = line.strip().split(None, 4)
        if len(fields) == 5:
            result.append(
                {
                    "pid": int(fields[0]),
                    "ppid": int(fields[1]),
                    "pgid": int(fields[2]),
                    "state": fields[3],
                    "command": fields[4],
                }
            )
    return result


def processes_for_group(pgid):
    return [process for process in process_snapshot() if process["pgid"] == pgid]


def processes_for_pids(pids):
    return [process for process in process_snapshot() if process["pid"] in pids]


def pid_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def wait_until(description, callback, timeout=60, interval=0.1):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            value = callback()
            if value:
                return value
            last = value
        except (ProofError, OSError) as error:
            last = repr(error)
        time.sleep(interval)
    raise ProofError(f"timeout waiting for {description}; last={last!r}")


def native_status(profile):
    endpoint = profile / "native.sock"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(0.5)
        stream.connect(str(endpoint))
        stream.sendall(b'{"native":"status"}\n')
        frame = b""
        while not frame.endswith(b"\n"):
            block = stream.recv(4096)
            if not block:
                break
            frame += block
            if len(frame) > 16384:
                raise ProofError("native status exceeded 16 KiB")
    if not frame.endswith(b"\n"):
        raise ProofError("native status did not end with newline")
    return decode_native_status(frame)


def decode_reply(reply, request_id):
    if reply.get("jsonrpc") != "2.0" or reply.get("id") != request_id:
        raise ProofError(f"unexpected protocol reply: {reply}")
    if "error" in reply:
        raise ProofError(f"JSON-RPC error: {reply['error']}")
    return reply["result"]


class Client:
    def __init__(self, command, env, root):
        self.command = command
        self.stderr_path = root / "mcp-connect.stderr.log"
        self.transcript_path = root / "transcript.jsonl"
        self.stderr = self.stderr_path.open("w")
        self.transcript = self.transcript_path.open("w")
        self.process = subprocess.Popen(
            command,
            cwd=root.parent.parent.parent,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.stderr,
            text=True,
            bufsize=1,
        )
        self.replies = queue.Queue()
        self.next_id = 0
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        for line in self.process.stdout:
            self.replies.put(line)
        self.replies.put(None)

    def request(self, method, params):
        self.next_id += 1
        request = {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}
        self.transcript.write(json.dumps({"direction": "request", "message": request}) + "\n")
        self.transcript.flush()
        self.process.stdin.write(json.dumps(request) + "\n")
        self.process.stdin.flush()
        try:
            line = self.replies.get(timeout=15)
        except queue.Empty as error:
            raise ProofError("MCP response timeout; no mutation replay") from error
        if line is None:
            raise ProofError(f"MCP EOF, proxy status {self.process.poll()}")
        reply = json.loads(line)
        self.transcript.write(json.dumps({"direction": "response", "message": reply}) + "\n")
        self.transcript.flush()
        return decode_reply(reply, self.next_id)

    def tool(self, name, arguments):
        result = self.request("tools/call", {"name": name, "arguments": arguments})
        if result.get("isError"):
            text = " ".join(item.get("text", "") for item in result.get("content", []))
            if "StaleRevision" in text:
                raise StaleDiscovery(text)
            raise ProofError(text or json.dumps(result))
        return result["structuredContent"]

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                code = self.process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.process.terminate()
                code = self.process.wait(timeout=15)
        else:
            code = self.process.returncode
        self.reader.join(timeout=2)
        self.process.stdout.close()
        self.stderr.close()
        self.transcript.close()
        return code


def browse_rows(tool, peer_id):
    pending = [""]
    seen = set()
    revision = ""
    pages = 0
    while pending:
        parent = pending.pop(0)
        cursor = []
        while True:
            pages += 1
            if pages > 128:
                raise ProofError("directory traversal exceeded 128 pages")
            page = tool(
                "tilo_query",
                {"peer_id": peer_id, "parent_resource": parent, "revision": revision, "cursor": cursor},
            )
            if page.get("stale_cursor"):
                raise StaleDiscovery("directory revision changed")
            revision = page["revision"]
            for row in page["rows"]:
                yield revision, row
                if row.get("has_children") and row["resource"] not in seen:
                    seen.add(row["resource"])
                    pending.append(row["resource"])
            if page["exhausted"]:
                break
            next_cursor = page.get("next_cursor")
            if not next_cursor or next_cursor == cursor:
                raise ProofError("directory pagination did not advance")
            cursor = next_cursor


def resolve_action(tool, peer_id, action):
    def discover():
        try:
            for revision, row in browse_rows(tool, peer_id):
                if row.get("action") == action and row.get("descriptor"):
                    target = {
                        "peer_id": peer_id,
                        "revision": revision,
                        "resource": row["resource"],
                        "action": action,
                    }
                    return target, tool("tilo_resolve", target)["row"]["descriptor"]
        except StaleDiscovery:
            return None
        return None

    return wait_until(f"action {action} on peer {peer_id}", discover, timeout=20)


def observe_terminal(tool, result):
    invocation_id = result["invocation_id"]

    def terminal():
        current = tool("tilo_status", {"invocation_id": invocation_id})
        return current if current["update"]["state"] in {"succeeded", "failed", "cancelled"} else None

    return wait_until(f"terminal invocation {invocation_id}", terminal, timeout=30)


def single_argument(descriptor, value):
    params = descriptor["params"]
    if len(params) != 1:
        raise ProofError(f"expected one action parameter, got {params}")
    return {params[0]["name"]: value}


class AppLaunch:
    def __init__(self, name, command, repo, env, log_dir):
        self.name = name
        self.command = command
        self.stdout_path = log_dir / f"{name}.stdout.log"
        self.stderr_path = log_dir / f"{name}.stderr.log"
        self.stdout = self.stdout_path.open("w")
        self.stderr = self.stderr_path.open("w")
        self.process = subprocess.Popen(
            command,
            cwd=repo,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=self.stdout,
            stderr=self.stderr,
            start_new_session=True,
        )
        self.pgid = self.process.pid
        self.actual_pid = None

    def find_actual_pid(self, repo):
        executable = str(repo / "target/debug" / self.name)

        def find():
            if self.process.poll() is not None:
                raise ProofError(f"{self.name} cargo run exited {self.process.returncode}")
            candidates = [
                process
                for process in processes_for_group(self.pgid)
                if process["command"].split()[0] == executable and "Z" not in process["state"]
            ]
            if len(candidates) > 1:
                raise ProofError(f"multiple {self.name} executable PIDs: {candidates}")
            return candidates[0]["pid"] if candidates else None

        self.actual_pid = wait_until(f"actual {self.name} executable", find, timeout=600)
        return self.actual_pid

    def terminate(self):
        known = {process["pid"] for process in processes_for_group(self.pgid)}
        for pid in sorted(known):
            if not pid_alive(pid):
                continue
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            try:
                os.kill(self.process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired as error:
                raise ProofError(f"failed to reap cargo run for {self.name}") from error
        terminate_owned_pids(known - {self.process.pid})
        self.stdout.close()
        self.stderr.close()
        return sorted(known), self.process.returncode


def terminate_owned_pids(pids):
    owned = sorted(set(pids))
    for pid in owned:
        if not pid_alive(pid):
            continue
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline and any(pid_alive(pid) for pid in owned):
        time.sleep(0.1)
    for pid in owned:
        if not pid_alive(pid):
            continue
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline and any(pid_alive(pid) for pid in owned):
        time.sleep(0.1)
    remaining = [pid for pid in owned if pid_alive(pid)]
    if remaining:
        raise ProofError(f"owned PIDs survived cleanup: {remaining}")


APP_IDS = {
    "word-processor": "dev.tilo.word-processor",
    "file-explorer": "dev.tilo.file-explorer",
    "photos": "dev.tilo.photos",
    "calculator": "dev.tilo.calculator",
}


def wait_for_apps(client, expected, absent=frozenset()):
    def observe():
        apps = client.tool("tilo_list_apps", {})["apps"]
        present = {app["app_id"] for app in apps}
        if set(expected).issubset(present) and set(absent).isdisjoint(present):
            selected = [app for app in apps if app["app_id"] in expected]
            return index_expected_apps(selected, expected)
        return None

    return wait_until(f"apps {sorted(expected)} absent {sorted(absent)}", observe, timeout=60)


def terminate_pid(pid):
    terminate_owned_pids([pid])


def remove_profile(profile):
    shutil.rmtree(profile)
    if profile.exists() or profile.is_symlink():
        raise ProofError(f"runtime profile remains after cleanup: {profile}")


def proof_environment(profile, app_state):
    env = {key: value for key, value in os.environ.items() if not key.startswith(("TILO_", "QUICK_"))}
    env.update(
        {
            "TILO_RUNTIME_PROFILE": str(profile),
            "TILO_USER_HOME": str(app_state / "home"),
            "TILO_WP_RECOVERY_DIR": str(app_state / "word-processor-recovery"),
            "XDG_CONFIG_HOME": str(app_state / "config"),
            "XDG_DATA_HOME": str(app_state / "data"),
            "XDG_STATE_HOME": str(app_state / "state"),
            "XDG_CACHE_HOME": str(app_state / "cache"),
            "XDG_RUNTIME_DIR": str(app_state / "run"),
        }
    )
    for directory in env.values():
        if isinstance(directory, str) and directory.startswith(str(app_state)):
            pathlib.Path(directory).mkdir(parents=True, exist_ok=True)
    return env


def run_proof(repo, root, host_source_snapshot):
    fixture = root / "source.png"
    shutil.copy2(repo / "ui/tests/fixtures/test_image.png", fixture)
    profile = pathlib.Path(tempfile.mkdtemp(prefix="tilo-auto-", dir="/tmp"))
    os.chmod(profile, 0o700)
    app_state = root / "isolated-app-state"
    app_state.mkdir()
    env = proof_environment(profile, app_state)
    commands = launch_commands(fixture)
    source_before = source_snapshot(repo)
    save(root / "source-hashes-before.json", source_before)
    built_host_sources = json.loads(host_source_snapshot.read_text())
    host_source_changes = changed_sources(built_host_sources, source_before)
    if host_source_changes:
        raise ProofError(
            f"host build inputs changed after its source snapshot: {host_source_changes}"
        )
    save(
        root / "metadata.json",
        {
            "git_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip(),
            "git_status": subprocess.check_output(["git", "status", "--short"], cwd=repo, text=True).splitlines(),
            "platform": subprocess.check_output(["uname", "-a"], text=True).strip(),
            "repo": str(repo),
            "runtime_profile": str(profile),
            "commands": [{"name": name, "argv": command} for name, command in commands],
            "environment": {key: env[key] for key in sorted(env) if key.startswith(("TILO_", "XDG_"))},
            "fixture_sha256": sha256(fixture),
            "host_build_source_snapshot": str(host_source_snapshot),
        },
    )
    if (profile / "native.sock").exists():
        raise ProofError("isolated profile unexpectedly has a daemon endpoint")
    runtime_binary = repo / "target/debug/tilo-runtime-mcp"
    if not runtime_binary.is_file():
        raise ProofError("build target/debug/tilo-runtime-mcp before the native proof")

    launches = {}
    client = None
    daemon = None
    proof = None
    cleanup = {
        "app_processes": {},
        "mcp_exit": None,
        "mcp_pid": None,
        "daemon_pid": None,
        "known_pids": [],
        "remaining": [],
    }
    try:
        first_name, first_command = commands[0]
        launches[first_name] = AppLaunch(first_name, first_command, repo, env, root)
        launches[first_name].find_actual_pid(repo)
        daemon = wait_until("first app to start the runtime daemon", lambda: native_status(profile), timeout=30)
        cleanup["daemon_pid"] = daemon["pid"]

        for name, command in commands[1:3]:
            launches[name] = AppLaunch(name, command, repo, env, root)
            launches[name].find_actual_pid(repo)

        client_command = [str(runtime_binary), "--profile", str(profile), "--connect"]
        client = Client(client_command, env, root)
        cleanup["mcp_pid"] = client.process.pid
        client.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                # The proof reads full rows (descriptors and contracts).
                "capabilities": {"experimental": {"so.tilo.mcp": {"detail": "full"}}},
                "clientInfo": {"name": "runtime-auto-attach-proof", "version": "1"},
            },
        )
        client.request("tools/list", {})
        initial = wait_for_apps(client, {APP_IDS[name] for name in launches})
        save(root / "initial-apps.json", initial)

        late_name, late_command = commands[3]
        launches[late_name] = AppLaunch(late_name, late_command, repo, env, root)
        launches[late_name].find_actual_pid(repo)
        all_apps = wait_for_apps(client, set(APP_IDS.values()))
        save(root / "all-apps.json", all_apps)

        target, descriptor = resolve_action(client.tool, all_apps[APP_IDS[late_name]]["peer_id"], "add-function")
        formula = "sin(x) + 0.271828"
        admitted = client.tool("tilo_invoke", dict(target, arguments=single_argument(descriptor, formula)))
        terminal = observe_terminal(client.tool, admitted)
        if terminal["update"]["state"] != "succeeded":
            raise ProofError(f"declared Calculator action failed: {terminal}")
        save(root / "action.json", {"target": target, "descriptor": descriptor, "arguments": single_argument(descriptor, formula), "admitted": admitted, "terminal": terminal})

        before_exit = {
            name: {"cargo_pid": launch.process.pid, "pgid": launch.pgid, "app_pid": launch.actual_pid, "processes": processes_for_group(launch.pgid)}
            for name, launch in launches.items()
        }
        save(root / "processes-before-exit.json", before_exit)
        stopped_pids, stopped_status = launches[first_name].terminate()
        cleanup["app_processes"][first_name] = {"pids": stopped_pids, "cargo_status": stopped_status}
        survivors = wait_for_apps(
            client,
            {APP_IDS[name] for name in launches if name != first_name},
            {APP_IDS[first_name]},
        )
        if client.process.poll() is not None or not pid_alive(daemon["pid"]):
            raise ProofError("MCP proxy or daemon ended when one app exited")
        for name, launch in launches.items():
            if name != first_name and (launch.process.poll() is not None or not pid_alive(launch.actual_pid)):
                raise ProofError(f"surviving app exited with {first_name}: {name}")
        save(root / "survivors.json", {"apps": survivors, "daemon": native_status(profile), "mcp_pid": client.process.pid})

        source_after = source_snapshot(repo)
        save(root / "source-hashes-after.json", source_after)
        changed = changed_sources(source_before, source_after)
        if changed:
            raise ProofError(f"relevant source changed during build/proof: {changed}")
        binaries = {name: repo / "target/debug" / name for name in APP_IDS}
        binaries["tilo-runtime-mcp"] = runtime_binary
        binary_metadata = {name: {"path": str(path), "sha256": sha256(path)} for name, path in binaries.items()}
        save(root / "binary-hashes.json", binary_metadata)
        proof = {
            "passed": True,
            "daemon": daemon,
            "mcp_pid": client.process.pid,
            "apps": all_apps,
            "actual_app_pids": {name: launch.actual_pid for name, launch in launches.items()},
            "action_terminal": terminal,
            "survivors_after_word_processor_exit": survivors,
            "source_changed": changed,
            "binaries": binary_metadata,
        }
    finally:
        if client is not None:
            cleanup["mcp_exit"] = client.close()
        for name, launch in launches.items():
            if name not in cleanup["app_processes"]:
                try:
                    pids, status = launch.terminate()
                    cleanup["app_processes"][name] = {"pids": pids, "cargo_status": status}
                except Exception as error:
                    cleanup["app_processes"][name] = {"cleanup_error": repr(error)}
        if daemon is not None:
            terminate_pid(daemon["pid"])
        known = {
            pid
            for launch in launches.values()
            for pid in (launch.process.pid, launch.actual_pid)
            if pid is not None
        }
        if cleanup["mcp_pid"] is not None:
            known.add(cleanup["mcp_pid"])
        if daemon is not None:
            known.add(daemon["pid"])
        cleanup["known_pids"] = sorted(known)
        cleanup["remaining"] = processes_for_pids(known)
        if (profile / "daemon.log").exists():
            shutil.copy2(profile / "daemon.log", root / "daemon.log")
        try:
            remove_profile(profile)
            cleanup["profile_removed"] = True
        except Exception as error:
            cleanup["profile_removed"] = False
            cleanup["profile_cleanup_error"] = repr(error)
        save(root / "cleanup.json", cleanup)
    if cleanup["mcp_exit"] != 0 or cleanup["remaining"] or not cleanup["profile_removed"]:
        raise ProofError(f"cleanup incomplete: {cleanup}")
    if proof is None:
        raise ProofError("proof did not reach its success gate")
    proof["cleanup"] = cleanup
    save(root / "proof.json", proof)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--repo", type=pathlib.Path, default=pathlib.Path(__file__).resolve().parents[1])
    parser.add_argument("--host-source-snapshot", type=pathlib.Path)
    parser.add_argument("--capture-host-source-snapshot", type=pathlib.Path)
    args = parser.parse_args()
    repo = args.repo.resolve()
    if args.capture_host_source_snapshot is not None:
        if args.output is not None or args.host_source_snapshot is not None:
            parser.error("snapshot capture cannot be combined with proof-run arguments")
        snapshot = args.capture_host_source_snapshot.resolve()
        snapshot.parent.mkdir(parents=True, exist_ok=True)
        save(snapshot, source_snapshot(repo))
        print(json.dumps({"captured": str(snapshot)}))
        return
    if args.output is None or args.host_source_snapshot is None:
        parser.error("proof runs require --output and --host-source-snapshot")
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    try:
        run_proof(repo, root, args.host_source_snapshot.resolve())
    except Exception:
        (root / "failure.txt").write_text(traceback.format_exc())
        raise
    print(json.dumps({"passed": True, "output": str(root)}))


if __name__ == "__main__":
    main()
