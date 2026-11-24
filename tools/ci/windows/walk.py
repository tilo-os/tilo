#!/usr/bin/env python3
"""Walk the Windows daemon under Wine as a person and their agent use it.

The daemon starts as a person's computer starts it, with no profile named, so `tilo-install`
reaches it as it reaches a shipping one. Video is installed from its package, signed by a
publisher no one vouches for, as a person installs their own app. Then, through the daemon's
MCP endpoint (`--connect`), as an agent:

1. Video is opened, and its window looked at as a picture;
2. a playlist is made, one action on its library;
3. the clip is put into the workspace, dropped on Video and played;
4. the library's History is read back, and must name what was done.

Every answer is printed, and the pictures of the window land in --out.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path


class Failed(Exception):
    pass


def windows_path(path: Path) -> str:
    """The path as a Windows program under Wine names it: Wine's Z: drive is the root."""
    return "Z:" + str(path.resolve()).replace("/", "\\")


class Mcp:
    """One MCP session over `tilo-runtime --connect`: JSON-RPC, one message a line."""

    def __init__(self, daemon: Path) -> None:
        self.process = subprocess.Popen(
            ["wine", str(daemon), "--connect"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )
        self.id = 0

    def ask(self, method: str, params: dict) -> dict:
        self.id += 1
        assert self.process.stdin and self.process.stdout
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.id, "method": method,
                                             "params": params}) + "\n")
        self.process.stdin.flush()
        while True:
            line = self.process.stdout.readline()
            if not line:
                raise Failed(f"the daemon hung up before answering {method}")
            message = json.loads(line)
            if message.get("id") == self.id:
                if "error" in message:
                    raise Failed(f"{method} was refused: {message['error']}")
                return message["result"]

    def tell(self, method: str) -> None:
        assert self.process.stdin
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method}) + "\n")
        self.process.stdin.flush()

    def tool(self, name: str, arguments: dict, *, picture_to: Path | None = None) -> str:
        result = self.ask("tools/call", {"name": name, "arguments": arguments})
        text = "\n".join(part["text"] for part in result.get("content", [])
                         if part.get("type") == "text")
        shown = {key: value for key, value in arguments.items() if key != "files"}
        print(f"--> {name} {json.dumps(shown)}\n{text}\n", flush=True)
        if result.get("isError"):
            raise Failed(f"{name} {shown} failed: {text}")
        if picture_to is not None:
            images = [part for part in result.get("content", []) if part.get("type") == "image"]
            if not images:
                raise Failed(f"{name} answered no picture")
            picture_to.write_bytes(base64.b64decode(images[0]["data"]))
            print(f"    the picture is {picture_to}", flush=True)
        return text

    def close(self) -> None:
        if self.process.stdin:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.process.kill()


def started(daemon: Path, log: Path, deadline: float) -> Mcp:
    """An MCP session once the daemon answers one."""
    while time.monotonic() < deadline:
        session = Mcp(daemon)
        try:
            session.ask("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                       "clientInfo": {"name": "windows-walk", "version": "1"}})
            session.tell("notifications/initialized")
            return session
        except Failed:
            session.close()
            time.sleep(1)
    raise Failed(f"the daemon answered no client; its log:\n{log.read_text(errors='replace')}")


def walk(walk_dir: Path, clip: Path, out: Path) -> None:
    daemon = walk_dir / "tilo-runtime-unconfined-test.exe"
    log = out / "daemon.log"
    with log.open("w") as written:
        running = subprocess.Popen(["wine", str(daemon)], stdout=written, stderr=subprocess.STDOUT)
    try:
        session = started(daemon, log, time.monotonic() + 300)
        apps = sorted((walk_dir / "apps").glob("*.tapp"), key=lambda tapp: tapp.name == "video.tapp")
        for tapp in apps:
            installed = subprocess.run(
                ["wine", str(walk_dir / "tilo-install.exe"), "install", "--allow-self-signed",
                 windows_path(tapp)],
                capture_output=True, text=True, timeout=600, check=False)
            print(f"--> tilo-install {tapp.name}\n{installed.stdout}{installed.stderr}", flush=True)
            if installed.returncode != 0:
                raise Failed(f"{tapp.name} did not install")

        session.tool("tilo_do", {"path": "video#open"})
        session.tool("tilo_view", {"path": "video", "picture": True}, picture_to=out / "opened.png")

        listed = session.tool("tilo_ls", {"path": "video"})
        library = re.search(r"(\S+)#create_playlist", listed)
        if not library:
            raise Failed("Video lists no library to make a playlist in")
        lib = library.group(1)
        session.tool("tilo_do", {"path": f"{lib}#create_playlist", "args": {"name": "Evening"}})

        put = session.tool("tilo_put_many",
                           {"files": [base64.b64encode(clip.read_bytes()).decode()]})
        content = re.search(r"tilo\.content:[0-9a-f]{64}", put)
        if not content:
            raise Failed("the clip's put answered no content reference")
        session.tool("tilo_do", {"path": f"{lib}#open_dropped", "args": {"files": [
            {"content": content.group(0), "name": "Test pattern.webm", "media": "video/webm"}]}})
        clips = session.tool("tilo_ls", {"path": lib})
        dropped = re.search(r"(\S*Test pattern\S*|\S+/clips/\S+)", clips)
        if not dropped:
            raise Failed("the dropped clip is not in the library")
        session.tool("tilo_do", {"path": f"{lib}#play_clip", "args": {"c": dropped.group(1)}})
        time.sleep(3)
        session.tool("tilo_view", {"path": "video", "picture": True}, picture_to=out / "playing.png")

        history = session.tool("tilo_do", {"path": f"{lib}#history"})
        for done in ("Evening", "opened a video", "played a video"):
            if done not in history:
                raise Failed(f"History does not say {done!r}")
        session.close()
    finally:
        running.terminate()
        try:
            running.wait(timeout=30)
        except subprocess.TimeoutExpired:
            running.kill()
        subprocess.run(["wineserver", "-k"], check=False)
    print("walked Video on the Windows daemon under Wine", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--walk", type=Path, required=True)
    parser.add_argument("--clip", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    os.environ.setdefault("WINEDEBUG", "-all")
    try:
        walk(args.walk, args.clip, args.out)
    except Failed as failure:
        print(f"the walk failed: {failure}", file=sys.stderr)
        log = args.out / "daemon.log"
        if log.exists():
            print(log.read_text(errors="replace")[-20000:], file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
