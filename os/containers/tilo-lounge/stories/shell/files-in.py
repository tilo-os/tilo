#!/usr/bin/env python3
"""Bring files into a workspace from the person's own computer, in a Tilo shell, and open each.

The shell is the person's computer, so there is no page to drop files on and no page's chooser.
What a person has there is Files, whose window lists their own folders and whose bar offers
"Upload files"; the `tilo` command of their own computer, which acts on the desktop they are
using; and, on a Tilo computer, the workspace's folder at /var/workspace. Each step walks one of
them the way a person does and reads where the file ended up off the shell's own state: the
records its workspace keeps, and the bytes on that person's disk.

A Tilo session on a computer that is not the image runs none of the daemons those three ways
rest on: no files service projects the workspace, so Files' Upload has no workspace folder to
copy into; the desktop's API has no answer to the command's question of what the workspace
already holds; and the folder the image mounts is not mounted. The steps say so in those words
instead of pretending a file arrived.

    python3 files-in.py OUT_DIR
"""

from __future__ import annotations

import os
import struct
import subprocess
import sys
import time
import zlib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from story import Failed, Person, Story, until, walk  # noqa: E402

FILES = "so.tilo.file-explorer"
PHOTO = "spring.png"
PDF = "ticket.pdf"
SHEET = "budget.csv"
DOCUMENT = "letter.txt"
LETTER = "The keys are under the flowerpot"
CLI = os.environ.get("TILO_CLI_BIN", "")
SWITCHER = "Switch workspace, now in "
APPS = "file-explorer settings terminal sheets pdf-viewer photos activity"
# The tilolang apps beside them, by their directory under apps/tl.
TILOLANG = ("tasks",)


