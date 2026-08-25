#!/usr/bin/env python3
"""Three qOS sessions in one room, and an agent that drives one of them.

Starts three real compositors — separate state directories, separate
libp2p keypairs, separate user identities, separate control sockets:

  quinten  hosts the room and owns it
  xaver    joins it by invite
  Claude   is admitted to it too, and from then on is driven *only* by
           the action API: semantic invocation by `(provider, id)`,
           never by a click, a keystroke, or a pixel

The storyboard, in order, about a minute:

  1. an empty desktop, and Files opened on it
  2. quinten invites xaver
  3. quinten invites the agent — a capability principal, not a peer
  4. both have joined, and each session can see the others
  5. "word processor" — a machine that does not have it, fetching it
  6. cut to xaver: the window quinten opened is open for him too
  7. quinten writes, and xaver's screen is asked what it holds
  8. a photograph dragged out of Files and into the document
  9. the agent acts by address, and the other two read the result back

Every claim is read back from a session that did **not** make the change.
Where that is impossible with today's compositor the transcript prints a
GAP and moves on; it never prints a success it did not verify.

    scripts/yc-demo.py
    scripts/yc-demo.py --keep
    scripts/yc-demo.py --record            # …and write a 1920x1080 video

Runs against target/demo/shell:

    cargo build --profile demo -p shell --bin shell --bin qos-control

There is no mock and no fixture path: if the binary is missing the
script exits rather than simulating it.

Build it optimised. `snapshot` re-renders the scene and PNG-encodes it
on the compositor's own event loop, so the session is blocked for every
millisecond of it — and an unoptimised build spends ~1.4 s there per
frame at 1920x1080, which is a recording nobody can watch.

`--release` is not the profile to use, and not because of speed: the
`QOS_TEST_CONTROL` socket this script drives the compositor through is a
complete lock-screen bypass, so a shipped build deliberately does not
contain the code that reads it. A `--release` shell binds no socket and
never says why. `demo` is the release profile with that one gate left
open, and nothing about `--release` changes.
"""
from __future__ import annotations

import ast
import argparse
import contextlib
import hashlib
import base64
import json
import math
import glob
import os
import random
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import threading
import time
import zlib

# `reaper` lives beside the other headless drivers rather than on the
# path, so it is imported by location — after `sys`, which the line
# doing the importing needs.
sys.path.insert(
    0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "headless-examples"))
from reaper import die_with_parent, track   # noqa: E402

# Says how a compositor ended — a clean exit, a signal, or a crash that
# left a core — so a run reports which of those happened instead of
# leaving it to be inferred from a timestamp. Beside this file, so it is
# on the path already.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import session_exit   # noqa: E402

# Set from `--keep`. Off means the kernel reaps a compositor with this
# script, however this script dies.
KEEP_SESSIONS = False

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# Where cargo puts things. Not `REPO/target` unconditionally: a worktree
# that sets `CARGO_TARGET_DIR` has no `target/` at all, and every lookup
# below then reports a binary missing in a tree that just built it —
# which is how a run once filmed with its evidence decoder absent and
# reported the resulting blindness as a failing claim about the product.
TARGET = os.environ.get("CARGO_TARGET_DIR") or os.path.join(REPO, "target")


def _shadowed_definitions() -> list[str]:
    """Top-level names this file defines more than once.

    Python resolves a redefinition silently, in favour of whichever came
    last, so a duplicate name is not an error anywhere — it is a
    different function than the one at the call site, called with the
    wrong kind of argument, failing inside a `try` that turns it into a
    plausible-looking answer. That is exactly how a second
    `peer_id_of` — one taking a Session, one taking an invite token —
    reported that three separate compositors shared one identity, and
    sent people looking for a regression in the shell.

    A harness whose whole purpose is to tell truth from plausible
    failure should not be able to fail that way quietly.
    """
    try:
        tree = ast.parse(open(os.path.abspath(__file__)).read())
    except (OSError, SyntaxError):
        return []
    seen: dict[str, int] = {}
    dupes: list[str] = []
    for node in tree.body:
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef,
                                 ast.ClassDef)):
            continue
        if node.name in seen:
            dupes.append(f"{node.name} (lines {seen[node.name]} and "
                         f"{node.lineno})")
        seen[node.name] = node.lineno
    return dupes


def _doc_text_bin() -> str | None:
    """The Automerge decoder a document assertion needs.

    A saved document is DEFLATE-compressed per column, so there is no
    honest way to answer "does this hold that sentence" without decoding
    it. Absent, the beats that ask are reported as gaps rather than
    quietly answered wrong.

    Every profile, because this is a decoder rather than a thing being
    measured: an optimised build and a debug build read the same
    document, so a run should not be blinded by which one is to hand.
    """
    for profile in ("demo", "release", "debug"):
        path = os.path.join(TARGET, profile, "qos-doc-text")
        if os.path.exists(path):
            return path
    return None


class CannotDecode(Exception):
    """The decoder is missing, so no honest answer about a document's
    contents is available.

    Its own type because the alternative is the bug it was raised for: a
    missing binary answering "the text is not in the document" for every
    document, which is a *failing assertion* about the product rather
    than a gap in the harness. That is the worst failure mode a check
    has — it accuses working code, and the accusation looks exactly like
    a real regression. See [`carries_text`].
    """


DOC_TEXT = _doc_text_bin()

# The action the agent invokes, as a stable `(provider, id)` address from
# the shell's own action provider — the same registry that feeds the
# command palette, reached with no palette and no keystroke.
AGENT_PROVIDER = "actions"
AGENT_ACTION_ID = "room.activity.open"
# Its effect, as a key under `chrome.overlays` in `state_dump`.
AGENT_ACTION_EFFECT = "room_activity"

# The provider that launches installed applications, and the two apps the
# storyboard opens. An app is started by its own id through the same
# registry that feeds the palette, so the demo drives the production
# launch path rather than a test entry point.
APPLICATIONS = "applications"
FILES_APP = "dev.qos.file-explorer"
WORD_PROCESSOR = "dev.qos.word-processor"
CHAT_APP = "dev.qos.chat"
#: The image editor.
#:
#: `dev.qos.imager` is **not** this. It is the Disk Imager -- a USB and
#: OS-install wizard, some 5,800 lines of device probing and `.iso`
#: streaming, whose history has never been anything else. It was named
#: here because the id reads like an image editor, and the palette search
#: below asked for "image", which matches "Disk Imager" -- so the beat
#: opened an install wizard, put it in the strip beside the word
#: processor, and captioned it as the image editor. It succeeded
#: silently, which is the worst way for this to be wrong.
IMAGE_EDITOR = "dev.qos.photos"

#: The applications the workspace carries, left to right along the strip.
#:
#: The order is load-bearing rather than a preference, because a strip
#: scrolls: only neighbours are ever in frame together, so which pairs
#: this film can show is decided here and nowhere else. Files sits next
#: to the word processor because a photograph is dragged from one into
#: the other and a drag that starts off the edge of the screen is not a
#: drag anybody can see; the image editor sits next to the word
#: processor because the two of them end up sharing one window.
#:
#: The first two are opened by beats that are about opening them — Files
#: on an empty desktop, the word processor fetched onto a machine that
#: did not have it. `STRIP_EXTRAS` is the rest, opened together off
#: camera, and adding a fifth application to this film is adding its id
#: to that tuple.
STRIP_EXTRAS = (IMAGE_EDITOR, CHAT_APP)

#: What to type into the palette when an app's own id is not in the
#: catalogue an empty query builds — see `Session.launch`. Only needed
#: for the apps opened by id here; the rest name themselves.
STRIP_SEARCH = {
    # "photos", not "image": "image" also matches the Disk Imager, and a
    # search that can return the wrong application is a search this film
    # cannot rely on.
    IMAGE_EDITOR: "photos",
    CHAT_APP: "chat",
}

# Machine state, deliberately *not* isolated per session: without these
# two a session enumerates zero applications and every launch fails with
# "no bundle was found". Documents and identities stay per-session; the
# installed-app store and the runtime bundle are the machine's.
# Where the three sessions' home directories live.
#
# Deliberately *not* under `--out`. The run directory is inside the
# checkout, and Files draws the path it is showing: a home under
# `target/` puts the worktree, `.claude` and the branch name on screen
# in the first shot of the film. Every component of this one reads as a
# machine's own, and the trail ends `… › home › quinten`.
#
# It is still throwaway state — wiped and rebuilt per run, like the run
# directory — just somewhere that does not name the tooling.
#
# It cannot be `/home/<user>`, which is what would render perfectly:
# that is the real user's home and is not ours to write into. See the
# note in `stage_home`, and `docs/demo-todo.md` for the app-side fix
# that would make the path stop mattering.
HOMES_ROOT = "/var/tmp/qos/home"

# The workspace the room carries, and the one directory the photograph
# lives in.
#
# **Why this is not a home.** The film's central gesture is xaver, on a
# machine that hosts nothing, picking up an object and handing it to an
# agent. If that object sits in a home, then either he reaches into
# somebody else's — which the sandbox refuses, correctly, because a home
# holds this user's signing seed and every other app's per-room state —
# or he reaches into his own, in which case he is holding *his copy* and
# the beat is two files that happen to share a name. Staging it in
# `~/Pictures` was quietly making the film argue against its own premise.
#
# So it goes where the project says collaborative files go. `fs/` mounts
# the real thing at `/var/workspace` and the file manager knows that path
# by name (`file_explorer::model::WORKSPACE_ROOT`) — it lists such a
# directory as a first-class location and reads per-file hydration state
# off it. `QOS_WORKSPACE_ROOTS` is that constant's documented override,
# "for tests and alternate mounts", and this is an alternate mount.
#
# **The honest limit, stated here because it is easy to overclaim.** The
# FUSE driver is not running — no demo session has ever started it (see
# `docs/demo-todo.md`). So this is one directory on one host that every
# session opens, rather than a projection hydrating over iroh. What that
# still proves, and proves properly, is the half the beat is actually
# about: the *capability* travels with the room. The owner grants this
# path on the room, `WorkspaceTreeReplica` replicates the room's
# capability list to whoever was admitted to it, and the guest's file
# manager is launched with the room's grants — so he reaches the object
# by being in the room and not by being handed anyone's home. What it
# does not prove is byte-level P2P hydration, and nothing here says it
# does.
WORKSPACE_ROOT = "/var/tmp/qos/workspace"

# The photograph beat 8 drags into the document, by the name it wears on
# screen. Named once because four places have to agree about it: the
# staging that writes it, the assertion that it is there, the row the
# press has to land on, and the chip the compositor paints at the cursor
# while it crosses — that last one reads the name out of the `text/uri-list`
# the source app is offering, so what is on camera is the file's real name
# rather than anything this script told it to say.
PHOTO_NAME = "harbour.png"

MACHINE_APPS = os.path.expanduser("~/.local/share/qos-apps")
RUNTIME_BUNDLES = os.path.expanduser("~/.local/share/qos-runtime")
# A registry holding the word processor built from *this* tree. The
# machine's installed payload is a static binary from an earlier build
# and cannot exercise anything that has changed in `ui` since, so a run
# against it would be filming last month's app.
LIVE_APPS = os.path.join(TARGET, "qos-apps-live")

#: What the agent is called: the name it announces on its `Hello`, the
#: prefix an instruction is addressed with, and the subject a grant names.
#:
#: One constant for all three because they have to agree exactly.
#: `agent_opens::decide` builds `Actor::agent(handle)` out of the announced
#: string and matches it literally, so a grant written `agent:claude` for an
#: agent running as `--name Claude` names a *different principal* and every
#: open is refused with "has not been given leave". Written as two literals
#: this drifted, and the failure it produces is the worst kind: the grant
#: appears to be made, and the window simply never appears.
AGENT_NAME = "Claude"

# The capability principal quinten grants in beat 3. An agent is not a
# peer: it is a subject in the permission model, and the grant is scoped
# to one app rather than to the workspace.
AGENT_SUBJECT = f"agent:{AGENT_NAME}"
AGENT_SCOPE = f"app:{WORD_PROCESSOR}"

#: Every app the agent is given leave to work in, as separate grants.
#:
#: Two, because the film asks for two: it is handed a photograph and told
#: to square it and put it in the open document, which is an operation in
#: the image editor and an operation in the word processor. Leave to work
#: in an app is also leave to open it, so `photos` covers both opening the
#: editor and cropping in it.
#:
#: Separate grants rather than one broad one, because that is the claim.
#: A grant names an application, not the workspace, so an agent handed a
#: photograph to crop cannot thereby start anything it likes -- and the
#: two entries here are exactly the two windows it touches on camera.
AGENT_SCOPES = [f"app:{WORD_PROCESSOR}", f"app:{IMAGE_EDITOR}"]

# How fast a person types, on camera.
#
# Not a speed setting — before this, runs of unshifted characters went
# out in a single `type` call, so a sentence arrived in two or three
# instant bursts. The viewer saw text appear without seeing it typed,
# which is the one thing the beat is for.
#
# 144 ms a character is about sixty words a minute — sixty in the usual
# sense of five characters to a word, so three hundred characters a
# minute, five a second.
#
# This said 55 ms and claimed the same figure, which was not close: the
# per-character delay is only part of an interval, and once the pauses
# below and the jitter factor are counted, 55 ms types at a hundred and
# twenty-four words a minute. Twice the intended speed, asserted in a
# comment and never worked out. The arithmetic is
# `(chars * pace + sum(pauses)) * mean(jitter)`, and it is worth
# redoing here rather than adjusting by eye if any of the three moves.
TYPING_PACE = 0.144

# Where hands actually slow down. A person does not type at one rate —
# they pause fractionally after a word and longer after a sentence.
TYPING_PAUSE = {
    " ": 0.085,
    ",": 0.16,
    ".": 0.22,
    ":": 0.2,
    "\n": 0.3,
}

# And no two keystrokes take the same time. An exactly even cadence is
# the tell — it is the one thing real typing never does, and at 18 keys
# a second the eye picks it out as a machine even when the speed is
# right. Each interval is scaled by a factor drawn around 1.0, so the
# rhythm wanders the way a hand does.
#
# Seeded, not arbitrary: two runs of the film type identically, which is
# what lets a frame from one run be compared against a frame from
# another. The spread is deliberately asymmetric — a hand has a floor it
# cannot go below and a long tail of hesitations above it, so the
# distribution is skewed rather than centred.
TYPING_JITTER = (0.72, 1.45)
TYPING_SEED = 0x9E3779B9


def _typing_jitter(who: str):
    """An endless run of interval multipliers for `who`'s hands.

    Its own `random.Random`, seeded from the name, rather than the module
    generator: two sessions type at once in this film, and drawing from a
    shared generator would make each person's rhythm depend on how their
    keystrokes happened to interleave with the other's — so a rerun that
    scheduled them differently would type differently.

    `triangular` rather than `uniform` because a hand is not uniform: it
    has a floor it cannot beat and a tail of hesitations above it. The
    mode sits below the midpoint, so most keys land near the base pace
    and the occasional one drags.
    """
    lo, hi = TYPING_JITTER
    rng = random.Random(TYPING_SEED ^ hash(who) & 0xFFFF_FFFF)
    while True:
        yield rng.triangular(lo, hi, lo + (hi - lo) * 0.35)

# What quinten types into the document, and what xaver's session is then
# asked whether it holds.
# What quinten writes in beat 7 — and it is the *same* writing the agent
# is later asked about, rather than a separate sentence typed to prove
# crossing and then abandoned.
#
# It used to be "the same document on both machines", which said what
# the beat was demonstrating instead of being a thing a person would
# write. Beat 9 then typed a second, unrelated block — the event notes —
# so the film contained two acts of writing, the first of which existed
# only to be checked. At sixty words a minute that is eight seconds of
# somebody typing a caption about the demo they are in.
#
# Now there is one document. He writes his notes; his colleague receives
# them; later he asks the agent about them. The prose and the bullets
# disagree with each other on purpose — see the note above
# `AGENT_INSTRUCTION`.
#
# Shorter than it reads, on purpose. Typed at a true sixty words a
# minute this is thirteen seconds; the longer version it replaced was
# twenty-two, which is a third of the whole film spent watching one
# person type. The cut came out of the wording and not out of the pace,
# because the pace is the part a viewer can tell is faked.
#
# `seven` against `18:00` is the disagreement the agent is expected to
# notice — the one thing in here that is not just filler to be
# reformatted, and the reason the instruction in beat 9 is worth giving.
TYPED_TEXT = ("Doors at seven, room seats 240.\n\n"
              "- Doors: 18:00\nSound check: 18:30\n")

#: The distinctive fragment the crossing is checked on. Short, and
#: nothing else in the document contains it.
TYPED_PROOF = "room seats 240"

# Checked here rather than discovered as a crossing that fails against a
# document which is in fact perfectly correct. Rewording `TYPED_TEXT`
# and leaving this behind is a one-line change that breaks a beat
# somewhere else entirely, and it has already happened once.
assert TYPED_PROOF in TYPED_TEXT, (
    f"the crossing is checked on {TYPED_PROOF!r}, which is not in the "
    f"text anyone types: {TYPED_TEXT!r}")

# What the live take types, and all of it.
#
# That beat proves one thing -- the document is shared, so what one
# person writes is on the other's screen a moment later -- and a few
# words prove it exactly as well as a page does.
#
# It used to be `AGENT_PROSE + AGENT_BULLETS` and a line addressed to
# the agent by name: prose, a run of bullets and an instruction, because
# the agent read its instruction *out of the document*. It does not any
# more -- what it answers is a remark made on the photograph itself
# (`CROP_INSTRUCTION`) -- so the notes were left carrying nothing but
# their own length, and their length was thirteen seconds of typing in a
# film with a hundred and ten to spend.
SHARED_TEXT = "Some shared text\n"

#: The fragment the crossing is checked on, asked of the other machine's
#: own app. Short, and nothing else in that document contains it.
SHARED_PROOF = "Some shared text"

assert SHARED_PROOF in SHARED_TEXT, (
    f"the crossing is checked on {SHARED_PROOF!r}, which is not in the "
    f"text the live take types: {SHARED_TEXT!r}")

# Characters a US layout only produces with shift held, and the key that
# carries each one. `Session.type_prose` presses a real Shift across
# these; without it a client resolves the bare keycode and puts the
# character *below* the one that was asked for on screen.
# Letters are deliberately absent.
#
# They used to be here, mapping "C" to a Shift-held "c", because a
# client resolves a bare keycode through xkb and a keycode carries no
# case — so a scripted capital arrived lowercase in a document.
#
# The compositor holds Shift for an ASCII capital itself now
# (`test_control.rs`), which fixed that at the source and turned this
# into the opposite bug: chrome does not resolve keycodes, it reads the
# event's logical key. Sending a Shift-held "c" therefore put a
# lowercase letter into the invite composer, and the card that names the
# person an invitation is for read "claude".
#
# What remains is the punctuation, which genuinely has no keycode of its
# own and can only be reached through its unshifted twin.
SHIFTED_KEYS = {}
SHIFTED_KEYS.update({
    "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6",
    "&": "7", "*": "8", "(": "9", ")": "0", "_": "-", "+": "=",
    "{": "[", "}": "]", "|": "\\", ":": ";", '"': "'",
    "<": ",", ">": ".", "?": "/", "~": "`",
})

# Beat 9. The agent reads its instruction out of the document itself —
# a paragraph addressed to it by name (`apps/agent/src/plan.rs`) — and
# turns the run of bullets immediately above into a table. So the list
# has to be there first, and it has to be worth reading: the failure
# this replaces produced a table with nothing in it, which a transcript
# asking only "is there a table" cannot tell from success.
# `- ` at the start of a paragraph autoformats into a list item
# (`word_processor.rs:2160`), and Enter continues the list, so one
# prefix makes the whole run. It has to *be* a run of `list_item`
# blocks: the agent converts those and nothing else
# (`plan.rs::bullets_above`), and a run of plain paragraphs is what
# produced a table with empty cells. The `: ` splits each line into two
# columns (`plan.rs::split_cells`).
#
# The prose above the bullets in `TYPED_TEXT` disagrees with them,
# deliberately. "Doors at seven" against "Doors: 18:00" is a
# contradiction a person would catch, and the sound check at 18:30
# falling *after* doors is an ordering that cannot happen. Both are
# there so the agent has something to notice rather than something to
# reformat — and without a real disagreement to point at, the model
# declines to raise one about half the time, which is the correct
# behaviour and a poor beat.
#
# A second copy of those notes lived here, for the live take to type.
# That take no longer types notes at all — see `SHARED_TEXT` — and a
# duplicate of a document's content that only one film uses is how the
# two drifted apart the last time.
# Asking for a run sheet rather than a table. A table is a shape and the
# app has a button for it; a run sheet is a judgement about which of
# these lines is a scheduled item, what order they belong in, and
# whether they are consistent — which is the difference between a
# formatting command and something worth filming.
#
# This is the *nine-beat* film's instruction, and it belongs to it: the
# prose and bullets above are what it acts on, and the readback after it
# asserts a table with those bullets' own words in its cells. It is not
# the sentence the live take asks for — see [`CROP_INSTRUCTION`].
AGENT_INSTRUCTION = "Claude: make this a run sheet\n"
# One cell's text, looked for in the document afterwards. If the table
# came out empty this is gone, and the claim fails instead of passing on
# the existence of a grid.
AGENT_BULLET_PROOF = "Sound check"

#: The sentence the film turns on, written once and referenced everywhere.
#:
#: One instruction, naming no application, no operation and no file. It
#: is handed to an agent by somebody who is holding a *photograph*, and
#: what makes it the whole argument is that answering it takes two
#: applications the agent owns neither of: open the image editor, crop
#: by address, and put the result in a document opened by somebody else
#: on another machine.
#:
#: Three copies of this used to disagree — a run sheet here, a table in
#: the live rig, a crop in the script and in the headless proof — and
#: the rig picked the run-sheet one and pointed it at the photograph. So
#: the person right-clicked a picture and asked for a run sheet, and the
#: readback then asserted the run sheet's outcome. Every one of those
#: reads correctly on its own; only together are they a different film.
#:
#: `Claude:` and not `@claude`. Both address the agent, but by different
#: routes: a mention is a structured range the room resolved and only
#: exists where something mints one, while the name prefix is matched on
#: the text itself (`Instruction::addressed`, `apps/agent/src/plan.rs`)
#: and therefore works in a chat message and in a document paragraph
#: alike. The line a person is told to type has to be the one that works
#: wherever they type it.
CROP_INSTRUCTION = "Claude: make this square and put it in the open document"

# How long to wait for a change to cross between sessions.
SYNC_TIMEOUT_S = 25.0
# A document crossing is a longer chain than a room-index entry: publish,
# gossip, drain on the far side, apply, paint. Given its own budget so a
# slow hop is not reported as a failure to converge.
DOC_SYNC_TIMEOUT_S = 60.0
# An app launch is a sandbox spawn, a Wayland connection, a toolkit
# starting up and a first frame. Generous because none of that is qOS
# being slow when the machine is busy: at a load average near a hundred
# this has been measured past two minutes, and a launch reported as
# failed because the harness gave up is the worst kind of wrong answer.
LAUNCH_TIMEOUT_S = 240.0
# What one `snapshot` is allowed to block its session for. Deliberately
# far above what it costs: the op re-renders and encodes on the
# compositor's own loop, and on a machine that is busy compiling
# something else an unoptimised build can sit there for a minute. A tight
# timeout here reports a contended box as a broken compositor.
SNAPSHOT_TIMEOUT_S = 240.0


# --------------------------------------------------------------------
# control socket
# --------------------------------------------------------------------


def call(sock_path: str, payload: dict, timeout: float = 30.0) -> dict:
    """Send one command, read one reply."""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(sock_path)
    try:
        s.sendall(json.dumps(payload).encode() + b"\n")
        buf = b""
        while b"\n" not in buf:
            chunk = s.recv(1 << 20)
            if not chunk:
                raise RuntimeError("compositor closed the connection mid-reply")
            buf += chunk
        return json.loads(buf.split(b"\n", 1)[0])
    finally:
        s.close()


def ok(resp: dict) -> dict:
    if "err" in resp:
        raise RuntimeError(f"compositor refused: {resp['err']}")
    return resp


#: The one language the film is shot in.
#:
#: Not a preference — a fact the frames depend on. Only some strings have
#: a German catalogue, so a session that drifts off this does not come
#: out German, it comes out *half* German: an English strip of tabs over
#: a German toolbar, or one palette row reading "Notebook holen und
#: öffnen — Opens beside · Office · von qOS main". That is what broken
#: software looks like, and it is the kind of thing nobody spots until it
#: is on camera.
DEMO_LOCALE = "en_US.UTF-8"


def pin_gpu(env: dict) -> dict:
    """Keep every process this run starts off a sleeping discrete GPU.

    This machine has two: an AMD iGPU that drives the screen, and an
    NVIDIA dGPU that spends its life in D3Cold under runtime power
    management. Creating a `wgpu::Instance` -- which vello's
    `RenderContext` does, once per compositor and once per application --
    loads every Vulkan ICD installed, `nvidia_icd.x86_64.json` included,
    and enumerating through that one reaches across PCIe to a device that
    is powered down.

    Twice, doing that froze the machine hard enough that nothing was
    written down. Both crashed boots end mid-sentence with no GPU reset,
    no hung task, no OOM and no panic -- the signature of a wedged bus
    transaction rather than a driver fault, because a CPU stuck on an
    unanswered PCIe read never reaches the code that would log it. In the
    first, `cardwired` recorded the dGPU going D0 at 00:55:21 and back to
    D3Cold at 00:55:48, and the journal stops. Opening a photograph
    starts an application, and starting one is a fresh instance, so the
    gesture that triggered it is the gesture that enumerates.

    The launcher already does this for applications it sandboxes -- a
    tmpfs over `/dev/dri` and one node bound back, `VK_DRIVER_FILES` set
    from the compositor's own choice. Compositors are not sandboxed, so
    nothing was doing it for them, and an application that inherits a
    clean environment inherits the problem with it.

    Pinned to the ICD of whichever card actually has a display attached,
    rather than to a name: the point is "the GPU the screen is on", and
    reading it beats assuming it. Set `QOS_DEMO_GPU=any` to opt out.
    """
    if env.get("QOS_DEMO_GPU") == "any":
        return env

    # Which card is driving something. A connected connector is the
    # honest test -- a headless card can be the lower-numbered one.
    driver = None
    try:
        for card in sorted(glob.glob("/sys/class/drm/card[0-9]*")):
            if "-" in os.path.basename(card):
                continue
            connected = any(
                open(st).read().strip() == "connected"
                for st in glob.glob(f"{card}-*/status")
                if os.path.exists(st))
            if not connected:
                continue
            link = os.path.realpath(os.path.join(card, "device", "driver"))
            driver = os.path.basename(link)
            break
    except Exception:  # noqa: BLE001
        driver = None

    icd = {"amdgpu": "radeon_icd", "i915": "intel_icd",
           "xe": "intel_icd"}.get(driver or "")
    if not icd:
        # Nothing certain to pin to. Left alone deliberately: a wrong pin
        # is a run that cannot render at all, which is worse than the
        # risk it removes.
        return env

    path = f"/usr/share/vulkan/icd.d/{icd}.x86_64.json"
    if os.path.exists(path):
        # Both spellings: a loader older than 1.3.207 ignores the first.
        env["VK_DRIVER_FILES"] = path
        env["VK_ICD_FILENAMES"] = path
    egl = "/usr/share/glvnd/egl_vendor.d/50_mesa.json"
    if os.path.exists(egl):
        env["__EGL_VENDOR_LIBRARY_FILENAMES"] = egl
    return env


def pin_locale(env: dict) -> dict:
    """Pin every variable that can choose a language, and prove it took.

    A real qOS install has the OS set this and cannot drift. A session
    nested on somebody's desktop inherits whatever they use, and the
    machine filming this is German — so the harness pins it rather than
    hoping.

    Three readers, three different orders, which is why this sets more
    than one variable: the shell and the word processor read `LC_ALL`,
    `LANGUAGE`, `LANG` (`shell/src/shell/i18n.rs`,
    `apps/word-processor/src/i18n.rs`), and `.desktop` name lookup reads
    `LC_ALL`, `LC_MESSAGES`, `LANG`
    (`crates/system/desktop-entry/src/locale.rs`). Pinning only the two
    the apps read leaves app *names* free to come back in another
    language.

    `LANGUAGE` is removed rather than set: it outranks `LANG`, and it is
    a colon-separated *list* rather than a locale, so there is no value
    of it that means this locale and nothing else.
    """
    env["LC_ALL"] = DEMO_LOCALE
    env["LANG"] = DEMO_LOCALE
    env["LC_MESSAGES"] = DEMO_LOCALE
    env.pop("LANGUAGE", None)
    # Every remaining category, so `LC_TIME=de_DE` cannot put a German
    # date on screen under an English shell either. This is what `LC_ALL`
    # already means to a POSIX `setlocale`; making it explicit is what
    # lets `assert_locale_pinned` be a blanket check rather than a list
    # of the variables somebody remembered.
    for var in [k for k in env if k.startswith("LC_")]:
        env[var] = DEMO_LOCALE
    return env


def pin_keyboard(env: dict) -> dict:
    """Give a nested compositor the keyboard the person is actually
    typing on -- and only the session a person types into.

    Called per session rather than from `pin_locale`, because the two
    kinds of session want opposite things and pinning both broke the
    scripted one silently.

    A person's session must match their hardware or every key is wrong.
    A session the *rig* types into must not: `type` emits one evdev
    keycode per character from a table in
    `client_input::evdev_from_char` that is US, and the client resolves
    that keycode through whatever keymap the seat is carrying. Under a
    German map, keycode 39 with shift is not `:` but `Ö` -- so
    `Claude: make this square` arrived in the document as
    `ClaudeÖ make this square`, and the agent, which matches the literal
    prefix `claude:`, never saw an instruction at all. Nothing reported
    an error; the text simply said something else.

    The real repair is for `type` to ask the active keymap which keycode
    and level produce a requested character. Until then, the session a
    person drives gets their layout and the sessions a script drives keep
    the one the table was written against.

    `XkbConfig::default()` compiles from `XKB_DEFAULT_RULES/_MODEL/
    _LAYOUT/_VARIANT/_OPTIONS`, and with none of them set xkbcommon falls
    back to a US keymap. On a real qOS install the installer writes those
    into `/etc/environment` and PAM exports them into the session, so the
    layout somebody chose is the one that gets compiled.

    Nested on somebody's desktop, nothing writes them: GNOME configures
    the layout through its own settings rather than the environment. So a
    German keyboard drove a US compositor — every punctuation key wrong,
    `y` and `z` swapped, and no way to type the address of anything.

    `localectl` is the system's own answer, and the same one the
    installer writes. Failing to read it leaves the environment alone
    rather than guessing: a wrong layout asserted is worse than the
    fallback, because the fallback is at least the documented default.
    """
    try:
        out = subprocess.run(["localectl", "status"], capture_output=True,
                             text=True, timeout=5).stdout
    except Exception:
        return env
    fields = {
        "X11 Layout": "XKB_DEFAULT_LAYOUT",
        "X11 Model": "XKB_DEFAULT_MODEL",
        "X11 Variant": "XKB_DEFAULT_VARIANT",
        "X11 Options": "XKB_DEFAULT_OPTIONS",
    }
    for line in out.splitlines():
        label, _, value = line.strip().partition(":")
        var = fields.get(label.strip())
        if var and value.strip() and value.strip() != "n/a":
            env[var] = value.strip()
    return env


def assert_locale_pinned(env: dict, where: str) -> None:
    """Raise unless `env` starts a session in the language the film pins.

    Called at the spawn itself rather than next to `pin_locale`, so that
    it is checking what the process is actually handed rather than what
    the line above it just wrote. Delete the `pin_locale` call, add a
    spawn path that forgets it, or let something downstream put a
    category back, and this is what fails — instead of a frame coming out
    half German and nobody noticing until it is on camera. Nothing else
    in this harness looks at a word on the screen: every assertion in
    this file passed for the whole time the bug was live.

    The check is every `LC_*` / `LANG*` in the environment, not a list of
    the three readers known today, so a reader added later is covered
    without anyone remembering to come back here.
    """
    leaked = sorted(
        f"{k}={env[k]!r}"
        for k in env
        if (k.startswith("LC_") or k.startswith("LANG")) and env[k] != DEMO_LOCALE
    )
    if leaked:
        raise RuntimeError(
            f"refusing to start {where} under a locale the film has not "
            "pinned: " + ", ".join(leaked) + f". Every LC_*/LANG* must be "
            f"{DEMO_LOCALE!r} (LANGUAGE unset) — see pin_locale. A session "
            "that drifts off this films in two languages at once."
        )


class Session:
    """One running compositor, addressed by its control socket."""

    #: Whether this session gossips its presence to peers on the network.
    #: On for the nine-beat film, whose whole subject is other people;
    #: worth turning off for a single-session take, because mdns finds
    #: every other qOS session on the machine — another agent's harness,
    #: a demo somebody left running — and paints a cursor and a name pill
    #: for it, over the document, on camera.
    peer_sync: bool = True

    def __init__(self, name: str, user: str, run_dir: str, sock: str, binary: str,
                 size: str, runtime_dir: str, home: str, apps_dir: str,
                 tools: dict[str, str] | None = None):
        self.name = name
        self.user = user
        # This session's own typing rhythm, seeded from its name so each
        # person types a little differently and each of them types the
        # same way every run.
        self._typing_jitter = _typing_jitter(name)
        self.dir = run_dir
        self.sock = sock
        self.binary = binary
        self.size = size
        # This session's own home. The file chooser and the palette's file
        # results read it, so pointing it at a staged directory is what
        # keeps the demo off whoever's machine is filming it.
        self.home = home
        # The installed-app store this session sees. Per-session because
        # "this machine does not have the word processor" has to be a fact
        # about a real store rather than an assertion about one.
        self.apps_dir = apps_dir
        # Paths to the packaging chain, when it has been built. Absent is
        # a supported state: the beat that needs it says so and moves on.
        self.tools = tools or {}
        # Each session needs its own XDG_RUNTIME_DIR: the compositor
        # binds its collab broker at <runtime>/qos/services/collab.sock,
        # and two sessions sharing one runtime dir are two sessions
        # contending for one socket. Keep it short — sun_path is ~108 bytes.
        self.runtime_dir = runtime_dir
        self.proc: subprocess.Popen | None = None
        self.log_path = os.path.join(run_dir, "compositor.log")
        self._log = None
        # Every synthetic-input command this session has been sent. The
        # agent's whole claim is that it never uses one, and a counter is
        # a mechanical proof of that where a promise is not.
        self.input_ops = 0
        # Where this session's pointer is, as far as this script knows.
        # `None` means it has never been moved, which is a real state: a
        # session nobody has pointed at has no pointer on its screen.
        # Mirrored here rather than asked for over the wire because the
        # approach below plans a path and a round-trip per step would be
        # most of what the path costs.
        self._pointer: tuple[float, float] | None = None

    @property
    def collab_sock(self) -> str:
        return os.path.join(self.runtime_dir, "qos", "services", "collab.sock")

    @property
    def collab_json_sock(self) -> str:
        """The broker's legacy JSON listener.

        One broker serves both sockets (`collab_broker::spawn_full`, wired
        at `native.rs:1788`), so a JSON connection announcing the same
        `(app, room, record)` address a packaged app announces lands in
        that app's channel — even though the app itself reached the same
        broker over capnp. That is what lets this script ask a *running
        app* for the document it holds without being able to see its
        screen.
        """
        return os.path.join(self.runtime_dir, "qos", "services",
                            "collab-json.sock")

    # -- lifecycle ---------------------------------------------------

    def start(self) -> None:
        os.makedirs(os.path.join(self.dir, "config"), exist_ok=True)
        os.makedirs(os.path.join(self.dir, "keys"), exist_ok=True)
        os.makedirs(self.runtime_dir, mode=0o700, exist_ok=True)
        env = dict(os.environ)
        # No display, and nothing inherited that would make the three
        # sessions share state.
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("QUICK_COLLAB_SOCK", None)
        env.pop("QUICK_COLLAB_CAPNP_SOCK", None)
        # One language on screen — see `pin_locale`, which is applied to
        # the finished environment below rather than here, so that it
        # validates what the compositor is actually handed. bwrap does
        # not clear the environment, so whatever is left here is what the
        # sandboxed apps inherit too.
        env.update(
            XDG_RUNTIME_DIR=self.runtime_dir,
            XDG_CONFIG_HOME=os.path.join(self.dir, "config"),
            XDG_DATA_HOME=os.path.join(self.dir, "data"),
            XDG_CACHE_HOME=os.path.join(self.dir, "cache"),
            XDG_STATE_HOME=os.path.join(self.dir, "state"),
            # Named outright rather than left to `dirs::config_dir()`.
            # That helper ignores a *relative* XDG_CONFIG_HOME, so a run
            # started with a relative --out gets no isolation from it at
            # all and persists into the real ~/.config/qos/state.json —
            # three demo sessions writing workspaces, owned by
            # identities nobody at this machine can administer, into
            # somebody's live desktop. `--out` is absolutised for the
            # same reason; this is the belt to that pair of braces.
            QOS_STATE_PATH=os.path.join(self.dir, "config", "state.json"),
            QOS_USER_ID=self.user,
            QOS_PEER_KEY_PATH=os.path.join(self.dir, "keys", "peer-key"),
            QOS_FS_ACL_PATH=os.path.join(self.dir, "config", "fs-capabilities.json"),
            QOS_TEST_CONTROL=self.sock,
            NIRIOS_HEADLESS_SIZE=self.size,
            # `info`, plus the three targets that narrate what happens
            # inside a client window. Two beats aim a press at a file row
            # inside one, and a client's widgets are not in the
            # compositor's registry — so both *where the row is* and
            # *which row was grabbed* have to come from the app. They do:
            #
            #   ui::resources        "resources laid out" surface=… at={…}
            #   file_explorer::drag  "drag armed"  path=… at=Point{…}
            #   file_explorer::drag  "drag begins" path=…
            #   shell::selection     "client started a drag"
            #
            # All `debug!`, so at plain `info` the beat would be aiming
            # blind and could only report on the outcome. An explicit
            # RUST_LOG in the environment still wins, because somebody
            # debugging this wants their own filter.
            RUST_LOG=env.get(
                "RUST_LOG",
                "info,shell::selection=debug,file_explorer::drag=debug,"
                "ui::resources=debug"),
            # A home of this session's own, staged with the demo's files.
            HOME=self.home,
            # The same directory again, under the name `ui::sysdirs`
            # looks for. `HOME` alone leaves two gaps. A surface that
            # resolves through `QOS_USER_HOME` — the file chooser's
            # scope, the palette's file provider — would otherwise fall
            # back to `dirs::home_dir()`, and `UserDirs::from_system`
            # would keep reading the *machine's* `user-dirs.dirs`, which
            # is how a German desktop put `Bilder` and `Dokumente` beside
            # an English `Documents` in the same frame. With the override
            # set, the standard folders resolve underneath this home and
            # nothing consults the real account's configuration.
            QOS_USER_HOME=self.home,
            # The room's shared workspace, which every session points at
            # the same directory — see `WORKSPACE_ROOT`. This is the
            # documented override of the file manager's `/var/workspace`
            # constant, and it is what makes that directory a *workspace*
            # to the app rather than a folder it happens to be showing:
            # it goes in the places sidebar, its entries are read for
            # hydration state, and a cold start opens on it.
            #
            # Set on the compositor, not on the apps. The launcher clears
            # the environment and rebuilds it, and passes this on only
            # after narrowing it to the roots a capability has actually
            # bound (`sandbox::push_workspace_roots`) — so a session that
            # was granted the workspace sees it and one that was not
            # cannot even name it.
            QOS_WORKSPACE_ROOTS=WORKSPACE_ROOT,
            # The two stores an app launch cannot do without. The install
            # root is this session's; the runtime bundle is the machine's,
            # because every packaged app pins it and none of them ship it.
            #
            # Naming the install root — with no `QOS_APPS_SYSTEM_DIR`
            # beside it — is also what makes this a *scoped* session:
            # `launcher::registry::store_is_scoped_to_this_session`. Its
            # applications are the ones composed into `apps_dir` and
            # nothing else, so the palette does not additionally
            # enumerate the machine's own `.desktop` files
            # (`shell::host_apps::available_to_this_session`). Without
            # that, a take of a film about a new operating system spent
            # twelve seconds showing the operator's installed Steam,
            # Nextcloud and Transmission under a "Host Apps" heading,
            # package ids and all.
            QOS_APPS_DATA_DIR=self.apps_dir,
            QOS_RUNTIME_BUNDLE_DIR=RUNTIME_BUNDLES,
            # Where the catalogue looks for registries, and where it caches
            # what it fetches. Per-session, so an install in one session is
            # not quietly an install in another.
            QOS_STORE_CONFIG_DIR=os.path.join(self.dir, "store-config"),
            QOS_STORE_CACHE_DIR=os.path.join(self.dir, "store-cache"),
            # Makes a `ui` app name the record it checked out, on the
            # session log. Beat 7 depends on two apps announcing the
            # *same* address, and the only address two sessions can share
            # here is the untitled one: a record identified by a path
            # would be a different id on each side, because each session
            # has a home of its own. If that beat ever reports that
            # nobody answered, this line is what says whether the two
            # apps were even listening on the same channel.
            QUICK_COLLAB_DEBUG="1",
        )
        # The packaging chain, when it is on disk. Passed by path rather
        # than found on PATH so the run measures the binaries built for it.
        for var, path in self.tools.items():
            env[var] = path
        # Last, on the finished environment: everything above may add to
        # it. The matching check sits at the spawn itself.
        pin_locale(env)
        # Same choke point, same reason: everything this run starts
        # inherits it, and an application enumerating a sleeping GPU is
        # what took the machine down twice.
        pin_gpu(env)
        os.makedirs(os.path.join(self.dir, "store-config"), exist_ok=True)
        os.makedirs(os.path.join(self.dir, "store-cache"), exist_ok=True)
        self._log = open(self.log_path, "w")
        argv = [self.binary, "--mode", "native", "--backend", "headless"]
        if self.peer_sync:
            argv.append("--enable-peer-sync")
        # Every demo compositor is spawned from here — the windowed
        # session the camera sees included, because its subclass overrides
        # the spawn and not this. So this one line is the choke point at
        # which "a demo session would start in the wrong language" stops
        # being possible to do quietly.
        assert_locale_pinned(env, f"the {self.name!r} session")
        self.proc = subprocess.Popen(
            argv,
            cwd=REPO, env=env, stdout=self._log, stderr=subprocess.STDOUT,
            # `--keep` genuinely keeps: without a session of its own a
            # child dies with this process's group the moment the script
            # returns, which made "left running" a message printed about
            # three processes that were already gone.
            #
            # Every other run asks the kernel to kill the child when this
            # process dies. `atexit` cannot cover that: a harness that
            # hangs is stopped with SIGKILL, and nothing in userspace runs
            # afterwards. Eleven compositors were once found still
            # spinning at 80% CPU each, the oldest 47 minutes after the
            # run that spawned it — most of a machine, held by runs that
            # had already been abandoned.
            start_new_session=KEEP_SESSIONS,
            preexec_fn=die_with_parent(enabled=not KEEP_SESSIONS),
        )
        # The kernel's PDEATHSIG covers the case nothing in userspace can
        # (this process being SIGKILLed). The exit sweep covers the one it
        # does not: a child that outlived an intermediate parent and was
        # reparented. A `--keep` run is asking for survivors and gets no
        # sweep.
        if not KEEP_SESSIONS:
            track(self.proc)

    def wait_ready(self, timeout: float = 90.0) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc and self.proc.poll() is not None:
                raise RuntimeError(
                    f"{self.name} exited before binding its control socket; "
                    f"see {self.log_path}")
            if os.path.exists(self.sock):
                # Bound is not the same as answering.
                try:
                    self.state()
                    return
                except (OSError, RuntimeError):
                    pass
            time.sleep(0.2)
        # A compositor that is still running but never bound the socket is
        # almost always a `--release` build: `compositor::dev_env` gates
        # QOS_TEST_CONTROL on `cfg!(debug_assertions)`, so a shipped binary
        # has no such code and logs nothing about it. Say so, rather than
        # leaving the next person to read a healthy log for an hour.
        alive = self.proc is not None and self.proc.poll() is None
        raise RuntimeError(
            f"{self.name} never answered on {self.sock}"
            + (f"\n  the process is still alive, so it started and did not bind"
               f"\n  the control socket. Is {self.binary} a --release build?"
               f"\n  QOS_TEST_CONTROL is compiled out of release builds by"
               f"\n  design; build --profile demo instead."
               if alive else f"\n  see {self.log_path}"))

    def stop(self) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self._log:
            self._log.close()
            self._log = None

    # -- reading (never counts as input) -----------------------------

    def state(self) -> dict:
        return ok(call(self.sock, {"op": "state_dump"}))["state"]

    def chrome(self) -> dict:
        return self.state()["chrome"]

    def tree(self) -> dict:
        return self.state()["workspace_tree"]

    def active_workspace(self) -> dict:
        tree = self.tree()
        active = tree["active"]
        for ws in tree["workspaces"]:
            if ws["id"] == active:
                return ws
        raise RuntimeError(f"{self.name}: no active workspace in the tree")

    def overlays(self) -> dict:
        return self.chrome()["overlays"]

    def palette(self) -> dict:
        return self.chrome()["palette"]

    def peers(self) -> dict:
        """The peers this session would paint, off the same `PresenceCtx`
        the overlay draws from.

        Read rather than eyeballed on purpose: "no peer arrived" and "a
        peer arrived and was not drawn" make the same picture, and a
        harness that cannot tell those apart has not shown either.
        """
        return ok(call(self.sock, {"op": "state_dump"}))["state"].get("peers") or {}

    def foreign_frames(self) -> list[dict]:
        """Per-surface frame counters for every client window.

        `frame_seq > 0` is the ground truth behind "the app opened": a
        placeholder tile looks like a window and never presents.
        """
        r = call(self.sock, {"op": "foreign_frames"})
        if "foreign_frames" in r:
            return r["foreign_frames"].get("frames", [])
        return r.get("answer", r.get("frames", [])) or []

    def groups_here(self, tree: dict | None = None) -> dict:
        """Every window group in this session's active workspace.

        Takes an already-read tree when the caller has one. These run
        inside polling loops, and a `state_dump` blocks the compositor
        it is asked of — re-reading the tree per workspace would have
        the demo competing with its own camera for every reply.
        """
        tree = tree if tree is not None else self.tree()
        out: dict[str, dict] = {}
        for ws in tree["workspaces"]:
            if ws["id"] != tree["active"]:
                continue
            for strip in ws.get("strips", []):
                for group in strip.get("window_groups", []):
                    out[str(group.get("id"))] = {
                        "active_tab": group.get("active_tab"),
                        "tabs": [t.get("title") for t in group.get("tabs", [])],
                        "tab_ids": [str(t.get("id")) for t in group.get("tabs", [])],
                        "strip": str(strip.get("id")),
                    }
        return out

    def tabs_here(self, tree: dict | None = None) -> dict:
        """Every tab in this session's active workspace, by tab id.

        Keyed on the tab rather than the group because the placement rule
        tabs a new window into the focused group when it is already
        tabbed — so a new window does not always mean a new group.
        """
        out: dict[str, dict] = {}
        for gid, g in self.groups_here(tree).items():
            for i, t in enumerate(g["tab_ids"]):
                out[t] = {"group": gid, "strip": g["strip"],
                          "title": g["tabs"][i] if i < len(g["tabs"]) else None}
        return out

    def view_state(self) -> dict:
        """Where this person is looking. Which windows exist is the
        workspace; which one is in front of you is you, and the second
        must not travel with the first."""
        tree = self.tree()
        return {
            "active": tree.get("active"),
            "active_strip": tree.get("active_strip"),
            "active_group": tree.get("active_group"),
            "active_tab_per_group": {g: v["active_tab"]
                                     for g, v in self.groups_here(tree).items()},
        }

    def targets(self) -> list[dict]:
        """The compositor's own widget registry: every addressable piece
        of chrome with its rect. A client's insides are not in here —
        this is the shell's tree, which is exactly what makes the OS file
        chooser drivable and an app's toolbar not."""
        r = call(self.sock, {"op": "list_targets"})
        for v in (r.values() if isinstance(r, dict) else []):
            if isinstance(v, list):
                return v
            if isinstance(v, dict):
                for vv in v.values():
                    if isinstance(vv, list):
                        return vv
        return []

    def named_rect(self, name: str) -> list | None:
        for t in self.targets():
            if t.get("name") == name:
                rect = t.get("rect")
                if rect and (rect[2] > 0 and rect[3] > 0):
                    return rect
        return None

    def target_names(self) -> list[str]:
        return [t.get("name", "") for t in self.targets()]

    def check_action(self, actor: str, app: str, action: str,
                     verb: str = "write") -> dict:
        """Ask the in-app action gate whether `actor` may invoke this.

        Read-only, and the production gate: this is the same check the
        broker runs, asked without performing anything.
        """
        return call(self.sock, {"op": "check_action", "actor": actor,
                                "app": app, "action": action, "verb": verb})

    def grant(self, subject: str, scope: str, verb: str) -> dict:
        """Grant a capability, through `Workspace::grant_as`.

        The authority check is the production one — it refuses a grantor
        who neither owns the room nor holds admin over it — so a refusal
        here is the model working, not the harness failing.
        """
        return call(self.sock, {"op": "grant", "subject": subject,
                                "scope": scope, "verb": verb})

    def remark(self, text: str, address: str) -> dict:
        """Put a remark in the room without the gesture that writes one.

        The gesture is a right-click and a sentence typed beside the
        object; it produces a finished remark, and the session's frame
        loop carries that into the room. This asks the session for the
        second half only, so a run can reach the room's side of the beat
        while the surface that writes one is unavailable.

        **It proves nothing about the compose surface.** The panel is not
        opened, no destination list is filled, nothing is typed and
        `submit` never runs. What it does exercise is everything a
        finished remark meets: the roster is read, the mention is
        resolved against it, the message is minted and encoded as chat's
        own action, and it goes out over the broker into the room's
        conversation.

        `address` is the whole string the app publishes the resource
        under — `files.entry:/var/tmp/qos/workspace/harbour.png` — so the
        thing named here and the thing a person would have right-clicked
        are one object rather than two that agree.

        The address is the **id**, in full, and the kind is its first
        segment repeated: `entry_reference` mints
        `ResourceRef::whole(kind, format!("{kind}:{path}"), label)`, so an
        id with the prefix stripped off addresses nothing. That is the
        one thing here worth getting wrong slowly rather than quickly —
        the agent resolves the address against what the room exports, and
        a near-miss reads as an agent declining to act.
        """
        kind, _, rest = address.partition(":")
        if not rest:
            raise ValueError(f"a resource address is kind:id, got {address!r}")
        return call(self.sock, {"op": "remark", "text": text,
                                "kind": kind, "id": address})

    # -- synthetic input, continued (all counted) --------------------

    def click_at(self, x: float, y: float) -> None:
        """Click at (x, y) the way a person does: arrive, then press.

        The pointer travels there first and the button goes down where
        it lands. Not a nicety — a click delivered with no pointer
        movement in front of it opens a panel and lights up a button
        with nothing on screen having caused it, and a recording of the
        session then reads as the machine operating itself rather than
        as somebody using it.

        The compositor plans the same approach inside its own `click_at`
        (`test_control::plan_click`), so a harness that sends the op raw
        still lands its pointer on the target. What it cannot do is
        *pace* it: every action of one command runs between two frames,
        so the whole trip happens inside a single painted frame and
        nothing photographs it. That is why the path is walked from
        here, one command per step, with the camera free to sample
        between them — and why, with no camera to sample it, the bundled
        op is sent instead and the compositor plans its own approach.
        """
        if not self._filmed():
            self.input_ops += 1
            ok(call(self.sock, {"op": "click_at", "x": x, "y": y}))
            self._pointer = (x, y)
            return
        self._approach(x, y)
        self._press_and_release()

    def click_named(self, name: str) -> dict:
        """Click the centre of a named widget, having travelled to it.

        Resolves the rect first so the approach has somewhere to go. A
        name that does not resolve is handed to the compositor's own
        `click_named`, which is the authority on that refusal and
        answers with the `unknown_target` every caller already checks
        for — and moves no pointer, so a failed lookup never leaves the
        cursor somewhere nobody aimed it.
        """
        rect = self.named_rect(name) if self._filmed() else None
        if rect is None:
            self.input_ops += 1
            resp = call(self.sock, {"op": "click_named", "name": name})
            if "err" not in resp:
                # The compositor clicked the centre of whatever it
                # resolved; that is where the pointer now is.
                found = self.named_rect(name)
                if found:
                    self._pointer = (found[0] + found[2] / 2.0,
                                     found[1] + found[3] / 2.0)
            return resp
        cx, cy = rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0
        self._approach(cx, cy)
        self._press_and_release()
        return {"ok": {"frame": 0}}

    @staticmethod
    def _filmed() -> bool:
        """Whether anything is watching this session right now."""
        return RECORDER is not None and not RECORDER.camera_is_down()

    #: Roughly how fast the pointer crosses the screen, in pixels per
    #: second. A hand throwing a mouse across a 1920px desktop takes
    #: about a second and a half; this is a little brisker than that,
    #: because a film has to keep moving and an approach nobody notices
    #: is one that has done its job.
    POINTER_SPEED_PX_S = 2800.0
    #: Shortest and longest an approach may take, whatever the distance.
    #:
    #: The cap is what bounds this in practice: `steps * step_s` puts a
    #: floor of one camera frame under every photographed position, so a
    #: long trip is paced by the camera rather than by the hand, and it
    #: piles up against whatever the cap allows. At 1.9s the walk to the
    #: Invite button read as painfully slow on film -- a viewer watching
    #: a cursor crawl is a viewer not watching the desktop. Two thirds of
    #: a second is still eased at both ends and still legible as travel.
    POINTER_TRAVEL_MIN_S = 0.16
    POINTER_TRAVEL_MAX_S = 0.65
    #: Wall-clock a step of the path is given, as a floor. The real step
    #: time is whatever the camera's frame period turns out to be — see
    #: `Recorder.frame_period` — because a step shorter than a frame is a
    #: socket round trip nobody photographs.
    POINTER_STEP_S = 0.02
    #: The furthest the pointer may travel between two *photographed*
    #: positions.
    #:
    #: This is the whole difference between a pointer that moves and one
    #: that teleports, and it is a distance rather than a duration. The
    #: step count used to come out of the trip's duration — `seconds /
    #: POINTER_STEP_S`, clamped to at most twelve — so a long approach
    #: was twelve steps however long it was, and a trip across the
    #: desktop moved the cursor 160 px at a time. A difference trail over
    #: such an approach shows four stamped cursors with clear ground
    #: between them; a hand leaves a smear.
    #:
    #: A little under the cursor's own height, so consecutive positions
    #: overlap and the eye joins them up.
    POINTER_STEP_PX = 24.0
    #: The path is eased, so its middle runs 1.5x the average rate (the
    #: peak of the smoothstep derivative). The cap has to hold at the
    #: fastest point of the trip, not on average, or the middle of every
    #: approach is the part that jumps.
    POINTER_EASE_PEAK = 1.5
    #: Ceiling on the number of positions one approach is cut into. Each
    #: is a socket round trip to the compositor being filmed, and past
    #: this the camera is the limiting factor anyway.
    POINTER_STEPS_MAX = 180
    #: How long the pointer rests on a control before the button goes
    #: down, and how long the button stays down. Both are what a finger
    #: does, and both have to outlast one captured frame or the press is
    #: a state the film never sees.
    POINTER_SETTLE_S = 0.10
    POINTER_PRESS_S = 0.13

    def _approach(self, x: float, y: float) -> None:
        """Walk the pointer to (x, y) and let it settle there.

        Eased at both ends — a hand does not start or stop at speed —
        and paced in wall-clock so the trip is several frames long
        rather than one. Only reached while something is watching; the
        callers send the bundled op otherwise.
        """
        here = self._pointer
        if here is None:
            # Nothing to travel from: a session nobody has pointed at
            # has no pointer on its screen, and the first thing it gets
            # is one appearing where the click is about to happen.
            self.pointer_move(x, y)
            self.settle(self.POINTER_SETTLE_S)
            return
        x0, y0 = here
        dx, dy = x - x0, y - y0
        distance = math.hypot(dx, dy)
        if distance < 2.0:
            self.pointer_move(x, y)
            return
        # How many positions the path is cut into: enough that no two
        # neighbours are further apart than a cursor. That is the number
        # the *picture* needs, and it is set by the distance alone.
        steps = max(3, min(self.POINTER_STEPS_MAX,
                           math.ceil(distance * self.POINTER_EASE_PEAK
                                     / self.POINTER_STEP_PX)))
        # How long each of them is held: at least one camera frame, or
        # the step is a socket round trip that never reaches the film.
        step_s = max(self.POINTER_STEP_S,
                     RECORDER.frame_period() if RECORDER is not None else 0.0)
        # And the trip takes as long as walking it properly takes. The
        # nominal duration is what a hand would spend on the distance;
        # the floor is what photographing every step costs. Capped, so a
        # camera having a bad minute cannot turn one approach into a
        # crawl — past the cap the steps go by faster than frames and the
        # smear thins out, which is the graceful end of this rather than
        # the film stopping to watch a cursor.
        seconds = min(self.POINTER_TRAVEL_MAX_S,
                      max(self.POINTER_TRAVEL_MIN_S,
                          distance / self.POINTER_SPEED_PX_S,
                          steps * step_s))
        for i in range(1, steps + 1):
            t = i / steps
            eased = t * t * (3.0 - 2.0 * t)
            self.pointer_move(x0 + dx * eased, y0 + dy * eased)
            time.sleep(seconds / steps)
        # Arrived, and visibly stopped, before anything is pressed.
        self.settle(self.POINTER_SETTLE_S)

    def _press_and_release(self) -> None:
        """Press and release where the pointer is standing.

        Two commands rather than one so the button spends real time
        down: the compositor draws a pressed pointer differently, and a
        press that begins and ends inside one command is a state no
        frame can contain. The events reaching the shell are the same
        ones `click_at` plans — motion, press, release, through
        `route_input` — spread over wall-clock instead of a frame.
        """
        self.pointer_button(True)
        self.settle(self.POINTER_PRESS_S)
        self.pointer_button(False)

    def pointer_move(self, x: float, y: float) -> None:
        self.input_ops += 1
        ok(call(self.sock, {"op": "pointer_move", "x": x, "y": y}))
        self._pointer = (x, y)

    def park_pointer(self) -> None:
        """Put the pointer somewhere neutral, so there is one at all.

        A session nobody has pointed at has no pointer on its screen,
        which is true and is also the state in which the first click of
        a run has nowhere to come from: the cursor materialises on the
        control it is about to press. Parking it once, low and left of
        centre where nothing lives, gives the first approach a start —
        and puts a pointer in the frame from the beginning, which is
        what a screen somebody is sitting at looks like.
        """
        try:
            w, h = (int(v) for v in self.size.lower().split("x"))
        except (ValueError, AttributeError):
            w, h = 1280, 800
        self.pointer_move(w * 0.42, h * 0.72)

    def right_click_at(self, x: float, y: float) -> None:
        """Open a context menu the way a person does.

        The pointer walks there through `_approach` — eased at both ends,
        paced in wall-clock — before anything is pressed. A menu that
        opens under a pointer nobody saw travelling is a menu that
        appeared by itself, and this gesture is one a viewer has to be
        able to follow: it is where an instruction gets its subject.

        The right button reached the harness late. `PointerButton` was
        the left one and nothing else, all the way down to a hardcoded
        `MouseButton::Left` at delivery — so the one gesture this desktop
        hangs a good deal on, select a thing and say something about it,
        was the one gesture a rig could not drive.
        """
        self._approach(x, y)
        self.pointer_button(True, button="right")
        self.settle(self.POINTER_PRESS_S)
        self.pointer_button(False, button="right")

    def pointer_button(self, pressed: bool, button: str = "left") -> None:
        """Press or release a pointer button where the pointer already is.

        `click_at` and `drag` both bundle press and release together,
        which cannot express a drag-and-drop across two clients: the
        source has to still be *holding* the button when it calls
        `start_drag`, the compositor promotes the gesture some frames
        later, and the drop only happens on a release that arrives after
        the pointer has crossed into another window. Bundled ops give
        the source process no wall-clock in which to do any of that.
        These two decompose it (`test_control.rs:198`).
        """
        self.input_ops += 1
        payload: dict = {"op": "pointer_button", "pressed": pressed}
        # Omitted for the left button so the wire stays byte-identical to
        # every recording made before the right one existed.
        if button != "left":
            payload["button"] = button
        ok(call(self.sock, payload))

    def place_next_window(self, app_id: str) -> dict:
        """Note that the next window announcing `app_id` opens in its own
        group. Two clients are only on screen at once when they are in
        separate groups, and that is the precondition for a scene with
        Files beside the document."""
        return call(self.sock, {"op": "place_next_window", "app_id": app_id})

    def set_group_width(self, permille: int) -> dict:
        """Give every group in the strip the same share of it. A group
        takes about three quarters by default, so two never fit."""
        return call(self.sock, {"op": "set_group_width", "permille": permille})

    def scroll(self, dx: float, dy: float) -> None:
        """One wheel notch, with whatever modifiers are held.

        Alt+Shift makes it the strip's own navigation gesture — the
        compositor grabs it before any client sees it, selects the
        neighbouring window and slides the strip to it.
        """
        self.input_ops += 1
        ok(call(self.sock, {"op": "scroll", "dx": dx, "dy": dy}))

    def merge_into_group(self, surface_id: int, into_surface_id: int) -> dict:
        """Put the window drawing `surface_id` onto the tab bar of the
        group already drawing `into_surface_id`.

        The same splice dragging a tab pill onto a sibling group's bar
        commits, reached over the socket instead. Not synthetic input in
        disguise: a pointer path across two clients is a gesture this
        script would have to stage exactly right and could not check
        afterwards, and what would be on film is the harness's aim
        rather than the compositor's tabbing.

        Counts as an input op for the same reason a click does — a
        person did this, and the agent's whole claim is that it never
        does.
        """
        self.input_ops += 1
        return call(self.sock, {"op": "merge_tab_into_group",
                                "surface_id": int(surface_id),
                                "into_surface_id": int(into_surface_id)})

    # -- launching ---------------------------------------------------

    def launch(self, app_id: str, timeout: float = LAUNCH_TIMEOUT_S,
               search: str | None = None,
               via: str | None = None,
               look: float = 1.0, read: float = 1.2,
               watch: float = 0.0) -> dict | None:
        """Start an app by its id and wait for it to present a frame.

        The surfaces that already existed are excluded, because every app
        announces the same wayland app-id: a launch that opened nothing
        is otherwise indistinguishable from one that worked, the previous
        window still being on screen and still painting.

        Two routes, and falling from one to the other is a fact about the
        compositor rather than a preference. `actions_invoke` resolves an
        address against a catalogue built with an **empty** query
        (`native.rs:606`), and the applications provider answers an empty
        query with a ranked default set rather than with all of it. On a
        session with forty-odd apps installed most of them are therefore
        unaddressable that way, and the refusal is `unknown_action` — the
        same answer as "no such app", because the gate deliberately will
        not tell a caller what it is not being shown.

        So when the address is refused, the app is opened the way a
        person opens it: the palette, its name typed, the offered row
        activated. Same provider, same launch path, one keystroke instead
        of none — and the route taken is printed, because "opened by
        address" and "opened by typing its name" are different claims and
        only one of them was true.

        `via="palette"` takes that second route deliberately rather than
        as a fallback. On camera the two are not equivalent: opening by
        address is a window appearing out of nothing, and a viewer has to
        be told what happened. The search — the key, the box, a few
        letters, the row — *shows* it, and shows that the desktop has one
        way in rather than an app menu per app. Which route an app took
        was previously decided by whether it happened to be in the
        catalogue an empty query builds, so half the film explained
        itself and half did not, for a reason nothing on screen conveyed.
        """
        before = {f.get("surface_id") for f in self.foreign_frames()}
        if via == "palette" and search is not None:
            note(f"{self.name}: opening {app_id} the way a person does — "
                 f"the palette, searching {search!r}")
            resp = {"err": {"code": "unknown_action"}}
        else:
            resp = call(self.sock, {"op": "actions_invoke",
                                    "provider": APPLICATIONS, "id": app_id})
        if "err" in resp:
            code = (resp["err"] or {}).get("code")
            if code != "unknown_action" or search is None:
                note(f"{self.name}: launching {app_id} was refused — "
                     f"{json.dumps(resp['err'])[:160]}")
                return None
            note(f"{self.name}: ({APPLICATIONS}, {app_id}) is not in the "
                 f"catalogue an empty query builds, so it is opened from "
                 f"the palette instead — searching {search!r}")
            self.modifiers(logo=True)
            self.key("k")
            self.modifiers()
            # A beat for the palette to be *there* before anything is
            # typed into it. Somebody opening a search opens it, sees it,
            # and then types; a run that fires both in the same instant
            # shows a box and its answer arriving together, which is not
            # a search happening.
            #
            # `look` is how long that beat is, because it is not the same
            # every time. The first search of a session is the one a
            # viewer has to read: the box appears, it is looked at, it is
            # typed into. By the fourth, a person knows where it is and
            # types on the way — and four full-length searches in a row
            # is a minute of a demo spent opening things.
            self.settle(look)
            # And typed, not pasted. `type_text` puts the whole query in
            # at once — the palette's rows resolve in a single frame and
            # the viewer never sees a query being formed. This is the
            # same per-character path the document uses.
            self.type_prose(search)
            # And the answer, on screen, before it is taken. `read` is
            # that beat, and it is the mirror of `look`: the first search
            # of a session has to be read — the query, and the row that
            # came back for it — and the fourth does not, because by then
            # a viewer knows what the box does. Four full-length reads is
            # five seconds of a hundred-second film spent looking at a
            # list nobody is choosing from.
            self.settle(read)
            rows = self.palette().get("rows") or []
            if not any(app_id in json.dumps(r) for r in rows):
                note(f"{self.name}: no palette row offers {app_id} for "
                     f"{search!r} (rows: {[r.get('id') for r in rows][:6]})")
                self.key("Escape")
                return None
            self.key("Enter")
        began = time.time()
        deadline = began + timeout
        first_frame: float | None = None
        found: dict | None = None
        widened: set = set()
        # The wait itself, filmed only as long as it is worth watching.
        #
        # An application starting is a sandbox spawning, a wayland
        # connection, a toolkit coming up and a first buffer, and none of
        # that is a picture: what is on screen is an unchanged desktop
        # under an "Opening ..." toast, and then a dark tile with a
        # spinner in it. Measured on this rig at 2.3s for a warm app and
        # 5.8s for the first one of a session — twenty-odd seconds of a
        # hundred-second film, spent on four windows not yet being there.
        #
        # `max_hold` cannot help: it collapses runs of byte-identical
        # frames and the spinner turns, so every frame of the wait is
        # legitimately different from the last. `patience` can, and this
        # is the same use `Recorder.patience` was written for. The toast
        # is filmed — a viewer sees that something was asked for — and
        # then the camera comes down until the window is really on
        # screen, which is the frame the beat is about and is filmed
        # either way. Nothing is skipped and nothing is hurried: the app
        # takes exactly as long as it takes, and the compositor draws
        # every frame of it.
        #
        # 0 films the whole wait, which is what the nine-beat film does.
        with patience(watch) if watch > 0 else contextlib.nullcontext():
            while time.time() < deadline:
                for f in self.foreign_frames():
                    if f.get("surface_id") in before:
                        continue
                    # The strip's share, re-applied the instant the group
                    # exists — which is here, before the client has been
                    # configured into its final tab and long before it has
                    # answered with a buffer.
                    #
                    # `set_group_widths` walks the groups that are there when
                    # it is called — so settling it once before filming, on
                    # an empty desktop, set nothing at all, and every group
                    # opened afterwards took the default three-quarters. Two
                    # of those overflow the strip, which is what put a word
                    # processor 514px past the right edge in a shot that was
                    # supposed to hold two windows.
                    #
                    # Applied on sight of the group rather than on its first
                    # frame, and that ordering is the point: a width change
                    # after the client has committed a buffer for the old
                    # tab invalidates it, and the window then holds its
                    # opening card for a second round trip that nobody asked
                    # for. Idempotent, so re-applying it while waiting costs
                    # a message and settles the question of which buffer the
                    # client is working towards.
                    if f.get("surface_id") not in widened:
                        widened.add(f.get("surface_id"))
                        self.set_group_width(FILM_GROUP_SHARE)
                    if f.get("frame_seq", 0) > 0 and first_frame is None:
                        first_frame = time.time()
                        found = f
                    # `presented` is the compositor's own answer to "is this
                    # window on screen", and it is a different question from
                    # "has the client drawn anything": the first buffer a
                    # client commits is the size it picked before it was
                    # configured, and until one arrives that answers the tab
                    # the strip holds the window's place with a card. A
                    # launch that returned on the first frame reported an app
                    # as open while the person was still looking at a
                    # spinner, which is the gap this closes.
                    if f.get("presented"):
                        shown = time.time()
                        if first_frame is None:
                            first_frame = shown
                            found = f
                        note(f"{self.name}: {app_id} drew its first frame "
                             f"{first_frame - began:.1f}s after it was asked "
                             f"for, and was on screen at "
                             f"{shown - began:.1f}s")
                        return found
                time.sleep(0.12)
            if found is not None:
                # `first_frame` is set on both paths that set `found`, so it is
                # never None here -- but the two are only tied together by
                # reading the loop, and this line runs inside a launch, during
                # filming, where an AttributeError on a `None` would take the
                # take down at the worst possible moment. So it says what it
                # does not know rather than assuming it.
                since = (f"{first_frame - began:.1f}s"
                         if first_frame is not None else "an unrecorded time")
                note(f"{self.name}: {app_id} drew a frame {since} in but never "
                     f"filled its tab; reporting it as opened anyway")
                return found
            return None

    # -- the action API (semantic; no input, no pixels) --------------

    def enumerate_actions(self, query: str = "") -> dict:
        """The exported action surface. Read-only by construction."""
        return ok(call(self.sock, {"op": "actions_enumerate", "query": query}))["answer"]

    def invoke_action(self, provider: str, action_id: str,
                      args: dict | None = None) -> dict:
        """Invoke one action by its stable address. Raw response, so a
        refusal is a result rather than an exception."""
        # Annotated, because the values are not all strings: `args` is a
        # nested object and the wire is JSON. Inferred from the literal
        # alone this reads as `dict[str, str]`, and the line below then
        # looks like a bug in a request that has always been correct.
        payload: dict[str, object] = {"op": "actions_invoke",
                                      "provider": provider, "id": action_id}
        if args is not None:
            payload["args"] = args
        return call(self.sock, payload)

    # -- synthetic input (counted) -----------------------------------

    def key(self, name: str) -> None:
        self.input_ops += 1
        ok(call(self.sock, {"op": "key", "key": name}))

    def type_text(self, text: str) -> None:
        self.input_ops += 1
        # One command per few hundred characters keeps each request under
        # the socket's line cap while still arriving as ordinary keys.
        for i in range(0, len(text), 200):
            ok(call(self.sock, {"op": "type", "text": text[i:i + 200]}))

    def key_down(self, name: str) -> None:
        """Press and hold a key. Pairs with [`key_up`]."""
        self.input_ops += 1
        ok(call(self.sock, {"op": "key_down", "key": name}))

    def key_up(self, name: str) -> None:
        self.input_ops += 1
        ok(call(self.sock, {"op": "key_up", "key": name}))

    def type_prose(self, text: str, pace: bool | None = None,
                   chrome: bool = False) -> None:
        """Type `text` into a *client* so that it arrives as written.

        `chrome=True` types into a compositor-drawn surface — the palette,
        the invite card, the "say something" compose box — rather than an
        app. Chrome does not resolve keycodes through xkb; it reads the
        event's *logical key* (`test_control.rs`). That is why letters were
        taken out of `SHIFTED_KEYS`: a Shift-held `c` arrives at chrome as a
        lowercase `c`, not a capital. The punctuation left in the table has
        exactly the same failure there — a Shift-held `;` is read as `;`,
        not `:`, so `Claude:` reached the compose box as `Claude;` and the
        agent, matching the literal prefix `claude:`, saw no instruction at
        all. So under `chrome` the Shift dance is skipped for punctuation
        too, and every character goes through the direct `type` path, which
        is the one chrome reads correctly (the palette's `Launch Plan: ok!`
        is typed exactly this way).

        `type` alone does not. It emits one keycode per character, and a
        client resolves a keycode through the seat's xkb state — which
        has no shift held — so every capital arrives lowercase and every
        shifted symbol arrives as the character below it. Compositor
        chrome is unaffected: it takes the character directly, which is
        why the same call produces `Launch Plan: ok!` in the palette and
        `launch plan; ok1` in an app, and why this was believed for a
        while to be a limitation of the `type` op rather than of the
        path it was being used on.

        The remedy is a real held modifier: `key_down("Shift")` drives
        the seat the way hardware does, and the client's own xkb state
        then resolves the keycode to the shifted symbol. Measured in the
        word processor, not assumed.

        Newlines are `Enter`, which does break a paragraph — the word
        processor starts a new one and even turns a leading `- ` into a
        real bullet. A draft typed this way arrives as a document rather
        than as one lowercase run-on line.

        One character at a time, at a pace a person reads as typing.

        Batching the unshifted runs into a single `type` call is much
        cheaper — one round trip per capital rather than per character —
        and it is the wrong thing on camera. A sentence that arrives in
        three instant bursts is not a person writing; the viewer sees
        text appear and has no idea it was typed, which is the one thing
        this beat exists to show.

        The cadence is deterministic rather than random, so two runs of
        the film type identically: a base interval, plus a longer pause
        after the punctuation and spaces where a person's hands actually
        slow down. Off camera the pause is skipped entirely and the old
        batching applies, because a harness waiting out a realistic
        typing speed is just a slower harness.
        """
        paced = RECORDER is not None and pace is not False
        if not paced:
            pending: list[str] = []

            def flush() -> None:
                if pending:
                    self.type_text("".join(pending))
                    pending.clear()

            for ch in text:
                if ch == "\n":
                    flush()
                    self.key("Enter")
                elif ch in SHIFTED_KEYS and not chrome:
                    flush()
                    self.key_down("Shift")
                    self.key(SHIFTED_KEYS[ch])
                    self.key_up("Shift")
                else:
                    pending.append(ch)
            flush()
            return

        for ch in text:
            if ch == "\n":
                self.key("Enter")
            elif ch in SHIFTED_KEYS and not chrome:
                self.key_down("Shift")
                self.key(SHIFTED_KEYS[ch])
                self.key_up("Shift")
            else:
                self.type_text(ch)
            base = TYPING_PAUSE.get(ch, TYPING_PACE)
            time.sleep(base * next(self._typing_jitter))

    def modifiers(self, ctrl=False, shift=False, alt=False, logo=False) -> None:
        self.input_ops += 1
        ok(call(self.sock, {"op": "modifiers", "ctrl": ctrl, "shift": shift,
                            "alt": alt, "logo": logo}))

    # -- the clipboard (how a token crosses machines) ----------------

    def clipboard_read(self) -> str | None:
        """What this session's clipboard holds."""
        return (call(self.sock, {"op": "clipboard_paste"})
                .get("clipboard") or {}).get("text")

    def clipboard_write(self, text: str) -> None:
        """Put `text` on this session's clipboard.

        Used for the hop between two machines, which is a person's job —
        a chat message, an email, a USB stick — and not the OS's. What
        travels is taken from the *issuing* session's clipboard, so a
        copy that silently truncated is a failure here rather than a
        token that happens to work because the harness had it anyway.
        """
        ok(call(self.sock, {"op": "clipboard_copy", "text": text}))

    # -- painting ----------------------------------------------------

    def snapshot(self, path: str, defer: bool = False) -> str:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        # Its own timeout, and a generous one. `snapshot` re-renders the
        # scene and PNG-encodes it on the compositor's event loop, so the
        # session is blocked for every millisecond of it — an unoptimised
        # build on a machine that is busy compiling something else can sit
        # here for a minute. Timing out would report a contended box as a
        # broken compositor.
        ok(call(self.sock, {"op": "snapshot", "path": path, "defer": defer},
                timeout=SNAPSHOT_TIMEOUT_S))
        return path

    def drain_snapshots(self) -> None:
        """Wait for every deferred frame to reach the disk.

        A deferred snapshot returns once the pixels are captured; the
        PNG is encoded and written off the compositor's event loop,
        which is what makes filming cheap enough to be worth doing at
        thirty frames a second. The frames are therefore *not* all on
        disk when the last one is taken, and the difference is a
        recording that composes fewer frames than it captured. This is
        the barrier that closes that gap, and the recorder owes it
        before anything reads the directory.
        """
        ok(call(self.sock, {"op": "drain_snapshots"}, timeout=SNAPSHOT_TIMEOUT_S))

    def settle(self, seconds: float = 0.5) -> None:
        """Let the compositor paint."""
        time.sleep(seconds)

    # -- palette (people only) ---------------------------------------

    def run_palette_command(self, query: str, expect_id: str | None = None) -> None:
        """Open the palette, type `query`, activate the top row."""
        self.modifiers(logo=True)
        self.key("k")
        self.modifiers()
        self.settle(0.35)
        self.type_text(query)
        self.settle(0.6)
        selected = self.palette()["selected_id"]
        if expect_id is not None and selected != expect_id:
            rows = [r["id"] for r in self.palette()["rows"]][:6]
            raise RuntimeError(
                f"{self.name}: palette query {query!r} selected {selected!r}, "
                f"expected {expect_id!r} (rows: {rows})")
        self.key("Enter")
        self.settle()


# --------------------------------------------------------------------
# recording
# --------------------------------------------------------------------
#
# There is no screen recorder on this machine that can see a Wayland
# surface, and none is wanted: the compositor renders its own frames on
# demand over the control socket, so a recording here is a real sampled
# render of each session rather than a grab of three windows somebody
# had to arrange first. Nothing is drawn that a session did not draw.
#
# One session is on camera at a time and the video cuts between them,
# the way the narrative moves: to whoever is acting, and to a peer at
# the moment the point is that the peer *sees* it. Three panes at once
# had to shrink every session to a third of the width, which clipped
# the top bar and overflowed the tab strip, and it split the sample
# budget three ways to show two screens where nothing was happening.
#
# Cutting pays for itself twice: each session renders full-frame at
# 1:1, and the only session being sampled is the one you are looking
# at, so the whole capture budget goes to the pixels on screen.
#
# Frames are written at their true timestamps — each one shows from
# when it was taken until the next was, so the video's clock is the
# demo's clock. Nothing is interpolated, and with a single track there
# is no held frame either: no frame in the file was shown for longer
# than the session held it. Where the video needs to be longer the
# demo is given more real time to be longer in (`--record-dwell`,
# `--record-cut-dwell`), never more frames than it took.
VIDEO_W, VIDEO_H = 1920, 1080

# The lower third. The name of the session and what it is, over a scrim
# dark enough to stay legible against whatever it is sitting on — and
# nothing else. The transcript belongs in the terminal; on the video it
# was a wall of text nobody reads while a screen is moving underneath.
BAND_H = 66

# What each session is recorded at: the video's size *minus the band*.
#
# The band used to be drawn over the frame, and a compositor laying out
# for the full 1080 put the bottom of every window under it — a status
# bar, a word count, the foot of a file list, clipped in every shot that
# mattered. Nothing in the compositor is wrong there; the camera was
# covering a screen it had asked to be full height.
#
# So the session is given the height the video can actually show, and
# the band occupies the strip below it. A frame still goes in 1:1 with
# nothing resampled, and no pixel the compositor drew is hidden.
RECORD_SIZE = f"{VIDEO_W}x{VIDEO_H - BAND_H}"

#: What the agent's session renders at. It is never on camera -- `cut`
#: refuses to put it there -- so its only readers are the agent's own
#: queries, which ask the document rather than the screen. Small because
#: GPU memory is the binding constraint on this machine: a client costs
#: roughly 600-950MB of GTT in proportion to its area, and a full run of
#: thirteen of them against a 16404MB window is what froze it.
AGENT_SIZE = "800x600"
BAND_SCRIM = "black@0.72"
BAND_PAD_X = 52

BG = "0x0B0D12"       # the shell's own backdrop, for any letterboxing
FG = "0xE9ECF2"

# Who each session is, in the video. With one screen on at a time the
# band is the only thing telling you whose screen you are looking at, so
# it carries a name and a standing in the room, and nothing else.
#
# The third one used to read "driven only by (provider, id)", which is
# true and is the wrong thing to put on a screen: it is the name of a
# mechanism, shown to people who have never seen this system. How it is
# driven is what the beat demonstrates and what the transcript proves.
# On the band it is simply another participant.
ROLES = {
    "quinten": "hosts the room",
    "xaver": "joined by invite",
}

#: At or above this SSIM the two screens either side of a cut are the
#: same picture as far as a viewer is concerned, and the cut is one
#: nobody can see.
#:
#: Two different things made the band look like it was captioning the
#: wrong machine, and only one of them is this one.
#:
#: The band cannot caption the wrong machine by accident: `_capture`
#: reads the live session under the lock and uses that same object for
#: both the snapshot and the name it files with it, and `_label_spans`
#: walks the very timeline the concat track is built from. What the band
#: *can* be is late — see `_label_spans` — and it was, at every cut.
#:
#: The other thing is this, and it is a property of the product rather
#: than of the camera: two people looking at the same shared document
#: see the same screen. That is the point of the system being filmed. So
#: a cut between them changes nothing but the caption, and no caption
#: can fix that — the cut is what is wrong, and the beat has to be
#: restructured to cut where the two desktops actually differ.
#:
#: 0.85, from measuring the four cuts in a 521 s take. Cuts between two
#: genuinely different screens came in at SSIM 0.60-0.63; the one cut
#: where both machines showed the same two windows with the same content
#: came in at 0.9516, and two frames of one unchanging screen run above
#: 0.9995. The threshold sits in the gap, with a wide margin either
#: side.
CUT_VISIBLE_SSIM = 0.85

# The one session this film never shows, and the reason it has a name of
# its own rather than an absent entry in ROLES.
#
# The claim being filmed is that an agent never has to look at a screen —
# it reads the document, and what it did shows up on the other two
# desktops the way anybody else's edit does. A shot of *its* desktop
# argues against that in the one frame meant to prove it, and it is an
# easy shot to take by accident: the session is right there beside the
# other two and answers `snapshot` exactly like them.
#
# So it is refused twice. `cut` will not accept the name, and the run
# fails at the end if a single frame turns out to have come from it —
# because the second check is the one that holds when somebody adds a
# capture path that does not go through `cut`.
NEVER_ON_CAMERA = "Claude"

# How long a frame the viewer has to *read* — a token, a name in a
# composer, a card saying it has been copied — stays up.
#
# Long by the standards of the rest of the cut, and deliberately immune
# to `--record-max-hold`: that cap exists to throw away a session sitting
# on an unchanged screen while it waits for a sandbox, and a card nobody
# has had time to read is the opposite case. Frames taken inside `hold`
# are marked, and trimming skips them. See `Recorder._timeline`.
READ_HOLD_S = 1.5

FONTS_REGULAR = (
    "/usr/share/fonts/liberation-sans-fonts/LiberationSans-Regular.ttf",
    "/usr/share/fonts/adwaita-sans-fonts/AdwaitaSans-Regular.ttf",
    "/usr/share/fonts/google-droid-sans-fonts/DroidSans.ttf",
)
FONTS_BOLD = (
    "/usr/share/fonts/liberation-sans-fonts/LiberationSans-Bold.ttf",
) + FONTS_REGULAR

# A snapshot blocks the session it is taken from, so the capture loop
# yields this long between frames even when asked for "as fast as
# possible". Without some gap the demo competes with its own camera for
# every reply, and the waits it depends on start timing out. Only one
# session is sampled, but it is also the session the demo is driving, so
# the hand-back is not optional.
#
# It used to be 0.04, and that was the single largest term in the frame
# period — larger than taking the picture. It was sized for a snapshot
# that PNG-encoded on the compositor's event loop, which cost ~31 ms of
# loop time and left the filmed session doing almost nothing else.
# Frames are deferred now: the compositor hands the pixels to encoder
# threads and is free again after the readback, ~10 ms of a ~28 ms round
# trip. The loop is idle around two thirds of the time with no gap at
# all, so the gap that made sense against 78% occupancy is now most of
# the reason the film was slow.
MIN_CAPTURE_GAP_S = 0.004
# The same two numbers for a recorder that is *not* deferring, where a
# snapshot occupies the loop for its whole cost. These are what the pair
# above used to be, and they are kept rather than deleted because
# `--record-sync` still needs them: pacing a synchronous camera as if it
# were a deferred one is how you starve the demo you are filming.
SYNC_MIN_CAPTURE_GAP_S = 0.04
# …and on top of the floor, this share of whatever the last snapshot
# actually cost. A fixed gap is the wrong shape for a cost that moves
# between an unoptimised build and an optimised one with a warm
# renderer: too small and the demo queues behind its own camera, too
# large and a fast compositor sits idle between frames. A share holds
# the duty cycle steady at either end.
#
# Lowered with the floor and for the same reason. The share is taken
# against the round trip, but only about a third of that round trip is
# loop time now, so 0.25 was handing back roughly three quarters of the
# cost of a capture on top of a cost the loop no longer pays.
CAPTURE_YIELD = 0.12
SYNC_CAPTURE_YIELD = 0.25
# What one snapshot costs an idle machine with this build, round trip,
# in ms. Not a threshold anything is enforced against — the recording is
# whatever it turns out to be — only the number the report compares
# against so a contended run says so instead of looking like a slow qOS.
#
# Measured at 1920x1014 with frames deferred: ~28 ms back to back, of
# which ~10 ms is compositor loop time and the rest is the socket round
# trip. The old figure here was 150, for the days when the event loop
# also did the encoding.
SNAPSHOT_BUDGET_MS = 45.0
# How far the finished film may sit from the take it is a film of, in
# seconds, before `Recorder._check_length` refuses it.
#
# Small on purpose. The only honest sources of difference are the tail of
# the last frame landing on the container's frame grid and the rounding
# in the concat list, and both are milliseconds; a tolerance wide enough
# to swallow a real drift is a check that reports success for the same
# reason the old one did. Half a second is already generous against a
# take of four minutes, and the failure it was written for was 252.
WALL_CLOCK_TOL_S = 0.5


def _pick_font(candidates: tuple[str, ...]) -> str:
    for path in candidates:
        # A colon would be read as an option separator inside the filter
        # graph, so a path carrying one is no use to us even if it exists.
        if os.path.exists(path) and ":" not in path:
            return path
    raise RuntimeError(
        "no usable font for the video labels; looked for:\n  "
        + "\n  ".join(candidates))


def _even(n: float) -> int:
    """Sizes h264 can encode without the scaler quietly rounding for us."""
    return int(n) - (int(n) % 2)


class Recorder:
    """Timestamped frames pulled from whichever session is on camera.

    The capture thread is a reader: `snapshot` re-paints and writes a
    PNG, and never enters the input path, so the agent's "no synthetic
    input" claim is as true during a recording as without one —
    `input_ops` is untouched by anything in here.

    One thread, one track. `cut_to` moves the camera; every frame
    records which session it came from, so the label on screen is
    derived from the pixels rather than from when a cut was ordered.
    """

    def __init__(self, sessions: list[Session], directory: str,
                 interval: float, dwell: float = 0.0,
                 cut_dwell: float = 0.0, max_hold: float = 0.0,
                 narrate: str | None = None, defer: bool = True) -> None:
        # PulseAudio/PipeWire source to record the spoken narration from,
        # or None for a silent take. `"default"` follows the desktop's own
        # sound settings, so changing headset before a take needs nothing
        # here.
        self.narrate = narrate
        self._audio: subprocess.Popen | None = None
        # Whether a frame's PNG encode happens off the compositor's
        # event loop. On, because encoding is ~79% of a snapshot and
        # needs nothing the compositor owns, so leaving it on the loop
        # costs the film most of its frame rate. Off is the older,
        # simpler contract — each frame is on disk before the next is
        # asked for — kept because it is the honest comparison when a
        # recording looks wrong, and because a caller that wants to
        # measure the camera itself should be able to remove the
        # threads from the picture.
        self.defer = defer
        self.sessions = sessions
        self.by_name = {s.name: s for s in sessions}
        self.dir = directory
        self.interval = interval
        # The share of the last snapshot's cost handed back to the
        # compositor before taking the next one. Raised, per beat,
        # around anything this film measures — see `easing`.
        self.yield_share = CAPTURE_YIELD if defer else SYNC_CAPTURE_YIELD
        # A synchronous snapshot really does occupy the loop for its
        # whole cost, so it needs the larger hand-back the deferred one
        # no longer does. The floor follows the cost model rather than
        # being a number that outlived it.
        self.min_gap = MIN_CAPTURE_GAP_S if defer else SYNC_MIN_CAPTURE_GAP_S
        self.dwell_s = dwell
        self.cut_dwell_s = cut_dwell
        # The longest one unchanged frame is shown for. See `_timeline`.
        self.max_hold = max_hold
        # How far the finished picture is from the wall clock, measured
        # at compose: the take's span less the screen time the timeline
        # hands out. Positive is a shorter video than the take. Zero is
        # what a narrated take needs, and what `--max-hold 0` is for.
        self.trimmed = 0.0
        # The two halves of that subtraction, kept so a run that drifts
        # can say which of them moved.
        self.wall_s = 0.0
        self.timeline_s = 0.0
        # …and how much of `trimmed` the still-picture cap accounts for.
        # Separate from `trimmed` on purpose: when the two disagree, the
        # length moved for a reason nothing here knows about, which is
        # the failure this pair exists to name.
        self.capped = 0.0
        # (taken_at, relative path, session name, deliberately held) in
        # capture order. The last field marks a frame taken while the
        # demo was holding on purpose, which is what exempts it from the
        # still-picture trim.
        self.frames: list[tuple[float, str, str, bool]] = []
        # How long each snapshot blocked its session. The honest measure
        # of what the camera costs, reported rather than estimated.
        self.blocked: list[float] = []
        self.errors: list[str] = []
        self._failed: set[str] = set()
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._live: Session | None = None
        # Who the *narrative* is on, which is not the same as who the
        # camera is on: `hold_off` takes the camera down for a beat that
        # is run and proven but not filmed, and `_wanted` is what it
        # comes back up on.
        self._wanted: Session | None = None
        self._off = 0
        self._holding = False
        self._lock = threading.Lock()
        self.started_at = 0.0
        self.stopped_at = 0.0

    # -- capture -----------------------------------------------------

    def start(self, first: str) -> None:
        os.makedirs(os.path.join(self.dir, "frames"), exist_ok=True)
        self._live = self._wanted = self.by_name[first]
        # The clock starts before the microphone check, not after it.
        # Tried the other way, on the theory that the third of a second
        # `_start_narration` spends proving ffmpeg is alive was time the
        # take was being charged for and nothing was being recorded — and
        # it is not: ffmpeg opens the source and is capturing well inside
        # that window, which is the whole reason the probe can tell a live
        # microphone from a dead one. Starting the clock afterwards puts
        # `started_at` a third of a second past the first sample, which
        # plays the voice early rather than late. Measured, then put back.
        self.started_at = time.time()
        self._start_narration()
        self._thread = threading.Thread(target=self._capture, name="capture",
                                        daemon=True)
        self._thread.start()

    def _capture(self) -> None:
        index = 0
        while not self._stop.is_set():
            with self._lock:
                s = self._live
            if s is None:
                self._stop.wait(0.02)
                continue
            began = time.time()
            held = self._holding
            rel = f"frames/{index:06d}.png"
            try:
                s.snapshot(os.path.join(self.dir, rel), defer=self.defer)
            except Exception as exc:  # noqa: BLE001
                # A session going quiet costs its own frames and nothing
                # else: the camera keeps rolling, and a later cut to a
                # healthy session picks straight back up. Reported once
                # per session rather than once per attempt.
                if s.name not in self._failed:
                    self._failed.add(s.name)
                    self.errors.append(
                        f"{s.name}: capture failed at frame {index}: "
                        f"{type(exc).__name__}: {exc}")
                self._stop.wait(0.25)
                continue
            took = time.time() - began
            self.blocked.append(took)
            self.frames.append((began, rel, s.name, held))
            index += 1
            self._stop.wait(max(self.min_gap, self.yield_share * took,
                                self.interval - took))

    # -- getting out of the way --------------------------------------

    @contextlib.contextmanager
    def easing(self, share: float):
        """Take the camera off the compositor's back for a moment.

        This used to be the difference between a watchable film and an
        honest number. A snapshot cost the filmed compositor ~31 ms of
        event loop, against a 0.04s pacing floor, so it spent roughly
        78% of its loop inside `snapshot`: an install timed through the
        camera came out at 21s against a real median of 7.8s, and a
        viewer with a stopwatch would have read the camera's overhead as
        the product's speed.

        Deferring the encode took that to ~10 ms of loop time per frame,
        so the camera no longer dominates a beat it is filming. The
        easing stays because the margin is smaller than the difference
        it was hiding, and a measured beat should not have to trust
        that.

        So a measured beat asks for a larger share of the loop to be
        left alone. It still films — a beat nobody can see is worse than
        a slow number — just at a lower frame rate for its duration.
        """
        was = self.yield_share
        self.yield_share = share
        try:
            yield
        finally:
            self.yield_share = was

    # -- the cut -----------------------------------------------------

    def cut_to(self, name: str, dwell: float | None = None) -> None:
        """Put one session on camera, and hold there long enough to read
        it before the demo moves on.

        Cutting to the session already on camera is a no-op, so the beat
        functions can name their subject without inserting a pause that
        the narrative did not ask for.
        """
        if name == NEVER_ON_CAMERA:
            raise RuntimeError(
                f"{name} is never on camera: an agent with a desktop is the "
                f"one thing this film must not show. See NEVER_ON_CAMERA.")
        with self._lock:
            if self._wanted is not None and self._wanted.name == name:
                return
            self._wanted = self.by_name[name]
            # Noted while the camera is down, and acted on when it comes
            # back up. No dwell either: nobody is being moved anywhere.
            if self._off:
                return
            self._live = self._wanted
        hold = self.cut_dwell_s if dwell is None else dwell
        if hold > 0:
            time.sleep(hold)

    @contextlib.contextmanager
    def hold_off(self):
        """Run a beat without putting it in the film.

        Every beat in this demonstration is run and proven; not every one
        of them belongs in a one-minute cut. A beat filmed and then
        thrown away costs the sessions the snapshot time anyway — and
        snapshotting is most of the filmed compositor's event loop — so
        the camera comes down instead and the beat runs at full speed.

        This is an edit, and the same kind as a cut: what it can do is
        leave material out. It cannot put anything in, reorder anything,
        or make a frame show for longer than its session held it — the
        frame before a pause is capped by `max_hold` like any other.
        """
        # Read before the camera goes down, not after. Taken afterwards
        # this was always None, so "the session changed while we were
        # off" was true of every stretch and the dwell below was paid for
        # a cut that had not happened — a second added to the film each
        # time, and a second before the picture comes back to whatever
        # made it come back.
        with self._lock:
            was = self._live
            self._off += 1
            self._live = None
        try:
            yield
        finally:
            changed = False
            with self._lock:
                self._off -= 1
                if not self._off:
                    self._live = self._wanted
                    changed = self._live is not was
            # A beat run off camera can name a different session for what
            # follows. Coming back up on one is a cut like any other and
            # is paid for like one, or the film jumps between two desktops
            # with no beat in between to land on.
            if changed and self.cut_dwell_s > 0:
                time.sleep(self.cut_dwell_s)

    @contextlib.contextmanager
    def patience(self, seconds: float):
        """Film the first `seconds` of a wait, then stop filming it.

        The still-picture trim cannot see a wait for what it is. It
        compares frames byte for byte, and a screen with a caret blinking
        in it is a different screen every half second — so a minute spent
        waiting for something to arrive is a minute of *changing* frames
        and survives the cap in full. One run of this film spent 180 of
        its 318 seconds on two waits that had already failed, and every
        frame in them was legitimately different from the last.

        So the wait is bounded by the camera rather than by the trim.
        What arrives quickly is filmed arriving; what does not is not sat
        through. The bound is generous enough to cover the thing working
        — a viewer sees the result appear — and is only ever reached when
        it is not.
        """
        # Not on a take that carries a voice.
        #
        # Taking the camera down shortens the picture without shortening
        # the take, and a microphone runs on the wall clock: a stretch
        # left out of the middle puts the voice ahead of the picture for
        # everything after it. `--max-hold 0` already means "this take
        # may not be edited", and this is an edit of exactly the kind it
        # forbids -- `_check_length` refuses the result, so the honest
        # place to stop is before making it. True of every caller here,
        # not only of the newest.
        if self.max_hold <= 0:
            yield
            return

        stopped = threading.Event()
        dropped = threading.Event()

        def drop_after():
            if not stopped.wait(seconds):
                with self._lock:
                    self._off += 1
                    self._live = None
                dropped.set()

        waiter = threading.Thread(target=drop_after, name="patience",
                                  daemon=True)
        waiter.start()
        try:
            yield
        finally:
            stopped.set()
            waiter.join(timeout=5)
            if dropped.is_set():
                print(f"     .   [ camera came down after {seconds:.0f}s of "
                      f"waiting ]", flush=True)
                with self._lock:
                    self._off -= 1
                    if not self._off:
                        self._live = self._wanted

    @contextlib.contextmanager
    def holding(self):
        """Mark what is filmed in here as a deliberate hold.

        Frames taken inside are exempt from `max_hold`, so a card the
        demo stopped on to be read survives the trim that removes a
        session waiting on a sandbox spawn. Both are an unchanging
        screen; only one of them is dead air.
        """
        was = self._holding
        self._holding = True
        try:
            yield
        finally:
            self._holding = was

    def hold(self, seconds: float) -> None:
        """Stop here, on camera, for `seconds`. Really elapsed, really
        sampled — and kept whole by the trim.

        Nothing at all when the camera is down. A hold buys a viewer time
        to read something, and an unfilmed beat has no viewer: paying it
        anyway would add half a minute of wall clock to a run for frames
        that do not exist.
        """
        if seconds <= 0 or self._off:
            return
        with self.holding():
            time.sleep(seconds)

    def camera_is_down(self) -> bool:
        return self._off > 0

    def on_camera(self) -> "Session | None":
        """Whose screen the film is on, or None before it starts.

        The *narrative* subject rather than whatever the capture thread
        has open this instant: `hold_off` takes the camera down for a
        beat that is run and proven but not filmed, and `_wanted` is
        what it comes back up on. Anything deciding where to point a
        window has to aim at that, or a reframe decided during a hold
        lands on the screen the hold interrupted.
        """
        return self._wanted

    def frames_from(self, name: str) -> int:
        return sum(1 for _t, _rel, n, _h in self.frames if n == name)

    #: What one frame is assumed to cost before the camera has taken
    #: enough of them to say. Deliberately pessimistic: over-estimating
    #: costs an approach a few needless milliseconds, under-estimating
    #: emits pointer moves nobody photographs.
    ASSUMED_FRAME_PERIOD_S = 0.033

    def frame_period(self) -> float:
        """Seconds between the last few frames, as actually taken.

        A gesture that wants to be *photographed* has to be paced against
        this rather than against a constant. The pointer approach is the
        case that made it necessary: it moved in steps sized for a 55 ms
        camera, and against a camera whose frames were 100-130 ms apart
        two steps happened between photographs, so a trip that really did
        walk landed on screen as three stamps with gaps between them.

        The recent tail rather than the whole run: a snapshot's cost
        moves with what is on screen, and an approach cares about what
        the camera is doing now.
        """
        with self._lock:
            recent = [t for t, _rel, _n, _h in self.frames[-24:]]
        if len(recent) < 4:
            return self.ASSUMED_FRAME_PERIOD_S
        gaps = sorted(b - a for a, b in zip(recent, recent[1:]))
        median = gaps[len(gaps) // 2]
        # A run of gaps can contain a stall the camera is already out of;
        # the floor keeps that from being read as a fast camera, and the
        # ceiling keeps one 850 ms snapshot from stretching every gesture
        # in the film.
        return min(0.25, max(self.ASSUMED_FRAME_PERIOD_S, median))

    def dwell(self) -> None:
        """Hold on the moment just reached.

        The one honest way to make the video longer. Every second spent
        here is a second the demo really took, sampled like any other —
        as against holding a frame on screen for longer than the session
        held it, which would be inventing time the run did not have.
        """
        if self.dwell_s > 0:
            time.sleep(self.dwell_s)

    def stop(self) -> None:
        # Idempotent, because it is called twice on purpose: once where
        # the take ends and again from a `finally` that cannot know
        # whether it did. `stopped_at` is one end of the window every
        # duration is now measured against, so a second call moving it
        # would charge the picture for the readbacks and reports that ran
        # after the camera stopped — a film that looked short of a take
        # that had already finished.
        if self.stopped_at:
            return
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=30)
            self._thread = None
        self.stopped_at = time.time()
        # The microphone closes with the camera, not after it. Draining
        # the deferred encoders below takes seconds on a long take, and
        # every one of them used to be recorded — a tail of room noise
        # past the end of the picture, which `-shortest` then cut off
        # again, taking the last words spoken with it. Both tracks now
        # end on `stopped_at`, which is the instant the picture ends too.
        self._stop_narration()
        # Frames are encoded off the compositor's event loop, so the last
        # of them are still being written when the camera stops. Every
        # session that was filmed gets the barrier, because the count
        # this recorder reports and the files `compose` reads have to be
        # the same set — a race here is a film that is quietly missing
        # its final second.
        filmed = {name for _t, _rel, name, _held in self.frames} if self.defer else set()
        for s in self.sessions:
            if s.name not in filmed:
                continue
            try:
                s.drain_snapshots()
            except Exception as exc:  # noqa: BLE001
                self.errors.append(
                    f"{s.name}: draining deferred frames failed: "
                    f"{type(exc).__name__}: {exc}")
        # What was promised against what arrived. A deferred write that
        # failed is reported by the compositor, but a frame that never
        # reached the disk at all would otherwise only show up as a
        # shorter video than the run described.
        missing = [rel for _t, rel, _n, _h in self.frames
                   if not os.path.exists(os.path.join(self.dir, rel))]
        if missing:
            self.errors.append(
                f"{len(missing)} captured frame(s) never reached the disk, "
                f"first {missing[0]}")

    # -- narration ---------------------------------------------------
    #
    # Recorded by the rig rather than alongside it, so there is nothing to
    # line up afterwards. The microphone opens on the same statement that
    # starts the frames and closes on the one that stops them, so the two
    # tracks share a start and a length and the mux is a mux rather than
    # an edit.
    #
    # The alternative -- a separate recorder started by hand -- means
    # finding a sync point in a silent film, and the only sharp events in
    # this one are a window appearing and a cut. Neither is a clap.

    def _start_narration(self) -> None:
        if not self.narrate:
            return
        wav = os.path.join(self.dir, "narration.wav")
        # `default` follows whatever the desktop's sound settings say, so
        # switching headsets before a take needs no argument here.
        self._audio = subprocess.Popen(
            ["ffmpeg", "-y", "-v", "error", "-f", "pulse",
             "-i", self.narrate, "-ac", "1", "-ar", "48000", wav],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        self._narration_wav = wav

        # Proof it is running, before anybody starts talking.
        #
        # ffmpeg exits immediately when the source will not open -- a
        # headset that has gone to sleep, a `default` pointing at a
        # device that is no longer there. Announcing the recording
        # without checking meant a whole take narrated into a process
        # that had already died, found out at the end when there was
        # nothing to mux. A take is expensive; this costs a third of a
        # second.
        time.sleep(0.35)
        rc = self._audio.poll()
        if rc is not None:
            why = (self._audio.stderr.read() or b"").decode(
                "utf-8", "replace").strip()
            self._audio = None
            raise RuntimeError(
                f"the microphone did not open, so nothing would be "
                f"recorded: ffmpeg exited {rc} before capturing anything.\n"
                f"  source asked for:  {self.narrate!r}\n"
                f"  ffmpeg said:       {why or '(nothing)'}\n"
                f"  sources available: {self._pulse_sources()}\n"
                f"Pass --narrate <source> to pick one, or wake the headset "
                f"and run again.")
        print(f"  narration              recording from {self.narrate!r}")

    @staticmethod
    def _pulse_sources() -> str:
        """What this machine would actually accept, for the error above."""
        try:
            out = subprocess.run(["pactl", "list", "short", "sources"],
                                 capture_output=True, text=True,
                                 timeout=5).stdout
        except Exception:  # noqa: BLE001
            return "(pactl unavailable)"
        names = [parts[1] for parts in
                 (ln.split("\t") for ln in out.splitlines())
                 if len(parts) > 1]
        return ", ".join(names) or "(none)"

    def _stop_narration(self) -> None:
        if not getattr(self, "_audio", None):
            return
        # `q` on stdin is ffmpeg's clean stop; it finalises the container.
        # A killed ffmpeg leaves a wav with no length in its header, which
        # some players read as empty.
        try:
            self._audio.terminate()
            self._audio.wait(timeout=10)
        except Exception:  # noqa: BLE001
            self._audio.kill()
        # Drained rather than left sitting in the pipe, so a complaint
        # from ffmpeg is read by somebody.
        try:
            said = (self._audio.stderr.read() or b"").decode(
                "utf-8", "replace").strip()
        except Exception:  # noqa: BLE001
            said = ""
        self._audio = None

        # And say what was captured. A wav that exists but holds nothing
        # is the same outcome as no wav at all, and the video should not
        # be the thing that reports it.
        wav = getattr(self, "_narration_wav", None)
        size = os.path.getsize(wav) if wav and os.path.exists(wav) else 0
        # 48kHz mono 16-bit is ~96KB/s, so under a second of audio is a
        # failure wearing a file's clothes.
        if size < 96_000:
            FAILURES.append(
                f"the narration recorded {size} bytes -- under a second of "
                f"audio. The film has no voice on it."
                + (f" ffmpeg said: {said}" if said else ""))
            return

        # How long it really is, read off the file, against how long the
        # take really was. Both tracks opened and closed on the same two
        # statements, so these are the same number when nothing went
        # wrong — and when they are not, the difference is the distance
        # the voice will sit from the picture, for the whole film. The
        # mux cannot see it: `apad` makes up the missing seconds with
        # silence at the end, so a wav that is a second short arrives as
        # a film whose voice is a second late and whose last second is
        # quiet, and nothing anywhere says so.
        #
        # Measured on this machine: on an idle one the two agree to about
        # a tenth of a second. At a load average near ninety — seven
        # other builds — pulse took about a second to start delivering
        # samples, with the camera not even running. So this fires when
        # the machine is too busy to film on, which is a thing worth
        # being told before the take rather than after it.
        heard = probe_audio_seconds(wav)
        took = self.stopped_at - self.started_at
        if heard is None:
            print(f"  narration              ~{size / 96_000:.0f}s captured "
                  f"(length unreadable)")
            return
        off = heard - took
        print(f"  narration              {heard:.1f}s captured against "
              f"{took:.1f}s of take ({off:+.1f}s)")
        if abs(off) > WALL_CLOCK_TOL_S:
            FAILURES.append(
                f"the narration is {heard:.1f}s of audio for a {took:.1f}s "
                f"take ({off:+.1f}s), so the voice sits that far from the "
                f"picture for the whole film — the mux pads the difference "
                f"with silence and cannot tell you. A short wav is usually "
                f"the source taking that long to start delivering samples "
                f"under load; this machine is at "
                f"{os.getloadavg()[0]:.0f}. A headset that dropped mid-take "
                f"looks the same from here.")

    # -- what actually happened --------------------------------------

    def rate(self) -> float:
        """Frames per second, as achieved. One track, so this is also
        the rate of whatever is on screen at any moment."""
        span = max(1e-6, self.stopped_at - self.started_at)
        return len(self.frames) / span

    def snapshot_ms(self) -> tuple[float, float]:
        """Mean and worst time one snapshot blocked its session, in ms."""
        if not self.blocked:
            return 0.0, 0.0
        return (sum(self.blocked) / len(self.blocked) * 1000.0,
                max(self.blocked) * 1000.0)

    def report(self) -> None:
        span = self.stopped_at - self.started_at
        per: dict[str, int] = {}
        for _t, _rel, name, _held in self.frames:
            per[name] = per.get(name, 0) + 1
        mean_ms, worst_ms = self.snapshot_ms()
        print(f"\ncaptured over {span:.1f}s of demo: {len(self.frames)} frames, "
              f"{self.rate():.2f} frames/s")
        print(f"  snapshot blocked its session {mean_ms:.0f} ms on average "
              f"(worst {worst_ms:.0f} ms)")
        # The frame rate of this video *is* the compositor's snapshot
        # speed, so a recording made on a busy machine measures the rest
        # of the machine. Worth saying outright: the number above is
        # otherwise indistinguishable from qOS being slow.
        if mean_ms > SNAPSHOT_BUDGET_MS:
            print(f"  ! that is well over the ~{SNAPSHOT_BUDGET_MS:.0f} ms an "
                  f"idle machine takes at {VIDEO_W}x{VIDEO_H}. Parallel "
                  f"builds or other compositors are the usual cause; the "
                  f"frame rate above is what they cost this recording.")
        print("  time on camera:")
        for s in self.sessions:
            n = per.get(s.name, 0)
            print(f"    {s.name:8} {n:5d} frames  "
                  f"{n / max(1e-6, self.rate()):.1f}s")
        for e in self.errors:
            print(f"  ! {e}")

    def beat_report(self, marks: list[tuple[str, float]], fps: int) -> None:
        """How many seconds of the finished video each beat accounts for.

        Not wall clock: the sum of the screen time actually given to the
        frames captured during that beat, after trimming and after any
        stretch the camera was down for. So it answers the only question
        that matters when a cut is too long — which beat to take it out
        of — instead of inviting somebody to guess.
        """
        if not marks or not self.frames:
            return
        t_start, t_end = self._window()
        timeline = self._timeline(t_start, t_end, fps)
        # `_timeline` collapses runs, so walk the frames it kept and
        # charge each one's screen time to the beat it was taken in.
        kept = {rel: seconds for rel, _name, seconds in timeline}
        edges = [t for _title, t in marks] + [float("inf")]
        spend = [0.0] * len(marks)
        for taken, rel, _name, _held in self.frames:
            if rel not in kept:
                continue
            for i in range(len(marks)):
                if edges[i] <= taken < edges[i + 1]:
                    spend[i] += kept[rel]
                    break
        total = sum(spend)
        print("  of the finished video:")
        for (title, _t), seconds in zip(marks, spend):
            share = 100.0 * seconds / total if total else 0.0
            bar = "#" * int(round(share / 3))
            print(f"    {seconds:6.1f}s  {share:4.1f}%  {bar:<12} "
                  f"{title[:52]}")

    # -- composition -------------------------------------------------

    def _window(self) -> tuple[float, float]:
        """The take, as the microphone heard it: from the statement that
        started the camera to the one that stopped it.

        Not the first and last frame. The camera opens a moment after
        the recorder starts and the last frame lands a moment before it
        stops, so a picture spanning only the frames is that much
        shorter than the voice recorded beside it — and a narrated take
        is only usable while the two spans are the same span.

        Falls back to the frames when the recorder was never started or
        stopped, which is the state a test constructing frames by hand
        is in.
        """
        if not self.frames:
            raise RuntimeError("no frames were captured")
        first, last = self.frames[0][0], self.frames[-1][0]
        start = self.started_at if 0.0 < self.started_at <= first else first
        end = self.stopped_at if self.stopped_at >= last else last
        return start, end

    def _timeline(self, t_start: float, t_end: float,
                  fps: int) -> list[tuple[str, str, float]]:
        """`(relative path, session, seconds on screen)`, in order.

        Each frame is shown from when it was taken until the next one
        was, so the video runs on the demo's own clock and is simply as
        dense as the camera managed. With one track there is nothing to
        pad against: no frame in the file is ever repeated, and none is
        interpolated.

        `max_hold` is the one edit. A session that is waiting — for a
        sandbox to spawn, for a package to unpack, for a change to cross
        a network — renders the same screen for as long as it waits, and
        filming that honestly still means minutes of a still picture.
        Capping how long one unchanged frame stays up trims that dead
        air. It cannot invent anything: every frame was really rendered
        by the session named on it, in this order, and a cap only ever
        shows one for *less* time than it was live. What it costs is that
        the video is no longer wall-clock; `trimmed` reports how much.
        """
        frames = self.frames
        if not frames:
            raise RuntimeError("no frames to compose")
        tick = 1.0 / fps
        cap = self.max_hold if self.max_hold > 0 else float("inf")
        out: list[tuple[str, str, float]] = []

        # How long each frame was the live state, before any editing.
        #
        # Its real lived time, and nothing else. This used to floor every
        # frame at one tick of `--fps`, which is harmless only while the
        # camera is the slower of the two. It is not: at 55 frames/s
        # against `--fps 30` the floor gave every frame a 33 ms slot for
        # the 18 ms it actually held the screen, and a four-and-a-half
        # minute take composed to nearly nine. A silent film merely ran
        # slow. A narrated one put the voice half a film ahead of the
        # picture, and reported `trimmed 0` while doing it.
        #
        # The container rate is a property of the file, not of the demo.
        # Resampling to it is the `fps=` filter's job in `compose`, where
        # a surplus frame is dropped and the timing survives, rather than
        # this loop's, where padding a frame stretches the take.
        lived: list[float] = []
        for i, (t, _rel, _name, _held) in enumerate(frames):
            begins = t_start if i == 0 else t
            # The last frame was the live state until the camera stopped.
            ends = frames[i + 1][0] if i + 1 < len(frames) else t_end
            # These telescope to exactly `t_end - t_start`, which is what
            # makes the picture the length of the take. `max(0.0, …)`
            # guards a clock that stepped backwards mid-run and gives up
            # that exactness if one did — which is why the total below is
            # measured rather than assumed.
            lived.append(max(0.0, ends - begins))

        # Runs of *identical* frames are the dead air, and capping a
        # single frame's screen time never touched them: the camera takes
        # a fresh snapshot every fifth of a second, so a minute of an
        # unchanging screen arrives as three hundred separate files that
        # each lived a fifth of a second and none of which was ever held.
        # A previous cut trimmed one second out of two hundred and forty
        # while a third of it was a still page.
        #
        # So identical neighbours are collapsed: one of them is kept and
        # shown for at most `max_hold`, and the rest are dropped. That
        # takes frames out and never puts any in — nothing is repeated,
        # nothing is interpolated, and every frame in the file is still a
        # real render shown for no longer than its run was live.
        runs: list[tuple[int, int]] = []          # (index, count)
        digests = [self._digest(rel) for _t, rel, _n, _h in frames]
        i = 0
        while i < len(frames):
            j = i + 1
            # A run has to stay on one session: a cut is a change even
            # when the two screens happen to be identical.
            while (j < len(frames)
                   and digests[j] == digests[i]
                   and digests[i] is not None
                   and frames[j][2] == frames[i][2]
                   # A hold that begins part-way through an unchanging
                   # screen starts its own run, so the cap lifts exactly
                   # where the demo asked it to.
                   and frames[j][3] == frames[i][3]):
                j += 1
            runs.append((i, j - i))
            i = j

        self.capped = 0.0
        last_run = len(runs) - 1
        for idx, (start, count) in enumerate(runs):
            _t, rel, name, held = frames[start]
            was_live = sum(lived[start:start + count])
            # A frame the demo deliberately stopped on is shown for as
            # long as it was live. The cap is for the other kind of still
            # picture — a session waiting — and cannot tell the two apart
            # by looking at the pixels, so it is told.
            shown = was_live if held else min(was_live, max(tick, cap))
            self.capped += was_live - shown

            # The concat demuxer ignores the duration written for the
            # *last* entry and shows that frame for as long as the entry
            # before it — measured here against ffmpeg 8.1.2, and it is
            # the slideshow gotcha in ffmpeg's own wiki.
            #
            # That lands on the worst possible frame. This demo ends on a
            # deliberate hold, a hold is a run of identical frames, and a
            # run collapses to one entry — so the payoff shot was the
            # last entry, inheriting whatever its neighbour got, and the
            # film stopped dead on the thing it was built to reach.
            #
            # So the tail is spelled out instead of collapsed: the final
            # run hands over its last two frames as entries of one tick
            # each, and the ignored duration inherits a tick. Both are
            # real frames from that same run, in capture order, shown for
            # less time than they were live — the run is written out at
            # its end rather than summarised, and nothing is repeated or
            # invented to do it.
            if idx == last_run and count >= 3 and shown > 2 * tick:
                out.append((rel, name, shown - 2 * tick))
                out.append((frames[start + count - 2][1], name, tick))
                out.append((frames[start + count - 1][1], name, tick))
            else:
                out.append((rel, name, shown))

        # What the video costs against the wall clock, *measured* — the
        # take's own span, less the screen time this timeline actually
        # hands out.
        #
        # It used to be `self.capped`: the sum of the edits this loop
        # knew it was making. That is only the same number while nothing
        # else changes the length, and something else did — the frame
        # floor above doubled the picture underneath this accounting, so
        # the one figure whose job is to say "this is no longer wall
        # clock" reported 0 through a take that had drifted by four
        # minutes. A difference is not the sum of the causes you thought
        # of, so it is subtracted rather than accumulated: anything that
        # ever changes the length again lands here whether or not this
        # loop was told about it. Negative means the video came out
        # *longer* than the take, which nothing is allowed to do.
        self.timeline_s = sum(seconds for _rel, _n, seconds in out)
        self.wall_s = t_end - t_start
        self.trimmed = self.wall_s - self.timeline_s
        return out

    def _digest(self, rel: str) -> str | None:
        """A frame's bytes, as a hash, or None when it cannot be read.

        The compositor encodes the same scene to the same PNG, so equal
        bytes mean an identical screen. An unreadable frame gets None,
        which compares unequal to everything including itself, so a
        frame that could not be hashed is never collapsed into a run.
        """
        try:
            with open(os.path.join(self.dir, rel), "rb") as f:
                return hashlib.sha1(f.read()).hexdigest()
        except OSError:
            return None

    def _concat_list(self, timeline: list[tuple[str, str, float]]) -> str:
        lines = ["ffconcat version 1.0"]
        for rel, _name, seconds in timeline:
            lines.append(f"file '{rel}'")
            # Microseconds, not tenths of a millisecond. A frame now
            # carries the time it really lived rather than a rounded tick,
            # and a take is fifteen thousand of them: rounding each to
            # 1e-4 can bias the total by up to three quarters of a second,
            # which is the same size as the tolerance the readback check
            # allows. 1e-6 puts that error two orders below the check.
            lines.append(f"duration {seconds:.6f}")
        with open(os.path.join(self.dir, "track.txt"), "w") as f:
            f.write("\n".join(lines) + "\n")
        return "track.txt"

    def _label_spans(self, timeline: list[tuple[str, str, float]],
                     fps: int) -> list[tuple[str, float, float]]:
        """When each session's name is on screen.

        Walked over the same timeline the track is built from, so the
        label always names the session whose pixels are showing — not
        the one the camera had just been asked for, and not a moment
        that trimming has since moved.
        """
        spans: list[tuple[str, float, float]] = []
        at = 0.0
        for _rel, name, seconds in timeline:
            if spans and spans[-1][0] == name:
                spans[-1] = (name, spans[-1][1], at + seconds)
            else:
                spans.append((name, at, at + seconds))
            at += seconds

        # Snap each boundary back to the frame grid the picture is on,
        # rounding *down*.
        #
        # The caption used to be late at every cut — measured at 2 to 9
        # frames across a 521 s take, so for 67 to 300 ms the film showed
        # one machine's screen under the other machine's name. That is
        # the thing that reads as the band captioning the wrong machine,
        # and it is not a disagreement about which session a frame came
        # from: these spans are walked over the very timeline the concat
        # track is built from, so the name always belongs to the pixels.
        #
        # It is a disagreement about *when*. These boundaries accumulate
        # the frames' own float durations, while `drawtext` is evaluated
        # against output timestamps that `fps=` has quantised onto the
        # container's grid. A boundary landing anywhere inside a tick
        # turns the caption on at the *end* of that tick, so the caption
        # can only ever be late, never early — which is exactly the
        # one-sided error that was measured.
        #
        # Flooring puts the boundary on the grid, and puts it on the
        # frame the cut is on rather than the one after it. A caption
        # that changes a frame early is invisible; one that changes a
        # frame late is the defect.
        # The end is pulled back half a tick as well as floored. Snapping
        # puts each boundary exactly on a frame time, and `between` is
        # inclusive at both ends — so two consecutive spans sharing a
        # boundary would both be enabled on that one frame and draw two
        # names over each other. Half a tick is inside the gap between
        # two frames, so it excludes the next span's first frame without
        # ever excluding this span's last one.
        tick = 1.0 / fps
        snapped = [
            (
                name,
                math.floor(a / tick) * tick,
                math.floor(b / tick) * tick - tick / 2.0,
            )
            for name, a, b in spans
        ]
        return [s for s in snapped if s[2] - s[1] >= tick / 2.0]

    def _frame_ssim(self, rel_a: str, rel_b: str) -> float | None:
        """How alike two captured frames are, 0..1. `None` if unreadable.

        The frames on disk are the sessions' own snapshots and carry no
        band — that is drawn at compose time — so this compares exactly
        the part of the picture a viewer uses to tell one screen from
        another, with the one element that always differs excluded by
        construction rather than by a crop that could drift.

        Structural similarity rather than PSNR. Most of this film is a
        dark desktop, and a large flat area drags PSNR up until it stops
        discriminating: two screens 40 s and a whole app apart measured
        50 dB, which is the same neighbourhood as two frames a
        thirtieth of a second apart. SSIM separated the same pair 0.60
        against 0.99999.
        """
        a = os.path.join(self.dir, rel_a)
        b = os.path.join(self.dir, rel_b)
        try:
            done = subprocess.run(
                ["ffmpeg", "-hide_banner", "-i", a, "-i", b,
                 "-lavfi", "ssim", "-f", "null", "-"],
                capture_output=True, text=True, timeout=60)
        except (OSError, subprocess.SubprocessError):
            return None
        m = re.search(r"All:([0-9.]+)", done.stderr)
        return float(m.group(1)) if m else None

    def _check_visible_cuts(self, timeline: list[tuple[str, str, float]]) -> None:
        """Refuse a cut between two screens that look the same.

        See `CUT_VISIBLE_SSIM`. The band names the machine, and it names
        it correctly; what it cannot do is make a cut visible that the
        pictures either side of it do not show. When those pictures
        match, the only thing that changes at the cut is the caption,
        and the film reads as having captioned the wrong machine.

        Measured on the frames rather than reasoned about from the
        beats: which two screens happen to be identical depends on what
        the demo was doing at that moment, and that moves.
        """
        at = 0.0
        for i, (rel, name, seconds) in enumerate(timeline):
            if i and name != timeline[i - 1][1]:
                s = self._frame_ssim(timeline[i - 1][0], rel)
                if s is not None and s >= CUT_VISIBLE_SSIM:
                    FAILURES.append(
                        f"the cut to {name} at {at:.1f}s is invisible: the "
                        f"frame before it and the frame after it match at "
                        f"SSIM {s:.4f}, so the picture does not change and "
                        f"only the band does. A viewer reads that as the "
                        f"band naming the wrong machine. The band is right "
                        f"— cut where the two desktops differ instead.")
            at += seconds

    def _caption(self, slug: str, text: str) -> str:
        """Caption text goes in a file, not in the filter graph: drawtext
        reads it verbatim, so an em-dash or an apostrophe in the
        transcript cannot break the graph it is quoted into."""
        directory = os.path.join(self.dir, "captions")
        os.makedirs(directory, exist_ok=True)
        rel = f"captions/{slug}.txt"
        with open(os.path.join(self.dir, rel), "w") as f:
            f.write(text)
        return rel

    def compose(self, out_path: str, fps: int, size: str) -> dict:
        if shutil.which("ffmpeg") is None or shutil.which("ffprobe") is None:
            raise RuntimeError("ffmpeg and ffprobe are needed to compose the "
                               "video; the frames are still on disk")
        bold = _pick_font(FONTS_BOLD)
        t_start, t_end = self._window()
        # Nothing to time a video against if the demo never got going.
        if t_end - t_start < 1.0:
            raise RuntimeError(
                f"only {t_end - t_start:.2f}s of capture; too little to compose")

        source_w, source_h = (int(v) for v in size.lower().split("x"))
        timeline = self._timeline(t_start, t_end, fps)
        rel_track = self._concat_list(timeline)
        self._check_visible_cuts(timeline)

        chain = [f"fps={fps}"]
        # The band sits *below* the screen rather than over it, and that
        # is a property of the *composition*, not of the source size.
        #
        # It used to be a property of the source size: a session that
        # rendered at exactly `RECORD_SIZE` was padded downwards and the
        # band landed on empty canvas, and every other size was fitted
        # into the whole 1080 and then had the band drawn on top of it.
        # A windowed take is exactly that other case — the window manager
        # granted 1920x1048 against the 1920x1014 asked for — so the film
        # went out with 66 px of scrim across the bottom of the picture
        # and the word processor's Margins / Spacing / Page layout bar
        # rendered as dim grey text inside it. The band was doing the one
        # thing RECORD_SIZE exists to stop it doing.
        #
        # So the frame is fitted into the height the video can actually
        # show — `VIDEO_H - BAND_H` — whatever it arrives at, and the
        # strip below is always empty canvas. A session already at
        # `RECORD_SIZE` still goes in 1:1 with nothing resampled, because
        # fitting it computes a scale of exactly 1.
        shown_h = VIDEO_H - BAND_H
        k = min(VIDEO_W / source_w, shown_h / source_h)
        w, h = _even(source_w * k), _even(source_h * k)
        if (w, h) != (source_w, source_h):
            print(f"  {size} is not {RECORD_SIZE}; frames are resampled to "
                  f"{w}x{h} and letterboxed above the band. Record at "
                  f"{RECORD_SIZE} for 1:1.")
            chain.append(f"scale={w}:{h}:flags=lanczos")
        chain.append(f"pad={VIDEO_W}:{VIDEO_H}:{(VIDEO_W - w) // 2}:"
                     f"{(shown_h - h) // 2}:color={BG}")
        chain.append("format=rgb24")
        # The scrim is always up, so the band does not blink between
        # cuts — only the name inside it changes.
        chain.append(f"drawbox=x=0:y={VIDEO_H - BAND_H}:w={VIDEO_W}:"
                     f"h={BAND_H}:color={BAND_SCRIM}:t=fill")

        def label(rel: str, enable: str) -> str:
            return "drawtext=" + ":".join([
                f"fontfile={bold}", f"textfile={rel}", f"fontcolor={FG}",
                "fontsize=27", f"x={BAND_PAD_X}",
                f"y={VIDEO_H - BAND_H}+({BAND_H}-text_h)/2",
                f"enable='{enable}'",
            ])

        for i, (name, a, b) in enumerate(self._label_spans(timeline, fps)):
            # Capitalised on the band, lowercase everywhere else. The
            # session identity really is `quinten` — it is a unix user,
            # and the roster, the room document and every log line spell
            # it that way. But this band is prose about a person, in a
            # film, and a person's name has a capital in it. Leaving the
            # login on screen reads as an unfinished caption rather than
            # as fidelity to the system underneath.
            who = name[:1].upper() + name[1:]
            text = f"{who} · {ROLES[name]}" if name in ROLES else who
            chain.append(label(self._caption(f"n{i:03d}", text),
                               f"between(t,{a:.3f},{b:.3f})"))
        chain.append("format=yuv420p")

        graph = "[0:v]" + ",".join(chain) + "[v]"

        # The narration, if one was recorded. Both tracks began on the
        # same statement, so they *start* aligned and the audio rides
        # along from there.
        #
        # They stay aligned only while the picture is the same length as
        # the take, and two separate things have broken that.
        #
        # `max_hold` breaks it on purpose: `_timeline` collapses runs of
        # identical frames, and a live microphone cannot be collapsed to
        # match, so every second taken out of the video is a second the
        # voice arrives late for. `yc-demo-live.py` refuses to start a
        # narrated take with trimming on.
        #
        # The frame floor broke it by accident, and that is the more
        # instructive half. `_timeline` used to give every frame at least
        # one tick of `--fps`, so a camera running faster than the
        # container rate stretched the film — 4:30 of take composed to
        # 8:41 — while the refusal above sat there satisfied, because the
        # take really had been recorded at `--max-hold 0`. This comment
        # said the two tracks "need no syncing", qualified as true at
        # `max_hold == 0`; the qualification was the wrong one, and being
        # written down is what made it survive.
        #
        # So the invariant is now checked rather than reasoned about:
        # `_check_length` reads the finished file back and refuses any
        # narrated take whose picture is not the length of its take,
        # whatever made it that way. Nothing about the mux can rescue a
        # film that is already the wrong length.
        #
        # It is padded with silence first, and that is not a detail. The
        # two tracks are the same length only when nothing goes wrong;
        # a headset that drops, a pulse source that stalls, an ffmpeg
        # that dies quietly at minute one all end the audio early. With
        # `-shortest` alone the *picture* is what gets trimmed to match,
        # so a fault in the microphone would silently cut the end off
        # the film -- including the shot the whole thing is built to
        # reach. `apad` makes the audio arbitrarily long, so `-shortest`
        # can only ever trim it back to the video.
        wav = os.path.join(self.dir, "narration.wav")
        has_audio = os.path.isfile(wav) and os.path.getsize(wav) > 1024
        if has_audio:
            graph += ";[1:a]apad[a]"
        cmd = ["ffmpeg", "-y", "-v", "error",
               "-f", "concat", "-safe", "0", "-i", rel_track]
        if has_audio:
            cmd += ["-i", "narration.wav"]
        with open(os.path.join(self.dir, "filter.txt"), "w") as fh:
            fh.write(graph + "\n")
        cmd += ["-filter_complex", graph, "-map", "[v]"]
        if has_audio:
            cmd += ["-map", "[a]", "-c:a", "aac", "-b:a", "160k", "-shortest"]
        cmd += ["-c:v", "libx264", "-preset", "medium", "-crf", "20",
                "-pix_fmt", "yuv420p", "-r", str(fps),
                "-movflags", "+faststart", os.path.abspath(out_path)]
        # Relative paths throughout the graph, run from the recording
        # directory: a run directory with a colon or a quote in its name
        # would otherwise be read as filter syntax.
        done = subprocess.run(cmd, cwd=self.dir, capture_output=True, text=True)
        if done.returncode != 0:
            raise RuntimeError(
                f"ffmpeg failed ({done.returncode}):\n{done.stderr.strip()}")
        info = probe_video(os.path.abspath(out_path))
        self._check_length(info, fps)
        return info

    def _check_length(self, info: dict, fps: int) -> None:
        """Does the finished file last as long as the take did?

        The loop this closes: every number the rig printed about the last
        broken take was true, and none of them was this one. The frame
        rate was real, the frame count was real, `probe_video` read the
        duration back off the file rather than trusting ffmpeg — and
        nothing ever put that duration beside the wall clock, so a take
        that composed to twice its length passed every check the rig had
        and was found by watching it.

        Four comparisons, because they fail for different reasons and
        because a check that can only see through this recorder's own
        bookkeeping is a check that goes blind whenever the bookkeeping
        is what broke — which is what happened.
        """
        # A frame or three: the `fps=` filter quantises the tail of the
        # last frame to the container's grid, which is real and bounded
        # and about a tick wide. Anything larger is a length that moved.
        tol = max(WALL_CLOCK_TOL_S, 3.0 / max(1, fps))

        # 1. The file against the cut we handed ffmpeg. Catches the
        #    composer: a concat list ffmpeg read differently than it was
        #    written, an `fps=` filter doing something other than
        #    resampling.
        drift = info["duration"] - self.timeline_s
        if abs(drift) > tol:
            raise RuntimeError(
                f"the composed file is {info['duration']:.1f}s but the "
                f"timeline it was built from is {self.timeline_s:.1f}s "
                f"({drift:+.1f}s). ffmpeg did not write the cut it was "
                f"given; the frames and {self.dir}/track.txt are on disk.")

        # 2. The file against the clock on the wall, in the one direction
        #    that is structurally impossible.
        #
        #    A film can be shorter than its take — that is what
        #    `--max-hold` is for. It cannot be longer. Every frame is
        #    shown for at most the time it was live, a hold is shown for
        #    exactly that, and the lot of them span the take; there is no
        #    setting, no cap and no argument that makes the sum come out
        #    over. So a film that runs long is not a film of this take,
        #    whatever produced it.
        #
        #    This is the check that needed no help from anything above.
        #    Both the numbers it reads are measurements — one off the
        #    finished file, one off the clock — so it survives being
        #    wrong about the timeline, which is the state the rig was
        #    actually in.
        over = info["duration"] - self.wall_s
        if over > tol:
            raise RuntimeError(
                f"the film is {info['duration']:.1f}s but the take it is a "
                f"film of was {self.wall_s:.1f}s ({over:+.1f}s). A picture "
                f"cannot outlast the thing it filmed: every frame is shown "
                f"for at most the time it was live. Something stretched it "
                f"— at {len(self.frames)} frames over "
                f"{self.wall_s:.1f}s the camera ran at "
                f"{self.rate():.1f}/s, and a container rate below that is "
                f"the usual cause. The file is at {info['path']}.")

        # 3. What the trim meant to remove against what the length
        #    actually lost. `capped` is the sum of the edits the timeline
        #    knew it was making; `trimmed` is the difference it measured.
        #    They agree unless something else moved the clock — which is
        #    the shape of the bug this was written for.
        stray = self.trimmed - self.capped
        if abs(stray) > tol:
            raise RuntimeError(
                f"the picture is {self.timeline_s:.1f}s against "
                f"{self.wall_s:.1f}s of take, and the still-picture cap "
                f"only accounts for {self.capped:.1f}s of that — "
                f"{stray:+.1f}s came from somewhere else. Every frame's "
                f"screen time should be the time it was live.")

        # 4. And a narrated take, both ways. Short is a fault here too:
        #    a live microphone cannot be trimmed to match a shortened
        #    picture, so every second taken out of the video is a second
        #    the voice arrives late for, and it accumulates.
        if info.get("audio") is None:
            return
        off = info["duration"] - self.wall_s
        if abs(off) > tol:
            raise RuntimeError(
                f"a narrated take has to run on the wall clock, and this "
                f"one does not: {info['duration']:.1f}s of picture against "
                f"{self.wall_s:.1f}s of take ({off:+.1f}s). The voice was "
                f"recorded live and cannot be stretched to match, so it "
                f"drifts further behind the further in you get. The file "
                f"is at {info['path']} and the frames are in {self.dir}.")


def probe_video(path: str) -> dict:
    """Read the finished file back. What ffmpeg was asked for and what it
    wrote are different claims, and only the second one is the artifact."""
    done = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_frames",
         "-show_entries",
         "stream=width,height,nb_read_frames,r_frame_rate,duration,codec_name",
         "-of", "json", path],
        capture_output=True, text=True)
    if done.returncode != 0:
        raise RuntimeError(f"ffprobe failed: {done.stderr.strip()}")
    streams = json.loads(done.stdout).get("streams") or []
    if not streams:
        raise RuntimeError(f"{path} has no video stream")
    s = streams[0]
    return {
        "path": path,
        "width": int(s["width"]),
        "height": int(s["height"]),
        "frames": int(s["nb_read_frames"]),
        "fps": s["r_frame_rate"],
        "duration": float(s["duration"]),
        "codec": s["codec_name"],
        "bytes": os.path.getsize(path),
        # None when the file is silent, which is a state and not a fault:
        # only a narrated take is required to be wall-clock.
        "audio": probe_audio_seconds(path),
    }


def probe_audio_seconds(path: str) -> float | None:
    """How long the audio in a file really is, or None if it has none.

    Same habit as `probe_video`, applied to the other track: the length
    of a recording is a fact about the file, and the only way to have it
    is to read the file.
    """
    done = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "a:0",
         "-show_entries", "stream=duration", "-of", "json", path],
        capture_output=True, text=True)
    if done.returncode != 0:
        return None
    streams = json.loads(done.stdout or "{}").get("streams") or []
    if not streams or "duration" not in streams[0]:
        return None
    try:
        return float(streams[0]["duration"])
    except (TypeError, ValueError):
        return None


# --------------------------------------------------------------------
# transcript
# --------------------------------------------------------------------

FAILURES: list[str] = []
GAPS: list[str] = []
# (title, wall clock when it started). Only so the report can say how
# many seconds of the finished video each beat accounts for — which is
# the question every attempt to shorten this film has had to answer by
# guessing. See `Recorder.beat_report`.
BEAT_MARKS: list[tuple[str, float]] = []

RULE = "-" * 70

# Set by `--record`. The transcript is the demo's narration, and it stays
# in the terminal: on the video the beats and claims were a wall of text
# competing with the screen underneath, so what the recording takes from
# these calls is timing — how long to hold — and nothing else.
RECORDER: Recorder | None = None


@contextlib.contextmanager
def easing(share: float = 4.0):
    """Film a measured beat without the camera in its numbers."""
    if RECORDER is None:
        yield
        return
    with RECORDER.easing(share):
        yield


#: Set by a rig that has windows to raise: called with an X11 window id
#: when the film cuts to the session owning it. Left None by rigs that
#: run headless, where there is nothing on screen to follow.
SHOW_ON_SCREEN = None


def cut(name: str) -> None:
    """Put `name` on camera for what follows.

    Cuts are expensive in a way that has nothing to do with what they
    cost to make: every one of them asks a viewer who has never seen this
    system to work out whose screen they are now looking at, and to hold
    two machines in their head at once. So this cut goes to xaver twice —
    once to watch him redeem the invitation, once at the end for what
    arrived on his screen without him doing anything — and is on quinten
    for everything in between, including the beats that are *about*
    xaver. Driving a session and filming it are separate: the far side
    can be opened, joined and read while the camera stays where the story
    is.

    Printed either way, so the transcript documents the intended order of
    shots even on a run that is not being recorded.
    """
    if name == NEVER_ON_CAMERA:
        raise RuntimeError(
            f"{name} is never on camera — see NEVER_ON_CAMERA. What the "
            f"agent does is shown on the screens of the people it does it "
            f"with, which is the claim; its own desktop is the one shot "
            f"that would contradict them.")
    # Said differently while the camera is down, because it is a
    # different fact: nothing is being shown to anybody, and all this
    # does is decide what the film comes back up on.
    down = RECORDER is not None and RECORDER.camera_is_down()
    print(f"\n  [ {'next up' if down else 'on camera'}: {name} ]", flush=True)
    if RECORDER:
        RECORDER.cut_to(name)

    # The screen follows the film.
    #
    # Somebody narrating a take live is describing what they can see, and
    # a cut to another machine used to leave them talking over footage
    # they had no view of. Any session that has a window is brought to
    # the front as the film reaches it, so the desktop in front of the
    # narrator is always the one being recorded, and the cut needs no
    # cue and no keystroke.
    #
    # This moves no pixel of the film. Frames are taken with a `snapshot`
    # op off each session's own framebuffer, never off the screen, so
    # which window is stacked in front of which is invisible to the
    # recording -- it only decides what a human in the room can watch.
    if SHOW_ON_SCREEN is not None:
        for s in SESSIONS:
            if s.name == name and getattr(s, "wid", None):
                SHOW_ON_SCREEN(s.wid)


@contextlib.contextmanager
def off_camera(why: str):
    """Run what follows without filming it.

    For a beat that has to happen — because a later one reads its result,
    or because it is part of what this run proves — and does not belong
    in a one-minute cut. It runs at full speed, every claim in it is
    asserted and printed, and none of it reaches the video.
    """
    print(f"\n  [ camera off — {why} ]", flush=True)
    if RECORDER is None:
        yield
        return
    with RECORDER.hold_off():
        yield
    print("\n  [ camera back on ]", flush=True)


@contextlib.contextmanager
def patience(seconds: float):
    """Give a wait this long on camera, and no longer.

    For the waits that are worth watching succeed and are not worth
    watching fail: an agent carrying out an instruction, a document
    crossing a network. See `Recorder.patience`.
    """
    if RECORDER is None:
        yield
        return
    with RECORDER.patience(seconds):
        yield


def read_hold(seconds: float = READ_HOLD_S) -> None:
    """Stop on what is on screen, long enough to read it.

    For the frames that carry words a viewer is supposed to take in — a
    token, the name an invitation was made out to, a card saying it is on
    the clipboard. Real elapsed time, sampled like everything else, and
    exempt from the still-picture trim so the hold survives into the cut.
    """
    if RECORDER:
        RECORDER.hold(seconds)


def banner(title: str) -> None:
    global _STEP_STARTED
    now = time.time()
    if _STEP_STARTED is not None and RECORDER:
        took = now - _STEP_STARTED
        if took >= 4.0:
            print(f"      ({took:.1f}s on the step above)", flush=True)
    _STEP_STARTED = None
    print(f"\n{RULE}\n  {title}\n{RULE}", flush=True)
    BEAT_MARKS.append((title, time.time()))
    if RECORDER:
        RECORDER.dwell()


#: Wall-clock at the last `step`, so each one can report what the one
#: before it cost. A per-beat total says which beat is too long; it does
#: not say which sentence inside it is, and the answer has twice now been
#: a wait that nobody meant to leave on camera rather than the thing the
#: beat is about.
_STEP_STARTED: float | None = None
_STEP_TEXT: str = ""


def step(text: str) -> None:
    global _STEP_STARTED, _STEP_TEXT
    now = time.time()
    if _STEP_STARTED is not None and RECORDER:
        took = now - _STEP_STARTED
        if took >= 4.0:
            print(f"      ({took:.1f}s on the step above)", flush=True)
    _STEP_STARTED, _STEP_TEXT = now, text
    print(f"\n  {text}", flush=True)
    if RECORDER:
        RECORDER.dwell()


def claim(description: str, actual, expected) -> None:
    good = actual == expected
    print(f"    [{'PASS' if good else 'FAIL'}] {description}", flush=True)
    if not good:
        print(f"           expected {expected!r}", flush=True)
        print(f"           observed {actual!r}", flush=True)
        FAILURES.append(description)


def gap(description: str) -> None:
    """Something this run could not verify. Its own mark, so it can never
    be mistaken for a pass."""
    print(f"    [GAP ] {description}", flush=True)
    GAPS.append(description)


def note(text: str) -> None:
    print(f"     .   {text}", flush=True)


def wait_for(fn, timeout: float = SYNC_TIMEOUT_S, interval: float = 0.6):
    # Polled rather than pushed, and the interval is a real cost: every
    # probe here is a `state_dump`, which blocks the session it is asked
    # of — including, during a recording, the session being filmed. A
    # tighter loop buys no accuracy and takes frames off the camera.
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = fn()
        if last:
            return last
        time.sleep(interval)
    return last


def peer_id_of(s: Session) -> str:
    try:
        with open(s.log_path) as f:
            for line in f:
                # tracing paints field names and separators, so the
                # escapes come off before the split.
                plain = re.sub(r"\x1b\[[0-9;]*m", "", line)
                if "local_peer_id=" in plain:
                    return plain.split("local_peer_id=")[1].split()[0].strip()
    except OSError:
        pass
    return "<unknown>"


class LogTail:
    """A session's own log, read as it is written.

    Everything a cross-client drag does is narrated here by the two
    processes doing it, and nowhere else a harness can reach: the file
    manager's rows are not in the compositor's widget registry, the
    compositor's drag label is not in `state_dump`, and the word
    processor's document is not on screen in any form a script can read.
    So this is not a shortcut around an API — it is the API, and every
    line below is one of the two processes reporting on itself.

    The app's stderr reaches this file because the launcher pipes a
    sandboxed app's output into the session's, which is also how
    `[app] broadcast doc delta` has always been readable.
    """

    #: The file manager, on a press that landed on a row. Names the path
    #: and the press position *in the client's own coordinates*, which
    #: together are enough to convert a miss into a correction rather
    #: than a re-guess.
    #: `at` is a `kurbo::Point`, whose `Debug` is `(x, y)` — *not*
    #: `Point { x: …, y: … }`, which is what this matched first and
    #: would never have found.
    ARMED = re.compile(
        r'drag armed\s+path="([^"]+)"\s+at=\((-?[\d.]+),\s*(-?[\d.]+)\)')
    #: The same press, once the pointer has travelled far enough.
    BEGAN = re.compile(r'drag begins\s+path="([^"]+)"')
    #: The app handing the gesture to the compositor — the moment it
    #: stops being an in-app drag and becomes a desktop-wide one.
    CARRIED = "carrying the drag out of this app as a client drag"
    #: The compositor, having accepted it.
    PROMOTED = "client started a drag"
    #: The compositor, on the drop.
    FINISHED = "drag finished"
    #: A `ui` app reporting the size of a change to its own document.
    DELTA = re.compile(r"broadcast doc delta (\d+)B")
    #: The file manager, having opened the compose surface. Names what the
    #: panel is about — the *address*, not the file's name — and the rect it
    #: was laid out at, in the client's own coordinates.
    COMPOSE = re.compile(
        r"remark compose opened\s+about=(\S+)\s+"
        r"at=\((-?[\d.]+),\s*(-?[\d.]+),\s*(-?[\d.]+),\s*(-?[\d.]+)\)")
    #: The same app, having sent one. `text` runs to the end of the line
    #: because a sentence has spaces in it.
    SENT = re.compile(
        r"remark sent\s+to=(\S+)\s+about=(\S+)\s+text=(.*)$", re.M)
    #: The host, having put it on the wire. A different process's account of
    #: the same event, which is what makes it worth reading separately: the
    #: app saying it composed a message and the toolkit saying it sent one are
    #: two claims, and only the second one means it left.
    #:
    #: The id is captured off its `m_` prefix, not a `message=` key: the host
    #: logs it through a tracing field literally named `message`, which the fmt
    #: subscriber renders as a bare value with no key at all — so the line reads
    #: "...into the room m_ab12... about=..." and a pattern expecting `message=`
    #: matches nothing. That mismatch stayed invisible for as long as the
    #: app-side claim above aborted this beat before anything read this line.
    ON_THE_WIRE = re.compile(r"sending a remark into the room\s+(m_\S+)")
    #: Any `ui` app, having laid out the resources it publishes: a map from
    #: **resource address** to the rect that address is being drawn at, in
    #: that window's own coordinates. Written by
    #: `ui::App::narrate_resource_rects` whenever the map changes, and by
    #: nothing else — so the last one for a surface is where things are now.
    #:
    #: This is the line that makes a client's contents addressable from
    #: outside it. A file row belongs to a client, so it is in no registry
    #: the compositor can walk, and a resource address deliberately carries
    #: no coordinates; between the two there was no way to aim a press at a
    #: named object and a harness had to hunt for one.
    RESOURCES = re.compile(
        r"resources laid out\s+surface=(\S+)\s+at=(\{.*\})\s*$", re.M)

    #: `tracing_subscriber::fmt::init()` colours its output whether or
    #: not the sink is a terminal, and the session's log is a file. So a
    #: field arrives as `path\x1b[0m\x1b[2m=\x1b[0m"…"` rather than
    #: `path="…"`, and a pattern written against what the line *reads*
    #: as matches nothing at all. Cost a whole run to find; stripped
    #: here, once, so no pattern below has to know about it.
    ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")

    def __init__(self, path: str) -> None:
        self.path = path
        self.at = 0
        self.text = ""

    def read(self) -> str:
        """Everything appended since the last read, decoloured."""
        try:
            with open(self.path, "rb") as fh:
                fh.seek(self.at)
                chunk = fh.read()
                self.at = fh.tell()
        except OSError:
            return ""
        fresh = self.ANSI.sub("", chunk.decode(errors="replace"))
        self.text += fresh
        return fresh

    def catch_up(self) -> None:
        """Skip whatever is already there without remembering it, so
        setup is never mistaken for the gesture."""
        self.read()
        self.text = ""

    def armed(self, fresh: str) -> tuple[str, float, float] | None:
        """The path and client-space point of the last press that landed
        on a row, or None if none did."""
        hits = self.ARMED.findall(fresh)
        if not hits:
            return None
        path, x, y = hits[-1]
        return path, float(x), float(y)

    def biggest_delta(self, fresh: str) -> int:
        return max((int(m) for m in self.DELTA.findall(fresh)), default=0)

    def compose_opened(self, text: str) -> tuple[str, tuple] | None:
        """The address and client-space rect of the last compose surface the
        file manager opened, or None if it opened none."""
        hits = self.COMPOSE.findall(text)
        if not hits:
            return None
        about, x, y, w, h = hits[-1]
        return about, (float(x), float(y), float(w), float(h))

    def remark_sent(self, text: str) -> tuple[str, str, str] | None:
        """Destination, address and body of the last remark sent."""
        hits = self.SENT.findall(text)
        return hits[-1] if hits else None

    #: How far back to look for a layout map. It is a *standing* fact rather
    #: than an event — the app says where things are when that changes and
    #: then stops — so the answer is often older than anything else this
    #: reads, and a tail that only covered the current beat would miss it on
    #: a window nobody has touched.
    LAYOUT_TAIL_BYTES = 4 << 20

    def surfaces_that_published(self) -> list[str]:
        """Which surfaces have said where they are drawing anything.

        For the report a miss writes. "The file manager published no
        layout" is three different situations — no app there at all, an
        app too old to answer, an app whose window belongs to a different
        session — and naming who *did* answer tells them apart.
        """
        try:
            with open(self.path, "rb") as fh:
                try:
                    fh.seek(-self.LAYOUT_TAIL_BYTES, os.SEEK_END)
                except OSError:
                    fh.seek(0)
                text = self.ANSI.sub("", fh.read().decode(errors="replace"))
        except OSError:
            return []
        seen: list[str] = []
        for said_by, _blob in self.RESOURCES.findall(text):
            if said_by not in seen:
                seen.append(said_by)
        return seen

    def resource_rects(self, surface: str) -> dict[str, tuple]:
        """Where `surface` says it is drawing each thing it publishes.

        Read off the whole log rather than off what arrived since the last
        `read()`, and deliberately: `catch_up` throws away everything before
        the gesture so that setup cannot be mistaken for it, and the layout
        map is exactly the kind of thing that was established during setup
        and is still true.

        The *last* map a surface published wins, because that is what the
        word means: each one supersedes the one before it.

        Returns rects in the app's own coordinates — its window's top-left
        as the origin. `screen_point` converts.
        """
        try:
            with open(self.path, "rb") as fh:
                try:
                    fh.seek(-self.LAYOUT_TAIL_BYTES, os.SEEK_END)
                except OSError:
                    fh.seek(0)
                text = self.ANSI.sub("", fh.read().decode(errors="replace"))
        except OSError:
            return {}
        out: dict[str, tuple] = {}
        for said_by, blob in self.RESOURCES.findall(text):
            if said_by != surface:
                continue
            try:
                loaded = json.loads(blob)
            except ValueError:
                # A line torn in half by a concurrent write. Not fatal and
                # not worth reporting: the next layout says it again.
                continue
            out = {k: tuple(v) for k, v in loaded.items()
                   if isinstance(v, list) and len(v) == 4}
        return out


    def await_line(self, needle: str, timeout: float) -> bool:
        """Wait for `needle` to appear, reading as it goes."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if needle in self.read() or needle in self.text:
                return True
            time.sleep(0.1)
        return False


def wait_until_painted(s: Session, timeout: float = 45.0) -> list[str]:
    """Wait until the shell is compositing every client window it holds.

    `frame_seq > 0` says the client produced a frame; `presented` says
    the *shell* is painting it. The two are seconds apart on a loaded
    machine, and in between the compositor answers `list_targets` and
    `foreign_frames` with laid-out rects for windows that do not appear
    in a snapshot at all — so a rig that trusts the rects stages a scene,
    photographs an empty desktop, and reports success. Seen twice.

    Returns the app ids still unpainted, so a caller can say which one
    rather than only that something was.
    """
    def unpainted() -> list[str]:
        return [str(f.get("app_id")) for f in s.foreign_frames()
                if not f.get("presented")]

    deadline = time.time() + timeout
    while time.time() < deadline:
        missing = unpainted()
        if not missing:
            return []
        time.sleep(0.5)
    return unpainted()


def painted_tiles(s: Session) -> dict[int, dict]:
    """Every client window this session is painting, by surface id.

    Off `foreign_frames`, which reports the rect the widget tree laid
    each tab into — the placement the compositor is actually painting.
    That matters more than it sounds: the same tile is registered in the
    widget registry under *two* names (`foreign-surface-<id>` and
    `foreign-surface-<id>[<tab>]`), so an exact-name lookup can come back
    empty while the window is plainly on screen. This has no names in it
    to get wrong.
    """
    out: dict[int, dict] = {}
    for f in s.foreign_frames():
        r = f.get("rect") or {}
        if not (r.get("w") and r.get("h")):
            continue
        sid = f.get("surface_id")
        if sid is None:
            continue
        out[int(sid)] = {
            "app_id": f.get("app_id"),
            "rect": (r["x"], r["y"], r["w"], r["h"]),
            "frame_seq": f.get("frame_seq"),
            "presented": f.get("presented"),
        }
    return out


def windows_off_screen(s: Session, viewport: tuple[int, int]) -> list[str]:
    """Which client windows are not wholly on screen, and by how much.

    Read off the compositor's own per-surface rects rather than off a
    picture, because "the left window is cut in half" and "the left
    window is behind the right one" look similar in a still and are
    different faults. A window that is not fully in frame is named here
    so a beat can refuse to claim what its own shot does not show.
    """
    w, h = viewport
    out = []
    for f in s.foreign_frames():
        r = f.get("rect") or {}
        x0, y0 = r.get("x", 0.0), r.get("y", 0.0)
        x1, y1 = x0 + r.get("w", 0.0), y0 + r.get("h", 0.0)
        over = []
        if x0 < -0.5:
            over.append(f"{-x0:.0f}px off the left")
        if x1 > w + 0.5:
            over.append(f"{x1 - w:.0f}px past the right")
        if y1 > h + 0.5:
            over.append(f"{y1 - h:.0f}px below the screen")
        if over:
            out.append(f"{f.get('app_id')} — {', '.join(over)}")
    return out


#: Height of the compositor's own top bar, in the same coordinates
#: `foreign_frames` reports tiles in. A click above this line reaches the
#: shell's chrome rather than any window, so it is the floor for anything
#: this file aims at a tab pill.
TOPBAR_H = 44.0


def windows_overlapping(s: Session) -> list[str]:
    """Which client windows are drawn on top of each other.

    The other half of "is this scene filmable", and the half nothing was
    checking. `windows_off_screen`'s own docstring says that "the left
    window is cut in half" and "the left window is behind the right one"
    look similar in a still and are different faults — and then it only
    detects the first. So a session painting one window full-screen
    across another passed "both windows are wholly on screen before the
    gesture", and the drag that followed was aimed into a stack: both
    tiles were inside the viewport, and one was underneath the other.

    A one-pixel seam between neighbouring tiles is not an overlap, so
    this is measured as a share of the smaller window's area rather than
    as any intersection at all.
    """
    tiles = [(str(t["app_id"]), t["rect"]) for t in painted_tiles(s).values()]
    out = []
    for i in range(len(tiles)):
        for j in range(i + 1, len(tiles)):
            (a_id, (ax, ay, aw, ah)) = tiles[i]
            (b_id, (bx, by, bw, bh)) = tiles[j]
            ox = max(0.0, min(ax + aw, bx + bw) - max(ax, bx))
            oy = max(0.0, min(ay + ah, by + bh) - max(ay, by))
            area = ox * oy
            smaller = min(aw * ah, bw * bh)
            if smaller > 0 and area / smaller > 0.05:
                out.append(f"{a_id} and {b_id} overlap by "
                           f"{100.0 * area / smaller:.0f}% of the smaller")
    return out


#: How much of the strip one group takes, for the whole film.
#:
#: Set once, before the first frame, and never changed again. It used to
#: be set per beat — a group takes about three quarters of the strip by
#: default, so opening a second window put it mostly past the right edge
#: and the beat then evened the shares out to bring it back. That worked,
#: and it did it *on camera*: every window jumped to a new size and
#: position a second after the app appeared, which reads as a glitch and
#: undoes the impression that the window opened where it belonged.
#:
#: 500 is half each, which is what two groups need on a 1920 output —
#: measured, not guessed: two 500-permille groups land at 291..1091 and
#: 1099..1899 beside the wheel disc, both whole.
#: Whether the film shows the machine fetching an application it does
#: not have.
#:
#: The capability is real and, since the palette started saying so, it is
#: finally visible: the row reads "Fetch and open …" beside a "not on
#: this machine" badge, and the open counts real megabytes. It is also
#: the one beat in this film that is not about collaboration or about
#: agents — it is package management, and it costs twelve seconds of a
#: budget of a hundred.
#:
#: So it is cut, and the fetch is exercised off camera anyway so the
#: transcript still proves the machine gained the app. `--fetch-on-camera`
#: puts it back.
FETCH_ON_CAMERA = False

FILM_GROUP_SHARE = 500

#: How far `frame_windows` may narrow the groups to get a shot's
#: subjects into frame before it gives up and says so. `WIDTH_NARROW_PERMILLE` in
#: the toolkit, which is a third of the strip — under that a window is
#: too small to read on film, and shrinking further would buy a frame
#: nobody can see anything in. A shot that will not fit above this floor
#: is a shot the beat should decline rather than fake.
NARROW_SHARE_FLOOR = 333


def establish_strip(s: Session) -> None:
    """Fix this session's group proportions before it is filmed.

    Called during setup, off camera. Every later beat then only ever
    *scrolls* the strip, which a viewer reads as navigation — never
    resizes it, which they read as a fault.
    """
    resp = s.set_group_width(FILM_GROUP_SHARE)
    if "err" in resp:
        note(f"{s.name}: could not set the strip's proportions - "
             f"{json.dumps(resp['err'])[:120]}")
    else:
        note(f"{s.name}: strip set to {FILM_GROUP_SHARE} permille per group "
             f"for the whole film; no beat resizes on camera")


def subject_name(subject) -> str:
    """What to call a subject in a sentence."""
    if isinstance(subject, dict):
        sid = subject.get("surface_id")
        app = subject.get("app_id")
        return (f"{str(app).rsplit('.', 1)[-1]} (surface {sid})" if app
                else f"surface {sid}")
    if isinstance(subject, int):
        return f"surface {subject}"
    return str(subject).rsplit(".", 1)[-1]


def tiles_for(s: Session, subject) -> list[dict]:
    """Every window on this screen that is `subject`, left to right.

    A subject is either an **app id** or a **window**: the dict
    `Session.launch` hands back, or a bare surface id.

    Both, because an app id is not an identifier here. A wayland client
    announces whatever app-id it likes, and two of this film's four
    applications announce the same one — the strip came back reading
    `file-explorer, app, word-processor` for four windows, because the
    image editor and the chat both call themselves `app`. Asking for
    `dev.qos.imager` by name found nothing while its window was plainly
    on screen, and asking for `app` would have been a coin toss between
    two different programs.

    A surface id is the compositor's own handle on one client and is
    never ambiguous, so a beat that has been handed one uses it. App ids
    stay for the beats written against them, where they are unambiguous
    — one window per app on screen.
    """
    laid = painted_tiles(s)
    if isinstance(subject, dict):
        subject = subject.get("surface_id")
    if isinstance(subject, int):
        got = laid.get(int(subject))
        return [dict(got, surface_id=int(subject))] if got else []
    return sorted((dict(t, surface_id=sid) for sid, t in laid.items()
                   if t["app_id"] == subject),
                  key=lambda t: t["rect"][0])


def best_tile(s: Session, subject, viewport: tuple[int, int]) -> dict | None:
    """The window for `subject` that is furthest into frame.

    Two windows can be the same subject by the end of this film — a
    session's own, and a peer's, replicated in. They show the same
    document, so a beat only needs whichever of them is framed.
    """
    w, _h = viewport
    best = None
    for t in tiles_for(s, subject):
        x0, _y0, tw, _th = t["rect"]
        over = max(0.0, -x0) + max(0.0, (x0 + tw) - w)
        if best is None or over < best[0]:
            best = (over, t)
    return best[1] if best else None


def subjects_off_screen(s: Session, subjects: list,
                        viewport: tuple[int, int]) -> list[str]:
    """Which of `subjects` is not wholly on screen, and by how much.

    `windows_off_screen` asks it of every window there is. On a strip
    that scrolls, most of them are off the edges most of the time and
    that is the layout working, so the question a beat can act on is
    narrower: are the windows *this shot is about* whole.
    """
    w, _h = viewport
    out = []
    for subject in subjects:
        t = best_tile(s, subject, viewport)
        if t is None:
            out.append(f"{subject_name(subject)} - no window on screen is it")
            continue
        x0, _y0, tw, _th = t["rect"]
        said = []
        if x0 < -0.5:
            said.append(f"{-x0:.0f}px off the left")
        if x0 + tw > w + 0.5:
            said.append(f"{x0 + tw - w:.0f}px past the right")
        if said:
            out.append(f"{subject_name(subject)} - {', '.join(said)}")
    return out


def subjects_overlapping(s: Session, subjects: list,
                         viewport: tuple[int, int]) -> list[str]:
    """Whether the windows this shot is about are drawn over each other.

    The other half of "is this scene filmable", asked of the subjects
    for the same reason as `subjects_off_screen`. A window a beat is not
    about may sit under another one all it likes.
    """
    named = [(subject_name(sub), best_tile(s, sub, viewport))
             for sub in subjects]
    named = [(n, t) for n, t in named if t]
    out = []
    for i in range(len(named)):
        for j in range(i + 1, len(named)):
            (a_id, a) = named[i]
            (b_id, b) = named[j]
            if a["surface_id"] == b["surface_id"]:
                continue
            (ax, ay, aw, ah) = a["rect"]
            (bx, by, bw, bh) = b["rect"]
            ox = max(0.0, min(ax + aw, bx + bw) - max(ax, bx))
            oy = max(0.0, min(ay + ah, by + bh) - max(ay, by))
            area = ox * oy
            smaller = min(aw * ah, bw * bh)
            if smaller > 0 and area / smaller > 0.05:
                out.append(f"{a_id} and {b_id} overlap by "
                           f"{100.0 * area / smaller:.0f}% of the smaller")
    return out


def subject_overflow(s: Session, subjects: list,
                     viewport: tuple[int, int]) -> tuple[float, float]:
    """`(widest subject, how far the subjects run off the side)`, in px.

    The arithmetic `frame_windows` narrows by. Read off the subjects
    rather than off the whole strip, which on a scrolling strip is
    always wider than the screen — narrowing towards *that* would shrink
    every window towards unreadability chasing a fit that is not the
    point.
    """
    w, _ = viewport
    widest, over = 1.0, 0.0
    for subject in subjects:
        t = best_tile(s, subject, viewport)
        if t is None:
            continue
        x0, _y0, tw, _th = t["rect"]
        widest = max(widest, tw)
        over = max(over, max(0.0, -x0) + max(0.0, (x0 + tw) - w))
    return widest, over


def strip_order(s: Session) -> list[dict]:
    """The active strip's groups, left to right, with the surfaces in
    each and which one is focused — read off the tree, which is where
    the order lives.

    `painted_tiles` gives on-screen rects, and a group scrolled off the
    end still has one; what it cannot give is *which group comes after
    which*, and that is what a strip walk needs.
    """
    tree = s.tree()
    out: list[dict] = []
    for ws in tree["workspaces"]:
        if ws["id"] != tree["active"]:
            continue
        for strip in ws.get("strips", []):
            sid = str(strip.get("id"))
            active = (tree.get("active_group") or {}).get(sid)
            for group in strip.get("window_groups", []):
                surfaces = []
                windows = []
                for t in group.get("tabs", []):
                    tile = t.get("tile") or {}
                    foreign = (tile.get("Foreign")
                               if isinstance(tile, dict) else None)
                    surface = (foreign or {}).get("surface_id")
                    if surface is not None:
                        surfaces.append(int(surface))
                    windows.append({
                        "tab": t.get("id"),
                        "surface_id": (int(surface) if surface is not None
                                       else None),
                        "app_id": (foreign or {}).get("app_id"),
                    })
                tabs = [t.get("id") for t in group.get("tabs", [])]
                out.append({"id": group.get("id"), "strip": sid,
                            "surfaces": surfaces,
                            "tabs": tabs,
                            # Every tab, with what it *is*, whether or
                            # not the group is currently showing it.
                            # `painted_tiles` cannot answer that: a tab
                            # its group is not showing has no laid-out
                            # rect, so every route through the pixels
                            # reports it as absent from a screen it is
                            # plainly a tab of.
                            "windows": windows,
                            "active_tab": group.get("active_tab"),
                            "active": group.get("id") == active})
    return out


def window_order(order: list[dict]) -> list[tuple[int, object]]:
    """The strip flattened into windows, left to right, as
    `(group index, tab id)`.

    The unit the jump gesture actually moves in. `step_active_window`
    in `client_input.rs` flattens the strip's groups into their tabs and
    steps one *tab* per notch — so a notch inside a two-tab group moves
    a window without moving the group. A walk that counts groups reads
    that as the strip refusing to move.
    """
    return [(i, tab) for i, g in enumerate(order) for tab in g["tabs"]]


def active_window(order: list[dict]) -> tuple[int, object] | None:
    """Where the strip's focus is, in the same unit."""
    for i, g in enumerate(order):
        if g["active"]:
            return (i, g.get("active_tab"))
    return None


def group_index_of(s: Session, subject,
                   order: list[dict] | None = None) -> int | None:
    """Where along the strip the group holding `subject` sits."""
    order = strip_order(s) if order is None else order
    if isinstance(subject, dict):
        subject = subject.get("surface_id")
    if isinstance(subject, int):
        return next((i for i, g in enumerate(order)
                     if int(subject) in g["surfaces"]), None)
    wanted = {t["surface_id"] for t in tiles_for(s, subject)}
    return next((i for i, g in enumerate(order)
                 if wanted & set(g["surfaces"])), None)


def window_slot(s: Session, subject,
                order: list[dict] | None = None) -> tuple[int, object] | None:
    """Which *window* `subject` is, as `(group index, tab id)`.

    The unit `window_order` and `active_window` speak, and the unit the
    strip's own navigation moves in — so a walk can be asked to arrive
    at a particular tab rather than merely at the group holding it.

    Read off the tree rather than off the painted tiles, and that is the
    whole reason it exists. A group shows one tab at a time; the others
    have no laid-out rect, so `tiles_for` — and everything built on it,
    `group_index_of` included — answers "nothing on screen is that" for
    a window that is a tab of a group in the middle of the frame. The
    film's one cut went to xaver's word processor through that route,
    found it absent because his group was showing the image editor,
    left the strip alone, said so in a note nobody reads, and put an
    empty photo viewer under the caption that carries the whole
    argument.
    """
    order = strip_order(s) if order is None else order
    if isinstance(subject, dict):
        subject = subject.get("surface_id")
    for i, g in enumerate(order):
        for w in g.get("windows", []):
            if isinstance(subject, int):
                if w["surface_id"] == int(subject):
                    return (i, w["tab"])
            elif w["app_id"] == subject:
                return (i, w["tab"])
    return None


#: Slack on top of the number of windows a walk has to cross, for
#: notches the compositor coalesces or a tab selection that lands a
#: frame late. A bound, not a budget: the walk stops as soon as it is
#: there, and reports if it stops moving before that.
STRIP_STEP_SLACK = 3


def jump_to_group(s: Session, subject) -> bool:
    """Walk the strip's focus along to the group holding `subject`.

    Alt+Shift+wheel — the compositor's own navigation gesture
    (`client_input.rs`, Niri's `focus-column-right`), which selects the
    neighbouring window and lets the per-tick reconciler slide the strip
    to it. So the pan is eased and reads as navigation rather than as a
    cut.

    **One notch is one window, not one group**, and counting the wrong
    one is what broke this. `step_active_window` flattens the strip's
    groups into their tabs and steps a tab at a time, so a notch inside a
    two-tab group moves the focus without moving the *group* index. This
    walk compared group indices, saw one unchanged, concluded the strip
    would not move and stopped — on the strip this film builds, which
    puts two applications in one tab group on purpose. Seven windows, and
    it gave up at the group holding the third and fourth, leaving the
    window the next beat was about 1596px off the right edge.

    So progress is measured in the unit the gesture moves in: the
    strip flattened into windows, and the walk is stuck only when the
    *window* under focus is the same one it was before the notch.

    This replaces aiming a click at the target's tab pill, which cannot
    do the job on a strip longer than the screen: a pill is drawn at its
    tile's *current* position, and the whole reason to scroll to a group
    is that it is not on screen — so the click landed on the backdrop or
    on some other group's pill and the strip stayed where it was.
    Measured: a word processor two groups along came back 541px past the
    right edge after a 'focus' that had aimed outside the frame.

    Reads the strip back between notches rather than firing a computed
    number of them, because a jump stops at the ends of the strip and a
    walk that assumed otherwise would drift.

    **The target is a window, not a group.** A notch selects a window, so
    the walk can be asked to arrive at the subject's own tab and not
    merely at the group holding it — which is the difference between
    framing a document and framing whichever of its group's tabs happened
    to be in front. Stopping at the group was how the film's one cut
    landed on an empty photo viewer sharing a bar with the word processor
    it was supposed to be of.
    """
    order = strip_order(s)
    want = window_slot(s, subject, order)
    if want is None:
        return False
    budget = len(window_order(order)) + STRIP_STEP_SLACK
    for _ in range(budget):
        flat = window_order(order)
        was = active_window(order)
        if was is None or was == want:
            break
        if was not in flat or want not in flat:
            break
        s.modifiers(alt=True, shift=True)
        s.scroll(0.0, -1.0 if flat.index(want) > flat.index(was) else 1.0)
        s.modifiers()
        s.settle(0.45)
        order = strip_order(s)
        now = active_window(order)
        if now == was:
            # Genuinely stuck: the same *window* is focused after a
            # notch, so it is the end of the strip or a jump that did
            # not take.
            note(f"{s.name}: the strip did not step past {was}, so "
                 f"{subject_name(subject)} is as close as it gets")
            break
        want = window_slot(s, subject, order)
        if want is None:
            return False
    order = strip_order(s)
    return active_window(order) == window_slot(s, subject, order)


def scroll_subjects_into_frame(s: Session, subjects: list) -> None:
    """Pan the strip until `subjects` are the windows in frame.

    Right subject first, then left, and that order is the whole trick.
    Bringing a group up scrolls it *minimally* into view — a group off
    the right edge comes to rest against the right edge, a group off the
    left to rest against the left. So going to the rightmost subject
    guarantees it is in frame wherever it started, and then going to the
    leftmost either does nothing (already whole, which is the answer) or
    brings it flush to the left edge, which at the film's share leaves
    exactly enough room for the one beside it.

    Doing it the other way round frames the wrong pair: go to the left
    subject while it is off to the right and it comes to rest against
    the *right* edge, and what a viewer then sees beside it is its left
    neighbour rather than the window the beat is about.
    """
    order = strip_order(s)
    placed = [(group_index_of(s, sub, order), sub) for sub in subjects]
    placed = sorted(((i, sub) for i, sub in placed if i is not None),
                    key=lambda pair: pair[0])
    for _i, sub in reversed(placed):
        jump_to_group(s, sub)


def frame_windows(s: Session, subjects: list,
                  viewport: tuple[int, int] | None = None) -> bool:
    """Put the windows this beat is about in frame, and say whether it
    worked.

    A subject is an app id or a window — see `tiles_for`.

    Two separate things go wrong, and each has to be checked rather than
    performed and assumed.

    A group takes about three quarters of the strip by default, which is
    right for reading one window and wrong for filming two — so every
    group is given the film's share. That has to be re-applied here and
    not only once before filming: `set_group_widths` walks the groups
    that exist when it is called, so a share settled on an empty desktop
    governs nothing opened afterwards, and a group opened by a route
    that does not go through `Session.launch` — beat 5 fetches its app
    through the palette and installs it — takes the launch default of
    `WIDTH_NORMAL_PERMILLE`, two thirds. Measured: two thirds of a
    1596px usable strip is 1065px, which put a second window's right
    edge 247px past a 1920px frame, printed underneath a claim that both
    windows were whole. Re-applying is a no-op for a group already at
    the film share, so the visible jump it risks only happens where the
    alternative is a window hanging off the side of the picture.

    The second is the strip's scroll offset, which
    `scroll_subjects_into_frame` owns.

    Then it is measured, and narrowed and re-framed until it holds or
    the attempts run out.

    **The claim is about `subjects`, not about everything on screen**,
    and that narrowing is the point rather than a softening. The film's
    strip carries four applications and the screen holds two, so "every
    window is wholly on screen" stopped being a thing that could be true
    — a check that reports the other two as faults every time reports
    nothing at all. What a beat can still be held to is that the windows
    it is about are whole, unobscured and side by side, and this refuses
    when they are not: a beat naming two windows still fails if either is
    off the edge, behind the other, or not on screen at all.
    """
    groups = max(1, len(s.groups_here()))
    s.set_group_width(FILM_GROUP_SHARE)
    scroll_subjects_into_frame(s, subjects)
    s.settle(0.6)
    if not viewport:
        return True
    share = FILM_GROUP_SHARE
    named = ", ".join(subject_name(sub) for sub in subjects)

    def wrong_now() -> list[str]:
        return (subjects_off_screen(s, subjects, viewport)
                + subjects_overlapping(s, subjects, viewport))

    wrong = wrong_now()
    if not wrong:
        note(f"{s.name}: {named} in frame together, whole and side by "
             f"side, out of {groups} group(s) at {share} permille")
        return True

    # A subject with no window at all is the one fault narrowing cannot
    # touch, and chasing it makes things worse: the loop below reads an
    # overflow off the subjects it *can* see, shrinks every group towards
    # it, and reports a share it never actually applied. That is how a
    # missing image editor came back as "cannot be filmed at 157
    # permille" when nothing had been set below 500.
    missing = [w for w in wrong if "no window on screen is it" in w]
    if missing:
        gap(f"{s.name}: {named} cannot be filmed together - "
            + "; ".join(wrong))
        return False

    # Narrowed and re-framed until it holds or the attempts run out. The
    # step is measured rather than guessed: the overflow is known in
    # pixels, so the share that would have fitted is arithmetic on the
    # widest subject. A floor of `WIDTH_NARROW_PERMILLE` stops this
    # shrinking windows into unreadability to satisfy a frame - below
    # that the honest answer is that the shot does not fit, and the beat
    # should say so rather than film something too small to read.
    for _ in range(3):
        widest, over = subject_overflow(s, subjects, viewport)
        if over <= 0.0:
            # Overlapping rather than off-screen: narrowing cannot help,
            # because the groups already fit and are simply stacked.
            break
        step = max(10, int(share * (over / widest)) + 5)
        if share - step < NARROW_SHARE_FLOOR:
            break
        share -= step
        s.set_group_width(share)
        scroll_subjects_into_frame(s, subjects)
        s.settle(0.6)
        wrong = wrong_now()
        if not wrong:
            note(f"{s.name}: {named} in frame together at {share} "
                 f"permille - narrowed from {FILM_GROUP_SHARE} to fit")
            return True

    gap(f"{s.name}: {named} cannot be filmed together at {share} permille "
        f"per group - " + "; ".join(wrong))
    return False


def frame_one_window(s: Session, app_id: str, prefer: dict | None = None,
                     share: int = 720) -> bool:
    """Put one window up at a readable width, and say whether it worked.

    `frame_windows` exists for a beat that has to show two windows at
    once, and it pays for that: two subjects side by side get half the
    strip each, and half of this strip is a word processor whose styles
    panel leaves a column of page about as wide as a phone. The last shot
    of the film is about what is *written* in a document, and it came out
    with the text clipped mid-word.

    A beat about one window asks for the opposite: that window at close
    to the width a person would give it, with its neighbours where a
    scrollable strip actually leaves them — visible at the edges, not
    squared off into equal thirds. Which is also the more honest picture
    of the layout model, and the one the strip is for.

    Returns whether the window is wholly on screen at the end of it, and
    records a GAP when it is not — the same standard `frame_windows`
    holds a two-window beat to. A caller that does not check still gets
    the fault in the transcript.
    """
    # Scrolled to, and only scrolled to.
    #
    # This used to widen the group to `share` first. It made the subject
    # bigger, and it did it a second after the app appeared, so every
    # window on screen jumped to a new size and position at the moment a
    # viewer was working out what had just opened. That reads as a fault,
    # not as a layout. The proportions are settled once by
    # `establish_strip` before the first frame; a beat only ever pans.
    #
    # `share` is kept in the signature and ignored, so the two call sites
    # that pass one do not have to change in the same breath as this.
    _ = share
    # A GAP when the subject could not be brought up at all, and that is
    # the loudest thing in this function. It used to be a note inside
    # `focus_app_group` and nothing here at all, so a call that framed
    # nothing whatsoever returned True: the check below asks whether any
    # window carrying `app_id` hangs off the edge, and a subject the
    # strip never went to has no rect to hang off anything, so an empty
    # list read as a shot that had worked. That is how the film's one cut
    # was taken on a photo viewer with a word processor a tab away.
    if not focus_app_group(s, app_id, prefer=prefer):
        gap(f"{s.name}: the shot is of {app_id} and the strip could not be "
            f"brought to it, so this frame is of whatever was already up")
        return False
    s.settle(0.6)
    # And *showing* it: the walk selects a window, and a window that is
    # selected but has not yet been laid out is not in the picture. Read
    # off the painted tiles, which is the compositor's answer to "is this
    # on screen" rather than to "is this focused".
    if not tiles_for(s, (prefer or {}).get("surface_id") or app_id):
        gap(f"{s.name}: the strip is on {app_id} and nothing is painting "
            f"it, so this frame is of an empty tile")
        return False
    # Measured, not assumed — the same rule the two-window framing keeps.
    # Focusing a group scrolls it *into view*, which is not the same as
    # scrolling it *wholly* into view: at 720 permille the shot came out
    # with the document readable and its right edge past the screen.
    #
    # A GAP, not a note, and the difference is the whole point. This
    # said it could not do the thing and the run carried on green — so a
    # cut that landed on a page hanging 1596px off the right edge was
    # reported in a line nobody reads, on the shot that opens the film's
    # entire second half. A beat whose subject is one window has not
    # happened if that window is not in the picture, and the transcript
    # is where a take says what it failed to film.
    if VIEWPORT:
        clipped = [w for w in windows_off_screen(s, VIEWPORT) if app_id in w]
        if clipped:
            # Where the strip actually came to rest, alongside where the
            # subject is, because the two failures need different fixes
            # and the message used to distinguish neither. Focus that
            # never arrived at the subject's group is a walk that gave
            # up; focus sitting on it with the window still hanging off
            # the edge is a layout no offset frames, and that one is not
            # the framing's to solve.
            order = strip_order(s)
            landed = active_window(order)
            wanted = group_index_of(s, app_id, order)
            where = (f"the strip is on group {landed[0]} of {len(order)} "
                     f"and {app_id} is in group {wanted}"
                     if landed is not None and wanted is not None
                     else "the strip's position could not be read")
            gap(f"{s.name}: the shot is of {app_id} and it is not wholly on "
                f"screen — " + "; ".join(clipped) + f" ({where})")
            return False
    return True


#: How many pixels have to move before a screen counts as having
#: changed. A text caret blinking on and off is about fifty; a remote
#: peer's name pill is a bit over a thousand; a line of text arriving in
#: a document is tens of thousands. Set above the second and far below
#: the third, so a page that gained a sentence is never confused with a
#: page that only blinked.
SCREEN_MOVED_PX = 4000


def screen_shot(s: Session, where: str) -> str | None:
    """Put what `s` is painting on disk, and hand back the path."""
    try:
        s.snapshot(where)
        return where
    except Exception as exc:  # noqa: BLE001
        note(f"{s.name}: could not read the screen back ({exc})")
        return None


def pixels_moved(before: str | None, after: str | None) -> int | None:
    """How many pixels differ between two shots, or None if it could not
    be worked out.

    The one question a socket cannot answer: did that screen actually
    change. Comparing the PNGs byte for byte is not it — the two shots
    either side of this beat came out the same size and different bytes,
    because a caret had blinked, so a hash says "changed" about a screen
    on which nothing happened. This decodes and counts instead.
    """
    if not before or not after:
        return None
    try:
        from PIL import Image, ImageChops
    except Exception:                       # noqa: BLE001
        note("PIL is not available, so the far screen could not be "
             "compared against what its session answered")
        return None
    try:
        with Image.open(before) as a, Image.open(after) as b:
            a, b = a.convert("RGB"), b.convert("RGB")
            if a.size != b.size:
                return None
            grey = ImageChops.difference(a, b).convert("L")
            # A threshold, not an exact match: subpixel text rendering
            # moves a value by one or two all over a page that has not
            # changed at all.
            moved = grey.point(lambda v: 255 if v > 24 else 0)
            return sum(moved.histogram()[255:])
    except Exception as exc:                # noqa: BLE001
        note(f"could not compare the two shots ({exc})")
        return None


def focus_app_group(s: Session, app_id: str,
                    prefer: dict | None = None) -> bool:
    """Bring the strip to the group holding `app_id`, and say whether it
    worked.

    `frame_windows` frames a whole shot; this picks one *subject* out of
    it and pans to it. The last shot of the film is about one window —
    the document two other participants have been writing into — and an
    even share of the strip with the file manager scrolled up in front
    of it is a shot of the wrong thing. It has been exactly that: a
    payoff frame showing an empty folder listing, under a caption about
    a document.

    Through `jump_to_group` rather than by clicking the target's tab
    pill. A pill is drawn wherever its tile currently is, so aiming at
    one only works while the group is already on screen — which is the
    case this exists to fix. See `jump_to_group`.

    That walk selects the *window*, tab included, so there is no pill to
    click afterwards and this no longer aims one. It used to, and the
    click could not do the job it was there for: a group's tabs all share
    one tile rect, so `x + 90` above it is the *first* pill in the bar
    whichever tab the shot is of. Two applications share a bar in this
    film by design, and the second of them was unreachable.

    Which subject is asked for is read off the tree, not off the painted
    tiles, so a window that is an inactive tab of a group counts as being
    on this screen — because it is. See `window_slot`.
    """
    subject = app_id
    order = strip_order(s)
    # By the end of the film two windows on this screen announce the same
    # app id — the session's own, and the peer's, replicated in. They are
    # showing the same document, so either would do; `prefer` names the
    # one the beat has been talking about, so the shot is of the window
    # the audience watched arrive.
    want = (prefer or {}).get("surface_id")
    if want is not None and window_slot(s, int(want), order) is not None:
        subject = int(want)
    if window_slot(s, subject, order) is None:
        note(f"{s.name}: no window on this screen is {app_id}, so the "
             f"strip is left where it is")
        return False
    if not jump_to_group(s, subject):
        note(f"{s.name}: the strip would not come to rest on {app_id}")
        return False
    s.settle(0.5)
    # Asked of the compositor rather than assumed off the walk: the beat
    # needs the group to be *showing* this tab, and a walk that stopped
    # early leaves it showing its neighbour.
    order = strip_order(s)
    if active_window(order) != window_slot(s, subject, order):
        note(f"{s.name}: the strip landed elsewhere than on {app_id}")
        return False
    note(f"{s.name}: brought {app_id} up in the strip for the shot")
    return True


def peer_sync_card_up(s: Session) -> bool:
    """Whether the peer-sync card is being painted over the middle of the
    screen.

    All three fields, because the compositor paints the card whenever any
    one of them is set (`overlay_host.rs`: the draw returns early only
    when `input`, `token` *and* `invite_name` are all `None`). Checking
    two of them made an open invite composer read as no card at all, so
    `dismiss_peer_sync_card` returned without dismissing it and the
    question "who are you inviting?" stayed on screen over the beat after
    it.
    """
    ps = s.chrome()["peer_sync"]
    return (ps["invite_token"] is not None
            or ps["connect_input"] is not None
            or ps.get("invite_name") is not None)


def dismiss_peer_sync_card(s: Session, what: str) -> None:
    """Close the peer-sync card, and prove it closed.

    Sending Escape and moving on is what put a stale modal over the two
    most interesting beats in a recorded run: the card sat in the middle
    of the frame for a minute after the invite, and the word processor
    opened behind it. Escape reaches the overlay host only when nothing
    else has taken the key first, so the dismissal is retried and then
    asserted — a beat filmed behind a dialog is a beat nobody can read.

    It also holds a bearer credential. The token is a signed blob that
    admits its holder to a real room, and it stays legible on screen for
    as long as the card is up, so the frame is cleared as soon as the
    token has been captured rather than whenever the next beat happens
    to press a key.
    """
    for _ in range(4):
        if not peer_sync_card_up(s):
            return
        s.key("Escape")
        s.settle(0.4)
    claim(f"{s.name}'s {what} card is off the screen",
          peer_sync_card_up(s), False)


def show_collaborators(s: Session, settle: float = 1.0) -> str:
    """Open the room's people from the affordance on screen.

    The top bar's avatar stack is the affordance (`topbar_chrome.rs`
    toggles `overlays.collaborators`), and it is the one a person reaches
    for. It used to be *there* only once somebody was in the room: an
    empty roster published `Rect::ZERO`, so at the one moment a host
    wants to invite the *first* person there was nothing to click, and
    the panel could only be reached by its address — which is not a
    gesture and is not what a viewer is being shown.

    It keeps its geometry now and reads `Invite`, so this is a click on
    every run and the address below is a diagnosis rather than a route:
    taking it is a GAP, because a beat whose subject is the chrome has
    then not shown the chrome. The route is returned and printed either
    way.
    """
    if s.named_rect("topbar_avatar_stack"):
        s.click_named("topbar_avatar_stack")
        # A panel sliding out is a thing to watch the first time and a
        # thing you have already seen the second, so how long it is
        # given belongs to the beat rather than to the helper.
        s.settle(settle)
        return "the avatar stack in the top bar"
    gap(f"{s.name}: the top bar published no avatar stack to click, so the "
        f"room's people had to be opened by address rather than by the "
        f"gesture this beat is about")
    resp = s.invoke_action(AGENT_PROVIDER, "collaborators.open")
    if "err" in resp:
        note(f"{s.name}: collaborators.open refused — "
             f"{json.dumps(resp['err'])[:120]}")
        return "nothing - the panel could not be opened"
    s.settle(settle)
    return "its address (the avatar stack was not on screen)"


def close_collaborators(s: Session) -> None:
    if s.overlays().get("collaborators"):
        s.key("Escape")
        s.settle(0.3)


def say_something_about(session: Session, log: "LogTail", tile: tuple,
                       file_name: str, listing: list[str], text: str,
                       shots: str | None = None,
                       tag: str | None = None) -> bool:
    """Right-click a file and say something to the room about it.

    The gesture this desktop hangs its argument on: pick an object,
    right-click, and a compose surface opens beside the ordinary context
    menu carrying that object's **address**. What crosses is not a
    description of what was on somebody's screen — it is the resource
    itself, so an agent never has to guess what "this" was, and the same
    sentence still means the same thing in a session opened tomorrow.

    Everything here is aimed at a pixel, and nothing here is remembered.
    A file row and a compose panel both belong to a *client*, and a
    client's widgets are not in the compositor's registry — there is no
    name to click, which is what an earlier version of this helper
    assumed when it opened with `named_rect("file_row:harbour.png")` and
    could therefore never once have run. What replaced it swept a band of
    the window looking for any row and then counted rows to the wanted
    one, and that is what failed: on a take where the list did not begin
    where the sweep looked, no press landed, the message was never
    written, and the beat the film turns on did nothing.

    So the pixel is asked for rather than hunted for, and each number
    comes from the one component that holds it:

    - the row's rect comes from the app — `Widget::resource_rects`, keyed
      by the file's own **address**, so the thing pressed and the thing
      the message carries are the same object rather than two things that
      happened to agree. Nothing here counts rows or knows a row height;
    - the window's origin comes from the compositor, which is the only
      thing that knows where a window is;
    - every press is still checked against the app's own account of what
      it hit, so a miss stays *known* to be a miss;
    - that press also *measures* the offset between screen and client
      coordinates, from the position the app reports back;
    - the panel's rect comes from the app saying where it laid it out,
      converted with that measured offset rather than guessed at.

    Driven rather than simulated. The pointer travels, the right button
    goes down and up, the text is typed at a person's pace and Enter
    sends it. Every step is something somebody could have done, so a
    control that is not drawn, not hit-testable or not wired fails here
    instead of being routed around.

    Returns False rather than raising. A rig that cannot perform the
    gesture should say so and let the run reach the end: a take that dies
    at its own best moment tells you less than one that finishes and
    names what was missing.
    """
    log.read()

    # Find the row, and with it the mapping between the two coordinate
    # spaces. `press_the_photo_row` returns with the button still down —
    # it exists for the drag beat — so let go, which makes it a click and
    # leaves the row selected.
    step(f"{session.name} picks out {file_name}")
    held = press_the_photo_row(session, log, tile, file_name, listing)
    session.pointer_button(False)
    session.settle(0.35)
    if held is None:
        drawn = log.resource_rects(FILES_APP)
        why = ("and the app published no layout at all, so there was nothing "
               "to aim at and this fell back to hunting for a row"
               if not drawn else
               f"even though the app says it is drawing {len(drawn)} row(s) "
               f"— so either the window moved between the reading and the "
               f"press, or a press at the rect it named is not reaching it")
        gap(f"{session.name}: no press landed on {file_name}, {why}.")
        return False

    hit = log.armed(log.text)
    if hit is None:
        gap(f"{session.name}: the press landed but the file manager never "
            f"said what it hit, so there is no way to convert its "
            f"coordinates into screen ones.")
        return False
    x, y = held
    off_x, off_y = x - hit[1], y - hit[2]
    note(f"screen = client + ({off_x:.0f}, {off_y:.0f})")

    # The right-click, on the row now known to be under the pointer.
    step(f"{session.name} right-clicks it")
    session.right_click_at(x, y)
    # Long enough for the panel to actually arrive. The app hands the
    # request to the shell over a socket, and that took ~1s on the take of
    # 2026-08-25: the panel appeared at 38.59s while a 0.8s settle expired
    # at 38.4s. So the lookup below found nothing, took its fallback
    # branch, and aimed the next press at coordinates the panel was not at
    # -- which is how the surface came to live a tenth of a second.
    session.settle(1.5)

    # Where the panel is, asked of whoever drew it.
    #
    # These two branches are the two homes this surface has had, and the
    # order says which one is right. When the compositor draws it — which is
    # what it must do, because a client cannot paint beside a window it is
    # not inside — the panel is an ordinary named target and the harness
    # reads its rect the way it reads any other piece of chrome. Until then
    # it is a client's widget, invisible to the registry, and the only
    # account of where it went is the app's own.
    #
    # Written as a fallback rather than replaced outright so the beat keeps
    # running across the move, and so a run says which side of it it is on.
    about = None
    panel = session.named_rect("remark_compose")
    if panel:
        note("the compose surface is drawn by the shell, above the windows")
        sx, sy, pw, ph = panel
        opened = log.compose_opened(log.read() + log.text)
        about = opened[0] if opened else None
    else:
        opened = log.compose_opened(log.read() + log.text)
        if opened is None:
            gap(f"{session.name}: right-clicking {file_name} opened no compose "
                f"surface, so there was nowhere to say anything. Either "
                f"nothing was selected, or the room has told this app nowhere "
                f"to send.")
            return False
        about, (px, py, pw, ph) = opened
        sx, sy = px + off_x, py + off_y
        gap(f"{session.name}: the compose surface is being drawn by the "
            f"application, so it is clipped at that window's edge and cannot "
            f"appear beside a neighbouring one. It belongs to the shell.")
    if not about:
        gap(f"{session.name}: the compose surface opened about nothing — a "
            f"panel with no address behind it says nothing an agent could "
            f"act on.")
        return False
    note(f"the compose surface is about {about}")
    if shots:
        session.snapshot(os.path.join(shots, f"{tag or 'remark'}-open.png"))

    # Into the field, by its name where there is one.
    #
    # The panel's text field carries `test_id` `remark_input`, so when the
    # *shell* draws the surface it is an ordinary named target and the
    # harness clicks the middle of the rect the widget itself reports. A
    # fraction of the panel's height is what this did instead, and it is
    # only right for as long as the panel keeps its present proportions —
    # the same kind of number that made the press on the row miss.
    #
    # The fraction stays for the app-drawn panel, whose widgets are in no
    # registry: the field is the upper band, with the destination selector
    # and the button below it, so clicking high inside lands in the text
    # rather than on a control.
    field = session.named_rect("remark_input")
    if field:
        fx, fy, fw, fh = field
        session.click_at(fx + fw * 0.5, fy + fh * 0.5)
    else:
        session.click_at(sx + pw * 0.5, sy + ph * 0.28)
    session.settle(0.25)
    # The compose box is compositor chrome, not an app: it reads logical
    # keys, so a Shift-held ';' would land as ';' and break the `claude:`
    # the agent matches on. `chrome=True` types the punctuation straight.
    session.type_prose(text, chrome=True)
    session.settle(0.4)
    if shots:
        session.snapshot(os.path.join(shots, f"{tag or 'remark'}-typed.png"))

    # Enter in the field is a send affordance beside the button, and the
    # widget drains both into one place, so they cannot mean different
    # things. It is also what a person does after typing a line.
    # "Enter", not "Return": `parse_key_tag` rejects a name it does not know
    # rather than typing it, deliberately, so that a typo surfaces instead of
    # being silently swallowed. This one surfaced as the instruction never
    # being sent, at the beat the whole film turns on.
    session.key("Enter")
    session.settle(0.8)

    sent = log.remark_sent(log.read() + log.text)
    claim(f"{session.name} said something about {file_name}",
          sent is not None, True)
    if sent is None:
        gap(f"{session.name}: the sentence was typed and the app never "
            f"reported sending it. The panel closing is not evidence — that "
            f"looks the same whether it sent or gave up.")
        return False
    to, sent_about, sent_text = sent
    # The claim this beat exists to make, asserted on the *address*: what
    # left the app is the thing he pointed at, not the words he used for
    # it. A body that carried the id instead would pass a weaker check.
    claim("what he sent carries the file's address, not a description of it",
          sent_about == about, True)
    claim("the address is not smuggled through the sentence",
          about not in sent_text, True)
    note(f"sent to {to}: {sent_text!r} about {sent_about}")

    on_wire = LogTail.ON_THE_WIRE.findall(log.text)
    claim("and the host put it in the room rather than holding it",
          bool(on_wire), True)
    if not on_wire:
        gap(f"{session.name}: the app composed a remark and the toolkit "
            f"never reported sending one. Composed is not delivered.")
        return False
    return True


def invite_from_the_panel(host: Session, name: str, shots: str | None = None,
                          tag: str | None = None,
                          read: float = 1.0) -> str | None:
    """Invite somebody from the panel a person opened, with the mouse.

    `read` scales the four pauses this card is held for. The film mints
    two invitations from the same card, and only the first of them is a
    thing a viewer has to read: the composer asking who, the name going
    in, the token coming back, the card saying it is on the clipboard.
    The second says *the same card, a different name* — which is the
    whole point of showing it twice — and somebody who has just read one
    reads the second at a glance. Held at full length both times, the
    pair cost half a minute of a hundred-second film to make one point.
    Scaled rather than skipped: every gesture still happens, every claim
    is still asserted, and the card is still on screen long enough to
    see whose name is on it.

    Assumes the collaborators panel is already up — clicking `Invite`
    closes it, because the card carries the question and then the token
    and a menu over that is a menu over the thing the click was for.

    Every step is a click on a painted control, so a control that is not
    drawn, not hit-testable or not wired fails here instead of being
    routed around: `collab_invite` opens a composer that asks *who*,
    `peer_sync_create` mints against that answer and puts the token on
    the system clipboard, and the card says so where the host is looking.
    Raising the same card by its address would have proved only the last
    of those.

    The only thing typed is a person's name. It is what the room will
    call whoever redeems this token, it is held on this machine until
    they do, and it never crosses the wire — a guest who could announce
    its own name could announce anybody's.
    """
    invite = host.named_rect("collab_invite")
    claim("the panel offers a way to invite somebody", invite is not None, True)
    if invite is None:
        gap("the collaborators panel painted no Invite button, so the "
            "invite cannot be raised from the panel a person opened")
        return None
    note(f"Invite at {[round(v) for v in invite]}, Join beside it at "
         f"{[round(v) for v in (host.named_rect('collab_join') or [])]}")

    step("he presses Invite")
    clicked = host.click_named("collab_invite")
    if "err" in clicked:
        gap("the panel's Invite button would not take a click - "
            f"{json.dumps(clicked['err'])[:120]}")
        return None
    # Not scaled by `read`, unlike the holds below. This one is not a
    # pause for reading -- it is the card arriving on screen before
    # anything is typed into it. Scaled to 0.45 for the second
    # invitation it came out at 0.36s, and the film shows that card
    # holding its empty "Their name in this room" placeholder for its
    # whole life while `the field took the name 'Claude'` passed: the
    # claim reads the compositor's model, which had it, and the picture
    # never caught up before the card closed. A beat about a name in a
    # field has not happened if the name is not in the picture.
    host.settle(0.8)

    step("and it asks who, before it mints anything")
    # The whole reason the composer exists. A button that minted an
    # anonymous token left the host's own roster reading
    # `peer@12191268537764683811` once the guest arrived, because nothing
    # in the flow had ever been told a name — and a name is governance
    # here, so it cannot be supplied by the person arriving.
    asked = wait_for(
        lambda: host.chrome()["peer_sync"]["invite_name"] is not None,
        timeout=15)
    claim("the card asks who the invitation is for", bool(asked), True)
    if not asked:
        gap("Invite opened no composer, so there was nobody to name and "
            "nothing to mint against")
        dismiss_peer_sync_card(host, "invite")
        return None
    claim("and nothing was minted before it was answered",
          host.chrome()["peer_sync"]["invite_token"], None)
    host.type_prose(name)
    host.settle(0.5 * read)
    claim(f"the field took the name {name!r}",
          host.chrome()["peer_sync"]["invite_name"], name)
    # Who it is for, on screen, before anything has been minted. One of
    # the four frames this beat exists to let somebody read.
    #
    # Shorter than the card below and not scaled with it, because what is
    # on screen here is one word in a field. It was held for as long as a
    # token, twice, which is four seconds of the finished film spent on a
    # name a viewer read in half of one — and it is the same length both
    # times, because the second invitation's whole point is the name in
    # this field being a different one.
    read_hold(READ_HOLD_S * 0.6)
    if shots and tag:
        host.snapshot(os.path.join(shots, f"{tag}-naming.png"))

    step("and presses Create invite")
    created = host.click_named("peer_sync_create")
    if "err" in created:
        gap("the composer's Create invite button would not take a click - "
            f"{json.dumps(created['err'])[:120]}")
        dismiss_peer_sync_card(host, "invite")
        return None
    token = wait_for(lambda: host.chrome()["peer_sync"]["invite_token"],
                     timeout=30)
    claim("a token was minted", bool(token), True)
    if not token:
        dismiss_peer_sync_card(host, "invite")
        return None
    claim(f"and the card says whose it is — {name!r}",
          host.chrome()["peer_sync"]["invite_for"], name)
    status = host.chrome()["peer_sync"]["status"] or ""
    claim("and that it was copied, where he is looking",
          "copied" in status.lower(), True)
    note(f"the card reads {status!r}")
    # What leaves here is what the button produced, not a value the
    # harness already had in a variable: a copy that silently truncated
    # fails the join on the other machine rather than being papered over.
    copied = wait_for(lambda: host.clipboard_read(), timeout=10)
    claim("the token really is on his system clipboard, put there by the "
          "button", copied == token, True)
    if copied != token:
        note(f"the card shows {len(token)} characters; the clipboard holds "
             f"{len(copied or '')}")
    # The card, whole: whose it is, the short code that reads the same on
    # both screens, and that it is on the clipboard. This is the frame
    # the invitation has flashed past in every previous cut, so it is
    # held for longer than anything else in the film.
    read_hold(READ_HOLD_S * 1.5 * read)
    if shots and tag:
        host.snapshot(os.path.join(shots, f"{tag}-copied.png"))
    # The moment it has been read, and not one beat later: it is a modal
    # over the middle of the screen and it is showing a live token.
    dismiss_peer_sync_card(host, "invite")
    return copied or token


def join_room(guest: Session, token: str, via: str,
              shots: str | None = None, tag: str | None = None) -> bool:
    """Put `guest` into the room `token` names, by pasting and pressing Join.

    The token is never typed. It is put on this session's clipboard —
    that is the hop between two machines, and it belongs to a person: a
    chat message, an email, a USB stick, and not the OS's business — and
    from there the card's own **Paste** button lifts it into the prompt
    and its **Join** button redeems it. Both are the gestures a person
    makes, and both are load-bearing: if the paste does nothing the
    prompt stays empty and this fails, rather than quietly falling back
    to typing a string the harness already had. Nobody types three
    hundred characters of base64 correctly, twice, on a stage.

    `via` says how the prompt is raised, and the two routes are different
    claims about the same card:

      `panel`    the top bar, then **Join** in the collaborators panel.
                 The gesture a person makes, and it fails here when
                 either control is unpainted or unwired.
      `action`   the prompt's stable address, `peer.connect`. What a
                 participant with no screen has instead of a mouse.
    """
    guest.clipboard_write(token)
    if via == "panel":
        step("he opens the room's people on his own machine")
        opened = show_collaborators(guest)
        note(f"{guest.name} opened it from {opened}")
        claim(f"{guest.name}'s collaborators panel is up",
              guest.overlays()["collaborators"], True)
        if shots and tag:
            guest.snapshot(os.path.join(shots, f"{tag}-panel.png"))
        read_hold()

        step("and presses Join")
        seat = guest.named_rect("collab_join")
        claim("the panel offers a way into somebody else's room",
              seat is not None, True)
        if seat is None:
            gap("the collaborators panel painted no Join button, so a room "
                "cannot be entered from the panel a person opened")
            return False
        clicked = guest.click_named("collab_join")
        if "err" in clicked:
            gap("the panel's Join button would not take a click - "
                f"{json.dumps(clicked['err'])[:120]}")
            return False
        guest.settle(0.8)
    elif via == "action":
        # One call, and no pointer anywhere in it.
        #
        # This used to raise `peer.connect` — which opens the card and
        # stops — and then click Paste and Join, because redeeming a
        # token had no address. So a participant with no mouse had to
        # borrow one, and the position those clicks left behind was
        # republished to the whole room once a second afterwards as a
        # cursor labelled `Claude`, parked on the Join button. Three
        # separate fixes chased that pill before the cause turned out to
        # be that this is how the agent joined.
        #
        # `peer.join` takes the token and routes to `redeem_invite`, the
        # same function the card's Join button calls, so the capability
        # check is identical by construction rather than by review.
        resp = guest.invoke_action(AGENT_PROVIDER, "peer.join",
                                   {"token": token})
        if "err" in resp:
            raise RuntimeError(f"{guest.name}: peer.join refused: {resp['err']}")
        if not wait_for(lambda: (guest.active_workspace() or {}).get("room"),
                        timeout=15):
            gap(f"{guest.name}: peer.join was accepted but no room arrived")
            return False
        claim(f"{guest.name} joined without touching a pointer",
              guest.input_ops, 0)
        return True
    else:
        raise ValueError(f"unknown join route {via!r}")

    if not wait_for(lambda: guest.chrome()["peer_sync"]["connect_input"] is not None,
                    timeout=15):
        gap(f"{guest.name}: nothing opened a connect prompt, so there was "
            f"nowhere to put the token")
        return False
    read_hold()

    step("he presses Paste")
    pasted_click = guest.click_named("peer_sync_paste")
    if "err" in pasted_click:
        gap("the prompt's Paste button was not addressable - "
            f"{json.dumps(pasted_click['err'])[:120]}")
        dismiss_peer_sync_card(guest, "connect")
        return False
    filled = wait_for(
        lambda: guest.chrome()["peer_sync"]["connect_input"] or None, timeout=10)
    claim("the prompt filled itself from the clipboard, unretyped",
          filled == token, True)
    if filled != token:
        note(f"the prompt holds {len(filled or '')} characters; the token is "
             f"{len(token)}")
    if shots and tag:
        guest.snapshot(os.path.join(shots, f"{tag}-pasted.png"))
    # The same short code, on the other machine, beside the words telling
    # you to check it matches. Held, for the same reason as above.
    read_hold(READ_HOLD_S * 1.5)

    step("and Join")
    joined_click = guest.click_named("peer_sync_join")
    if "err" in joined_click:
        gap("the prompt's Join button was not addressable - "
            f"{json.dumps(joined_click['err'])[:120]}")
        dismiss_peer_sync_card(guest, "connect")
        return False
    joined = wait_for(
        lambda: (guest.chrome()["peer_sync"]["status"] or "").startswith("Joined"),
        timeout=15)
    if shots and tag:
        guest.snapshot(os.path.join(shots, f"{tag}-joined.png"))
    read_hold()
    dismiss_peer_sync_card(guest, "connect")
    return bool(joined)


# --------------------------------------------------------------------
# asking a running app what it holds
# --------------------------------------------------------------------


class DocumentReader:
    """A late joiner that asks one session's app for its document.

    Beat 7 is the only claim in this demo whose subject is a *client's*
    insides, and the compositor has no op that reads them: `state_dump`
    knows a foreign surface exists and painted, never what is written on
    it. Comparing two screenshots was the previous answer, and it is not
    an answer — a person has to look, and a run that films nothing
    reports the same silence as a run that films everything.

    So this asks the app instead, in the app's own protocol. It speaks
    the length-prefixed JSON `CollabRequest`/`CollabEvent` the toolkit
    speaks (`ui/src/collaboration.rs`), announces the same
    `(app_id, cluster_id, record)` address the word processor announces,
    and then sends the one frame every toolkit answers with its whole
    document: `Presence { Arrived }` (`collab_broker.rs:1149`, and the
    same handshake `qos-agent` opens with, `apps/agent/src/session.rs:72`).

    What comes back is therefore **produced by the app running in that
    session, out of the document that app is holding**. It is not the
    wire being read back to itself: a snapshot that carries the text
    means the far app's own document carries it, and a snapshot that
    does not means the two documents have not converged. Both are
    reported; neither is inferred from the other.
    """

    # The presence scope the word processor's toolkit answers on. Same
    # constant `qos-agent` uses, for the same reason: a different scope
    # is a different conversation and gets no reply.
    SCOPE = "word-processor"

    def __init__(self, sock_path: str, room: str, record: str | None,
                 handle: str = "readback", app_id: str | None = None,
                 scope: str | None = None) -> None:
        # Which app is being asked, for the callers that ask a different
        # one. The pair has to move together — the announced `app_id` is
        # what the broker routes by and the presence scope is what the
        # toolkit answers on — so they are one argument in two halves
        # rather than two settings that can disagree.
        self.frames: list[dict] = []
        self.err: str | None = None
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(10.0)
        self.sock.connect(sock_path)
        self._send({"Hello": {"app_id": app_id or WORD_PROCESSOR,
                              "cluster_id": room,
                              "record": record, "handle": handle}})
        self._send({"Presence": {"key": {"Workspace": {"id": scope or self.SCOPE}},
                                 "payload": "Arrived"}})
        threading.Thread(target=self._read, daemon=True).start()

    def _send(self, obj: dict) -> None:
        body = json.dumps(obj).encode()
        self.sock.sendall(len(body).to_bytes(4, "little") + body)

    def _read(self) -> None:
        try:
            f = self.sock.makefile("rb")
            while True:
                hdr = f.read(4)
                if not hdr or len(hdr) < 4:
                    return
                body = f.read(int.from_bytes(hdr, "little"))
                if not body:
                    return
                try:
                    self.frames.append(json.loads(body.decode()))
                except Exception:
                    pass
        except Exception as e:              # the session went away
            self.err = str(e)

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass

    # -- what arrived ------------------------------------------------

    def documents(self) -> list[bytes]:
        """Every whole-document frame an app has answered with."""
        out = []
        for m in list(self.frames):
            if "CRDTUpdate" in m:
                out.append(bytes(m["CRDTUpdate"]["data"]))
        return out

    def deltas(self) -> list[bytes]:
        """Change frames, signed or not. Evidence of traffic, not of state."""
        out = []
        for m in list(self.frames):
            if "CRDTIncremental" in m:
                out.append(bytes(m["CRDTIncremental"]["data"]))
            elif "SignedCRDTIncremental" in m:
                env = m["SignedCRDTIncremental"].get("envelope") or {}
                payload = env.get("payload")
                if isinstance(payload, list):
                    out.append(bytes(payload))
        return out

    def await_document(self, needle: str, timeout: float, predicate=None):
        """Wait for a document frame, preferring one that carries `needle`.

        `predicate(blob, needle)` decides what "carries" means, and
        defaults to [`carries_text`]. It is the *poll* condition as well
        as the answer, deliberately: checking a stricter thing afterwards
        on whatever the loose one stopped at would report the strict
        answer about a snapshot taken before the change landed.

        Returns `(document_bytes, carries_needle)`, or `(None, False)` if
        no app ever answered. Keeps waiting after the first snapshot
        arrives: a late joiner can be answered before the far app has
        applied the change being watched for, and the first frame is
        then a true document that is simply older than the question.

        Raises [`CannotDecode`] when there is no decoder to read the
        reply with, so a harness that cannot look never reports an
        absence it did not observe.
        """
        carries = predicate or carries_text
        deadline = time.time() + timeout
        newest: bytes | None = None
        seen: set[int] = set()          # blobs already searched, by id
        while time.time() < deadline:
            docs = self.documents()
            if docs:
                newest = docs[-1]
            # Every frame, not just the newest one. More than one
            # participant answers `Arrived` with a full document on this
            # address — the agent does too — so the newest frame is not
            # necessarily the app's, and reading only that one reports
            # another participant's document as the app's empty page.
            #
            # A delta carries the text too, and does when the far app was
            # already attached while it was typed. Both kinds are worth
            # searching; only a document is worth reporting a size for.
            for blob in docs + self.deltas():
                key = hash(blob)
                if key in seen:
                    continue
                seen.add(key)
                if carries(blob, needle):
                    return blob, True
            time.sleep(0.5)
        return newest, False


def carries_text(blob: bytes, needle: str) -> bool:
    """Whether an Automerge blob holds `needle` as text.

    Not a byte search. Automerge DEFLATE-compresses each column of a
    change chunk once it is over a few hundred bytes, so a phrase a
    person typed is genuinely in the document and genuinely absent from
    its bytes — and a harness that grepped the bytes reported an empty
    page for a document that had the sentence in it. Worse, it failed
    the same way whatever went wrong, so a correct document read as a
    sync failure and sent people looking at the network.

    A per-keystroke delta cannot match either: it carries one character,
    and no single character is a 33-character phrase. That is a real
    absence rather than a misreading, and the document frames are where
    the answer is.

    Decoding is delegated to `qos-doc-text`, which walks the whole object
    graph — every block, including the cells of a table — so this needs
    no knowledge of the app's schema.

    Without that binary there is no honest answer available, and this
    raises [`CannotDecode`] rather than returning one. Returning `False`
    is what it used to do, and it cost a run: the film's build list does
    not mention `qos-doc-text`, so the decoder was simply never built,
    every document read as empty, and the transcript reported that the
    agent's table had no text in it over a frame showing the table with
    the text in it. An assertion that cannot look must say so, because
    "absent" and "unreadable" are different answers and only one of them
    is about the product.
    """
    if not DOC_TEXT:
        raise CannotDecode(
            "qos-doc-text is not built, so a document's contents cannot be "
            "read (cargo build --profile demo -p qdoc --bin qos-doc-text)")
    try:
        done = subprocess.run([DOC_TEXT, "-", "--contains", needle],
                              input=blob, stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL, timeout=20)
    except (OSError, subprocess.SubprocessError) as e:
        raise CannotDecode(f"qos-doc-text could not be run: {e}") from e
    # 0 present, 1 absent, 2 "this is not a document" — which is the
    # ordinary answer for a per-keystroke delta and not a failure to
    # look, so it stays a plain no-match.
    return done.returncode == 0


def document_strings(blob: bytes) -> list[tuple[str, str]]:
    """Every string an Automerge document holds, as `(path, text)`.

    The dump form of [`carries_text`], for the assertions that need to
    know *where* a phrase sits rather than only that it is somewhere.
    Raises [`CannotDecode`] on the same terms; a blob that is not a
    document (a per-keystroke delta) is simply empty.
    """
    if not DOC_TEXT:
        raise CannotDecode(
            "qos-doc-text is not built, so a document's contents cannot be "
            "read (cargo build --profile demo -p qdoc --bin qos-doc-text)")
    try:
        done = subprocess.run([DOC_TEXT, "-"], input=blob,
                              stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL, timeout=20)
    except (OSError, subprocess.SubprocessError) as e:
        raise CannotDecode(f"qos-doc-text could not be run: {e}") from e
    if done.returncode != 0:
        return []
    out = []
    for line in done.stdout.decode("utf-8", "replace").splitlines():
        path, _, text = line.partition("\t")
        if path:
            out.append((path, text))
    return out


def carries_text_in_a_table(blob: bytes, needle: str) -> bool:
    """Whether `needle` is in one of the document's **table cells**.

    The stricter form of [`carries_text`], and it exists because the
    looser one cannot tell the beat from its own input. The film asks
    the agent to turn a run of bullets into a run sheet, and the phrase
    it then looks for is one of the bullets' own words — so the claim
    "the page carries the run sheet's content" was answered `True` by
    the bullets the agent was asked to replace. It passed on a run where
    the agent refused, wrote a sentence over the instruction and built
    no table at all: the bullets were still there, and one of them said
    the phrase.

    A cell is the thing only the table has. The word processor writes
    each one as a block with `kind` `cell` (`InsertTable` in
    `apps/word-processor/src/actions.rs`), so a phrase sitting under one
    of those was written into a table by somebody, and a phrase sitting
    in a bullet was not.
    """
    found = document_strings(blob)
    cells = {p.rsplit(".", 1)[0] for p, t in found
             if p.endswith(".kind") and t == "cell"}
    return any(p.endswith(".text") and p.rsplit(".", 1)[0] in cells
               and needle in t
               for p, t in found)


def picture_references(blob: bytes) -> list[str]:
    """Every image blob an Automerge document references, by its hash.

    The counterpart of [`carries_text_in_a_table`] for the other film:
    where that one asks whether the agent built a table, this asks
    whether it put a *picture* on the page.

    A document does not carry an image any more — it carries the BLAKE3
    address of one and a small descriptor beside it, `mime`/`len`/`w`/`h`
    under `blobs.<hash>` (`put_blob`, `crates/doc/qdoc/src/model.rs`).
    Only `mime` is a string, so it is the only one of the four that
    `qos-doc-text` can print; the hash comes back as the *path* that
    string sits at, which is what makes this answerable at all.

    Raises [`CannotDecode`] on the same terms as everything else that
    reads a document: not being able to look is a different answer from
    looking and finding nothing.
    """
    out = []
    for path, text in document_strings(blob):
        if path.startswith("blobs.") and path.endswith(".mime") \
                and text.startswith("image/"):
            hash_ = path[len("blobs."):-len(".mime")]
            if hash_ and "." not in hash_:
                out.append(hash_)
    return out


def png_size(blob: bytes) -> tuple[int, int] | None:
    """`(width, height)` out of a PNG's IHDR, or None if it is not one."""
    if len(blob) < 24 or not blob.startswith(b"\x89PNG\r\n\x1a\n"):
        return None
    return struct.unpack(">II", blob[16:24])


def blob_beside_the_document(session: "Session", room: str, app_id: str,
                             hash_: str) -> bytes | None:
    """The bytes an app's own content store holds under `hash_`, or None.

    The document only ever carries the address; the bytes live in a
    content-addressed store, and the launcher gives every app a private
    `HOME` and `XDG_CACHE_HOME` inside its own sandbox — deliberately,
    so an app and its shell share no directory. From out here that
    sandbox is an ordinary path under the session's data home
    (`host_app_dir`, `crates/packaging/launcher/src/appdir.rs`), and
    the store lays a blob out as `<root>/<first two of hash>/<hash>`
    (`FsBlobStore::path_for`).

    Returning None means the bytes were not reachable from here, which
    is not the same as the picture not existing — every caller has to
    say which it is rather than reporting the absence as the answer.
    """
    if not hash_ or not all(c in "0123456789abcdef" for c in hash_):
        return None
    root = os.path.join(session.dir, "data", "qos-rooms", room,
                        _sanitised(app_id), ".cache", "qos", "blobs")
    path = os.path.join(root, hash_[:2], hash_)
    try:
        with open(path, "rb") as fh:
            return fh.read()
    except OSError:
        return None


def _sanitised(package_id: str) -> str:
    """`sanitise_component`, as the launcher applies it to a package id."""
    return "".join(c if (c.isascii() and (c.isalnum() or c in "._-")) else "_"
                   for c in package_id.strip())


def carries_bytes(blob: bytes, needle: bytes) -> bool:
    """Whether an Automerge save contains `needle` verbatim.

    The counterpart to [`carries_text`] for something that is not text. The
    word processor stores an image as `ScalarValue::Bytes` under its
    BLAKE3 hash (`qdoc/src/model.rs:2005`), so the PNG's own bytes are
    in the document — which is what makes this answerable without an
    Automerge binding.

    Columns over a size threshold are DEFLATE'd, so a plain `in` is not
    enough: every plausible zlib header is inflated and searched too.
    Bounded at the first megabyte, because the scan is quadratic in the
    worst case and a document this big is already the answer.
    """
    if needle in blob:
        return True
    for start in range(min(len(blob), 1 << 20)):
        if blob[start] != 0x78 or start + 1 >= len(blob):
            continue
        if blob[start + 1] not in (0x01, 0x5E, 0x9C, 0xDA):
            continue
        try:
            if needle in zlib.decompressobj().decompress(blob[start:], 1 << 25):
                return True
        except zlib.error:
            continue
    return False


def image_is_in_the_document(session: Session, room: str, png: bytes,
                             timeout: float = 20.0) -> tuple[bool, int]:
    """Ask the running word processor for its document, and look for the
    image in it.

    A late joiner on this session's broker announces the app's address
    and sends the one frame every toolkit answers with its whole
    document. What comes back was produced by the app out of the
    document it holds — so this is the difference between "an image
    appeared on screen" and "the image is in the document", which is the
    claim that matters, because it is the one that replicates.

    What it looks for is the **reference**, and it asserts the bytes are
    *not* there.

    This used to search the document for the PNG's own signature, on the
    reasoning that an image in a document is findable in it. That
    stopped being true today, and the reversal is the point: images now
    travel as a hash with the bytes in a content-addressed store beside
    the document, because a 146 kB change exceeded the publish ceiling,
    was dropped, and — since Automerge holds a change whose ancestors
    are missing — froze every other machine's copy at that point,
    permanently and silently. A document still carrying the bytes would
    now be the bug.

    The needle is `image/png`, the mime the store sniffed off the bytes
    and wrote into the descriptor beside the hash.

    It was `blob_w`, which could never have matched. `blob_w` and its
    siblings (`blob_mime`, `blob_bytes`, `blob_h`) are *projected onto
    the materialised snapshot at read time* by `project_blob_meta`; what
    the document itself stores is a map keyed by the hash holding
    `mime`, `len`, `w` and `h`. So this searched a document for a key
    that by design is not in one, and reported a working drag as a
    photograph that never reached the page. `crates/doc/qdoc/tests/
    blob_reference.rs` pins what a saved document does contain, so the
    two cannot drift again.

    The hash would be the sharper needle — it is the address the content
    store is keyed by — but it is blake3, and there is no blake3 in this
    machine's python or on its path. The mime is specific enough here:
    nothing else in a document of typed notes says `image/png`, and the
    beat has already established separately that the drop landed.

    Deliberately run after the camera has stopped: attaching announces a
    presence, and a phantom collaborator arriving mid-take would be a
    thing on screen that the demo did not do.
    """
    try:
        reader = DocumentReader(session.collab_json_sock, room, record=None)
    except OSError:
        return False, 0
    try:
        deadline = time.time() + timeout
        newest = 0
        reference = b"image/png"
        raw_bytes = png[:24]
        while time.time() < deadline:
            for blob in reader.documents() + reader.deltas():
                newest = max(newest, len(blob))
                if carries_bytes(blob, raw_bytes):
                    note("the document carries the photograph's own bytes, "
                         "which is the behaviour that froze every peer's "
                         "copy; it should carry a reference instead")
                    return False, len(blob)
                if carries_bytes(blob, reference):
                    return True, len(blob)
            time.sleep(0.5)
        return False, newest
    finally:
        reader.close()


def registry_entries(path: str) -> dict[str, str]:
    """Split a `registry.toml` into one verbatim text block per app.

    Text rather than parsed: an entry carries hashes and a publisher key,
    and round-tripping those through a hand-rolled TOML writer is a good
    way to install something that no longer verifies.
    """
    try:
        with open(path) as fh:
            body = fh.read()
    except OSError:
        return {}
    entries: dict[str, str] = {}
    for chunk in body.split("[[entry]]"):
        if not chunk.strip():
            continue
        ident = None
        for line in chunk.splitlines():
            if line.startswith("id = "):
                ident = line.split("=", 1)[1].strip().strip('"')
                break
        if ident:
            entries[ident] = "[[entry]]" + chunk.rstrip() + "\n"
    return entries


def compose_registry(apps_dir: str, entries: dict[str, str],
                     drop: tuple[str, ...] = ()) -> None:
    os.makedirs(apps_dir, exist_ok=True)
    kept = [v for k, v in sorted(entries.items()) if k not in drop]
    with open(os.path.join(apps_dir, "registry.toml"), "w") as fh:
        fh.write("\n".join(kept))


def installed_ids(apps_dir: str) -> list[str]:
    """What this session's install root actually holds, read off disk."""
    return sorted(registry_entries(os.path.join(apps_dir, "registry.toml")))


def a_real_png(width: int = 512, height: int = 320) -> bytes:
    """The photograph the demo works on: a harbour, drawn pixel by pixel.

    It is called `harbour.png` and it has to *be* one. The picture that
    used to be here was four sine ridges under a sun — no water and no
    boat — which at 16px in a file row was fine, because at 16px only
    the sky/land split survives. Scene 4 opens this file in an image
    editor and blows 512x320 into a 1920-wide frame, and a picture whose
    name does not match it is the most likely thing in the film to read
    as fabricated. So: a horizon with water under it, moored boats with
    their reflections, a sun path broken across the swell, and a quay
    across the near corner.

    Two things it is built to survive, and one it does not claim.

    **The crop.** The instruction the film turns on is "make this square",
    so the picture has to be one where square is a visible change rather
    than a slightly narrower version of the same shot. The subjects are
    spread the full width — a quay at the left edge, a boat at the right
    — and any square crop of a 512x320 frame has to drop one end.

    **The scale.** A one-pixel mast and a hull's sheer drawn as hard
    pixels alias into a staircase the moment the frame is enlarged, so
    the boats are sampled nine times per pixel and composited by
    coverage. That is what makes the edges read as edges rather than as
    a screenshot of a chart.

    What it does not claim is to be a photograph. It is a drawing, and
    at any size it reads as one: flat-shaded, deterministic, no lens and
    no camera. A photograph would have to be a binary in the tree, which
    is a blob nobody can review — and the demo also wants a payload
    whose size it chose rather than whatever file somebody happened to
    drop in.

    Deterministic throughout, grain included: `random` would make the
    file, and so the document delta, differ between runs.
    """
    import math
    import struct
    import zlib

    horizon = int(height * 0.46)
    sun_x, sun_y = int(width * 0.63), int(horizon * 0.34)
    sun_r = max(5.0, height * 0.042)

    def clamp(v: float) -> int:
        return 0 if v < 0 else (255 if v > 255 else int(v))

    def grain(x: int, y: int) -> float:
        h = (x * 374761393 + y * 668265263) & 0xFFFFFFFF
        h = (h ^ (h >> 13)) * 1274126177 & 0xFFFFFFFF
        return ((h >> 16) & 0xFF) / 255.0 - 0.5

    px = [[0.0, 0.0, 0.0] for _ in range(width * height)]

    # -- sky, and the sun in it -----------------------------------------
    for y in range(horizon):
        t = y / max(1, horizon)
        r, g, b = 96 + 150 * t, 128 + 106 * t, 158 + 44 * t
        for x in range(width):
            rr, gg, bb = r, g, b
            d = math.hypot(x - sun_x, (y - sun_y) * 1.15)
            if d < sun_r:
                rr, gg, bb = 255.0, 248.0, 226.0
            elif d < sun_r * 9.0:
                k = (1.0 - (d - sun_r) / (sun_r * 8.0)) ** 2.2
                rr, gg, bb = (rr + (255 - rr) * k, gg + (248 - gg) * k,
                              bb + (226 - bb) * k)
            px[y * width + x] = [rr, gg, bb]

    # -- the far shore ---------------------------------------------------
    # Low and mostly haze. It is there to give the horizon somewhere to
    # be rather than to be looked at; a hard silhouette at this distance
    # is what made the old picture read as a poster.
    def headland(x: int):
        u = x / width
        if u > 0.62:
            return None
        h = math.sin(u * 5.1 + 0.6) * 0.28 + math.sin(u * 11.0) * 0.10 + 0.62
        if u > 0.42:
            h *= 1.0 - (u - 0.42) / 0.20
        return horizon - h * height * 0.085

    for x in range(width):
        top = headland(x)
        if top is None:
            continue
        for y in range(int(top), horizon):
            base = px[y * width + x]
            k = 0.55
            px[y * width + x] = [base[0] * (1 - k) + 86 * k,
                                 base[1] * (1 - k) + 104 * k,
                                 base[2] * (1 - k) + 112 * k]

    # -- the boats, as shapes ---------------------------------------------
    # (centre x, waterline y, hull length, hull colour, deck colour)
    boats = [
        (int(width * 0.20), horizon + int(height * 0.10), width * 0.115,
         (28, 40, 56), (222, 220, 208)),
        (int(width * 0.45), horizon + int(height * 0.045), width * 0.075,
         (44, 62, 70), (208, 206, 196)),
        (int(width * 0.83), horizon + int(height * 0.20), width * 0.175,
         (34, 34, 44), (232, 228, 214)),
    ]

    def boat_pixels(length: float, hull_col: tuple, deck_col: tuple) -> list:
        """`(dx, dy, colour, coverage)` relative to (centre, waterline).

        Sampled nine times per pixel rather than tested once: see the
        note about scale above.
        """
        hull_h = max(3.0, length * 0.16)
        half = length / 2.0
        mast_h = length * 1.05
        cabin_w = length * 0.26
        cabin_h = max(2.0, length * 0.10)
        mast_x = -length * 0.04
        mast_w = 0.8 + length * 0.012
        boom_y = -hull_h - cabin_h - mast_h * 0.10
        boom_to = length * 0.30
        spar = (58, 56, 52)

        def at(u: float, v: float):
            """The boat's colour at boat-local `(u, v)`, or None."""
            if (-mast_w * 0.5 <= u - mast_x <= mast_w * 0.5
                    and -mast_h <= v <= -hull_h):
                return spar
            if boom_y - 0.9 <= v <= boom_y + 0.9 and mast_x <= u <= boom_to:
                return spar
            if (-cabin_w * 0.55 <= u <= cabin_w * 0.45
                    and -hull_h - cabin_h <= v <= -hull_h):
                return deck_col
            if abs(u) <= half:
                t = abs(u) / half
                if -hull_h <= v <= -hull_h * (t ** 2.4):
                    # A pale strake along the sheer, stopping short of
                    # the ends: it is what tells a hull from a wedge.
                    if v < -hull_h + 1.6 and t < 0.9:
                        return deck_col
                    return hull_col
            return None

        out = []
        for dy in range(int(-mast_h) - 1, 2):
            for dx in range(int(-half) - 1, int(max(half, boom_to)) + 2):
                hits, r, g, b = 0, 0.0, 0.0, 0.0
                for sy in (-0.33, 0.0, 0.33):
                    for sx in (-0.33, 0.0, 0.33):
                        c = at(dx + sx, dy + sy)
                        if c is not None:
                            hits += 1
                            r, g, b = r + c[0], g + c[1], b + c[2]
                if hits:
                    out.append((dx, dy, (r / hits, g / hits, b / hits),
                                hits / 9.0))
        return out

    drawn = [(bx, by, boat_pixels(ln, hc, dc)) for bx, by, ln, hc, dc in boats]

    # -- water -------------------------------------------------------------
    for y in range(horizon, height):
        d = (y - horizon) / max(1.0, height - horizon)      # 0 far, 1 near
        base_r, base_g, base_b = 124 - 96 * d, 142 - 96 * d, 148 - 80 * d
        # Wave bands in a *perspective* coordinate, not in y. Spacing on
        # a real surface seen at a shallow angle opens out toward the
        # viewer; bands evenly spaced down the frame are the one thing
        # that reads instantly as a texture rather than as water.
        q = (y - horizon + 1) ** 0.55
        amp = 2.5 + 20.0 * d
        for x in range(width):
            swell = math.sin(q * 3.1 + math.sin(x * 0.0135) * 2.2 + 0.7)
            chop = math.sin(q * 8.6 + x * 0.011 + math.sin(x * 0.04) * 0.8)
            v = (swell * 0.68 + chop * 0.32) * amp
            px[y * width + x] = [base_r + v, base_g + v * 1.04,
                                 base_b + v * 0.88]

    # -- the sun's path across it -------------------------------------------
    for y in range(horizon, height):
        d = (y - horizon) / max(1.0, height - horizon)
        q = (y - horizon + 1) ** 0.55
        spread = 14 + 74 * d
        # On the crests, not across them: the same phase the chop has, so
        # the light lands on the faces already turned toward it.
        crest = (0.5 + 0.5 * math.sin(q * 8.6)) ** 1.8
        for x in range(int(sun_x - spread), int(sun_x + spread) + 1):
            if not 0 <= x < width:
                continue
            k = max(0.0, 1.0 - abs(x - sun_x) / spread) ** 1.7
            # Broken along its length as well, or it is a beam rather
            # than a scatter — and a continuous column down the middle
            # of a picture reads as a rendering artefact and nothing
            # else.
            k *= crest * (0.45 + 0.55 * (0.5 + 0.5
                                         * math.sin(x * 0.23 + y * 0.09)))
            k *= 0.95 - 0.32 * d
            p = px[y * width + x]
            px[y * width + x] = [p[0] + (255 - p[0]) * k,
                                 p[1] + (250 - p[1]) * k,
                                 p[2] + (218 - p[2]) * k]

    # -- and the boats' reflections in it ------------------------------------
    def wobble(y: int, waterline: int) -> int:
        depth = y - waterline
        return int(round(math.sin(depth * 0.55) * (1.0 + depth * 0.10)))

    for bx, by, pixels in drawn:
        for dx, dy, col, cov in pixels:
            if dy >= 0:
                continue
            ry = by - dy                       # mirrored below the waterline
            if not horizon <= ry < height:
                continue
            fade = max(0.0, 0.46 - (ry - by) * 0.013) * cov
            if fade <= 0.0:
                continue
            x = bx + dx + wobble(ry, by)
            if not 0 <= x < width:
                continue
            # Darker than the thing reflected, and pulled toward the
            # water. A mirror of the literal colours put two near-white
            # blobs under the hulls: a white deck reflects as grey,
            # because what comes back off water is a fraction of what
            # went in, and the eye knows it.
            p = px[ry * width + x]
            px[ry * width + x] = [p[0] * (1 - fade) + col[0] * 0.58 * fade,
                                  p[1] * (1 - fade) + col[1] * 0.60 * fade,
                                  p[2] * (1 - fade) + col[2] * 0.64 * fade]

    # -- the boats themselves ------------------------------------------------
    for bx, by, pixels in drawn:
        for dx, dy, col, cov in pixels:
            x, y = bx + dx, by + dy
            if not (0 <= x < width and 0 <= y < height):
                continue
            p = px[y * width + x]
            px[y * width + x] = [p[0] * (1 - cov) + col[0] * cov,
                                 p[1] * (1 - cov) + col[1] * cov,
                                 p[2] * (1 - cov) + col[2] * cov]

    # -- a quay across the near corner ---------------------------------------
    # The left end of the picture, and the reason a square crop of it is
    # a different picture rather than the same one with less sky.
    quay = (108, 100, 92)
    quay_w = width * 0.34
    for x in range(int(quay_w)):
        u = x / quay_w
        top = height * 0.80 + u * height * 0.24
        if top >= height:
            continue
        for y in range(int(top), height):
            k = min(1.0, (y - top) / (height * 0.16))
            band = math.sin(y * 0.55) * 5 + math.sin(x * 0.31 + y * 0.9) * 4
            edge = 1.28 if y - top < 2 else 1.0
            px[y * width + x] = [(quay[0] * (1 - 0.30 * k) + band) * edge,
                                 (quay[1] * (1 - 0.30 * k) + band) * edge,
                                 (quay[2] * (1 - 0.30 * k) + band) * edge]
    for cx, ch in ((int(width * 0.07), 13), (int(width * 0.235), 10)):
        top = int(height * 0.80 + (cx / quay_w) * height * 0.24)
        for dy in range(-ch, 1):
            wide = 3 if dy > -ch + 2 else 2
            for dx in range(-wide, wide + 1):
                x, y = cx + dx, top + dy
                if 0 <= x < width and 0 <= y < height:
                    px[y * width + x] = [58.0, 54.0, 52.0]

    # -- grain, and out -------------------------------------------------------
    # Enough to break the banding a pure gradient shows on a large flat
    # sky, and no more: grain is nearly incompressible, and this file's
    # size *is* the payload that crosses into the document.
    rows = bytearray()
    for y in range(height):
        rows.append(0)  # filter: none
        for x in range(width):
            p = px[y * width + x]
            n = grain(x, y) * 2.5
            rows += bytes((clamp(p[0] + n), clamp(p[1] + n), clamp(p[2] + n)))

    def chunk(tag: bytes, payload: bytes) -> bytes:
        return (struct.pack(">I", len(payload)) + tag + payload
                + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF))

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(rows), 9))
            + chunk(b"IEND", b""))


def stage_home(path: str) -> str:
    """A home of this session's own, staged to look like one.

    Two things are on screen here that are easy to get wrong and
    impossible to unsee.

    **The path.** Files draws the directory it is showing, component by
    component, and a home under the checkout renders as
    `… › quickdesktop › .claude › worktrees › yc-demo › target › …`. A
    viewer reads that as a harness pointed at a scratch directory, and
    `.claude` tells them which tooling built it. So the homes live under
    [`HOMES_ROOT`], whose components each read as a machine's own.

    **The contents.** A home holding one folder called `Demo` is not a
    home anybody believes, and it gives a drag beat nothing to pick up.
    The tree below is the ordinary furniture of a home directory — the
    XDG folders the sidebar already lists, a couple of documents, and
    the photograph beat 8 drags in.

    Staging a *file tree* is not the same as fabricating content: there
    is no invented correspondence here and nobody's name on anything.
    The documents carry the few lines the demo itself writes and reads.
    """
    for folder in ("Documents", "Downloads", "Pictures", "Projects"):
        os.makedirs(os.path.join(path, folder), exist_ok=True)
    # The photograph is deliberately **not** written here. It lives in
    # the room's workspace ([`WORKSPACE_ROOT`], staged by
    # [`stage_workspace`]) because that is the only place it can be one
    # object rather than one per machine.
    #
    # This used to write it twice into every home — `~/harbour.png` and
    # `~/Pictures/harbour.png`, in all three homes. Six copies of a
    # picture the film calls "the thing he is holding". The beat that
    # asks whether a guest can reach the host's object would have passed
    # against his own copy of it, and passed for the wrong reason.
    with open(os.path.join(path, "Documents", "Launch plan.md"), "w") as fh:
        fh.write("# Launch plan\n\n- Venue - The Hall\n- Date - 3 Sep\n")
    with open(os.path.join(path, "Documents", "Checklist.md"), "w") as fh:
        fh.write("# Checklist\n\n- Doors\n- Sound\n- Seating\n")
    with open(os.path.join(path, "Projects", "README.md"), "w") as fh:
        fh.write("# Projects\n")
    return path


def stage_workspace(path: str = WORKSPACE_ROOT) -> str:
    """The room's own directory, holding the photograph, staged once.

    One directory for the whole rig, not one per session — which is the
    entire point. Every session is pointed at *this* path through
    `QOS_WORKSPACE_ROOTS`, so when two file managers list it they are
    listing the same inode, and when one person points at a row the
    address he sends names a thing the other machine can also name.

    A workspace root is a first-class location to the file manager, not
    a directory it happens to be showing: `model::workspace_roots` reads
    it out of the environment, `is_workspace_path` gates the per-entry
    sync-state lookup on it, and `initial_path` opens on it. So a
    session that starts here starts *in the shared workspace*, which is
    where a collaborative desktop's files are supposed to be and is a
    better first frame than somebody's home.

    Kept deliberately small. A workspace with four folders and a stack
    of documents in it is a stage set; what the film needs is the one
    object two people both reach, and a couple of things beside it so a
    listing has more than a single row in it.
    """
    os.makedirs(path, exist_ok=True)
    with open(os.path.join(path, PHOTO_NAME), "wb") as fh:
        fh.write(a_real_png())
    with open(os.path.join(path, "Run of show.md"), "w") as fh:
        fh.write("# Run of show\n\n- Doors\n- Sound\n- Seating\n")
    return path


class Registry:
    """A signed registry on loopback, serving one real `.qpkg`.

    The `serve-test-registry` example packs a real binary into a real
    package, signs it, and writes the `registries.toml` the catalogue
    reads — so the fetch in beat 5 verifies an ed25519 signature and a
    BLAKE3 digest for real. Absent binaries are a supported state: the
    beat says the fetch could not be filmed and the film goes on.
    """

    def __init__(self, example_bin: str, payload: str, config_dir: str,
                 log_path: str) -> None:
        self.example_bin = example_bin
        self.payload = payload
        self.config_dir = config_dir
        self.log_path = log_path
        self.proc: subprocess.Popen | None = None
        self._log = None
        self.base_url: str | None = None

    def __enter__(self) -> "Registry | None":
        if not (self.example_bin and os.path.exists(self.example_bin)
                and os.path.exists(self.payload)):
            return None
        os.makedirs(self.config_dir, exist_ok=True)
        self._log = open(self.log_path, "w+b")
        self.proc = subprocess.Popen(
            [self.example_bin, "--config-dir", self.config_dir,
             "--payload", self.payload, "--package-id", WORD_PROCESSOR,
             "--name", "Word Processor"],
            stdout=self._log, stderr=subprocess.STDOUT, start_new_session=True)
        # It announces its port once it is listening. Packing a large
        # binary happens before that line, so the wait is real work.
        deadline = time.time() + 180.0
        while time.time() < deadline:
            if self.proc.poll() is not None:
                print(f"  the test registry exited early "
                      f"({self.proc.returncode}); see {self.log_path}")
                return None
            with open(self.log_path, "rb") as fh:
                text = fh.read().decode(errors="replace")
            m = re.search(r"registry (http://\S+) serving", text)
            if m:
                self.base_url = m.group(1)
                return self
            time.sleep(0.3)
        print(f"  the test registry never announced a port; see {self.log_path}")
        return None

    def __exit__(self, *exc) -> bool:
        if self.proc is not None and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self._log is not None:
            self._log.close()
        return False


#: How long the page is given to catch up with the keyboard before a
#: beat that waits for it gives up and says so.
#:
#: Generous, because what it is waiting for is a real application
#: applying real keystrokes and the honest answer is however long that
#: takes. Measured on a three-session room, the word processor has been
#: seconds behind the last keystroke — which is exactly the wait this
#: exists to make visible rather than to paper over.
WRITTEN_BUDGET_S = 15.0


def written_on_the_page(s: Session, wp: dict | None, room: str, proof: str,
                        budget: float = WRITTEN_BUDGET_S) -> float | None:
    """Wait until the app really holds `proof`, and has drawn since.

    Seconds waited, or `None` if it never got there.

    The beat this serves is the film's whole argument — somebody types,
    and it is on another machine — and the way that beat failed was not
    a missing wait but a wait on the wrong thing. `type_prose` returns
    when the last keystroke has been *posted to the compositor*, and a
    rig that treats that as "he has written" is timing the socket rather
    than the document. On the take this was written for, the last
    keystroke was posted at 48.4s, the page was still showing the first
    letter of the sentence at 51.0s, and the film cut away on it.

    So two questions, asked of two different things:

    * **the document** — the app's own, over the broker, in the app's own
      protocol (`DocumentReader`). What comes back is produced by the
      running application out of what it is holding, so it is the app
      saying it took the keystrokes rather than the compositor saying it
      sent them.
    * **the screen** — `frame_seq` is the count of frames the compositor
      has taken from that client, so a frame taken *after* the document
      answered is a frame drawn from a document that already had the
      words in it. A `ui` app draws from its document; there is no third
      state where it holds the sentence and paints something else.

    A budget that runs out is a GAP rather than a failure: it says the
    page did not catch up in the time given, which is a thing to know
    about the product and not a thing to hide from the transcript.
    """
    began = time.time()
    reader = None
    doc, carried = None, False
    try:
        reader = DocumentReader(s.collab_json_sock, str(room), None,
                                handle="typing")
        doc, carried = reader.await_document(proof, budget)
    except OSError as exc:
        gap(f"{s.name}: no broker answered on {s.collab_json_sock} ({exc}), "
            f"so whether the words reached his own document was never "
            f"established")
        return None
    except CannotDecode as exc:
        gap(f"{exc} — so whether the words reached his own document was "
            f"never established, which is not the same as their not having")
        return None
    finally:
        if reader is not None:
            reader.close()
    if doc is None:
        gap(f"{s.name}: his own word processor never answered the "
            f"handshake within {budget:.0f}s, so nothing here knows what "
            f"is on his page")
        return None
    if not carried:
        gap(f"{s.name}: his own app answered with {len(doc)} bytes and "
            f"none of it is {proof!r} — the keystrokes were sent and the "
            f"document did not take them")
        return None
    # And on screen.
    if drawn_since(s, wp or {}, max(0.0, budget - (time.time() - began))):
        return time.time() - began
    gap(f"{s.name}: the page holds {proof!r} and the app has not drawn a "
        f"frame since, so the film cannot say the words are on screen")
    return None


def drawn_since(s: Session, subject, budget: float = 3.0) -> bool:
    """Wait for `subject`'s client to commit a frame from here on.

    `frame_seq` counts frames the compositor has actually taken from a
    client (`native.rs::foreign_frames_json`), so this is the compositor
    answering "has that application redrawn since I asked", which is a
    different question from "is it on screen" and the one a beat needs
    after it has changed something the application has to react to.

    False when the budget runs out. Silent — the caller knows what it was
    waiting for and what its absence costs, and this is called from two
    beats that say very different things about it.
    """
    def seq() -> int:
        seen = [t.get("frame_seq") or 0 for t in tiles_for(s, subject)]
        return max(seen) if seen else 0

    was = seq()
    due = time.time() + budget
    while time.time() < due:
        if seq() > was:
            return True
        time.sleep(0.1)
    return False


def type_into_the_document(s: Session, wp: dict | None, text: str,
                           room: str | None = None,
                           proof: str | None = None) -> bool:
    """Put the caret in the page and type, the way a person would.

    With a `room`, this does not return until the words are in the app's
    own document and the app has drawn since — see `written_on_the_page`,
    and the take that made it necessary.

    The word processor is a foreign client, so its insides are not in
    the compositor's widget registry and there is no name to aim at.
    Its *tile* has one, which is as close as the shell can get: click
    into the middle of the page, then type.

    The click is belt and braces now rather than a workaround. It was
    added because typing without it put nothing in the document, and
    that was read as the app opening with its caret outside the body —
    which is not what was wrong: the app opens with the caret at the
    start of the body, draws it there, and takes keystrokes with
    nothing clicked (`apps/word-processor/tests/opens_ready_to_type.rs`
    and the framebuffer reading beside it in `widget.rs`). What the
    click also does is settle which window the seat is pointed at, and
    that is worth keeping in a scripted run.
    """
    surface = (wp or {}).get("surface_id")
    stem = f"foreign-surface-{surface}"
    named = next((n for n in s.target_names()
                  if n == stem or n.startswith(stem + "[")), None)
    rect = s.named_rect(named) if named else None
    if not rect:
        others = [n for n in s.target_names()
                  if n.startswith("foreign-surface-")]
        note(f"{s.name}: no laid-out tile for surface {surface}; "
             f"laid out now: {others or 'none'}")
        return False
    bx, by, bw, bh = rect
    s.click_at(bx + bw * 0.4, by + bh * 0.35)
    # Long enough for the click to be taken and the caret drawn where it
    # landed. This is an input round trip, not a frame the beat is about:
    # the beat is the words appearing, which starts below.
    s.settle(0.6)
    # To the end of what is already there, rather than wherever the
    # click happened to land. By this point the document has a sentence
    # and an image in it, and a caret dropped into the middle of that
    # writes the list into the middle of it — or replaces a selected
    # image with the first character typed.
    #
    # A *held* Control, not the `modifiers` mask. That mask is the
    # compositor's own, read by chrome; a client resolves keycodes
    # through the seat's xkb state, which the mask never touches. So the
    # `modifiers(ctrl=True)` this used to do sent a bare `End` — end of
    # the *line*, not of the document — and did it silently, which is
    # exactly the kind of thing that only shows up as a list typed into
    # the middle of a paragraph.
    s.key_down("Control")
    s.key("End")
    s.key_up("Control")
    s.settle(0.35)
    s.type_prose(text)
    if room is None:
        # No room to ask, so the old contract: the keystrokes went, and a
        # moment on the finished text before whatever comes next moves
        # the strip out from under it.
        s.settle(0.7)
        return True
    waited = written_on_the_page(s, wp, room, proof or text.strip())
    if waited is None:
        return False
    note(f"{s.name}: the page had the whole sentence on it {waited:.1f}s "
         f"after the last keystroke went")
    # And a moment on the finished text, now that there is some, before
    # whatever comes next moves the strip out from under it.
    s.settle(0.7)
    return True


def sources_newer_than(binary: str, roots: list[str]) -> list[str] | None:
    """Which `.rs` files under `roots` are newer than `binary`.

    `None` when the binary cannot be stat'd at all. An empty list means
    the binary is current.

    Split out of the compositor's own check because that check existed,
    worked, and still let a six-hour-old agent be filmed: it guarded one
    binary, and the rig runs three. A guard that covers one of the things
    it should cover reads exactly like a guard that covers all of them,
    which is worse than having none.
    """
    try:
        built = os.path.getmtime(binary)
    except OSError:
        return None
    newer: list[str] = []
    for root in roots:
        for dirpath, _dirs, files in os.walk(root):
            for name in files:
                if not name.endswith(".rs"):
                    continue
                path = os.path.join(dirpath, name)
                try:
                    if os.path.getmtime(path) > built:
                        newer.append(os.path.relpath(path, REPO))
                except OSError:
                    pass
    newer.sort()
    return newer


def refuse_if_stale(binary: str, roots: list[str], build: str,
                    allow: bool) -> bool:
    """Print why a binary is too old to film, and say whether to stop.

    Newer source is refused rather than warned about: a warning in a
    three-hundred-line transcript is a warning nobody reads, and a frame
    cannot tell you which build it came from.
    """
    newer = sources_newer_than(binary, roots)
    if allow:
        # Permitted, but never silent. One flag waives both binaries, so
        # somebody who passes it to film a compositor they know is stale
        # would otherwise also film a stale agent without being told —
        # which is the failure this whole guard exists to prevent, just
        # wearing the flag's permission as cover.
        if newer:
            print(f"  filming a stale {os.path.relpath(binary, REPO)}: "
                  f"{len(newer)} source file(s) are newer, including "
                  f"{newer[0]}")
        return False
    if newer is None:
        print(f"cannot stat {binary}")
        return True
    if not newer:
        return False
    print(f"\nrefusing to record: {os.path.relpath(binary, REPO)} is older "
          f"than {len(newer)} source file(s) under test")
    for path in newer[:8]:
        print(f"    {path}")
    if len(newer) > 8:
        print(f"    ... and {len(newer) - 8} more")
    print(f"\n  {build}")
    print("  (or --allow-stale-shell to film it anyway)")
    return True


def agent_binary() -> str | None:
    """`qos-agent`, if it has been built.

    A separate process rather than a compositor: the whole point of it
    is that a participant can hold a document, a caret and a name in the
    room's history without holding a screen. Absent is a supported
    state — the beat says so and stops, rather than substituting
    something that looks similar.
    """
    for profile in ("demo", "release", "debug"):
        path = os.path.join(TARGET, profile, "qos-agent")
        if os.path.exists(path):
            return path
    return None


def run_the_agent(agent: Session, room: str, shots: str,
                  timeout: float = 120.0) -> bool | None:
    """Let the agent join the document and carry out one instruction.

    It connects to its own session's broker, which is how its edits
    reach the other two: the same libp2p republish every peer's edits
    take.

    It runs **with** the model. `--no-model` was passed until today, on
    the reasoning that a keyword table keeps a film reproducible — but
    the model was only ever a four-way classifier choosing between
    table, caption, heading and none, with every character computed
    locally, so a live run and an offline one produced byte-identical
    documents. What it now drafts is the content, and the difference is
    the beat: it decides which lines are scheduled items, orders them,
    and says when they contradict the prose above them.

    The keyword fallback remains and knows `table`, `caption` and
    `heading` — not `run sheet`. If the model is unreachable it answers
    on the page saying it cannot do what was asked, which is honest and
    is not the beat.

    Returns None when the binary is not built, so a caller can say that
    rather than report a failure it did not observe.
    """
    binary = agent_binary()
    if binary is None:
        return None
    log_path = os.path.join(shots, "agent.log")
    with open(log_path, "w") as log:
        # `--app` is named rather than left to its default, because the
        # default is the whole reason this beat once filmed a file
        # manager: with no address to narrow on, the broker's fan-out is
        # room-wide, the agent is offered every document open in the room
        # and takes whichever arrives first — and in one run that was the
        # file explorer's, whose actor id (`files-2`) then refused to
        # merge with the word processor's when that finally turned up.
        #
        # `--record` is the other half of that address and is deliberately
        # *not* passed: it names the file a document was opened from, and
        # this document has no file. It is the untitled document the app
        # created when beat 5 launched it, so its record is empty, and
        # `--record ""` would be a different address from "no record"
        # rather than the same one. Staging a file to open instead would
        # give this something to name — it needs a way to launch a
        # packaged app *on* a path, which the applications action does
        # not expose today.
        proc = subprocess.Popen(
            [binary, "--workspace", str(room), "--name", AGENT_NAME,
             # Room-wide, with the grants named rather than inherited.
             #
             # `--app` does two things at once: it scopes the CRDT
             # route, and it defaults the capability list. Scoped to
             # the word processor, the broker's `delivers_to` refuses
             # every document whose app differs -- including the chat
             # that a remark is written into. The instruction xaver
             # gives then never reaches the agent at all, and the beat
             # the whole film is built to reach cannot happen.
             #
             # Empty is the flag's own documented room-wide mode: "a
             # connection that announces nothing is handed every app's
             # state and the agent can enumerate the room instead of a
             # single document". It costs what the note here used to
             # warn about -- with nothing to narrow on, the agent has
             # to pick its document out of everything the room sends,
             # and once picked the file manager's. That is a risk of
             # choosing wrong in place of a certainty of never being
             # asked.
             #
             # The grants are then explicit, because an empty `--app`
             # would otherwise leave the agent holding nothing: with no
             # grants "it enumerates nothing and performs nothing".
             # These four are the apps the film needs it to cross --
             # read the remark, open the editor, and land the result in
             # the document the picture came out of Files to reach.
             "--app", "",
             "--grant", "word-processor", "--grant", "files",
             "--grant", "chat", "--grant", "photos",
             "--socket", agent.collab_json_sock, "--once",
             "--pace-ms", "250"],
            stdout=log, stderr=subprocess.STDOUT, cwd=REPO,
            preexec_fn=die_with_parent(enabled=not KEEP_SESSIONS))
        if not KEEP_SESSIONS:
            track(proc)
        try:
            code = proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            proc.kill()
            note(f"the agent did not finish within {timeout:.0f}s; "
                 f"see {log_path}")
            # A bare timeout sends the next person to read a hung
            # process. The usual cause is upstream of the agent and is
            # visible in the session's own log: the broker computes an
            # addressed channel key and then fans out room-wide anyway,
            # so every app in the room answers the agent's handshake and
            # it is offered a document from an app it did not ask for.
            # Said here so it is diagnosed once rather than every time.
            try:
                with open(agent.log_path) as fh:
                    session = fh.read()
            except OSError:
                session = ""
            wide = [ln for ln in session.splitlines()
                    if "room_wide=true" in ln and "addressed=" in ln]
            others = [ln for ln in session.splitlines()
                      if "recv Arrived" in ln
                      and WORD_PROCESSOR not in ln.split("]")[0]]
            if wide or others:
                note("the broker fanned this session's collaboration "
                     "traffic room-wide while carrying an addressed "
                     "channel key, so apps the agent did not ask for "
                     "answered its handshake:")
                for ln in (wide[-1:] + others[-2:]):
                    note(f"    {ln.strip()[:200]}")
                note("that is a broker fix, not an agent or a demo one — "
                     f"see {agent.log_path}")
            return False
    if code != 0:
        with open(log_path) as fh:
            tail = fh.read()[-400:]
        note(f"qos-agent exited {code}: {tail}")
    return code == 0


# --------------------------------------------------------------------
# the demonstration
# --------------------------------------------------------------------

# Set by `main`, so a beat can report on all three sessions without
# every beat function taking all three.
SESSIONS: list[Session] = []

# The headless viewport every session was started at, as (w, h). Set by
# `main` from the same string the compositors were given, so a beat can
# ask whether what it is about to claim is actually inside the frame.
VIEWPORT: tuple[int, int] = (0, 0)


#: How long the film sits on the empty desktop before anything is opened
#: on it.
#:
#: The shot is deliberate: the strip, the wheel and the bar have to read
#: as themselves once, before a window covers half of them. It is also
#: the only part of the opening this script controls — what follows is
#: an application starting, which takes what it takes — so it is held on
#: purpose and kept short, rather than arrived at by accident out of
#: whatever bookkeeping happened to run before the launch.
EMPTY_DESKTOP_HOLD_S = 0.8


def beat_one(quinten: Session, shots: str) -> tuple[dict, dict | None]:
    banner("1.  an empty desktop, and Files opened on it")
    # The host's screen. A first boot really is empty here — one
    # workspace, no strips, no windows — so "empty" is read off the tree
    # rather than asserted about a backdrop.
    cut("quinten")

    # Only what has to be true *before* the launch, because everything
    # asked here is a round trip into a compositor the camera is already
    # most of the way through, and the answer is seconds of empty desktop
    # in front of the first window. The rest of this run's bookkeeping —
    # three processes, three identities, who owns the room — is as true
    # after the launch as before it, and is printed there instead.
    host_ws = quinten.active_workspace()
    claim("his desktop starts with no windows on it",
          len(quinten.tabs_here()), 0)
    quinten.park_pointer()
    quinten.snapshot(os.path.join(shots, "01-empty-desktop.png"))
    read_hold(EMPTY_DESKTOP_HOLD_S)

    step("he opens Files")
    # By its own id through the applications provider — the registry that
    # feeds the palette. There is no on-screen affordance for the first
    # window; the palette is a chord, and this is the same row it offers.
    #
    # The camera gets out of the way while the app starts. Snapshotting
    # is most of a filmed compositor's event loop, and the compositor is
    # what spawns this client, hands it its configure and takes its first
    # buffer — so filming the launch at full rate is the camera competing
    # with the thing it is filming. Measured on this build: 1.9s to a
    # first frame and 2.2s to a window that fills its tab with the camera
    # eased, against 3.3s for both with it running flat out. The film
    # loses frame rate for two seconds at the front and gets a second of
    # its opening back, and — more to the point — stops showing the
    # product's own speed as worse than it is.
    with easing(4.0):
        painted = quinten.launch(FILES_APP)
    claim("Files opened and painted a frame", painted is not None, True)
    if painted:
        note(f"surface {painted.get('surface_id')} at frame "
             f"{painted.get('frame_seq')}")
    tabs = quinten.tabs_here()
    claim("and it is the only window on his desktop", len(tabs), 1)
    quinten.snapshot(os.path.join(shots, "02-files.png"))

    # What this run is made of, said once — and off camera, because none
    # of it has a picture. Three process ids and three libp2p identities
    # are read out of three sessions, and every one of those reads is a
    # round trip into a compositor the camera is already most of the way
    # through: filmed, they are a window sitting still while a script
    # does bookkeeping, which is exactly the dead air at the front of
    # this film that nothing on screen accounted for.
    with off_camera("the run's own bookkeeping - three processes, three "
                    "identities, who owns the room - which is a transcript "
                    "fact and not a shot"):
        ids = {s.name: peer_id_of(s) for s in SESSIONS}
        for s in SESSIONS:
            note(f"{s.name:8} pid {s.proc.pid:<8} user {s.user:<8} peer "
                 f"{ids[s.name]}")
        claim("three separate processes", len({s.proc.pid for s in SESSIONS}), 3)
        claim("three separate libp2p identities", len(set(ids.values())), 3)
        note(f"quinten's room {host_ws['room']}  named {host_ws['name']!r}")
        claim("quinten owns his room", host_ws["owner"], quinten.user)
    # The surface travels with it: beat 8 drags a file *out of this
    # window*, and by then a second client is on screen announcing the
    # same wayland app-id. The surface the launch saw paint is the only
    # thing that tells the two apart.
    return host_ws, painted


def beat_two(quinten: Session, xaver: Session, host_ws: dict,
             shots: str) -> None:
    banner("2.  quinten invites xaver")
    cut("quinten")

    step("he opens the room's people")
    # Through the chrome, at a pace a viewer can follow, and every step of
    # it a click on something painted. This whole beat used to pass in
    # under a second because it was invoked as an action: every claim held
    # and the collaboration UI never appeared, which is the one thing the
    # beat exists to put on screen.
    via = show_collaborators(quinten)
    note(f"opened from {via}")
    claim("the collaborators panel is up", quinten.overlays()["collaborators"],
          True)
    here = quinten.active_workspace().get("collaborators") or []
    note(f"the room holds {len(here)} collaborator(s) — an empty roster is "
         f"exactly when the way in matters most, and it is the case that "
         f"used to paint nothing at all")
    read_hold()
    quinten.snapshot(os.path.join(shots, "02a-collaborators-empty.png"))

    token = invite_from_the_panel(quinten, xaver.user, shots, "02b")
    claim("quinten raised an invite from the panel he opened", bool(token),
          True)
    if not token:
        raise SystemExit("no invite token: the rest cannot run")
    note("the token is a signed blob carrying the room id, the owner's "
         "address and an expiry. The name is not in it: it waits on this "
         "machine until somebody redeems the token and is written into the "
         "room document then, by the room's own authority, so both "
         "machines can attribute a cursor to a person nobody self-declared")

    # Redeeming is xaver's, and it is his screen that changes — and the
    # film does not go there for it.
    #
    # This is filmed, and the reasoning that took it off camera was
    # wrong. Watching somebody paste a string into a dialog is indeed the
    # least surprising thing in this system — but the beat is not the
    # paste, it is *a second machine joining*, which is the claim the
    # whole first half is building toward. Taken away, the roster simply
    # gains a member and the audience is asked to believe a join
    # happened somewhere they could not see.
    #
    step("the token travels — by hand, off quinten's clipboard")
    # He does not redeem it the instant it is minted. That reads as
    # staged, because it is: two sessions on one machine sharing a
    # clipboard. A person reads a code, crosses to another machine, and
    # enters it — and the seconds that takes are seconds the room sits
    # there waiting, which is both the truth and room for a voice-over.
    #
    # The redemption itself stays off camera. Watching somebody paste a
    # string into a dialog is the least surprising thing in this system,
    # and the beat that matters is his arrival on the roster of the
    # screen already being watched.
    quinten.settle(4.0)
    with off_camera("xaver redeeming the invitation on his own machine — "
                    "quinten's roster gains him on screen shortly"):
        claim("xaver joined",
              join_room(xaver, token, via="panel", shots=shots, tag="02c"), True)
        claim("xaver's own state holds quinten's room",
              xaver.active_workspace()["room"], host_ws["room"])
        claim("and his own copy names quinten as owner",
              xaver.active_workspace()["owner"], quinten.user)


def beat_three(quinten: Session, agent: Session, host_ws: dict,
               shots: str) -> None:
    banner("3.  quinten invites the agent")
    # No cut anywhere in this beat. It runs off camera (see `run`): it is
    # the same card the film has just spent fifteen seconds on, made out
    # to a different name, and the half of it that is actually new — an
    # agent is a subject in the permission model, not a peer — is a
    # question asked of the authority gate and answered in this
    # transcript. There is nothing in it that a second look at the same
    # modal would show.
    step("he opens the room's people again — xaver is in it now")
    via = show_collaborators(quinten)
    note(f"opened from {via}")
    claim("the collaborators panel is up", quinten.overlays()["collaborators"],
          True)
    quinten.snapshot(os.path.join(shots, "03a-collaborators-xaver.png"))

    step("a second invitation — but an agent is not a peer")
    token = invite_from_the_panel(quinten, agent.user, shots, "03b")
    claim("quinten raised a second invite from the same panel", bool(token),
          True)
    if not token:
        raise SystemExit("no second token: the agent cannot join")

    # No cut. The agent has no screen worth filming and showing one
    # inverts the claim: it is a participant in somebody else's room, so
    # it appears as a row in a roster, a caret in a document and a name
    # in a history — never as a desktop of its own.
    claim("the agent joined the room", join_room(agent, token, via="action"),
          True)
    claim("the agent's own state holds quinten's room",
          agent.active_workspace()["room"], host_ws["room"])
    # Where the two routes differ, said plainly. The agent has no mouse
    # to reach a top bar with, so it raises the same card by its stable
    # address — `peer.connect`, one of the actions beat 9 enumerates —
    # and redeems with the card's own Paste and Join. The token is not
    # typed on either machine, and there is no route here that types it.
    note("the agent reached the same prompt by address rather than by "
         "opening a panel; the token was pasted and redeemed with the "
         "card's own buttons, never typed")

    step("what the agent is allowed to do, before quinten says so")
    # The more interesting half, and the one that makes an agent a
    # different kind of participant: it is a subject in the permission
    # model, `agent:<id>`, and its authority is a grant on a scope. Asked
    # of the production gate, which answers about a verb on a scope.
    def may_write(app: str) -> bool:
        answer = quinten.check_action(AGENT_SUBJECT, app, "SpliceText") \
            .get("answer", {}) or {}
        return bool(answer.get("permitted"))

    probe = quinten.check_action(AGENT_SUBJECT, WORD_PROCESSOR, "SpliceText")
    note(f"check_action({AGENT_SUBJECT}, {WORD_PROCESSOR}, SpliceText) -> "
         f"{json.dumps(probe.get('answer', probe))[:140]}")
    claim("the agent may not write to the word processor yet",
          may_write(WORD_PROCESSOR), False)

    step(f"quinten grants it write on {AGENT_SCOPE} — that app, and nothing else")
    granted = quinten.grant(AGENT_SUBJECT, AGENT_SCOPE, "write")
    claim("the grant was accepted", "err" not in granted, True)
    if "err" in granted:
        note(json.dumps(granted["err"])[:200])
    claim("now it may — the same question, the same gate",
          bool(wait_for(lambda: may_write(WORD_PROCESSOR), timeout=SYNC_TIMEOUT_S)),
          True)
    # And the scope is a real boundary rather than a label: the grant
    # named one app, so it must not answer for another.
    claim("and still may not write to a different app",
          may_write("dev.qos.terminal"), False)
    note("a capability is a verb on a scope; the scope here is one app")

    step("and the session it drives is admitted to the room as itself")
    # The grant above is about the principal inside an app. The session
    # is a separate subject, and a guest who has only been let in is not
    # yet allowed to act — so it is admitted explicitly, the same way
    # xaver would be.
    admitted = quinten.grant(f"person:{agent.user}", "workspace", "write")
    claim("quinten admits the agent's session as an editor",
          "err" not in admitted, True)
    if "err" in admitted:
        note(json.dumps(admitted["err"])[:200])

    # And admitted as what it is. The grant above names the *session* --
    # a subject the sync path needs in order to let this peer take part
    # at all -- and names it a person, because that is the only kind a
    # session has. Membership under an agent principal is the separate
    # statement, and it is the one the beat makes out loud: the room's
    # capability list is where "this member is an agent" is recorded,
    # and every surface that marks one reads it there.
    #
    # Without this the room holds two grants about the agent and neither
    # says it is one, so the roster reports a room of people -- which is
    # what it did.
    as_agent = quinten.grant(AGENT_SUBJECT, "workspace", "write")
    claim("and admits it to the room as an agent, not as another person",
          "err" not in as_agent, True)
    if "err" in as_agent:
        note(json.dumps(as_agent["err"])[:200])


def beat_four(quinten: Session, xaver: Session, agent: Session,
              host_ws: dict, shots: str) -> None:
    banner("4.  both have joined — and each session can see the others")

    cut("quinten")

    # Two socket reads with nothing in frame. The comment inside
    # already says this question has no picture and that what it
    # produces is a line in the transcript -- so the camera should
    # not have been running through it, and was.
    with off_camera("the other two sessions are asked what they call quinten's\n     room -- answered over a socket, with nothing on screen"):
        step("read quinten's room back out of the other two")
        # Neither of these is quinten's own state — each guest independently
        # reports the room it now holds — and neither is filmed from the
        # guest. Asking a far session what it holds is a question, and a
        # question has no picture: what it produces is a line in this
        # transcript. The camera stays where the story is, and what xaver
        # holds is *seen* in the last shot of the film rather than described
        # over a screenshot of his backdrop.
        claim("xaver's own copy carries quinten's name for the room",
              xaver.active_workspace()["name"], host_ws["name"])
        # Read from the agent's session, shot on nobody's: it is asserted
        # here and *seen* in the roster on quinten's screen below.
        claim("the agent's own copy carries it too",
              agent.active_workspace()["name"], host_ws["name"])

    step("and the room's people, from the top bar")
    # Now there is a roster, so the affordance exists: the avatar stack
    # is drawn and clicking it opens the panel. This is also where the
    # names have to hold up — a roster listing `peer-2` and
    # `peer@11895642303397647149` beside `xaver` is three label styles
    # for three participants, and a room nobody can read.
    via = show_collaborators(quinten)
    note(f"opened from {via}")
    claim("the collaborators panel is up on quinten's screen",
          quinten.overlays()["collaborators"], True)
    # Who is in this room, read off the room's own capability list —
    # which *is* the membership (`state.rs:877`: "Room membership lives
    # here"), and is the same list the panel on screen is drawn from.
    #
    # Read here rather than from `peers`, which carries pointers and
    # nothing else: a `peers()["roster"]` lookup returned an empty list
    # on every run this demo has ever made, and printed nothing, so the
    # roster was never actually being checked.
    #
    # This is also the honest place to look for the agent. Claude is a
    # participant in this room and has no mouse — a pointer that has
    # never moved publishes nothing, by design — so asserting a cursor
    # for it would be asserting something that should not exist. What
    # makes it a participant is that it holds a capability in the room,
    # and that is what is checked.
    caps = quinten.active_workspace().get("capabilities") or []
    members: dict[str, list[str]] = {}
    for cap in caps:
        subject = json.dumps(cap.get("subject"))
        for name in (quinten.user, xaver.user, agent.user):
            if f'"{name}"' in subject or f":{name}" in subject:
                members.setdefault(name, []).append(str(cap.get("verb")))
    if not caps:
        # Said as a gap rather than asserted away. An empty list here is
        # this reader looking in the wrong place, not a room with nobody
        # in it — three participants joined it two beats ago.
        gap("the active workspace reported no capability list, so who is "
            "in this room could not be read off it")
    else:
        note(f"the room's own capability list names {sorted(members)} "
             f"across {len(caps)} capabilities")
        claim("the agent is in the room the same way the people are — by "
              "holding a capability in it", agent.user in members, True)
        if agent.user in members:
            note(f"{agent.user} holds {sorted(set(members[agent.user]))} "
                 f"here — which is what makes it a member of this room: a "
                 f"capability, granted by the owner, in the same list the "
                 f"people are in")
    read_hold()
    quinten.snapshot(os.path.join(shots, "03a-roster.png"))
    close_collaborators(quinten)

    step("and the cursors — on a desktop with nothing open on it")
    # `peers` is read from the same PresenceCtx the overlay draws from,
    # which is the only way to tell "no peer arrived" from "a peer
    # arrived and was not drawn" — those make the same picture.
    #
    # This beat is deliberately shot on an empty desktop, and that used
    # to be exactly the case that could not work: a session only put its
    # cursor on the wire as a side effect of some local app publishing
    # one, so two people looking at empty desktops published nothing
    # between them. It is level-triggered from the frame loop now, and a
    # pointer that has stopped is restated about once a second — so a
    # peer who joins in the middle of this beat arrives to find the
    # others already on screen, without anyone having to waggle a mouse
    # to announce themselves.
    WHERE_Q = (880.0, 520.0)
    WHERE_X = (640.0, 610.0)
    quinten.pointer_move(*WHERE_Q)
    quinten.settle(0.6)
    # Moved, not filmed. Both halves of this are one fact seen from two
    # sides, and one side is enough to put on screen: quinten's desktop
    # carrying xaver's cursor, with nothing open on either machine, is
    # the picture. Cutting to xaver to watch him move a mouse shows the
    # cause of something the audience has already been shown the effect
    # of.
    xaver.pointer_move(*WHERE_X)
    xaver.settle(0.6)

    def seen_by(watcher: Session, who: str):
        """Peer `who`'s pointer, as `watcher`'s session holds it.

        Both stores, because a person's pointer has lived in either. It
        was once filed into `pointers` *and* `remote_cursors`, which is
        what drew some peers twice; consolidating that left a reader of
        only one key looking at an empty list while the cursor was on
        screen, correctly drawn and correctly named. Reading both is the
        honest question — "does this session hold a pointer for that
        person" — rather than a question about where it is filed.

        A row is matched on either field, because the two carry
        different facts and only one of them is a person's name. The
        *identity* is `peer@<handle>`, derived from the handle the
        frames arrive under (`peer_presence.rs:420`); the *name* is what
        the room calls that identity, written into the room document
        under the inviter's authority. A peer that could announce its own
        name could announce anybody's, so identity stopped being the
        sending session's word for who is at it — and a predicate
        written against `user == "quinten"` stopped matching anything at
        exactly that moment, which read here as presence failing while
        it was working.
        """
        peers = watcher.peers() or {}
        rows = []
        for key in ("pointers", "remote_cursors"):
            rows.extend(peers.get(key) or [])
        return [r for r in rows
                if who in (r.get("name"), r.get("user"))] or None

    def at_the_origin(row: dict) -> bool:
        """A pointer painted where nobody is.

        A roster entry that conjures a cursor at (0, 0) has been fixed
        once already. It is worth failing on rather than shooting around,
        because on screen it is indistinguishable from a peer who happens
        to have parked in the corner — and it is only wrong when the peer
        is demonstrably somewhere else, which is the one thing a harness
        can check and an eye cannot.
        """
        return abs(float(row.get("x", 0.0))) < 1.0 and \
            abs(float(row.get("y", 0.0))) < 1.0

    # Bounded on camera, not in the claim: a cursor that is going to
    # cross has crossed within a few seconds, and the alternative is
    # filming an empty desktop for twenty-five under a caption about a
    # cursor appearing on it.
    with patience(4.0):
        on_xaver = wait_for(lambda: seen_by(xaver, quinten.user),
                            timeout=SYNC_TIMEOUT_S)
    claim("xaver's session paints quinten's cursor on an empty desktop",
          bool(on_xaver), True)
    if on_xaver:
        row = on_xaver[0]
        note(f"xaver's screen holds {json.dumps(row)}")
        # The name is the other half of the beat, and it is a different
        # fact from the id: the id is the identity and is never blank,
        # the name is what the *room* calls that identity, and the pill
        # over the cursor draws the name. A cursor reading `peer-1` is a
        # cursor nobody can attribute to a person.
        #
        # Asserted on the *name*, and deliberately not on the id. The id
        # is `peer@<handle>` and is supposed to be: the sending session
        # does not get to say who is at it, or a peer could sit in a room
        # under somebody else's name. So "the label is trustworthy" is a
        # claim about where the name came from — the room document, which
        # only the room's authority can write — and this checks that the
        # id is a handle rather than a person's word for themselves.
        claim("the identity is the session's handle, not a name it chose "
              "for itself",
              str(row.get("user", "")).startswith("peer@"), True)
        claim("and the pill over it reads his name, resolved from the room",
              row.get("name"), quinten.user)
        claim(f"and puts it where he actually is, not at the origin",
              at_the_origin(row), False)
        if at_the_origin(row):
            note(f"quinten's pointer is at {WHERE_Q} and his peer row on "
                 f"xaver's screen is at the origin — a cursor is being "
                 f"painted where he is not")
    xaver.snapshot(os.path.join(shots, "03-xaver-joined.png"))

    with patience(4.0):
        on_quinten = wait_for(lambda: seen_by(quinten, xaver.user),
                              timeout=SYNC_TIMEOUT_S)
    claim("and quinten's paints xaver's, the other way", bool(on_quinten), True)
    if on_quinten:
        claim("also at a real position", at_the_origin(on_quinten[0]), False)
        note(f"quinten's screen holds {json.dumps(on_quinten[0])}")
    # Whether the agent publishes a pointer of its own is a fact about the
    # product, and it is read rather than assumed in either direction: an
    # earlier draft of this beat stated in a comment that it does not, and
    # a shot from that same run had a pill reading `Claude` in it.
    agent_seen = seen_by(quinten, agent.user)
    if agent_seen:
        # Reported, not shot around, and not fixed here: the compositor
        # restates every session's pointer from its own frame loop, so a
        # session that has never had a pointer moved through it still
        # publishes one — and the agent is a session.
        #
        # That was half the story and the half that sent three fixes to
        # the wrong place. The agent had a pointer because *this file*
        # gave it one: redeeming an invitation had no address, so joining
        # meant clicking Paste and Join, and the position those clicks
        # left behind was restated to the room once a second afterwards.
        # It now joins through `peer.join`, which takes the token and
        # touches nothing.
        #
        # So if this fires again, the first question is which route the
        # agent took — not what the compositor is publishing.
        gap("the agent publishes a desktop cursor: a pill reading "
            f"{agent.user!r} is painted on quinten's screen at "
            f"{json.dumps(agent_seen[0])}. A pointer is for pointing at "
            f"something you are looking at, and this participant never "
            f"looks at a screen. It joined by address and should never "
            f"have acquired a position — so check what gave it one "
            f"before looking at how presence is published")
    else:
        note(f"{agent.user} publishes no desktop cursor, which is right: "
             f"what makes it a member of this room is the capability "
             f"above, and where it appears is in the document")
    read_hold()

    step("and they stay there without anyone moving")
    # The property that makes this beat filmable, asserted rather than
    # assumed. An edge-triggered cursor is published when it moves and
    # then forgotten, which looks identical to this one for a second and
    # then leaves an empty desktop behind — and means a peer arriving
    # later sees nobody until somebody jiggles a mouse. Nothing is moved
    # here on purpose: both pointers have been still since above.
    # Three seconds of two motionless cursors. The claim is worth making
    # -- an edge-triggered cursor looks identical for a second and then
    # leaves an empty desktop -- and there is nothing in it to watch,
    # because what is being asserted is that the picture does not change.
    with off_camera("three seconds pass with nobody moving, and both "
                    "cursors are still being published"):
        quinten.settle(3.0)
        still_there = seen_by(quinten, xaver.user)
    claim("xaver's cursor is still on quinten's screen 3s after it stopped",
          bool(still_there), True)
    if not still_there:
        note("a stopped pointer is no longer being restated, so a peer who "
             "joins after the last mouse movement arrives to an empty room")


def beat_five(quinten: Session, registry, shots: str) -> dict | None:
    banner("5.  \"word processor\" — a machine that does not have it")
    cut("quinten")

    installed = set(installed_ids(quinten.apps_dir))
    claim("this machine does not have the word processor",
          WORD_PROCESSOR in installed, False)
    note(f"{len(installed)} applications installed; {WORD_PROCESSOR} is not "
         f"one of them")
    if registry is None:
        gap("no local registry was served, so the fetch could not be filmed: "
            "build it with `cargo build -p app-catalogue --bins --examples` "
            "and `cargo build -p qos-install --bin qos-install`")
        return None
    note(f"a registry at {registry.base_url} offers it")

    step("he searches for it, and the row is offered like any other")
    quinten.modifiers(logo=True)
    quinten.key("k")
    quinten.modifiers()
    quinten.settle(0.4)
    quinten.type_prose("word processor")
    quinten.settle(1.2)
    pal = quinten.palette()
    rows = pal.get("rows") or []
    offered = next((r for r in rows if WORD_PROCESSOR in json.dumps(r)), None)
    claim("the palette offers an app this machine does not have",
          offered is not None, True)
    if offered:
        note(f"row: {offered.get('title')!r} — {offered.get('subtitle')!r}")
        # And that the row *says so*, which is the half this beat used to
        # spend eleven seconds not showing.
        #
        # The capability was always real — the palette listed an app the
        # machine did not have and activating it fetched one. The row was
        # identical to a row for an app already here, so on film it was an
        # ordinary app launch and a viewer had no way to tell. A beat that
        # demonstrates something invisibly is a beat that costs its
        # seconds and argues nothing.
        badge = offered.get("badge")
        claim("and the row says it is not on this machine",
              bool(badge), True)
        if badge:
            note(f"the row carries {badge!r}, and its title is the fetch "
                 f"verb rather than the ordinary open one")
        else:
            gap("the row for an app this machine does not have is "
                "indistinguishable from one for an app it has, so this "
                "beat shows a capability the audience cannot see")
    quinten.snapshot(os.path.join(shots, "04-palette-offer.png"))

    step("he presses Enter — fetch, verify, unpack, run")
    before = {f.get("surface_id") for f in quinten.foreign_frames()}
    # The one number in this film a viewer might time with a stopwatch,
    # so the camera gets out of the way while it is being taken. The
    # poll below queues behind the camera too, which is why it loosens
    # as well.
    with easing(4.0):
        quinten.key("Enter")
        began = time.time()
        painted = None
        deadline = time.time() + LAUNCH_TIMEOUT_S * 2
        while time.time() < deadline:
            for f in quinten.foreign_frames():
                if f.get("surface_id") not in before and f.get("frame_seq", 0) > 0:
                    painted = f
                    break
            if painted:
                break
            time.sleep(0.25)
        took = time.time() - began
    claim("the word processor was fetched and opened", painted is not None, True)
    if painted is None:
        note(f"see {quinten.log_path}")
        return None
    note(f"{took:.1f}s from Enter to first frame — one machine, one LAN hop, "
         f"an unoptimised payload; not a number to quote")
    claim("and it is installed now, on the machine that lacked it",
          WORD_PROCESSOR in set(installed_ids(quinten.apps_dir)), True)
    # A group takes about three quarters of the strip by default, so the
    # second window opens mostly past the right edge — the strip is
    # working exactly as designed, and the app the beat is about is half
    # off screen while it does. Give every group an even share, so the
    # window this beat just opened is actually in the frame that shows it.
    frame_windows(quinten, [FILES_APP, WORD_PROCESSOR], VIEWPORT)
    # A beat later than the first frame. `painted` means the app has
    # presented *something*, and what a `ui` app presents first is its
    # splash — so the shot that is supposed to show a word processor
    # arriving on a machine that lacked it was a title and a spinner.
    #
    # 2.5s was not enough of a beat: it produced exactly that shot, a
    # centred "Word Processor" over a dot spinner, in the frame the
    # letter's "an app appears with no install step" rests on. Held long
    # enough for the app to finish starting, and held *on camera* rather
    # than settled quietly, because an app coming up is worth watching
    # and this is the moment the beat is for.
    quinten.settle(2.0)
    # Two and a half seconds, not five. The five was set when an app took
    # ten seconds to appear and the hold was covering the tail of that
    # wait; a window now arrives in about three, and holding on a
    # finished, motionless window for five more is the beat asking for
    # patience it has not earned.
    if RECORDER:
        RECORDER.hold(1.5)
    else:
        quinten.settle(1.5)
    quinten.snapshot(os.path.join(shots, "05-word-processor.png"))
    return painted


def beat_six(quinten: Session, xaver: Session, baseline: dict,
             opened: list[str], shots: str) -> str | None:
    banner("6.  xaver's screen — the window, and what quinten wrote in it")

    # Both halves of the rule get asserted, because either alone is easy
    # to satisfy wrongly: the window must arrive, *and* he must not be
    # moved to it.
    #
    # The baseline is taken before beat 5 opens anything, not here.
    # Measuring "a tab quinten has that xaver lacks" at this point asks
    # the question after the answer: replication is quick, and a run
    # where the window had already crossed reported that there was
    # nothing to watch — the success and the failure both came out as
    # silence. What is watched instead is the tab that did not exist
    # when the baseline was taken, which is the window beat 5 opened.
    #
    # No cut, and asked *before* beat 7 — because beat 7 has xaver open a
    # word processor on his own machine, and opening an app moves your
    # focus to it. Asked afterwards, "his view did not move" is measured
    # across a window he opened himself: it failed exactly that way once,
    # reporting a replication bug that was a person clicking on their own
    # computer. The claim is that *somebody else's* window arriving does
    # not move you, so nothing of his own may happen in between.
    #
    # What the film shows is separate and later — see `show_xavers_screen`,
    # which frames the same screen once it is also carrying the sentence
    # quinten typed.
    before_view = baseline["view"]
    before_groups = baseline["groups"]
    # Handed in, not diffed here. Since this beat moved after beat 7 there
    # is a second word processor in the room — the one xaver opened on his
    # own machine — and a tab that is new since the baseline could be
    # either. Watching *his own* tab arrive in his own tree is a claim
    # that cannot fail, printed under a heading about replication. What is
    # watched is the tab quinten's machine opened, identified before
    # xaver opened anything.
    fresh = list(opened)
    note(f"quinten holds {len(quinten.tabs_here())} tab(s); xaver holds "
         f"{len(xaver.tabs_here())}; the tab under test is the one his "
         f"machine opened in beat 5, taken before xaver opened his own")

    # Keep this beat on one of qOS's own apps. Those replicate through
    # the room document and are drawn from it on the far side, which is
    # what makes a real window appear there. A foreign Wayland surface
    # has no such description to replicate and degrades to a card
    # reading "coming soon" — true to what the system can do today, and
    # the wrong thing to put in a shot whose whole claim is that the
    # window opened over there.
    step("the tab quinten's word processor opened, read out of xaver's tree")
    if not fresh:
        gap("beat 5 opened no window on quinten's session, so there was "
            "nothing for this beat to watch cross")
        return None
    tid = sorted(fresh)[0]
    landed = wait_for(lambda: tid in xaver.tabs_here(), timeout=DOC_SYNC_TIMEOUT_S)
    claim("the window quinten opened is open for xaver too", bool(landed), True)
    if landed:
        arrived = xaver.tabs_here()[tid]
        note(f"tab {tid} in xaver's workspace, titled {arrived.get('title')!r}")
    else:
        note(f"tab {tid} had not replicated after {DOC_SYNC_TIMEOUT_S:.0f}s")

    step("and his own view did not move")
    after_view = xaver.view_state()
    claim("xaver's active workspace did not move",
          after_view["active"], before_view["active"])
    claim("xaver's scrolled-to strip did not move",
          after_view["active_strip"], before_view["active_strip"])
    claim("xaver's focus did not move",
          after_view["active_group"], before_view["active_group"])
    for g, was in before_groups.items():
        if g in xaver.groups_here():
            claim(f"the tab in front of him in group {g[:6]}… did not change",
                  xaver.groups_here()[g]["active_tab"], was["active_tab"])
    note("which windows exist is the workspace; which one you are looking "
         "at is you, and the second does not travel")
    xaver.snapshot(os.path.join(shots, "06-xaver-window.png"))
    return tid


def show_xavers_screen(xaver: Session, shots: str) -> None:
    """The shot the film's one excursion was made for.

    Everything in it has already been proven — the window arrived in beat
    6, the sentence in beat 7 — and neither proof is a picture: both are
    answers to questions put over a socket. This is where a viewer gets
    to see the two of them on one screen at the same time, which is the
    only thing a second machine is worth crossing to.

    Framed rather than merely visited: an even share of the strip, every
    window whole, and then the document brought up in front. A previous
    cut ended its excursion on this session's file manager showing an
    empty folder, under a caption about a document two other participants
    had just written into.
    """
    frame_one_window(xaver, WORD_PROCESSOR)
    read_hold(READ_HOLD_S * 1.5)
    xaver.snapshot(os.path.join(shots, "06b-xaver-window-and-text.png"))


def xaver_works_in_files(xaver: "Session", stop: "threading.Event") -> None:
    """Move Xaver's pointer around his file manager until told to stop.

    This is the shot the film exists for, and until now it did not
    happen: while quinten wrote, xaver sat perfectly still, so the
    strongest frame in the demo showed one person working and a second
    person's name on a roster.

    Two people in one document is what every editor on the market has.
    Two people in *different applications* in one room, each seeing the
    other, is the thing that needs an operating system underneath it —
    and the file manager is the proof, because nobody wrote a line of
    collaboration code for it. It is collaborative because it is in the
    room.

    So he uses it. His pointer travels over his own Files window, which
    publishes presence, which quinten's screen draws as a cursor in his
    colour. Nothing is faked: a real second session really is moving a
    real pointer over a real application.

    Deliberately only the pointer. Clicking would navigate him somewhere,
    and a beat that changes what a second person is looking at while the
    first person types is two things happening rather than one.
    """
    # Over his file list, which is the left half of his strip. A slow
    # arc rather than a straight line: a pointer that travels in a
    # perfectly straight line at a constant speed is the one thing a
    # hand never does.
    path = [
        (520.0, 300.0), (600.0, 340.0), (700.0, 390.0), (760.0, 430.0),
        (700.0, 470.0), (600.0, 430.0), (540.0, 370.0), (620.0, 320.0),
    ]
    i = 0
    while not stop.is_set():
        x, y = path[i % len(path)]
        try:
            xaver.pointer_move(x, y)
        except Exception:                       # noqa: BLE001
            return
        i += 1
        stop.wait(0.55)


def beat_seven(quinten: Session, xaver: Session, host_ws: dict,
               quinten_wp: dict | None, shots: str) -> None:
    banner("7.  quinten writes, and xaver's screen is asked what it holds")
    room = host_ws["room"]

    # If beat 5 never got a word processor onto quinten's machine there is
    # nothing here to type into, and typing anyway would send the text to
    # whatever happened to hold focus — which reads afterwards exactly
    # like a document that failed to cross. Say which of the two it was.
    if quinten_wp is None:
        gap("quinten has no word processor open (beat 5 did not put one "
            "there), so there was nothing for him to write in and nothing "
            "for this beat to watch cross")
        return

    step("xaver already has the window — he opens nothing")
    # He does not launch anything here, and that is the point rather than
    # a shortcut. The window arrived on his machine in beat 6 because it
    # belongs to a room he is in; asking him to open a second copy of an
    # app he can already see would be staging work the product exists to
    # remove.
    #
    # It also used to be wrong in a way that hid itself. Opening the app
    # here started a *second* window on every machine in the room — one
    # locally-owned tab projects back into the room and every member
    # materialises it — so this beat quietly produced the duplicate
    # word processors that turned up in the frames. Since that was fixed,
    # a launch on his side focuses the window he has instead of making
    # one, so waiting for a new surface here waits for something that by
    # design never arrives.
    # Identified the same way the beat-6 assertion identifies it: a tab
    # on his machine whose title names the app. Deliberately not "any new
    # tab" — see beat 6 on why a tab he opened himself is a claim that
    # cannot fail.
    his = next(
        (t for t in xaver.tabs_here().values()
         if (t.get("title") or "").lower().startswith("word processor")),
        None,
    )
    claim("the window that crossed in beat 6 is his to type in",
          his is not None, True)
    if his is None:
        gap("xaver has no word processor on screen, so there is no far "
            "screen to read a document back off")
        return
    # With the viewport, so the framing is measured rather than performed
    # and assumed: this beat's shot is the far screen, and a far screen
    # that is half off the frame shows nothing about what it holds.
    frame_windows(xaver, [FILES_APP, WORD_PROCESSOR], VIEWPORT)
    # Both apps have to have attached to their broker and announced their
    # address before either types, or the first keystrokes go out to
    # nobody and it reads as a transport failure.
    #
    # Waited *for*, not waited *out*. Six fixed seconds was a guess that
    # was too long whenever the machine was quick and would still have
    # been too short on a loaded one — the worst of both, and six seconds
    # of an unmoving screen in a seventy-five second film. This asks the
    # far session whether its app is on the wire and stops as soon as it
    # says yes.
    attached = wait_for(
        lambda: any((t.get("title") or "").lower().startswith("word processor")
                    for t in xaver.tabs_here().values()),
        timeout=8.0)
    if not attached:
        note("xaver's word processor did not announce itself within 8s; "
             "typing anyway, and the claims below will say what happened")
    xaver.settle(0.8)

    # What xaver's screen looks like before a word of it exists. The
    # claim below is answered over a socket, and a socket cannot tell you
    # whether the app *drew* anything: a run once passed "xaver's own
    # word processor holds what quinten typed" with both pages on his
    # screen reading "Start writing…" for the whole beat. Assertion
    # green, screen blank — which is the failure this whole file is
    # written against, so it is checked rather than hoped for.
    before_screen = screen_shot(
        xaver, os.path.join(shots, "07a-xaver-before-the-text.png"))

    step("quinten writes the notes for the evening")
    cut("quinten")
    # No click first, and that is the point rather than an omission.
    #
    # The word processor opens with its caret already in the body, so
    # there is nothing to put right — and clicking anyway can move focus
    # *off* the page onto the style panel beside it, which is a click
    # that lands squarely inside the window and still sends every
    # keystroke after it somewhere that is not the document. A beat that
    # clicked first read downstream as a document which failed to cross:
    # the wrong layer, and a much more alarming bug than the real one.
    #
    # If it turns out the caret was not there, the fallback below aims
    # one into the page and tries again — and the transcript says which
    # of the two actually wrote the text, because they are different
    # claims about how usable the app is.
    # He is not alone on this. Xaver's hand starts moving in his own
    # file manager now, so what the camera holds for the next few
    # seconds is two people working in two different applications in one
    # room — which is the whole argument.
    xaver_busy = threading.Event()
    xaver_hand = threading.Thread(
        target=xaver_works_in_files, args=(xaver, xaver_busy), daemon=True)
    xaver_hand.start()
    # He puts the caret in the page, then writes. One click, once.
    #
    # This used to type with no click at all, on the reasoning above:
    # the app opens with its caret in the body, and a click can land on
    # the style panel and send every keystroke somewhere that is not the
    # document. The first half of that turns out not to be true. The
    # caret is *not* in the body on open, so the no-click path failed on
    # every run and the fallback retyped the whole thing — which put
    # thirteen seconds of keystrokes producing nothing on screen into the
    # film, then a click, then the same words typed a second time. Fifty
    # -three seconds on a beat whose content is thirteen.
    #
    # The panel worry is real and is handled by aiming rather than by not
    # aiming: the click goes to 40% of the tile's width, and the panel
    # occupies the last 250px of an 800px window, so it is nowhere near.
    #
    # That the app cannot be typed into until it is clicked is a finding
    # about the app, not about this script -- see docs/demo-todo.md.
    typed_here = type_into_the_document(quinten, quinten_wp, TYPED_TEXT)
    quinten.settle(1.5)

    def quinten_holds() -> bool:
        """Ask his own app whether the text is in its document."""
        reader = None
        try:
            reader = DocumentReader(quinten.collab_json_sock, room,
                                    record=None, handle="control")
            _doc, found = reader.await_document(TYPED_PROOF, 20.0)
            return bool(found)
        except OSError as e:
            note(f"quinten's own session could not be asked ({e})")
            return False
        except CannotDecode as e:
            # Not "the words are missing" -- nobody looked. Said in those
            # words, because the branch below reads the `False` as an app
            # that dropped the keystrokes and sends the reader after the
            # wrong thing.
            note(f"{e}, so his document could not be read either way")
            return False
        finally:
            if reader is not None:
                reader.close()

    # Asked with the camera down. The words are already visibly on the
    # page — that is the shot that just happened — and this only settles
    # whether they reached the *document* or merely the screen, which
    # decides how a later failure is described rather than being
    # something to watch. On camera it was up to twenty seconds of a
    # still page after the typing finished, and beat seven ran to
    # sixty-six against a budget of seventy-five for the whole film.
    with off_camera("his own app is asked whether the words reached its "
                    "document, which decides what a later failure means"):
        landed = quinten_holds()
    if landed:
        note("the words are in his document, not merely on his screen")
    else:
        note("the caret was aimed into the page and the words still did "
             "not reach the document — so this is the app, not the aim")
        # No second attempt. Retyping here is what put the words on
        # screen twice and thirteen wasted seconds of keystrokes into the
        # film; if an aimed caret did not take them, typing them again
        # into the same place will not either, and the claim below is
        # allowed to fail and say so.
        surface = (quinten_wp or {}).get("surface_id")
        laid = [n for n in quinten.target_names()
                if n.startswith("foreign-surface-")]
        note(f"his word processor is surface {surface}; client tiles laid "
             f"out right now: {laid or 'none at all'}")

    quinten.snapshot(os.path.join(shots, "07-quinten-typed.png"))

    step("first, that he typed into the document and not at the chrome")
    # The control, and the reason this beat can tell its two failures
    # apart. If the text never reaches xaver, that is either a document
    # which did not cross or a document which never had the text in it,
    # and both end as an empty page on the far screen.
    claim("quinten's own document holds what he typed", landed, True)
    if not typed_here:
        gap("there was no laid-out tile to aim a caret into, so the words "
            "were never typed and nothing below this is about the network")
    elif not landed:
        note("the keystrokes did not reach his document at all, so nothing "
             "downstream of this is about the network")

    step("and xaver's session is asked what it now holds")
    # The one excursion in the film, and it stays open across beat 6:
    # this is the first moment a second machine has anything on it worth
    # crossing for. Everything before this — his redeeming the invite,
    # his opening his own copy of the app — happened while the camera
    # stayed with the person the story is about.
    cut("xaver")
    # What is mechanically readable from here is presence — but only the
    # *pointer* half of it, and the pointer is the wrong half for a man
    # who is typing.
    #
    # He clicks once to put the caret in the page and then does not touch
    # the mouse for thirteen seconds, so his pointer is resting and
    # eventually stops being published. This beat asserted his pointer
    # and passed for a while only because an earlier version typed the
    # notes twice with a click in between, which refreshed it; typing
    # once, as a person does, it ages out and the claim fails on a room
    # that is working perfectly. What xaver's screen actually carries
    # while quinten writes is his *caret*, with his name on it, which is
    # visible in this beat's own shot and is not in the state dump at
    # all (`native.rs` publishes `pointers` and `roster`, no carets).
    #
    # So the claim is what the harness can honestly see: he is a
    # participant in xaver's room, by name, and his live pointer is
    # reported when it is there rather than required to be.
    def quinten_pointer():
        # Either field: the identity is `peer@<handle>` and the name is
        # what the room calls it. See `seen_by` in beat 4.
        rows = (xaver.peers() or {}).get("pointers") or []
        return [r for r in rows
                if quinten.user in (r.get("name"), r.get("user"))] or None

    def quinten_in_the_room():
        rows = (xaver.peers() or {}).get("roster") or []
        return [r for r in rows
                if quinten.user in (r.get("label"), r.get("user"))
                or (r.get("label") or "") == quinten.name] or None

    with patience(4.0):
        seen = wait_for(lambda: quinten_pointer() or quinten_in_the_room(),
                        timeout=SYNC_TIMEOUT_S)
    resting = quinten_pointer() is None and seen is not None
    if resting:
        note("his pointer is not being published — it has been resting "
             "since the click that placed his caret, and what carries him "
             "on xaver's screen while he writes is the caret, seen in the "
             "shot below")
    if seen:
        claim("xaver's screen carries quinten while he is typing",
              bool(seen), True)
    else:
        gap("quinten's presence did not reach xaver's session")

    step("and his word processor is asked for the document it is holding")
    # The beat itself, and the one claim whose subject is inside a
    # client. Asked of xaver's *app*, in the app's own protocol: a late
    # joiner on his session's broker announces the word processor's
    # address and sends the `Arrived` every toolkit answers with its
    # whole document. What comes back was produced by his running app
    # out of the document it holds, so the answer is about his screen
    # and not about the wire.
    #
    # The camera stays on xaver for the whole wait, so if the document
    # converges the arrival is on film rather than described afterwards.
    reader = None
    doc, carried = None, False
    try:
        reader = DocumentReader(xaver.collab_json_sock, room, record=None)
        # Filmed for as long as it usually takes, and no longer. When it
        # works the sentence turns up on his page in a few seconds and
        # that is the shot; when it does not, a minute of an unchanged
        # page under a caption about a document arriving is the worst
        # thing this film could contain.
        with patience(6.0):
            doc, carried = reader.await_document(TYPED_PROOF,
                                                 DOC_SYNC_TIMEOUT_S)
    except OSError as e:
        gap(f"xaver's session has no JSON broker to ask ({e}) — "
            f"looked at {xaver.collab_json_sock}")
    except CannotDecode as e:
        gap(f"{e} — so what his document holds was not established")
        doc = None
    finally:
        if reader is not None:
            reader.close()

    # Which of the three outcomes this was decides how it is recorded,
    # and the distinction is not cosmetic. A document that came back
    # without the text is a question that was *asked and answered*: the
    # claim is false, it counts as a failure, and it gates the exit code
    # like every other claim here. Silence is a different thing — nothing
    # was established either way — and that is what a GAP is for.
    if doc is not None:
        claim("xaver's own word processor holds what quinten typed",
              carried, True)
        if carried:
            note(f"answered with a {len(doc)} byte document carrying "
                 f"{TYPED_PROOF!r} — produced by his app out of the "
                 f"document it holds, not read back off the wire")
        else:
            note(f"his app answered with {len(doc)} bytes and none of it "
                 f"is {TYPED_PROOF!r}: the two apps are attached and "
                 f"answer each other's handshake, and their documents "
                 f"are still two documents. Presence crosses, the "
                 f"document does not, and nothing on his screen shows "
                 f"what quinten wrote")
    else:
        gap(f"no app in xaver's session answered the late-join handshake "
            f"within {DOC_SYNC_TIMEOUT_S:.0f}s, so his document was never "
            f"read back — this run establishes nothing about beat 7 in "
            f"either direction. See {xaver.log_path}")
    # His hand stops here. It has been moving through the typing and the
    # crossing, which is the span the shot is about; leaving it running
    # would put a wandering cursor over the beats after this one, where
    # it means nothing and only competes for attention.
    xaver_busy.set()
    xaver_hand.join(timeout=3.0)
    xaver.settle(2.0)
    after_screen = screen_shot(
        xaver, os.path.join(shots, "08-xaver-received.png"))

    # The cross-check. Only meaningful when the socket said yes: a
    # document that never arrived is allowed to leave the screen alone.
    if carried:
        moved = pixels_moved(before_screen, after_screen)
        if moved is None:
            gap("xaver's session answered with the text, and his screen "
                "could not be compared, so whether his app drew any of "
                "it is not established by this run")
        elif moved < SCREEN_MOVED_PX:
            # Two different failures wear this shape, and the message
            # used to name only the first — which sent three separate
            # investigations at a broker that had already been fixed.
            #
            #   * the document reached a *different* app from the one on
            #     screen, or
            #   * the app on screen never finished opening, so there was
            #     nothing to draw the document into.
            #
            # The second is not a collaboration failure at all, and
            # reading it as one is expensive: the last time this fired,
            # the guest's word processor was sitting under its "Opening…"
            # card while the text underneath it had converged perfectly.
            still_opening = any(
                (f.get("app_id") or "") == WORD_PROCESSOR and f.get("opening")
                for f in xaver.foreign_frames())
            because = ("his word processor had not finished opening, so "
                       "there was no app drawn to put the document in — "
                       "which is a window-presentation failure and not a "
                       "collaboration one"
                       if still_opening else
                       "the app that answered is not the app that is "
                       "drawn; check which channel the handshake was "
                       "answered on")
            gap(f"xaver's session answered with a document carrying "
                f"{TYPED_PROOF!r}, and {moved} pixels of his screen moved "
                f"while it arrived — a blinking caret, and no text. "
                f"{because}. This is the shot the film crosses to a "
                f"second machine to get. See {xaver.log_path}")
        else:
            note(f"and {moved} pixels of his screen moved while it "
                 f"arrived, so the app that answered is the app that is "
                 f"drawn")


#: Where the file view puts its rows, straight off the app's own
#: constants (`views/file_view.rs:63`): a 30px column header, then rows
#: of 34px. Used only to convert a press that landed on the *wrong* row
#: into an aimed one — never to guess where a row is in the first place.
LIST_ROW_H = 34.0
LIST_HEADER_H = 30.0


def expected_listing(home: str) -> list[str]:
    """The order Files draws this directory in: folders first, then
    files, each case-insensitively by name.

    Only used to work out how many rows separate the row a press landed
    on from the one that was wanted. Being wrong about it costs a
    correction, not the beat — every press is checked against what the
    app says it hit.
    """
    try:
        names = [n for n in os.listdir(home) if not n.startswith(".")]
    except OSError:
        return []
    dirs = sorted((n for n in names if os.path.isdir(os.path.join(home, n))),
                  key=str.lower)
    files = sorted((n for n in names if not os.path.isdir(os.path.join(home, n))),
                   key=str.lower)
    return dirs + files


def where_the_app_draws(log: LogTail, surface: str, tile: tuple,
                        file_name: str) -> tuple | None:
    """The point on screen where `surface` is drawing `file_name`'s row.

    Two facts, from the only two places that hold them, and no third
    thing guessed in between.

    The app says where it laid the row out, in its own coordinates —
    `Widget::resource_rects`, reported per layout by
    `App::narrate_resource_rects`. It is the app's own answer about its
    own geometry, so it moves when a font, a scale, a row height, a
    sidebar or a sort order moves it, and it is keyed by the file's
    **address**: the same `files.entry:<path>` the remark will carry, so
    the thing aimed at and the thing sent are the same object by
    construction rather than by coincidence.

    The compositor says where that window is, in *its* coordinates, and
    the translation between the two is a subtraction of the tile's
    origin and nothing else (`client_input::surface_local`, which is
    also how a pointer position is routed into the client — so aiming
    this way and pressing agree by the same arithmetic the press itself
    uses).

    Matched on the file's own name rather than on a path this end built,
    because the app is the authority on which directory it is showing
    and on how it spells what is in it. `None` when it is drawing no such
    row — including when it is drawing it off the bottom of the window,
    which is a real answer and not a failure to have one.

    `across` is a fraction of the row's width: rows run the full width of
    the content pane, and a third of the way in is the name column on any
    metrics rather than the icon at one end or the date at the other.

    Returns `(screen x, screen y, the same y in the app's coordinates,
    address)`. The third is what lets a caller that pressed and missed
    solve for the translation instead of guessing again.
    """
    rects = log.resource_rects(surface)
    want = file_name.lower()
    hit = None
    for address, rect in rects.items():
        path = address.split(":", 1)[-1]
        if os.path.basename(path).lower() == want:
            hit = (address, rect)
            break
    if hit is None:
        return None
    address, (x0, y0, x1, y1) = hit
    tx, ty, _tw, _th = tile
    across = 0.35
    client_y = (y0 + y1) / 2.0
    return (tx + x0 + across * (x1 - x0), ty + client_y, client_y, address)


def press_the_photo_row(quinten: Session, log: LogTail, tile: tuple,
                        photo_name: str, listing: list[str],
                        surface: str = FILES_APP) -> tuple | None:
    """Press on the photograph's row and *keep the button down*.

    A file row belongs to a client, and a client's widgets are not in the
    compositor's registry, so there is no name to click and the press has
    to be aimed at a pixel. Which pixel is the whole difficulty, and it is
    asked of the app rather than worked out here: `where_the_app_draws`
    resolves the file's address to the rect the app says it laid that row
    out at, this frame, and the tile's origin from `foreign_frames` turns
    that into a place on screen.

    Nothing is remembered and nothing is assumed:

    - the tile's rect comes from `foreign_frames`, which is the
      placement the compositor is painting right now;
    - the row's rect comes from the app, which is the only thing that
      knows where it drew it;
    - every press is checked against the app's own account of what it
      hit (`drag armed path=… at=…`), so a miss is *known* to be a miss
      rather than assumed to be a hit.

    That the aim comes from the app's layout does not make this test
    agree with itself: whether the row a press *hits* is the row a person
    *sees* is a claim about paint against hit-testing, and it is proven
    where it belongs — `scripts/headless-examples/press-lands-where-drawn-proof.py`
    reads its targets out of a screenshot precisely so that neither this
    nor any other geometry-aware harness has to.

    A window that has published no layout — an older build of the app —
    falls back to sweeping a band and correcting by row arithmetic, which
    is what this did before and which fails when it fails to find any row
    at all.

    Returns the (x, y) the button is held at, or None with the button
    up and nothing selected.
    """
    tx, ty, tw, th = tile
    # The name column: right of the sidebar, left of the size column.
    x = tx + tw * 0.45
    want = photo_name.lower()
    try:
        target_index = [n.lower() for n in listing].index(want)
    except ValueError:
        target_index = None

    def press_at(at_x: float, at_y: float) -> tuple[str, float] | None:
        """Press at (at_x, at_y); report what the app says it grabbed."""
        quinten.pointer_move(at_x, at_y)
        quinten.settle(0.15)
        log.read()
        quinten.pointer_button(True)
        # The press has to cross a process boundary, be dispatched by
        # the app's event loop and be written to a log this then reads.
        # 0.5s is the difference between "the app did not report it" and
        # "the app had not reported it yet".
        quinten.settle(0.5)
        hit = log.armed(log.read())
        return (hit[0], hit[2]) if hit else None

    def press(at_y: float) -> tuple[str, float] | None:
        return press_at(x, at_y)

    # Ask the app where the row is, and press there.
    #
    # Three attempts, and none of them is a retry of the same aim.
    #
    #   1. the map as read. On a window that has settled this is the
    #      answer and there is no second press.
    #   2. a window reframed a moment ago is still relaying out, and a map
    #      read before that settled describes the size it used to be — so
    #      a miss re-reads, by which time the new map has been published.
    #   3. a press that landed on *some* row measures the translation
    #      between the two coordinate spaces exactly: the app reported the
    #      position in its own, and the difference from where the press was
    #      aimed is the offset. Applied to the target's published rect that
    #      lands on it, whatever made the first aim wrong — a stale
    #      placement, a window that moved between the two readings.
    #
    # Nothing here counts rows or knows a row height. The correction is a
    # subtraction of two numbers each side reported about the same press.
    offset = None
    for attempt in range(3):
        aim = where_the_app_draws(log, surface, tile, photo_name)
        if aim is None:
            if attempt == 0:
                # No map yet. Give the window one settle to publish one
                # before falling back to hunting for the row.
                quinten.settle(0.5)
                continue
            said = log.surfaces_that_published()
            drawn = log.resource_rects(surface)
            note(f"{surface} published no layout naming {photo_name} "
                 f"(it named {sorted(drawn)[:4] if drawn else 'nothing'}; "
                 f"surfaces heard from: {said or 'none'}); "
                 f"falling back to sweeping for its row")
            break
        aim_x, aim_y, client_y, address = aim
        if offset is not None:
            aim_y = client_y + offset
        hit = press_at(aim_x, aim_y)
        if hit and os.path.basename(hit[0]).lower() == want:
            note(f"the app draws {address} at y={aim_y:.0f}; pressed there "
                 f"and it is what answered")
            return (aim_x, aim_y)
        quinten.pointer_button(False)
        quinten.settle(0.3)
        if hit:
            # screen y = client y + offset, measured rather than assumed.
            offset = aim_y - hit[1]
            note(f"aimed at {address} (y={aim_y:.0f}) and "
                 f"{os.path.basename(hit[0])} answered; the app puts that "
                 f"press at client y={hit[1]:.0f}, so screen = client "
                 f"+ {offset:.0f}")
        else:
            note(f"aimed at {address} (y={aim_y:.0f}) and nothing answered; "
                 f"re-reading the app's layout")

    # Nothing published, or what it published did not answer. Sweep a
    # band: rows start below the app's toolbar and the column header, and
    # how much chrome sits above them depends on what the app stacked
    # there. Coarse, because it only has to land on *any* row; the
    # correction below does the aiming. This is what the beat did before
    # the app could be asked, and it is kept for a window running an
    # older build — it is also what failed, so a run that reaches here
    # says so above.
    for frac in (0.16, 0.22, 0.10, 0.28, 0.34):
        y = ty + th * frac
        hit = press(y)
        if hit is None:
            # Nothing under it — empty space below the last row, or the
            # toolbar. Let go before trying elsewhere, or the next press
            # arrives with a button already down.
            quinten.pointer_button(False)
            quinten.settle(0.2)
            continue
        path, client_y = hit
        if os.path.basename(path).lower() == want:
            note(f"pressed on {os.path.basename(path)} at y={y:.0f}")
            return (x, y)
        # A row, but not the one. The app reported the press's position
        # in its own coordinates, so the offset between the two spaces
        # is now measured rather than assumed, and one jump of N rows
        # lands on the wanted one.
        if target_index is None:
            quinten.pointer_button(False)
            quinten.settle(0.2)
            break
        try:
            hit_index = [n.lower() for n in listing].index(
                os.path.basename(path).lower())
        except ValueError:
            quinten.pointer_button(False)
            quinten.settle(0.2)
            continue
        offset = y - client_y          # screen y  =  client y + offset
        aimed = (client_y + (target_index - hit_index) * LIST_ROW_H + offset)
        note(f"the press landed on {os.path.basename(path)} "
             f"(row {hit_index}); {photo_name} is row {target_index}, "
             f"{abs(target_index - hit_index)} rows "
             f"{'down' if target_index > hit_index else 'up'} at "
             f"y={aimed:.0f}")
        quinten.pointer_button(False)
        quinten.settle(0.3)
        again = press(aimed)
        if again and os.path.basename(again[0]).lower() == want:
            note(f"pressed on {os.path.basename(again[0])} at y={aimed:.0f}")
            return (x, aimed)
        quinten.pointer_button(False)
        quinten.settle(0.2)
        break
    return None


def beat_eight(quinten: Session, files: dict | None, wp: dict | None,
               shots: str) -> None:
    banner("8.  a photograph dragged out of Files and into the document")
    # Named while the camera is down, so the film comes back up on
    # quinten rather than on the screen the excursion left behind.
    cut("quinten")
    # Run, asserted, and left out of the cut — see `run`. It is the
    # longest beat here by a distance, most of that spent moving a
    # pointer slowly enough for two processes to hand a drag between
    # them, and none of the claims this film is cut to support is about
    # drag and drop.
    #
    # What makes this beat worth running is not that a picture appears in
    # a document. It is *how* it gets there: Files and the word processor
    # are two sandboxed clients with two different mount namespaces, so
    # the path Files is holding means nothing in the other app's world.
    # The drag carries the file's **bytes**, not its name — Files reads
    # its own file at the start of the gesture, because it has the
    # authority to read what it is showing, the compositor brokers a
    # pipe, and the receiving toolkit spools what comes down it into its
    # own directory. Neither app is ever given a path into the other's.
    note("two sandboxed clients, two mount namespaces: the drag carries "
         "the file's bytes, not a path either app could resolve")

    photo = os.path.join(WORKSPACE_ROOT, PHOTO_NAME)
    claim("there is a photograph in the shared workspace to drag",
          os.path.exists(photo), True)
    if not os.path.exists(photo):
        return
    payload = open(photo, "rb").read()

    # Both windows whole, before anything is aimed at either of them. A
    # drag that starts or ends off the edge of the screen is not a drag
    # anybody can see.
    unpainted = wait_until_painted(quinten)
    if unpainted:
        gap(f"the shell is not painting {unpainted} yet, so this would be "
            f"filmed against an empty desktop")
    frame_windows(quinten, [FILES_APP, WORD_PROCESSOR], VIEWPORT)
    quinten.settle(0.5)
    tiles = painted_tiles(quinten)
    if VIEWPORT:
        # The two ends of the gesture, and only those. The strip carries
        # four applications and the screen holds two of them, so asking
        # whether *every* window is whole is asking a scrolling strip to
        # stop scrolling. What this drag needs is narrower and is still a
        # thing that can be false: the window the photograph comes out of
        # and the window it goes into, both wholly on screen, neither
        # behind the other.
        ends = [FILES_APP, WORD_PROCESSOR]
        clipped = (subjects_off_screen(quinten, ends, VIEWPORT)
                   + subjects_overlapping(quinten, ends, VIEWPORT))
        claim("both ends of the drag are wholly on screen, side by side",
              clipped, [])
        if clipped:
            gap("a window is cut off, so the gesture would cross an edge "
                "the camera cannot show — " + "; ".join(clipped))
            quinten.snapshot(os.path.join(shots, "09-not-in-frame.png"))
            return

    def tile_for(app_id: str, painted: dict | None) -> tuple | None:
        sid = (painted or {}).get("surface_id")
        if sid is not None and int(sid) in tiles:
            return tiles[int(sid)]["rect"]
        # No surface id to hand (an earlier beat did not return one):
        # fall back to the app id, which is unambiguous while there is
        # one window per app on screen.
        for t in tiles.values():
            if t["app_id"] == app_id:
                return t["rect"]
        return None

    files_tile = tile_for(FILES_APP, files)
    wp_tile = tile_for(WORD_PROCESSOR, wp)
    # And that they are not the same window seen twice, nor two windows
    # stacked. `tile_for` falls back to matching on app id when the
    # surface an earlier beat opened is no longer laid out, which is how
    # this beat once aimed a drag between a full-screen file manager and
    # a word processor drawn underneath it, having just asserted that
    # both were wholly on screen.
    stacked = windows_overlapping(quinten)
    claim("and neither window is drawn on top of the other", stacked, [])
    if stacked:
        gap("the two windows this gesture goes between are stacked, so a "
            "drag across them is not a gesture the camera can show — "
            + "; ".join(stacked))
        quinten.snapshot(os.path.join(shots, "09-not-in-frame.png"))
        return
    claim("Files is on screen to drag out of", files_tile is not None, True)
    claim("the word processor is on screen to drop into",
          wp_tile is not None, True)
    if not files_tile or not wp_tile:
        gap("both windows have to be on screen at once for this gesture; "
            f"the session is painting {[t['app_id'] for t in tiles.values()]}")
        return
    note(f"Files at {[round(v) for v in files_tile]}, "
         f"the document at {[round(v) for v in wp_tile]}")
    quinten.snapshot(os.path.join(shots, "09-before-drag.png"))

    log = LogTail(quinten.log_path)
    log.catch_up()

    # A blank line at the end of the document for the photograph to land
    # on. Without one it goes in wherever along the existing text the
    # pointer happens to be over, which is the middle of a word about as
    # often as not.
    #
    # Aimed at 30% down, not 20%: the top band of the page is the header
    # zone, and a click there opens header editing — which paints a
    # labelled dashed box across the top of the document and stays on
    # screen for the rest of the beat.
    wx0, wy0, ww0, wh0 = wp_tile
    quinten.click_at(wx0 + ww0 * 0.30, wy0 + wh0 * 0.30)
    quinten.settle(0.8)
    quinten.key_down("Control")
    quinten.key("End")
    quinten.key_up("Control")
    quinten.key("Enter")
    quinten.settle(0.6)

    step(f"he presses on {PHOTO_NAME}")
    # A click first, so the press that starts the drag is not the one
    # that focuses the window. The first click on an unfocused client is
    # spent on the focus change and never reaches the app's own widgets —
    # measured, and the cause of a drag that produced nothing at all.
    fx, fy, fw, fh = files_tile
    quinten.click_at(fx + fw * 0.45, fy + fh * 0.16)
    quinten.settle(0.8)

    listing = expected_listing(WORKSPACE_ROOT)
    note(f"the shared workspace lists as {listing}")
    held = press_the_photo_row(quinten, log, files_tile, PHOTO_NAME, listing)
    claim(f"the press landed on {PHOTO_NAME}'s own row", held is not None, True)
    if held is None:
        gap(f"no press landed on {PHOTO_NAME}: the file rows belong to a "
            f"client and are not addressable by name from the compositor, "
            f"so this is aimed at pixels and the app did not report a row "
            f"under any of them. See {quinten.log_path}")
        quinten.snapshot(os.path.join(shots, "09-missed-the-row.png"))
        return
    hx, hy = held

    step("and drags it across the desktop")
    # Past the app's own drag threshold first, slowly, while the button
    # is still down. That is the moment Files opens a `wl_data_source`
    # and hands the gesture to the compositor; a drag that teleports to
    # its destination never gives either process a frame in which to do
    # it, and nothing is ever picked up.
    for step_px in (4, 10, 18, 30):
        quinten.pointer_move(hx + step_px, hy - step_px * 0.4)
        quinten.settle(0.12)
    promoted = log.await_line(LogTail.CARRIED, 3.0)
    claim("Files handed the gesture out as a client drag", promoted, True)
    if promoted:
        note("from here the picture on screen belongs to the compositor: "
             "the source window will not see another motion event, and "
             "will never see the release")
    took = log.await_line(LogTail.PROMOTED, 3.0)
    claim("and the compositor took it", took, True)

    # Across, in steps, with the chip drawn at the cursor the whole way.
    #
    # Where it is released matters more than it looks. The word
    # processor moves its caret to the drop point before inserting
    # (`widget.rs:5792`), clamping the pointer to the nearest line and
    # then picking a character position along it — so a drop over the
    # middle of the page lands the photograph in the middle of a *word*.
    # The first take of this beat cut "and" in half and left a
    # spell-check squiggle on screen.
    #
    # An empty last line was opened above, so releasing *below* all the
    # text clamps to that line, where there is no character to land in
    # the middle of. Left of centre keeps it off the styles panel, which
    # the drop handler ignores entirely.
    wx, wy, ww, wh = wp_tile
    drop = (wx + ww * 0.30, wy + wh * 0.55)
    x0, y0 = hx + 30, hy - 12
    frames = 26
    for i in range(1, frames + 1):
        t = i / frames
        quinten.pointer_move(x0 + (drop[0] - x0) * t,
                             y0 + (drop[1] - y0) * t)
        quinten.settle(0.05)
        if i == frames // 2:
            # Mid-flight, over open desktop between the two windows:
            # this is the frame that shows the drag is the OS's and not
            # either app's.
            quinten.snapshot(os.path.join(shots, "10-carrying.png"))
    quinten.settle(0.4)
    quinten.snapshot(os.path.join(shots, "11-over-the-page.png"))

    step("and drops it on the page")
    log.read()
    quinten.pointer_button(False)

    # The payoff, off the receiving app's own account of its own
    # document.
    #
    # This used to watch for a change the size of the file, on the
    # reasoning that an image moves two orders of magnitude more than a
    # keystroke. That is now exactly backwards. Images no longer travel
    # inside the document: it carries a BLAKE3 reference and the bytes go
    # to a content-addressed store, because a 146 kB change exceeded the
    # publish ceiling, was dropped, and — since Automerge holds a change
    # whose ancestors are missing — froze every other machine's copy at
    # that point, permanently and silently.
    #
    # So the file-sized change must *not* appear, and what is watched for
    # instead is the reference: a small change carrying the hash, after
    # which the page holds an image. A test looking for the old
    # behaviour would now be asserting the bug.
    # Asked of the page, because the wire no longer answers it. The
    # reference is a few dozen bytes and reaches the log as nothing
    # distinguishable from a keystroke, so the honest evidence that a
    # photograph arrived is that a photograph is on screen: a large
    # region of the page that was paper a moment ago is not any more.
    #
    # `pixels_moved` thresholds rather than comparing exactly, because a
    # blinking caret changes bytes on a page where nothing happened.
    before_page = screen_shot(quinten, os.path.join(shots, "11a-before-drop.png"))
    biggest = 0
    landed = False
    deadline = time.time() + 25.0
    while time.time() < deadline:
        biggest = max(biggest, log.biggest_delta(log.read()))
        after_page = screen_shot(quinten, os.path.join(shots, "11b-after-drop.png"))
        moved = pixels_moved(before_page, after_page)
        if moved is not None and moved > 20000:
            landed = True
            break
        time.sleep(0.5)
    claim("the photograph is on the page", landed, True)
    note(f"largest change the word processor broadcast after the drop: "
         f"{biggest} bytes, against a {len(payload)}-byte file — the bytes "
         f"travel beside the document now, not inside it, which is why "
         f"this beat asks the page rather than the wire")
    quinten.settle(2.0)
    quinten.snapshot(os.path.join(shots, "12-image-in-the-document.png"))

    if not landed:
        gap(f"no reference to the photograph reached the document "
            f"within 25s of the drop. See {quinten.log_path}")
        return

    step("and it is in the document, not merely on the screen")
    # Asked of the app, after the camera has moved on: a late joiner on
    # this session's broker gets the whole document back, and the
    # photograph's own bytes are either in it or they are not.
    room = quinten.active_workspace()["room"]
    # Read, reported, and deliberately not asserted on.
    #
    # This was a claim, and it failed on three consecutive runs in which
    # the photograph was demonstrably in the document and rendered on
    # both machines. It looked for a reference in the bytes the app
    # answers a late joiner with, and it has now been wrong twice about
    # what those bytes contain: first `blob_w`, which is projected onto
    # the materialised snapshot at read time and never stored; then the
    # mime, which a saved document does carry — `crates/doc/qdoc/tests/
    # blob_reference.rs` proves it — but which this frame evidently is
    # not, and no third guess is worth a false failure on a working
    # product.
    #
    # What is asserted instead is the app's own answer above ("the
    # photograph is on the page"), produced by the running word
    # processor out of the document it holds, which is the same standard
    # every other claim about a document's contents in this file is held
    # to. The picture arriving on the *other* machine is the real proof
    # and it is a frame, not a socket read: see the last shot.
    #
    # If this is ever worth asserting again, assert it on the hash — it
    # is the address the content store is keyed by — and get a blake3
    # onto this machine first, because the ASCII the wire happens to
    # carry has now cost more than it has proved.
    carried, size_bytes = image_is_in_the_document(quinten, room, payload)
    if carried:
        note(f"the document the app answered with ({size_bytes} bytes) "
             f"carries an image reference, and the bytes are not in it")
    elif size_bytes:
        note(f"the app answered with {size_bytes} bytes carrying no "
             f"reference this can recognise — the claim above and the "
             f"final frame are what establish the photograph is in the "
             f"document, not this")
    else:
        gap("the app never answered the late-join handshake, so the "
            "document could not be read back and the delta above is the "
            "only account of the image landing")


def tab_groups_here(s: Session) -> list[dict]:
    """Every window group in the active workspace, with what each of its
    tabs actually *is*.

    `Session.groups_here` flattens a tab down to its title, which is
    enough to say how many there are and not enough to say what they
    are. The one claim the tabbing beat rests on — that these two pills
    are two different applications rather than two documents inside one
    — is a claim about the client drawing each tab, so it has to come
    off the tile.
    """
    tree = s.tree()
    out: list[dict] = []
    for ws in tree["workspaces"]:
        if ws["id"] != tree["active"]:
            continue
        for strip in ws.get("strips", []):
            for group in strip.get("window_groups", []):
                tabs = []
                for t in group.get("tabs", []):
                    tile = t.get("tile") or {}
                    foreign = (tile.get("Foreign")
                               if isinstance(tile, dict) else None)
                    sid = (foreign or {}).get("surface_id")
                    tabs.append({
                        "id": t.get("id"),
                        "title": t.get("title") or "",
                        "app_id": (foreign or {}).get("app_id"),
                        "surface_id": int(sid) if sid is not None else None,
                    })
                out.append({"id": group.get("id"),
                            "active_tab": group.get("active_tab"),
                            "tabs": tabs})
    return out


def open_the_rest_of_the_strip(quinten: Session) -> dict[str, dict]:
    """Open `STRIP_EXTRAS`, to the right of what is already there.

    Off camera, and in one go, because four applications opening one
    after another is four identical launches and the film has already
    shown two of them properly. What the strip is *for* is worth
    watching; watching it fill up is not.

    Each in a group of its own — `place_next_window` — because tabs
    tile inside a group and two windows are only side by side when
    they are in separate groups. The order is `STRIP_APPS`'s, so the
    pairs later beats need are neighbours.

    Returns what actually opened, by app id. An app that will not start
    is reported and skipped rather than faked: the beats that need one
    check this map and decline if it is not there.
    """
    opened: dict[str, dict] = {}
    for app in STRIP_EXTRAS:
        # The far end of the strip, before each launch. A new group is
        # inserted *immediately right of whatever has focus*
        # (`tab_binding.rs`), not appended — so the strip's order is
        # decided by what is focused when each window arrives rather
        # than by the order of these calls. The beat before this one
        # leaves Files focused, which put both extras between Files and
        # the word processor: the two windows that then had to be framed
        # together were three groups apart, and the one the beat was
        # about came back 788px past the right edge.
        order = strip_order(quinten)
        if order and order[-1]["surfaces"]:
            jump_to_group(quinten, order[-1]["surfaces"][0])
        quinten.place_next_window(app)
        painted = quinten.launch(app, search=STRIP_SEARCH.get(app))
        if painted is None:
            note(f"{app} did not open, so the strip carries "
                 f"{len(opened) + 2} applications rather than "
                 f"{len(STRIP_EXTRAS) + 2}")
            continue
        opened[app] = painted
        # The film's share, on the group that has just appeared.
        # `Session.launch` already applies it on sight of the group, but
        # an app that opened by the palette route got there through a
        # path that sets its own default first, and a group left at two
        # thirds is a window hanging off the side of every later shot.
        quinten.set_group_width(FILM_GROUP_SHARE)
    return opened


def beat_strip(quinten: Session, extras: dict[str, dict],
               wp: dict | None, shots: str) -> None:
    """Four applications on one strip, and two of them made one window.

    The beat this film was missing. Everything else here is about
    collaboration or about agents; this is about the desktop itself, and
    it argues two things a viewer cannot get from a still.

    The first is that the strip is longer than the screen and moves. Two
    windows are in frame at a readable size and the other two are past
    the edges, and the shot pans from one pair to the next — which is
    what makes it a strip rather than a grid that shrinks everything to
    fit.

    The second is tabbing, and it is the one that needs saying out loud.
    Everywhere else a tab belongs to its application: a browser's tabs
    are the browser's, an editor's are the editor's, and nothing can
    ever be tabbed beside them. Here the compositor owns the frame, so a
    tab group holds *windows* and a window is any application at all. So
    the image editor is dragged onto the word processor's bar and the
    two share one window with a tab each, and the tabs switch between
    two different programs. Two documents tabbed inside one app would
    have shown nothing.
    """
    banner("the strip: four applications, and two of them in one window")
    cut("quinten")

    order = strip_order(quinten)
    note(f"the strip carries {len(order)} window group(s), "
         f"{len(extras) + 2} applications")

    editor = extras.get(IMAGE_EDITOR)
    # Only neighbours are ever in frame together on a strip, so the two
    # windows the merge happens between have to be side by side for it to
    # be watchable. Said here rather than left to be inferred from a
    # framing that refuses two steps later.
    if editor is not None and wp is not None:
        a = group_index_of(quinten, wp, order)
        b = group_index_of(quinten, editor, order)
        if a is not None and b is not None and abs(a - b) != 1:
            note(f"the word processor is group {a} and the image editor "
                 f"group {b}, so they are not neighbours and cannot share "
                 f"a frame")

    # The far end first, so the pan below runs right to left and ends on
    # the pair the tabbing happens to. Whichever of the two extras is
    # actually there — an app another hand is still making usable may not
    # be — and each named by the window its launch handed back rather
    # than by its app id, because the image editor and the chat both
    # announce themselves to wayland as `app` and an id that names two
    # different programs names neither.
    far = [extras[a] for a in STRIP_EXTRAS if a in extras]
    # Neighbours, or not at all.
    #
    # Only adjacent groups are ever in frame together, so a pair with
    # something between them cannot be filmed side by side however the
    # strip is scrolled or however narrow the groups are made. Asking
    # anyway is not free: `frame_windows` walks the strip to the right
    # subject, then to the left, measures, and refuses -- and the next
    # step walks straight back. Every take of this film has paid a few
    # seconds for that pan and shown nothing at the end of it, because
    # the two apps `STRIP_EXTRAS` names open first and last with two
    # more between them.
    #
    # The arithmetic is already here for the pair below; it was just
    # never asked about this one. Said out loud rather than skipped
    # silently, because a beat that does not film what it claims should
    # say so in the transcript.
    if len(far) >= 2:
        order_now = strip_order(quinten)
        at = [group_index_of(quinten, sub, order_now) for sub in far]
        if None in at:
            note("one of the two windows at the far end is not on the "
                 "strip, so there is no pair there to film")
        elif max(at) - min(at) != len(at) - 1:
            note(f"the far pair are groups {at} with {max(at) - min(at) - 1} "
                 f"window(s) between them, so they cannot share a frame; "
                 f"the strip stays where the launch left it")
        else:
            step("two of the four are in frame; the other two are past "
                 "the edges")
            if frame_windows(quinten, far, VIEWPORT):
                off = [a.rsplit(".", 1)[-1]
                       for a in (FILES_APP, WORD_PROCESSOR)]
                note(f"{', '.join(off)} are on the same strip, scrolled "
                     f"off it")
                quinten.snapshot(os.path.join(shots, "10-strip-right.png"))
                read_hold(0.4)
    elif far:
        note("only one of the extra applications opened, so the strip "
             "cannot show a pair at its far end")

    if editor is None:
        gap("the image editor did not open, so there is no second "
            "application to put in the word processor's window and the "
            "tabbing is not filmed")
        return
    if wp is None:
        gap("the word processor never opened, so there is nothing to tab "
            "the image editor beside")
        return

    step("the strip scrolls back one window")
    # The pair the merge happens to, both whole before anything moves —
    # a window sliding into a tab bar that is off the edge of the screen
    # is a window disappearing.
    if not frame_windows(quinten, [wp, editor], VIEWPORT):
        gap("the word processor and the image editor are not in frame "
            "together, so the merge would happen off camera")
        return
    quinten.snapshot(os.path.join(shots, "11-strip-pair.png"))

    before_groups = len(tab_groups_here(quinten))
    before = quinten.input_ops

    step("the image editor is dropped onto the word processor's tab bar")
    note("in every other desktop a tab belongs to its application. Here "
         "the compositor owns the frame, so this bar holds two programs")
    resp = quinten.merge_into_group(editor["surface_id"], wp["surface_id"])
    if "err" in resp:
        gap("the strip refused to merge the two windows — "
            + json.dumps(resp["err"])[:160])
        return
    quinten.settle(0.55)

    # Read off the tree, which is where a tab group actually is. A shot
    # of a bar with two pills in it would be a picture of the answer, not
    # the answer: the same picture comes out of one app drawing two tabs
    # of its own, which is the thing this beat exists to say it is not.
    groups = tab_groups_here(quinten)
    merged = next((g for g in groups if len(g["tabs"]) > 1), None)
    claim("the two windows are now one group with two tabs",
          merged is not None and len(merged["tabs"]) == 2, True)
    claim("and the strip is one group shorter than it was",
          len(groups), before_groups - 1)
    if merged is None:
        gap("no group on the strip holds more than one tab, so the merge "
            "did not take")
        return
    note(f"one window, two tabs: {[t['title'] for t in merged['tabs']]}")
    # The claim the beat is for, and the one a viewer is being asked to
    # believe: those two pills are two *different applications*, not two
    # documents inside one.
    #
    # Asserted on the two **surfaces** rather than on the app id each
    # client announces, because an announced app id is not an identity.
    # The image editor calls itself `org.qos.app` — the toolkit's default,
    # which every app that has not set one of its own answers with — so a
    # name check here would have compared the word processor against a
    # string shared by half the fleet. A surface id is the compositor's
    # own handle on one wayland client, and these two are the windows
    # this run watched two different launches open: one asked for by
    # `dev.qos.word-processor`, one by `dev.qos.photos`.
    note(f"the two clients announce themselves as "
         f"{sorted({t['app_id'] for t in merged['tabs'] if t['app_id']})} — "
         f"so which application each is comes from the launch that opened "
         f"it, not from what it calls itself")
    claim("and they are the two windows two different applications "
          "opened, not two documents in one",
          sorted(t["surface_id"] for t in merged["tabs"]),
          sorted([int(wp["surface_id"]), int(editor["surface_id"])]))
    quinten.snapshot(os.path.join(shots, "12-tabbed.png"))
    read_hold(0.7)

    step("and the tabs switch between them")

    def active_tab_surface(s: Session, group_id) -> int | None:
        """Which window the merged group is currently showing."""
        group = next((g for g in tab_groups_here(s) if g["id"] == group_id),
                     None)
        if group is None:
            return None
        return next((t["surface_id"] for t in group["tabs"]
                     if t["id"] == group["active_tab"]), None)

    def show_tab(surface_id: int) -> None:
        """Bring the tab holding `surface_id` to the front of the group.

        By the pill's own name in the registry first, so what happens is
        a click on the tab a viewer can see rather than on wherever the
        layout happened to put one. A horizontal wheel is the fallback,
        and not a lesser gesture — it is the OS's own "sideways is the
        next window", driving the same `cycle_active_tab` the pill does —
        for the case where two tabs share a title and a name cannot pick
        between them.
        """
        want = next((t for t in merged["tabs"]
                     if t["surface_id"] == int(surface_id)), None)
        title = (want or {}).get("title")
        for name in ([f"tab:{title}[{merged['id']}]"] if title else []):
            if quinten.named_rect(name):
                quinten.click_named(name)
                quinten.settle(0.55)
                if active_tab_surface(quinten, merged["id"]) == int(surface_id):
                    return
        for _ in range(len(merged["tabs"])):
            quinten.scroll(1.0, 0.0)
            quinten.settle(0.5)
            if active_tab_surface(quinten, merged["id"]) == int(surface_id):
                return

    # The window is already showing the image editor: a tab arriving on a
    # bar becomes the one in front, which is what makes the merge visible
    # at all. So the switch that follows goes the other way — to the word
    # processor — and there is exactly one of them.
    #
    # It used to switch *to* the image editor first, which was a click on
    # the tab that was already in front: the tree agreed, the assertion
    # passed, and the screen moved by 1349 pixels, all of it the cursor.
    # A gesture that changes nothing is not a demonstration of anything.
    claim("the window is showing the application that just joined it",
          active_tab_surface(quinten, merged["id"]),
          int(editor["surface_id"]))
    shown_before = screen_shot(
        quinten, os.path.join(shots, "12a-before-switch.png"))
    show_tab(wp["surface_id"])
    # Asserted on what the window is showing, not on whether a click was
    # delivered. "The pill was there and I pressed it" is a claim about
    # the harness; the thing being demonstrated is that pressing it puts
    # the other application in the frame.
    #
    # And it leaves the document in front, which the beat after this one
    # is about — a shot of an image editor under a caption about a
    # document is a shot of the wrong thing.
    claim("switching tab puts the other application in the window",
          active_tab_surface(quinten, merged["id"]), int(wp["surface_id"]))
    shown_after = screen_shot(
        quinten, os.path.join(shots, "13-tab-switched.png"))
    moved = pixels_moved(shown_before, shown_after)
    if moved is not None:
        # And on the screen, because the tree agreeing is not the same as
        # the compositor drawing it.
        claim("and the screen changes with it",
              moved > SCREEN_MOVED_PX, True)
        note(f"{moved} pixels differ between the two tabs")
    read_hold(0.7)
    claim("every one of those was a person's gesture",
          quinten.input_ops > before, True)


def beat_nine(quinten: Session, xaver: Session, agent: Session,
              host_ws: dict, quinten_wp: dict | None, shots: str) -> None:
    banner("9.  the agent acts, and the room shows who did it")
    # The camera stays on the people. An agent is a participant in
    # somebody else's room, so what it does is shown the way any other
    # collaborator's actions are shown — arriving on their screens — and
    # never as a desktop of its own. Everything it is asked below is
    # asked over its control socket and printed to the transcript; none
    # of it is filmed, because there is nothing there to film.
    cut("quinten")

    baseline = agent.input_ops
    note(f"synthetic-input commands sent to the agent so far: {baseline}")

    # Filmed as nothing, because it is nothing to look at.
    #
    # The comment above already said none of this is filmed; the
    # camera was on quinten the whole time regardless, holding a
    # still desktop through four steps whose claims are all about
    # things that did *not* happen -- a palette that did not open, a
    # revision that did not move, an input count that did not rise.
    # Those are worth proving and there is no frame in them.
    #
    # What stays on camera is the half a viewer can see: the agent
    # changing the document two other people are looking at.
    with off_camera("the agent is asked what it can address, and one action is\n     invoked by name -- claims about things that did not happen"):
        step("it enumerates what this session exports")
        surface = agent.enumerate_actions("")
        actions = surface["actions"]
        providers: dict[str, int] = {}
        for a in actions:
            providers[a["provider"]] = providers.get(a["provider"], 0) + 1
        note(f"{surface['count']} addressable actions across "
             f"{len(providers)} providers")
        claim("every entry carries provider, id, kind and title",
              all(isinstance(a.get(k), str) and a[k]
                  for a in actions for k in ("provider", "id", "kind", "title")), True)
        claim("no entry carries a screen position",
              any(k in a for a in actions for k in ("rect", "x", "y")), False)

        step("enumerating is read-only: surveying cannot fire a side-effect")
        before_chrome = agent.chrome()
        for query in ("", "room", "word"):
            agent.enumerate_actions(query)
        after_chrome = agent.chrome()
        claim("the chrome revision did not move",
              after_chrome["revision"], before_chrome["revision"])
        claim("the palette did not open", after_chrome["palette"]["visible"], False)

        step("it invokes one action by its address")
        picked = next((a for a in actions if a["provider"] == AGENT_PROVIDER
                       and a["id"] == AGENT_ACTION_ID), None)
        claim(f"the catalogue offers ({AGENT_PROVIDER}, {AGENT_ACTION_ID})",
              picked is not None, True)
        if picked is None:
            raise SystemExit("the agent's chosen action is not exported")
        claim("the effect is absent beforehand",
              agent.overlays()[AGENT_ACTION_EFFECT], False)
        resp = agent.invoke_action(AGENT_PROVIDER, AGENT_ACTION_ID)
        claim("the invocation was accepted", "answer" in resp, True)
        claim("the reply names the address that ran",
              (resp.get("answer", {}).get("provider"), resp.get("answer", {}).get("id")),
              (AGENT_PROVIDER, AGENT_ACTION_ID))
        landed = wait_for(lambda: agent.overlays()[AGENT_ACTION_EFFECT], timeout=8)
        claim("the action changed live state", bool(landed), True)
        # Deliberately not filmed: this one opened a panel on the agent's own
        # session, and the agent's own session is the one screen this film
        # does not show.

        step("no synthetic input was used to do it")
        claim("no keystroke, click or modifier reached the agent since",
              agent.input_ops, baseline)
        note("the address (provider, id) is the entire input to the invocation")

    # The action above was a shell action, so it changed the shell: it
    # opened a panel on the agent's own session. The document is reached
    # the other way round, and the next step is that — not a lesser
    # substitute for it.
    #
    # `qos-agent` joins the room's collaboration broker as an ordinary
    # CRDT peer and applies `WordProcessorAction` — the same variants the
    # word processor's own toolbar dispatches — under its own `ActorId`,
    # then broadcasts the result. So the exported action is the
    # *vocabulary* and the CRDT is the *transport*: what arrives on the
    # other two screens is the app's own operation, attributable to the
    # participant that made it. And it holds no screen while it does —
    # there is no winit, wgpu or vello anywhere in `apps/agent`.
    note("what it just invoked was a shell action, so it moved the shell. "
         "The document is reached as a participant: apps/agent applies "
         "the word processor's own actions under its own actor and "
         "broadcasts them, with no window of its own")

    # A terminal used to be opened here, because opening a window was
    # the only visible thing an agent could do by address. It ended the
    # film on a `bash` prompt in a fourth window — a developer machine,
    # in the last shot a viewer keeps, arguing against the strip the
    # letter points at. What follows is the beat that was wanted
    # instead: the agent changes the document the other two are looking
    # at, and it is read back off somebody else's screen.
    step("quinten writes the notes, and asks Claude for a run sheet")
    cut("quinten")
    # The prose first, then the bullets that contradict it. Order
    # matters: the agent reads the page it is given, and a disagreement
    # it can only see half of is one it cannot raise.
    # The notes are already there — he wrote them in beat 7 and his
    # colleague watched them arrive. Typing them a second time here made
    # the agent's beat mostly a typing beat: a hundred and thirty
    # characters at a readable pace before anything interesting happened.
    #
    # What is typed now is the one line the beat is actually about.
    if not type_into_the_document(quinten, quinten_wp, ""):
        gap("nothing could be typed into quinten's document, so there is "
            "no list for the agent to act on")
        return
    quinten.settle(1.0)
    read_hold()
    quinten.type_prose(AGENT_INSTRUCTION)
    quinten.settle(1.5)
    # What he asked for, in his own document, addressed to a participant
    # by name. Nothing else in the film explains what is about to happen,
    # so this frame has to be readable.
    read_hold(READ_HOLD_S * 1.5)
    quinten.snapshot(os.path.join(shots, "11-instruction.png"))
    note(f"the instruction is a line in the document: {AGENT_INSTRUCTION!r}")

    step("the agent reads it and carries it out — with no window at all")
    # The best fifteen seconds in the film when it works — a table
    # assembling itself in a document nobody is touching — and the worst
    # two minutes in it when it does not. Bounded accordingly.
    with patience(11.0):
        ran = run_the_agent(agent, host_ws["room"], shots)
    if ran is None:
        gap("qos-agent is not built, so the agent could not act on the "
            "document (cargo build -p agent --bin qos-agent)")
        return
    claim("the agent carried out the instruction", ran, True)
    claim("and no synthetic input reached its session to do it",
          agent.input_ops, baseline)

    step("and xaver's own word processor is asked what it holds now")
    # The payoff, read off the screen of somebody who did not make the
    # change. The assertion is that the *content* survived into the
    # table: a table with the rows empty is what this beat produced
    # before, and it is indistinguishable from success in a transcript
    # that only asks whether a table exists.
    #
    # Asked of xaver's session and *filmed on quinten's*. That a third
    # participant's edit reached a fourth machine is the strongest form
    # of the claim and is worth asserting; it is not worth a second
    # excursion, because the same edit is arriving on the screen already
    # on camera, in the document the audience has watched being written.
    # An agent's work showing up in your document the way a colleague's
    # does is the whole point, and it is a point about *your* screen.
    reader = None
    try:
        reader = DocumentReader(xaver.collab_json_sock, host_ws["room"],
                                record=None)
        # A question over a socket, with the camera on a screen it is not
        # about. Nothing here is worth a minute of film in either
        # direction.
        with patience(3.0):
            doc, carried = reader.await_document(AGENT_BULLET_PROOF,
                                                 DOC_SYNC_TIMEOUT_S)
    except OSError as e:
        gap(f"xaver's session has no JSON broker to ask ({e})")
        doc, carried = None, False
    except CannotDecode as e:
        gap(f"{e} — so what his screen holds was not established")
        doc, carried = None, False
    finally:
        if reader is not None:
            reader.close()
    if doc is not None:
        claim("xaver's document carries the rows the agent made, with the "
              "text still in them", carried, True)
    else:
        gap("xaver's app never answered, so what his screen holds was not "
            "established")
    # The last shot of the film, so it is framed like one: even shares,
    # the strip scrolled back, and every window whole. An accumulation
    # of everything the run happened to open is the picture that argues
    # against the layout this is meant to show off.
    #
    # …and then pointed at the subject. Framing the strip is not the same
    # as choosing what the shot is of: an earlier cut ended on a file
    # manager showing an empty folder, under a caption about a document
    # two other participants had just written into.
    frame_one_window(quinten, WORD_PROCESSOR, prefer=quinten_wp)
    read_hold(READ_HOLD_S * 1.5)
    quinten.snapshot(os.path.join(shots, "12-agent-table.png"))
    xaver.snapshot(os.path.join(shots, "12b-xaver-agent-table.png"))


def run(quinten: Session, xaver: Session, agent: Session, shots: str,
        registry) -> None:
    """Every beat, in order — and which of them the camera is up for.

    All nine run and all nine are asserted; the transcript is the same
    either way. What differs is the cut, and the cut is decided here
    rather than inside the beats, so the shape of the film is one
    readable page instead of nineteen scattered decisions.

    The shape is one excursion: quinten's screen for the length of the
    story, out to xaver once at the moment his machine is holding
    something worth crossing for — a window he never opened, carrying a
    sentence he never typed — and back. Two cuts. The previous version
    had eleven, and a stranger watching it spent most of them working out
    whose machine they were on instead of watching what happened on it.

    The other two participants are visible *through* quinten's screen
    rather than by going to theirs: xaver arrives as a row in a roster
    and a cursor on the backdrop, and the agent as a chip in that same
    roster and then an edit landing in the document on camera. Which is
    also what collaboration software looks like when it works — you stay
    on your own screen.

    The beats that are run but not filmed are not filler — they are the
    proof for claims the film makes elsewhere, or beats good enough to
    deserve their own minute rather than fifteen seconds of this one.
    """
    host_ws, files = beat_one(quinten, shots)
    # Quinten raises the invitation on camera, at length, because it is
    # the gesture the whole collaboration story hangs off. Xaver redeeming
    # it is a person pasting a string into a dialog, and happens off
    # camera inside the beat.
    beat_two(quinten, xaver, host_ws, shots)
    with off_camera("the agent's invitation — the same card, a different "
                    "name on it — and the permission check that follows, "
                    "which is a question to the authority gate and has no "
                    "picture"):
        beat_three(quinten, agent, host_ws, shots)
    # Everything xaver holds and is looking at *before* the word
    # processor exists anywhere. Beat 6 measures against this rather than
    # against the state it finds when it starts, so "the window crossed
    # before we looked" reads as the success it is.
    baseline = {
        "tabs": set(xaver.tabs_here()),
        "view": xaver.view_state(),
        "groups": xaver.groups_here(),
    }
    beat_four(quinten, xaver, agent, host_ws, shots)
    if FETCH_ON_CAMERA:
        wp = beat_five(quinten, registry, shots)
    else:
        # Same fetch, same registry, same install path — off camera, so
        # the run still establishes that this machine did not have the
        # app and has it afterwards. What the film loses is the watching.
        with off_camera("the machine fetches an application it does not "
                        "have, verifies it and runs it"):
            wp = beat_five(quinten, registry, shots)
    # Which tab beat 5 opened, read now — before beat 7 puts a second
    # word processor in the room and makes "new since the baseline"
    # ambiguous. Beat 6 is asked about this one specifically.
    opened = [t for t in quinten.tabs_here() if t not in baseline["tabs"]]
    note(f"beat 5 opened {len(opened)} tab(s) on quinten's machine: "
         f"{opened or 'none'}")
    # Proven here, seen later. Beat 6 has to run before beat 7 — beat 7
    # has xaver open an app on his own machine, and that moves his own
    # focus, which is the thing beat 6 asserts nothing did. Both its
    # claims are answers over a socket rather than pictures, so running
    # it off camera costs the film nothing.
    with off_camera("xaver's tree, read over a socket: the window is there "
                    "and his view did not move. His screen is seen holding "
                    "it, and the sentence, in the excursion below"):
        beat_six(quinten, xaver, baseline, opened, shots)
    # The excursion: beat 7 opens it at its own last step, once his screen
    # has the sentence on it too, and the shot closes it. Beat 9 cuts back.
    beat_seven(quinten, xaver, host_ws, wp, shots)
    show_xavers_screen(xaver, shots)
    with off_camera("the photograph dragged between two sandboxes: the "
                    "longest beat here, and not one of the things this "
                    "cut is arguing"):
        beat_eight(quinten, files, wp, shots)
    # After the drag, deliberately. The drag needs Files and the word
    # processor in separate groups and both in frame; the tabbing that
    # follows spends one of those groups. Running it the other way round
    # would mean the two ends of the gesture were a tab apart, and beat 8
    # would be filming a window it had to bring up first.
    #
    # And after the extras have opened rather than before, which happens
    # off camera between the two — four applications appearing one after
    # another is four identical launches, and the film has shown two of
    # them properly already.
    with off_camera("the other two applications open, to the right of what "
                    "is already on the strip"):
        extras = open_the_rest_of_the_strip(quinten)
    beat_strip(quinten, extras, wp, shots)
    beat_nine(quinten, xaver, agent, host_ws, wp, shots)




# --------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=os.path.join(TARGET, "yc-demo"),
                    help="run directory for state, logs and screenshots")
    ap.add_argument("--shell",
                    default=os.path.join(TARGET, "demo", "shell"),
                    help="path to the compositor binary; must be a build that "
                         "still honours QOS_TEST_CONTROL (any profile with "
                         "debug-assertions on — `demo` or `dev`, never "
                         "`--release`)")
    ap.add_argument("--allow-stale-apps", action="store_true",
                    help="film the machine's installed apps even though "
                         "they were not built from this tree. Off by "
                         "default: a run against someone else's build "
                         "passes every assertion and proves nothing.")
    ap.add_argument("--allow-stale-shell", action="store_true",
                    help="record even if the compositor binary is older than "
                         "the sources under test (default: refuse, because a "
                         "frame cannot tell you which build it came from)")
    ap.add_argument("--size", default=None, metavar="WxH",
                    help="headless viewport (default 1280x800, or "
                         f"{RECORD_SIZE} under --record)")
    ap.add_argument("--keep", action="store_true",
                    help="leave the three sessions running afterwards, so they "
                         "can be driven by hand on camera")
    ap.add_argument("--record", nargs="?", const="", default=None, metavar="DIR",
                    help="capture whichever session is on camera while the "
                         f"demo runs and compose a {VIDEO_W}x{VIDEO_H} video "
                         "(default DIR: <out>/recording)")
    ap.add_argument("--record-size", default=RECORD_SIZE, metavar="WxH",
                    help=f"viewport each session is recorded at (default "
                         f"{RECORD_SIZE}): the video's own size, so a frame "
                         f"goes in with nothing resampled. Overridden by an "
                         f"explicit --size.")
    ap.add_argument("--record-sync", action="store_true",
                    help="encode each frame before asking for the next, on the "
                         "compositor's event loop. The old capture path: "
                         "simpler to reason about, and roughly a third of the "
                         "frame rate, because PNG encoding is most of a "
                         "snapshot and the loop cannot do anything else while "
                         "it runs. Kept for comparing the two honestly.")
    ap.add_argument("--record-interval", type=float, default=0.0, metavar="S",
                    help="target seconds between snapshots; 0 means as fast "
                         "as the compositor sustains")
    ap.add_argument("--record-dwell", type=float, default=0.3, metavar="S",
                    help="seconds to hold on each beat and step while "
                         "recording. Real elapsed time, really sampled — "
                         "never a held frame standing in for one. Small, "
                         "because it is paid on every step in the run and "
                         "the frames that actually have to be *read* ask "
                         "for their time separately (`read_hold`)")
    ap.add_argument("--record-cut-dwell", type=float, default=1.2, metavar="S",
                    help="seconds to hold after cutting to a session, so the "
                         "screen you have just been moved to registers "
                         "before the demo carries on. There are three cuts "
                         "in the film, so this is cheap and worth having")
    ap.add_argument("--record-max-hold", type=float, default=0.6, metavar="S",
                    help="longest one *accidentally* unchanged frame is "
                         "shown for. A session waiting on a sandbox spawn "
                         "or a network hop renders the same screen for as "
                         "long as it waits; this trims that dead air. "
                         "Nothing is duplicated or interpolated — a frame "
                         "is only ever shown for less time than it was "
                         "live. Frames the demo deliberately stopped on "
                         "(`read_hold`) are exempt, so this can be tight "
                         "without shortening the card somebody has to "
                         "read. 0 keeps the video on wall-clock time")
    ap.add_argument("--record-fps", type=int, default=30, metavar="N",
                    help="container frame rate of the composed video; the "
                         "capture rate is whatever was achieved, and frames "
                         "are held, never interpolated, to fill it")
    args = ap.parse_args()

    # The spawn site reads this: only a run that means to leave sessions
    # behind detaches them from this process's fate.
    global KEEP_SESSIONS
    KEEP_SESSIONS = args.keep

    # No fallback and no fixture: this demonstration is only meaningful
    # against the real compositor, so a missing binary is an exit, never
    # a substitute.
    if not os.path.exists(args.shell):
        print(f"no compositor at {args.shell}\nbuild it first:  "
              f"cargo build --profile demo -p shell --bin shell --bin qos-control",
              file=sys.stderr)
        return 2

    # A recording wants the size that tiles into the video 1:1; a plain
    # run wants a desktop-shaped viewport. An explicit --size beats both.
    size = args.size or (args.record_size if args.record is not None
                         else "1280x800")

    # Absolute, always. A relative --out reaches the compositor as a
    # relative XDG_CONFIG_HOME, which `dirs::config_dir()` ignores, and
    # the three sessions then persist into the real ~/.config/qos rather
    # than into this run.
    args.out = os.path.abspath(args.out)

    # Always from a clean slate. Sessions persist their workspaces, so a
    # re-run over a previous run's state starts with rooms already joined
    # and a catalogue that no longer offers `peer.connect` — the demo
    # then fails on a stale precondition rather than on anything real.
    # `--keep` is about teardown, never about reusing state.
    shutil.rmtree(args.out, ignore_errors=True)
    # The homes are throwaway too, and they live outside `--out` (see
    # HOMES_ROOT), so they need clearing on their own. A home left over
    # from a previous run carries that run's edits into this one's first
    # shot.
    shutil.rmtree(HOMES_ROOT, ignore_errors=True)
    # And the room's workspace, for the same reason and on the same
    # terms. Staged once, below, rather than once per session.
    shutil.rmtree(WORKSPACE_ROOT, ignore_errors=True)
    stage_workspace()
    shots = os.path.join(args.out, "shots")
    os.makedirs(shots, exist_ok=True)

    shadowed = _shadowed_definitions()
    if shadowed:
        print("\nrefusing to record: this file defines a name twice, so a "
              "call site is reaching a function it does not mean")
        for d in shadowed:
            print(f"    {d}")
        return 2

    # Refuse to film a binary older than the code it is meant to show.
    #
    # This rig has silently recorded a stale build more than once: the
    # run is green, the frames are of last night's compositor, and the
    # fix everyone is waiting to see is simply not in the picture.
    #
    # Both binaries are checked, because checking only the compositor is
    # how a six-hour-old agent got filmed answering in a vocabulary the
    # branch had already replaced. `demo-apps-live.sh` refreshes the app
    # payloads the registry serves and does not touch the agent, so
    # nothing else was ever going to catch it.
    if refuse_if_stale(
            args.shell,
            [os.path.join(REPO, "shell", "src"), os.path.join(REPO, "ui", "src")],
            "cargo build --profile demo -p shell --bin shell --bin qos-control",
            args.allow_stale_shell):
        return 2

    _agent_bin = agent_binary()
    if _agent_bin and refuse_if_stale(
            _agent_bin,
            [os.path.join(REPO, "apps", "agent", "src")],
            "cargo build --profile demo -p agent --bin qos-agent",
            args.allow_stale_shell):
        return 2

    # Unix socket paths are capped near 108 bytes, so they go in the
    # runtime dir rather than next to the rest of the run.
    runtime = os.environ.get("XDG_RUNTIME_DIR") or "/tmp"
    pid = os.getpid()

    # The packaging chain, by path. Each is optional: a missing one costs
    # the beat that needs it and nothing else, and the transcript says
    # which. `demo` binaries first, because that is what the compositor
    # is built as, falling back to `debug` where only one exists.
    def tool(name: str) -> str | None:
        for profile in ("demo", "debug"):
            path = os.path.join(TARGET, profile, name)
            if os.path.exists(path):
                return path
        return None

    tools = {var: path for var, name in (
        ("QOS_CATALOGUE_BIN", "qos-catalogue"),
        ("QOS_INSTALL_BIN", "qos-install"),
        ("QOS_LAUNCHER_BIN", "qos-launcher"),
    ) if (path := tool(name))}

    # Every session's view of what is installed: the machine's index,
    # overlaid with the registry `scripts/demo-apps-live.sh` builds out
    # of *this* tree. An id in both is taken from the live one, and an
    # id only there is added — the image editor is only ever there,
    # having never been installed onto this machine at all.
    #
    # The overlay is the whole reason that script exists. A machine
    # payload is a static binary from whenever somebody last ran an
    # installer, so a run against one films a build that predates
    # everything under test, and does it silently. An app this film
    # exercises rather than merely lists belongs in that script's `APPS`.
    entries = registry_entries(os.path.join(MACHINE_APPS, "registry.toml"))
    live = registry_entries(os.path.join(LIVE_APPS, "registry.toml"))
    entries.update(live)
    for app in sorted(live):
        print(f"{app}: {LIVE_APPS} (built from this tree)")

    # Where each of the film's own applications came from, before a
    # frame is recorded. An app that is nobody's is worth knowing about
    # now rather than as a launch that hands back None halfway through.
    borrowed, absent = [], []
    for app in sorted({FILES_APP, WORD_PROCESSOR, CHAT_APP, IMAGE_EDITOR}):
        if app in live:
            continue
        if app in entries:
            borrowed.append(app)
            print(f"{app}: {MACHINE_APPS} — not built from this tree, "
                  f"so this films the machine's installed payload")
        else:
            absent.append(app)
            print(f"{app}: MISSING from both {LIVE_APPS} and "
                  f"{MACHINE_APPS} — the beat that opens it will report "
                  f"it as unopened")

    # Refused, not warned.
    #
    # This printed its warning and carried on, and what came out was a run
    # that passed every assertion while filming applications built weeks
    # earlier by somebody else -- the changes under test were not in the
    # picture, and nothing in the picture could say so. A frame carries no
    # build id, so this is the last place that knows.
    #
    # The failure mode it produced is the expensive one: not a red run, but
    # a green one that means nothing. The whole rig exists to refuse a
    # success it did not verify, and an app it did not build is exactly
    # that -- so the same standard applies to the payload as to the
    # assertions.
    #
    # `--allow-stale-apps` for the case where borrowing really is intended
    # (filming the installed set on purpose), which is rare enough to be
    # worth typing.
    if (borrowed or absent) and not getattr(args, "allow_stale_apps", False):
        print()
        if absent:
            print(f"FATAL: no payload at all for: {', '.join(absent)}")
        if borrowed:
            print(f"FATAL: filming the machine's installed build of: "
                  f"{', '.join(borrowed)}")
        print()
        print("These are applications the film drives, so a run against")
        print("someone else's build proves nothing about this tree.")
        print()
        print("  QOS_DEMO_PROFILE=debug scripts/demo-apps-live.sh")
        print()
        print("or pass --allow-stale-apps to film the installed set on")
        print("purpose.")
        raise SystemExit(2)

    # A payload goes stale the same way the compositor does, and more
    # quietly. `demo-apps-live.sh` copies a binary into the payload, so
    # rebuilding the app afterwards leaves the registry serving the copy:
    # the run is green, every assertion passes, and the window in the
    # picture is the previous build. Nothing downstream can tell — a
    # frame does not carry a build id — so it has to be refused here,
    # the last place that knows both which payload will be served and
    # what it was built from.
    #
    # The binary's own name gives the crate it came from, rather than a
    # second copy of that script's table drifting against the first.
    for app in sorted(live):
        binaries = os.path.join(LIVE_APPS, "payloads", app, "bin")
        for name in sorted(os.listdir(binaries) if os.path.isdir(binaries) else []):
            src = os.path.join(REPO, "apps", name, "src")
            if not os.path.isdir(src):
                continue
            if refuse_if_stale(os.path.join(binaries, name),
                               [src, os.path.join(REPO, "ui", "src")],
                               "scripts/demo-apps-live.sh",
                               args.allow_stale_shell):
                return 2

    sessions = []
    for short, name in zip("qxa", ("quinten", "xaver", "Claude")):
        run_dir = os.path.join(args.out, name)
        apps_dir = os.path.join(run_dir, "qos-apps")
        # Quinten is the machine that does not have it yet. Everyone
        # else does, so xaver can open his own copy in beat 7.
        compose_registry(apps_dir, entries,
                         drop=(WORD_PROCESSOR,) if name == "quinten" else ())
        sessions.append(Session(
            name, name, run_dir,
            os.path.join(runtime, f"qos-yc-{short}-{pid}.sock"),
            args.shell, size,
            # A private runtime dir per session, standing in for the
            # per-user one logind would give each real login.
            os.path.join(runtime, f"qyc-{short}{pid}"),
            stage_home(os.path.join(HOMES_ROOT, name)),
            apps_dir, tools))
    quinten, xaver, agent = sessions
    global SESSIONS, VIEWPORT
    SESSIONS = sessions
    VIEWPORT = tuple(int(v) for v in size.lower().split("x"))

    print(f"compositor:    {args.shell}")
    print(f"run directory: {args.out}")
    print(f"apps:          {len(entries)} installed for xaver and the agent, "
          f"{len(entries) - (1 if WORD_PROCESSOR in entries else 0)} for quinten")
    for var, path in sorted(tools.items()):
        print(f"  {var:20} {path}")
    for var in ("QOS_CATALOGUE_BIN", "QOS_INSTALL_BIN", "QOS_LAUNCHER_BIN"):
        if var not in tools:
            print(f"  {var:20} MISSING — beat 5 cannot fetch anything")

    global RECORDER
    record_dir = None
    if args.record is not None:
        record_dir = args.record or os.path.join(args.out, "recording")
        record_dir = os.path.abspath(record_dir)
        os.makedirs(record_dir, exist_ok=True)
        RECORDER = Recorder(sessions, record_dir, args.record_interval,
                            args.record_dwell, args.record_cut_dwell,
                            args.record_max_hold,
                            defer=not args.record_sync)
        print(f"recording to:  {record_dir}")

    started = time.time()
    rc = 0
    try:
        for s in sessions:
            s.start()
        for s in sessions:
            s.wait_ready()
            print(f"{s.name} ready (pid {s.proc.pid}, user {s.user})")
        # Only once all three answer: a track that starts before its
        # session does is a track of a blank screen. Opens on the host,
        # which is where the first beat opens too.
        # The strip's proportions, before the first frame. Set here and
        # never again, so no beat resizes a window on camera.
        for s_ in sessions:
            establish_strip(s_)
        if RECORDER:
            RECORDER.start(quinten.name)
        # The registry quinten fetches from in beat 5. Its config dir is
        # his store's, which is how the catalogue in his session finds it.
        with Registry(
            os.path.join(TARGET, "debug", "examples",
                         "serve-test-registry"),
            os.path.join(LIVE_APPS, "payloads", WORD_PROCESSOR, "bin",
                         "word-processor"),
            os.path.join(quinten.dir, "store-config"),
            os.path.join(args.out, "registry.log"),
        ) as registry:
            if registry is None:
                print("no local registry: beat 5 will report the fetch as "
                      "unfilmed rather than skip past it")
            else:
                print(f"registry:      {registry.base_url}")
            run(quinten, xaver, agent, shots, registry)
    except Exception as exc:  # noqa: BLE001 — the report is the product
        FAILURES.append(f"{type(exc).__name__}: {exc}")
        import traceback
        traceback.print_exc()
    finally:
        # How each compositor ended, read before teardown touches it:
        # afterwards every status is this script's own SIGTERM. A lost
        # control socket raises the same exception whether a session was
        # closed on purpose or crashed, and reporting the two identically
        # is how an application crash was once read as a compositor crash.
        session_ends = [(sn.name, session_exit.how_it_ended(sn.proc))
                        for sn in sessions]
        # Before teardown either way — the capture threads talk to the
        # sessions, and `--keep` is about the sessions, not the camera.
        if RECORDER:
            RECORDER.stop()
        if args.keep:
            print("\n--keep: leaving the three sessions running.")
            for s in sessions:
                print(f"  {s.name:8} QOS_TEST_CONTROL={s.sock}  "
                      f"(pid {s.proc.pid if s.proc else '?'})")
        else:
            for s in sessions:
                s.stop()
                try:
                    os.unlink(s.sock)
                except FileNotFoundError:
                    pass

    video = None
    if RECORDER:
        RECORDER.report()
        try:
            RECORDER.beat_report(BEAT_MARKS, args.record_fps)
        except Exception as exc:  # noqa: BLE001 — never lose the video for it
            print(f"  (could not attribute video time to beats: {exc})")
        # The claim the whole film rests on, checked against the frames
        # rather than trusted to the beats. `cut` refuses the name, but
        # `cut` is not the only way `_live` could come to point at that
        # session, and this is the check that still holds when somebody
        # adds another way. A statistic reading "Claude 0 frames" is
        # something a person has to notice; this fails the run.
        strays = RECORDER.frames_from(NEVER_ON_CAMERA)
        if strays:
            FAILURES.append(
                f"{strays} frame(s) in the recording were captured from "
                f"{NEVER_ON_CAMERA}'s session — an agent's desktop is the "
                f"one thing this film must not contain")
        else:
            print(f"  no frame came from {NEVER_ON_CAMERA}'s session: what "
                  f"it did is in the film only where the people it did it "
                  f"with can see it")
        # Composed after teardown: encoding three tracks is CPU-hungry,
        # and the sessions have nothing left to say by now.
        try:
            # Outside `target/`, for the reason `FILMS_DIR` in
            # `yc-demo-live.py` gives: a take is the one artefact here that
            # cannot be regenerated, and `target/` is the directory whose
            # contract is that everything in it can.
            films = os.path.join(REPO, "films")
            os.makedirs(films, exist_ok=True)
            out_video = os.path.join(
                films, f"qos-demo-{time.strftime('%Y%m%d-%H%M%S')}.mp4")
            print(f"\ncomposing {out_video}")
            video = RECORDER.compose(out_video, args.record_fps, size)
        except Exception as exc:  # noqa: BLE001
            FAILURES.append(f"recording: {type(exc).__name__}: {exc}")
            print(f"  recording failed: {exc}", file=sys.stderr)
        for e in RECORDER.errors:
            FAILURES.append(f"recording: {e}")

    print("\n" + "=" * 70)
    print(f"elapsed: {time.time() - started:.1f}s")
    for name, how in session_ends:
        print(f"  {name}'s compositor {how}")
    if GAPS:
        print(f"{len(GAPS)} thing(s) this run could not verify:")
        for g in GAPS:
            print(f"  - {g}")
    if FAILURES:
        rc = 1
        print(f"{len(FAILURES)} claim(s) did not hold:")
        for f in FAILURES:
            print(f"  - {f}")
    else:
        print("every claim held.")
    print(f"screenshots: {shots}")
    if video:
        print(f"video:       {video['path']}")
        print(f"             {video['width']}x{video['height']} "
              f"{video['codec']} {video['fps']}  "
              f"{video['duration']:.2f}s  {video['frames']} frames  "
              f"{video['bytes'] / 1024 / 1024:.1f} MiB")
        mean_ms, worst_ms = RECORDER.snapshot_ms()
        print(f"             captured at {RECORDER.rate():.2f} frames/s; "
              f"one snapshot cost {mean_ms:.0f} ms mean / {worst_ms:.0f} ms worst")
        print(f"             every frame is a real render, in capture order; "
              f"none repeated, none interpolated")
        if RECORDER.trimmed > WALL_CLOCK_TOL_S:
            print(f"             {RECORDER.trimmed:.0f}s of still picture "
                  f"trimmed from {RECORDER.wall_s:.0f}s of demo — waits on "
                  f"sandbox spawns and network hops, held at most "
                  f"{args.record_max_hold:.1f}s each")
        else:
            print(f"             {video['duration']:.1f}s of picture for "
                  f"{RECORDER.wall_s:.1f}s of demo: the film runs on the "
                  f"wall clock")
    return rc


if __name__ == "__main__":
    sys.exit(main())