def photo(width: int, height: int) -> bytes:
    """A PNG, as a camera or a screenshot would leave one: a small picture."""
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            raw += bytes((60 + 160 * x // width, 90 + 120 * y // height,
                          190 - 110 * (x + y) // (width + height)))

    def part(kind: bytes, data: bytes) -> bytes:
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + part(b"IHDR", header)
            + part(b"IDAT", zlib.compress(bytes(raw))) + part(b"IEND", b""))


def pdf(words: str) -> bytes:
    """A PDF with one page and a line of text on it, as a document from another program
    arrives."""
    objects = ["", "<< /Type /Catalog /Pages 2 0 R >>",
               "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
               "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 420 360] /Contents 4 0 R "
               "/Resources << /Font << /F1 5 0 R >> >> >>", None,
               "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
    drawn = f"BT /F1 24 Tf 60 300 Td ({words}) Tj ET"
    objects[4] = f"<< /Length {len(drawn)} >>\nstream\n{drawn}\nendstream"
    text = "%PDF-1.4\n"
    offsets = [0] * len(objects)
    for number in range(1, len(objects)):
        offsets[number] = len(text.encode("latin1"))
        text += f"{number} 0 obj\n{objects[number]}\nendobj\n"
    xref = len(text.encode("latin1"))
    text += f"xref\n0 {len(objects)}\n0000000000 65535 f \n"
    for number in range(1, len(objects)):
        text += f"{offsets[number]:010d} 00000 n \n"
    text += f"trailer\n<< /Size {len(objects)} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    return text.encode("latin1")


def in_workspace(one: Person, name: str) -> bool:
    """Whether a record of `one`'s workspace names `name`: the bytes the workspace keeps in its
    own store, which is where a file in the workspace shows up and a file left in the computer's
    folders does not."""
    return any(name.encode() in path.read_bytes()
               for path in one.state.rglob("*.automerge")
               if "identity" not in str(path) and "config" not in str(path))


def run_command(one: Person, *args: str, deadline: float = 120.0) -> subprocess.CompletedProcess:
    """One `tilo` command of the person's own computer, acting on the desktop they are using: the
    command finds that desktop at the socket it serves its person's programs on."""
    return subprocess.run([CLI, *args], env=one.session.env(), capture_output=True, text=True,
                          timeout=deadline)


def steps(story: Story) -> None:
    ada = story.person("Ada")
    ada.prepare(APPS, TILOLANG)
    if not CLI or not Path(CLI).is_file():
        raise Failed(f"no tilo command to run: TILO_CLI_BIN names {CLI!r}")
    workspace = {}

    def opening():
        took = ada.start(apps=APPS, tilolang=TILOLANG)
        story.timing("Ada's session start, to its first frame", took["tookMs"])
        story.timing("Ada's session start, to its first frame, with the development launcher's installing", took["launcherMs"])
        until("Ada's shell to hold a workspace", lambda: (
            (ada.session.state_dump() or {}).get("workspace_tree") or {}).get("active"), 60)
        # The workspace's name, as the shell's top bar says it: the name a person gives the
        # command is the one their desktop calls the workspace, not the dump's durable id.
        workspace["name"] = until("the name of the workspace in front", lambda: next(
            (name.removeprefix(SWITCHER) for name in ada.names()
             if name.startswith(SWITCHER)), None), 30, 0.2)
        return {"tookMs": took["tookMs"]}

    story.step("Ada opens Tilo", opening, people=[ada])

    story.step("Ada gives their name", lambda: ada.name_self("Ada"), people=[ada])

    # The files on Ada's computer, written where her own files are.
    home = ada.state / "home"
    home.mkdir(parents=True, exist_ok=True)
    (home / PHOTO).write_bytes(photo(320, 200))
    (home / PDF).write_bytes(pdf("Spring trip"))
    (home / SHEET).write_text("item,cost\nrent,900\nfood,320\ncoffee,45\n")
    (home / DOCUMENT).write_text(f"Dear Ada,\n\n{LETTER}.\n\nGrace\n")

    def brings_in():
        ada.session.launch("file-explorer")
        ada.session.answering("file-explorer", deadline=120)
        until("Files' window bar", lambda: ada.shows("New folder"), 30, 0.25)
        # Upload files is the bar's own action where it fits, and behind More actions where the
        # bar folds it, as a person finds it. The menu a press opens takes its frame, as any
        # menu does: what the person sees is the row, once it is drawn.
        if not ada.target("Upload files", exact=False):
            ada.press("More actions", "the window bar's More actions")
        story.note("files-window-before", ada.names())
        try:
            until("Upload files in the window bar, or in its More menu",
                  lambda: ada.target("Upload files", exact=False), 10, 0.25)
        except Failed:
            raise Failed(
                "Files cannot put the photo and the PDF into the workspace: its window bar "
                "folds Upload files behind More actions, and that menu shows no Upload files "
                "once it is open.") from None
        ada.press("Upload files", "Files' Upload files", exact=False)
        for name, also in ((PHOTO, False), (PDF, True)):
            row = until(f"{name} in the chooser", lambda: ada.target(name, exact=False), 30, 0.25)
            left, top, width, height = row["rect"]
            if also:
                ada.session.control({"op": "key_down", "key": "Control"})
            try:
                ada.session.click_at(left + width / 2, top + height / 2)
            finally:
                if also:
                    ada.session.control({"op": "key_up", "key": "Control"})
        story.note("controls-chooser", ada.names())
        for label in ("Use the chosen file", "Open", "Choose", "Upload"):
            if ada.target(label, exact=False):
                ada.press(label)
                break
        for name in (PHOTO, PDF):
            until(f"{name} in Ada's workspace", lambda: in_workspace(ada, name), 60, 0.25)

    story.step("Ada brings a photo and a PDF in with Files", brings_in, people=[ada])

    def command():
        began = time.monotonic()
        ran = run_command(ada, "files", "put", str(home / SHEET), "-w", workspace["name"] or "")
        if ran.returncode != 0:
            raise Failed(
                f"the `tilo` command cannot put {SHEET} into the workspace: "
                f"{ran.stderr.strip() or ran.stdout.strip()} — this desktop answers no question "
                "about what its workspace holds, so the command stops before it sends a byte.")
        until(f"{SHEET} in Ada's workspace", lambda: in_workspace(ada, SHEET), 60, 0.25)
        return {"tookMs": (time.monotonic() - began) * 1000}

    story.step("Ada copies a spreadsheet in with the `tilo` command", command, people=[ada])

    def folder():
        # On a Tilo computer the workspace's folder is the image's own filesystem service: fs.service
        # mounts it at /var/workspace. She copies the document there with `cp`, as a person does.
        at = Path("/var/workspace")
        mounted = [line for line in Path("/proc/self/mounts").read_text().splitlines()
                   if " /var/workspace " in line or " tilo " in line.lower()]
        if not mounted or not at.is_dir() or not any(at.iterdir()):
            raise Failed(
                "this computer has no folder of the workspace to copy a document into: the "
                "workspace's folder is mounted by the image's filesystem service (fs.service "
                "mounts /var/workspace) or by the peer `tilo login` leaves running, and a Tilo "
                "session on this computer mounts neither.")
        (at / DOCUMENT).write_text((home / DOCUMENT).read_text())
        until(f"{DOCUMENT} in Ada's workspace", lambda: in_workspace(ada, DOCUMENT), 60, 0.25)

    story.step("Ada copies a document into the workspace’s folder", folder, people=[ada])

    def opens_each():
        # Each one opened from the shell's search, where a person who knows the name looks: the
        # file opens in the app that reads it — a picture in Photos, a PDF in PDF Viewer, a table
        # in Sheets — and where the file's own words are on screen, they are read back too.
        wanted = [{"name": PHOTO, "app": "so.tilo.photos"},
                  {"name": PDF, "app": "so.tilo.pdf-viewer"},
                  {"name": SHEET, "app": "so.tilo.sheets", "words": "rent"}]
        for file in wanted:
            if not in_workspace(ada, file["name"]):
                raise Failed(f"{file['name']} never reached the workspace, so there is nothing "
                             "there to open")
            ada.session.chord("Super", "Space")
            answer = ada.session.wait_search_rows(file["name"], wanted=(file["name"],), deadline=60)
            rows = [row.get("title") or "" for row in answer.get("rows") or []]
            index = next((at for at, title in enumerate(rows) if file["name"] in title), None)
            if index is None:
                raise Failed(f"the search does not find {file['name']}: {rows}")
            for _ in range(index):
                ada.session.key("ArrowDown")
            ada.session.key("Enter")
            until(f"{file['name']} opening in its app",
                  lambda: any(w.get("app_id") == file["app"] for w in ada.session.windows()),
                  60, 0.25)
            if file.get("words"):
                until(f"{file['name']} on screen", lambda: ada.shows(file["words"]), 30, 0.25)

    story.step("Ada opens each file in the workspace", opens_each, people=[ada])


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: files-in.py OUT_DIR", file=sys.stderr)
        sys.exit(2)
    sys.exit(walk("files-in", Path(sys.argv[1]), steps))
