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
import os
import random
import re
import shutil
import signal
import socket
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

#: The three applications beat 2 opens, in the order it opens them.
#:
#: Chat, then Files, then the word processor. The order is load-bearing
#: rather than a preference: it is how somebody actually sets a
#: workspace up, and the earlier cut had it backwards — one application,
#: then invitations into a room with nothing in it, then everything else
#: arriving later with no explanation of why.
#:
#: The image editor is deliberately **not** here. Beat 5 has the agent
#: open it, and an editor already on the strip would turn the film's
#: payoff into a window that was there all along.
BEAT_TWO_ORDER = (CHAT_APP, FILES_APP, WORD_PROCESSOR)

#: What to type into the palette for each of them.
#:
#: Every app in beat 2 is opened by searching, so each needs a query
#: that reaches it and nothing else. "photos", not "image", is the
#: lesson these were checked against: "image" also matches the Disk
#: Imager, and a search that can return the wrong application is a
#: search this film cannot rely on.
BEAT_TWO_SEARCH = {
    CHAT_APP: "chat",
    FILES_APP: "files",
    WORD_PROCESSOR: "word processor",
}

#: What to call each of them in the transcript — the name on the row a
#: person would have clicked, rather than a package id.
BEAT_TWO_TITLE = {
    CHAT_APP: "chat",
    FILES_APP: "Files",
    WORD_PROCESSOR: "the word processor",
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

# The photograph xaver right-clicks in beat 4, by the name it wears on
# screen. Named once because four places have to agree about it: the
# staging that writes it, the assertion that it is there, the row the
# press has to land on, and the compose surface that opens over it —
# that last one carries the file's *address*, read out of the app's own
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

# The capability principal quinten grants in beat 3. An agent is not a
# peer: it is a subject in the permission model, and the grant is scoped
# to one app rather than to the workspace.
AGENT_SUBJECT = "agent:claude"
#: What the agent is called on the roster, and the name an instruction
#: addresses. One constant because three things have to agree about it:
#: the `--name` the process runs under, the row a viewer reads, and the
#: `@mention` the room resolves to a participant id.
AGENT_ROSTER_NAME = "Claude"
#: Every app the agent is given leave to act in, by the resource
#: namespace its ids start with (`apps/agent/src/surface.rs`).
#:
#: Three, and they are not interchangeable. Two are the windows it
#: touches on camera: it is handed a photograph and told to square it
#: and put it in the open document, which is an operation in the image
#: editor and an operation in the word processor. Leave to work in an
#: app is also leave to *open* it, so `photos` covers both bringing the
#: editor into the room and cropping in it.
#:
#: The third is the channel it was spoken to in. The instruction reaches
#: it as a message in the room's conversation, and every summons is
#: answered in the channel it came from — including the ones it
#: declines, because somebody who mentions an agent and sees nothing
#: cannot tell refusal from a crash from never having been heard. An
#: agent that could read the room but not answer in it would be the
#: silent case by construction.
#:
#: Separate grants rather than one broad one, because that is the claim.
#: A grant names an application, not the workspace, so an agent handed a
#: photograph to crop cannot thereby start anything it likes.
#:
#: One list, used twice: quinten grants these in beat 3 and the agent
#: process runs with exactly them in beat 5. Two lists would let the
#: film claim a boundary the run did not actually impose.
AGENT_NAMESPACES = ("word-processor", "photos", "chat")

#: The same three, as the scopes the shell's authority gate is asked
#: about. `Scope::App` takes the package id rather than the namespace.
AGENT_SCOPE_APPS = {
    "word-processor": WORD_PROCESSOR,
    "photos": IMAGE_EDITOR,
    "chat": CHAT_APP,
}
AGENT_SCOPES = [f"app:{AGENT_SCOPE_APPS[ns]}" for ns in AGENT_NAMESPACES]

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
# The prose above the bullets disagrees with them, deliberately. "Doors
# open at seven" against "Doors: 18:00" is a contradiction a person would
# catch, and the sound check at 18:30 falling *after* doors is an
# ordering that cannot happen. Both are there so the agent has something
# to notice rather than something to reformat — and without a real
# disagreement to point at, the model declines to raise one about half
# the time, which is the correct behaviour and a poor beat.
#: The one instruction in the film, typed by xaver into the compose
#: surface his right-click opened.
#:
#: `@claude` rather than `Claude:` because this is a message in the room
#: rather than a paragraph in a document, and the room resolves an
#: `@mention` to a participant id at send. That is a stronger address
#: than a name matched out of prose: `apps/agent/src/listen.rs` reads the
#: structured mention list chat mints, never the body, so two people
#: called Claude cannot both be meant.
#:
#: One sentence, covering two applications. Not one instruction per app —
#: that is the claim. "make square" is a crop in the image editor and
#: "put in open document" is a placement in the word processor, and the
#: agent resolves each clause against what the room exports rather than
#: against anything written here.
#:
#: What it does **not** carry is the photograph's address. That rides
#: along as the compose surface's subject, because the right-click picked
#: it — so "this" is never a word the agent has to interpret. The
#: assertion in `say_something_about` that the id is *not* in the
#: sentence is what keeps the two apart.
AGENT_INSTRUCTION = "@claude make square and put in open document"

#: What the document must carry afterwards, and where it must carry it.
#:
#: The mime the content store sniffed off the cropped bytes and wrote
#: into the descriptor beside the hash. Deliberately not the PNG's own
#: signature: an image travels as a hash with its bytes in a
#: content-addressed store beside the document, so a document still
#: carrying the bytes would now be the bug. See
#: `image_is_in_the_document`.
AGENT_IMAGE_PROOF = "image/png"

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
    #: On for this film, whose whole subject is other people;
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
            # `info`, plus the two targets that narrate a cross-client
            # drag. Beat 8 aims a press at a file row inside a client,
            # and a client's widgets are not in the compositor's
            # registry — so the only way to know *which row was grabbed*
            # is to have the app say so. It does:
            #
            #   file_explorer::drag  "drag armed"  path=… at=Point{…}
            #   file_explorer::drag  "drag begins" path=…
            #   shell::selection     "client started a drag"
            #
            # Both are `debug!`, so at plain `info` the beat would be
            # aiming blind and could only report on the outcome. An
            # explicit RUST_LOG in the environment still wins, because
            # somebody debugging this wants their own filter.
            RUST_LOG=env.get(
                "RUST_LOG",
                "info,shell::selection=debug,file_explorer::drag=debug"),
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
            # The two stores an app launch cannot do without. The install
            # root is this session's; the runtime bundle is the machine's,
            # because every packaged app pins it and none of them ship it.
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
    POINTER_SPEED_PX_S = 2200.0
    #: Shortest and longest an approach may take, whatever the distance.
    POINTER_TRAVEL_MIN_S = 0.16
    POINTER_TRAVEL_MAX_S = 0.55
    #: Wall-clock a step of the path is given. Near the camera's own
    #: frame time, so consecutive steps land in consecutive frames
    #: rather than several to a frame.
    POINTER_STEP_S = 0.055
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
        seconds = min(self.POINTER_TRAVEL_MAX_S,
                      max(self.POINTER_TRAVEL_MIN_S,
                          distance / self.POINTER_SPEED_PX_S))
        steps = max(3, min(12, round(seconds / self.POINTER_STEP_S)))
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
               look: float = 1.0) -> dict | None:
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
            self.settle(1.2)
            pal = self.palette()
            rows = pal.get("rows") or []
            if not any(app_id in json.dumps(r) for r in rows):
                note(f"{self.name}: no palette row offers {app_id} for "
                     f"{search!r} (rows: {[r.get('id') for r in rows][:6]})")
                self.key("Escape")
                return None
            # Offered is not the same as *selected*, and Enter takes the
            # selection. A query that reaches the wanted app somewhere
            # down the list and something else at the top opens the
            # something else — and then paints, and answers every
            # question this function asks afterwards, so the run reports
            # a launch that worked. That is not hypothetical: searching
            # "image" once opened the Disk Imager, put it in the strip
            # beside the word processor and captioned it as the image
            # editor, and the only sign was on screen.
            #
            # `selected_id` is the row's id and the ids are *positional*
            # (`result:0`), so it says which row rather than which app —
            # comparing an app id against it directly is a check that can
            # only ever fail, which is what it did. The row it names is
            # what has to be looked at.
            selected = pal.get("selected_id")
            chosen = next((r for r in rows if r.get("id") == selected), None)
            if chosen is None and isinstance(selected, str):
                m = re.fullmatch(r"\w+:(\d+)", selected)
                if m and int(m.group(1)) < len(rows):
                    chosen = rows[int(m.group(1))]
            if chosen is None:
                note(f"{self.name}: the palette says {selected!r} is "
                     f"selected and no row answers to that, so which row "
                     f"Enter takes could not be checked")
            elif app_id not in json.dumps(chosen):
                note(f"{self.name}: {search!r} offers {app_id} but has "
                     f"{chosen.get('label')!r} selected, and Enter takes "
                     f"the selection — refusing to open the wrong "
                     f"application")
                self.key("Escape")
                return None
            self.key("Enter")
        began = time.time()
        deadline = began + timeout
        first_frame: float | None = None
        found: dict | None = None
        widened: set = set()
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
            note(f"{self.name}: {app_id} drew a frame "
                 f"{first_frame - began:.1f}s in but never filled its tab; "
                 f"reporting it as opened anyway")
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
        payload = {"op": "actions_invoke", "provider": provider,
                   "id": action_id}
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

    def type_prose(self, text: str, pace: bool | None = None) -> None:
        """Type `text` into a *client* so that it arrives as written.

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
                elif ch in SHIFTED_KEYS:
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
            elif ch in SHIFTED_KEYS:
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
                 defer: bool = True) -> None:
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
        # How much still picture the cap took out, filled in at compose.
        self.trimmed = 0.0
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
        self.started_at = time.time()
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
        with self._lock:
            self._off += 1
            self._live = None
        was = self._live
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

    def frames_from(self, name: str) -> int:
        return sum(1 for _t, _rel, n, _h in self.frames if n == name)

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
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=30)
            self._thread = None
        self.stopped_at = time.time()
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
        if not self.frames:
            raise RuntimeError("no frames were captured")
        return self.frames[0][0], self.frames[-1][0]

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
        self.trimmed = 0.0

        # How long each frame was the live state, before any editing.
        lived: list[float] = []
        for i, (t, _rel, _name, _held) in enumerate(frames):
            begins = t_start if i == 0 else t
            # The last frame was the live state until the camera stopped;
            # it is shown for one tick and the video ends.
            ends = frames[i + 1][0] if i + 1 < len(frames) else t_end + tick
            lived.append(max(tick, ends - begins))

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

        for start, count in runs:
            _t, rel, name, held = frames[start]
            was_live = sum(lived[start:start + count])
            # A frame the demo deliberately stopped on is shown for as
            # long as it was live. The cap is for the other kind of still
            # picture — a session waiting — and cannot tell the two apart
            # by looking at the pixels, so it is told.
            shown = was_live if held else min(was_live, max(tick, cap))
            self.trimmed += was_live - shown
            out.append((rel, name, shown))
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
            lines.append(f"duration {seconds:.4f}")
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
        return [s for s in spans if s[2] - s[1] >= 1.0 / fps]

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

        chain = [f"fps={fps}"]
        # The band sits *below* the screen rather than over it. A session
        # recorded at the video's height minus the band goes in 1:1 and
        # is padded downwards, so the strip the band occupies is empty
        # canvas and no window is ever covered by it. See RECORD_SIZE.
        if (source_w, source_h) == (VIDEO_W, VIDEO_H - BAND_H):
            chain.append(f"pad={VIDEO_W}:{VIDEO_H}:0:0:color={BG}")
        # No scale filter at all when the session already renders at the
        # video's size, which is what `--record-size` picks by default.
        # Resampling a 1:1 frame would cost exactly the legibility that
        # recording full-frame was for.
        elif (source_w, source_h) != (VIDEO_W, VIDEO_H):
            k = min(VIDEO_W / source_w, VIDEO_H / source_h)
            w, h = _even(source_w * k), _even(source_h * k)
            print(f"  {size} is not {VIDEO_W}x{VIDEO_H}; frames are resampled "
                  f"to {w}x{h} and letterboxed. Record at {RECORD_SIZE} "
                  f"for 1:1.")
            chain.append(f"scale={w}:{h}:flags=lanczos")
            chain.append(f"pad={VIDEO_W}:{VIDEO_H}:{(VIDEO_W - w) // 2}:"
                         f"{(VIDEO_H - h) // 2}:color={BG}")
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
        with open(os.path.join(self.dir, "filter.txt"), "w") as f:
            f.write(graph + "\n")

        cmd = ["ffmpeg", "-y", "-v", "error",
               "-f", "concat", "-safe", "0", "-i", rel_track,
               "-filter_complex", graph, "-map", "[v]",
               "-c:v", "libx264", "-preset", "medium", "-crf", "20",
               "-pix_fmt", "yuv420p", "-r", str(fps),
               "-movflags", "+faststart", os.path.abspath(out_path)]
        # Relative paths throughout the graph, run from the recording
        # directory: a run directory with a colon or a quote in its name
        # would otherwise be read as filter syntax.
        done = subprocess.run(cmd, cwd=self.dir, capture_output=True, text=True)
        if done.returncode != 0:
            raise RuntimeError(
                f"ffmpeg failed ({done.returncode}):\n{done.stderr.strip()}")
        return probe_video(os.path.abspath(out_path))


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
    }


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
    #: `to=` is a human-readable destination and can contain spaces —
    #: "the room" is the ordinary case — so it is matched lazily up to
    #: the ` about=` that follows it rather than as one non-space run.
    #: Written as `(\S+)` first, which stopped at "the", failed to find
    #: `about=` after it, and reported a remark that had in fact been
    #: sent as one the app never reported sending.
    SENT = re.compile(
        r"remark sent\s+to=(.+?)\s+about=(\S+)\s+text=(.*)$", re.M)
    #: The host, having put it on the wire. A different process's account of
    #: the same event, which is what makes it worth reading separately: the
    #: app saying it composed a message and the toolkit saying it sent one are
    #: two claims, and only the second one means it left.
    #: The message id follows the sentence directly — `into the room
    #: m_450b76…` — rather than under a `message=` field, which is what
    #: this looked for and never found. The claim it backs is the one
    #: that separates composed from delivered, so a pattern that cannot
    #: match reports every successful send as a remark the toolkit held
    #: on to.
    ON_THE_WIRE = re.compile(r"sending a remark into the room\s+(m_\w+)")
    #: The compositor, having been told that a participant carried content
    #: from one application into another — the crossing beat 5 is *for*.
    #:
    #: Logged by `collab_broker` where it handles
    #: `CollabRequest::CarriedContent`, in the session whose collaboration
    #: socket the reporter connected to. `drawn` is the answer from
    #: `action_flight::note_reported_flight`: true when the crossing was
    #: queued to be painted on the next frame, false when it was declined —
    #: content placed back into the app that produced it has not crossed an
    #: application boundary, and drawing one for it would claim something
    #: that did not happen.
    #:
    #: Read rather than inferred from the document, because they are
    #: different claims. A document that gained an image says the work
    #: landed; only this says a viewer saw it travel.
    CARRIED = re.compile(
        r"a participant reported carrying content between applications")
    #: The fields on that line, which say *which* two applications and
    #: whether it will be drawn. Written as a separate pattern because
    #: `tracing` emits fields ahead of the message, so one expression
    #: spanning both would depend on their order.
    CARRIED_FIELDS = re.compile(
        r"from_app=(\S+)\s+into_app=(\S+)\s+who=(\S+)\s+drawn=(true|false)")

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
                for t in group.get("tabs", []):
                    tile = t.get("tile") or {}
                    foreign = (tile.get("Foreign")
                               if isinstance(tile, dict) else None)
                    if foreign and foreign.get("surface_id") is not None:
                        surfaces.append(int(foreign["surface_id"]))
                out.append({"id": group.get("id"), "strip": sid,
                            "surfaces": surfaces,
                            "active": group.get("id") == active})
    return out


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


#: How many neighbours a strip walk steps past before giving up. A strip
#: with more groups than this is not a strip this film composes.
MAX_STRIP_STEPS = 8


def jump_to_group(s: Session, subject) -> bool:
    """Walk the strip's focus along to the group holding `subject`.

    Alt+Shift+wheel, one notch per group — the compositor's own
    navigation gesture (`client_input.rs`, Niri's `focus-column-right`),
    which selects the neighbouring window and lets the per-tick
    reconciler slide the strip to it. So the pan is eased and reads as
    navigation rather than as a cut.

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
    """
    order = strip_order(s)
    want = group_index_of(s, subject, order)
    if want is None:
        return False
    for _ in range(MAX_STRIP_STEPS):
        here = next((i for i, g in enumerate(order) if g["active"]), None)
        if here is None or here == want:
            break
        s.modifiers(alt=True, shift=True)
        s.scroll(0.0, -1.0 if want > here else 1.0)
        s.modifiers()
        s.settle(0.45)
        order = strip_order(s)
        moved = next((i for i, g in enumerate(order) if g["active"]), None)
        if moved == here:
            # The walk is not moving — the end of the strip, or a jump
            # that did not take. Said rather than spun on.
            note(f"{s.name}: the strip did not step past group {here}, so "
                 f"{subject_name(subject)} is as close as it gets")
            break
        want = group_index_of(s, subject, order)
        if want is None:
            return False
    order = strip_order(s)
    return next((i for i, g in enumerate(order) if g["active"]), None) == want


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
    that does not go through `Session.launch` — a peer window opens its
    app on the far side itself — takes the launch default of
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
                     share: int = 720) -> None:
    """Put one window up at a readable width, with the strip around it.

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
    focus_app_group(s, app_id, prefer=prefer)
    s.settle(0.6)
    # Measured, not assumed — the same rule the two-window framing keeps.
    # Focusing a group scrolls it *into view*, which is not the same as
    # scrolling it *wholly* into view: at 720 permille the shot came out
    # with the document readable and its right edge past the screen.
    # Said rather than silently accepted, because the next person to
    # widen this will otherwise re-find it from the frame.
    if VIEWPORT:
        clipped = [w for w in windows_off_screen(s, VIEWPORT) if app_id in w]
        if clipped:
            note(f"{s.name}: the framed window is not wholly on screen — "
                 + "; ".join(clipped))


#: How many pixels have to move before a screen counts as having
#: changed. A text caret blinking on and off is about fifty; a remote
#: peer's name pill is a bit over a thousand; a line of text arriving in
#: a document is tens of thousands. Set above the second and far below
#: the third, so a page that gained a sentence is never confused with a
#: page that only blinked.
SCREEN_MOVED_PX = 4000


def wait_for_xavers_windows(xaver: Session,
                            timeout: float = DOC_SYNC_TIMEOUT_S) -> dict[str, dict]:
    """Wait until xaver's machine is holding the room's applications.

    Joining a room does not copy pictures of somebody else's windows.
    The room document says which applications are open in it, and each
    machine that holds the room **starts its own local instance** of
    each — `peer_windows::open_for` calls
    `launcher::launch_in_workspace` on this side, with the room's
    capabilities published first, exactly as a launch from the palette
    does.

    That is why the next two beats are his rather than a click on a
    replicated view: the file manager under his pointer is a process on
    his machine reading his home, and it is the reason the gesture in
    beat 4 can produce a compose surface and a log line at all.

    It is also why this waits rather than asserts. Three sandbox spawns
    do not finish the instant a room index entry replicates, and a beat
    that cut to his screen the moment he joined would be filming an
    empty desktop under a caption about a shared workspace.

    Returns what actually arrived, keyed by app id, so a caller can say
    which of them did not — an app that is not installed on this machine
    comes back as a card saying so rather than as a window, and that is
    a different failure from a slow spawn.
    """
    want = set(BEAT_TWO_ORDER)

    def here() -> dict[str, dict]:
        found: dict[str, dict] = {}
        for app_id in want:
            tile = best_tile(xaver, app_id, VIEWPORT)
            if tile is not None:
                found[app_id] = tile
        return found

    deadline = time.time() + timeout
    landed = here()
    while time.time() < deadline and len(landed) < len(want):
        time.sleep(0.6)
        landed = here()
    if landed:
        note(f"xaver's machine opened {len(landed)} of {len(want)} "
             f"application(s) for itself: {sorted(landed)}")
    return landed


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

    The pill still gets clicked afterwards, and only then: once the
    group is up, selecting the right *tab* inside it is a click on a
    pill that is now where the compositor says it is. That matters from
    the moment two applications share a bar.
    """
    subject = app_id
    # By the end of the film two windows on this screen announce the same
    # app id — the session's own, and the peer's, replicated in. They are
    # showing the same document, so either would do; `prefer` names the
    # one the beat has been talking about, so the shot is of the window
    # the audience watched arrive.
    want = (prefer or {}).get("surface_id")
    if want is not None and tiles_for(s, int(want)):
        subject = int(want)
    if not tiles_for(s, subject):
        note(f"{s.name}: nothing on screen is {app_id}, so the strip is "
             f"left where it is")
        return False
    jump_to_group(s, subject)
    s.settle(0.5)
    # And the tab, if the group it landed on holds more than one. The
    # jump selects a window, so this is usually already done; it is not
    # when the beat wants a particular tab of a group that was already
    # focused on another.
    tile = tiles_for(s, subject)
    if tile:
        x, y, w, _h = tile[0]["rect"]
        py = y - 18.0
        # One window filling the strip wears no pill, and 18px above it
        # is the workspace switcher. A gesture meant to frame a window
        # must never be able to change which workspace is on camera.
        if py >= TOPBAR_H and x >= 0.0:
            s.click_at(max(4.0, x + min(90.0, w / 2.0)), py)
            s.settle(0.5)
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


def show_collaborators(s: Session) -> str:
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
        s.settle(1.0)
        return "the avatar stack in the top bar"
    gap(f"{s.name}: the top bar published no avatar stack to click, so the "
        f"room's people had to be opened by address rather than by the "
        f"gesture this beat is about")
    resp = s.invoke_action(AGENT_PROVIDER, "collaborators.open")
    if "err" in resp:
        note(f"{s.name}: collaborators.open refused — "
             f"{json.dumps(resp['err'])[:120]}")
        return "nothing - the panel could not be opened"
    s.settle(1.0)
    return "its address (the avatar stack was not on screen)"


def close_collaborators(s: Session) -> None:
    if s.overlays().get("collaborators"):
        s.key("Escape")
        s.settle(0.4)


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
    name to click and no rect to read, which is what an earlier version
    of this helper assumed when it opened with
    `named_rect("file_row:harbour.png")` and could therefore never once
    have run. So it uses the technique the drag beat already proved:

    - the row is found by `press_the_photo_row`, which sweeps a band and
      checks every press against the app's own account of what it hit;
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
        gap(f"{session.name}: no press landed on {file_name}. Its rows belong "
            f"to a client, so there is no name to click and the press has to "
            f"be aimed at a pixel; none of the band this swept was one of "
            f"its rows.")
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
    session.settle(0.8)

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

    # The field is the upper band of the panel; the destination selector
    # and the button sit below it. Clicking high inside it lands in the
    # text rather than on a control.
    session.click_at(sx + pw * 0.5, sy + ph * 0.28)
    session.settle(0.25)
    session.type_prose(text)
    session.settle(0.4)
    if shots:
        session.snapshot(os.path.join(shots, f"{tag or 'remark'}-typed.png"))

    # Enter in the field is a send affordance beside the button, and the
    # widget drains both into one place, so they cannot mean different
    # things. It is also what a person does after typing a line.
    #
    # `Enter`, not `Return`: the control socket's key vocabulary is the
    # named-key set (`test_control.rs`: "Enter", "Escape", "Tab",
    # "ArrowLeft"…), and an unknown name is a `parse_error` rather than
    # a keystroke that does nothing. This said `Return` and had never
    # once run — the compose surface had never opened far enough for
    # the line to be reached.
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
                          tag: str | None = None) -> str | None:
    """Invite somebody from the panel a person opened, with the mouse.

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
    host.settle(0.5)
    claim(f"the field took the name {name!r}",
          host.chrome()["peer_sync"]["invite_name"], name)
    # Who it is for, on screen, before anything has been minted. One of
    # the four frames this beat exists to let somebody read.
    read_hold()
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
    read_hold(READ_HOLD_S * 1.5)
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
                 handle: str = "readback") -> None:
        self.frames: list[dict] = []
        self.err: str | None = None
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(10.0)
        self.sock.connect(sock_path)
        self._send({"Hello": {"app_id": WORD_PROCESSOR, "cluster_id": room,
                              "record": record, "handle": handle}})
        self._send({"Presence": {"key": {"Workspace": {"id": self.SCOPE}},
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
    # An empty `png` would make the negative guard below vacuous —
    # `carries_bytes(blob, b"")` is True of every blob there is — so the
    # "and the document is *not* carrying the bytes themselves" half
    # would report a pass it never performed. Refused rather than
    # defaulted: a caller with no image to compare against is asking a
    # question this function cannot answer, and saying so is the
    # difference between absent and unreadable.
    if len(png) < 24:
        raise CannotDecode(
            f"image_is_in_the_document needs the image's own bytes to check "
            f"that the document is not carrying them; it was given "
            f"{len(png)}B, and an empty needle matches every document")
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
    """A photograph — a sun over ridged hills, drawn pixel by pixel.

    It has to read as a photograph at two sizes at once: as a 16px
    thumbnail in a file row, where only the sky/land split and the sun
    survive, and as a picture in the document, where a flat gradient
    would be obviously synthetic. So there is a horizon, a light source,
    receding ridges that get paler with distance, and a little grain.

    Generated rather than embedded because a checked-in JPEG is a binary
    blob nobody can review, and because the demo wants a payload whose
    size it chose: an image of ~98 KB crosses into the document where a
    keystroke moves ~100 bytes, and
    that number should come from this function rather than from whatever
    file somebody happened to drop in the tree.
    """
    import math
    import struct
    import zlib

    horizon = int(height * 0.56)
    sun_x, sun_y = int(width * 0.72), int(horizon * 0.42)
    sun_r = max(6.0, height * 0.055)

    # Each ridge: how far below the horizon its crest sits, its
    # amplitude, its wavelength, a phase, and how dark it is. Front
    # ridges are darker and rougher — that is what makes them read as
    # nearer rather than merely lower.
    # The `base` of each has to clear its own amplitude, or the crest
    # rises above the horizon, gets clipped by the sky, and paints as a
    # dead flat plate with a notch in it — which is precisely what the
    # first version of this drew.
    ridges = [
        (0.045, 0.055, 1.7, 0.4, (96, 122, 132)),
        (0.130, 0.075, 1.1, 2.1, (66, 94, 100)),
        (0.270, 0.095, 0.8, 4.3, (42, 68, 70)),
        (0.470, 0.130, 0.6, 1.2, (24, 44, 44)),
    ]

    def clamp(v: float) -> int:
        return 0 if v < 0 else (255 if v > 255 else int(v))

    # Deterministic grain. `random` would make the file — and so the
    # document delta — differ between runs, and the demo prints that
    # number.
    def grain(x: int, y: int) -> float:
        h = (x * 374761393 + y * 668265263) & 0xFFFFFFFF
        h = (h ^ (h >> 13)) * 1274126177 & 0xFFFFFFFF
        return ((h >> 16) & 0xFF) / 255.0 - 0.5

    rows = bytearray()
    for y in range(height):
        rows.append(0)  # filter: none
        for x in range(width):
            if y < horizon:
                # Sky: deep above, warm and pale toward the horizon.
                t = y / max(1, horizon)
                r, g, b = (58 + 150 * t, 108 + 122 * t, 165 + 70 * t)
                # The sun, and the haze around it.
                d = math.hypot(x - sun_x, y - sun_y)
                if d < sun_r:
                    r, g, b = 255, 250, 214
                elif d < sun_r * 7.0:
                    k = (1.0 - (d - sun_r) / (sun_r * 6.0)) ** 2.4
                    r, g, b = (r + (255 - r) * k, g + (250 - g) * k,
                               b + (214 - b) * k)
            else:
                # Land, painted back to front so a nearer ridge covers
                # the one behind it. What shows through above the
                # furthest crest is haze, not ground — filling this with
                # the ground colour put a dark band along the horizon,
                # and filling it with one flat haze colour put a flat
                # one there instead. So it fades out of the sky.
                t = min(1.0, (y - horizon) / max(1.0, height * 0.10))
                r, g, b = (208 - 76 * t, 230 - 78 * t, 235 - 77 * t)
                for base, amp, freq, phase, colour in ridges:
                    crest = (horizon + height * base
                             + math.sin(x / width * math.pi * 2 * freq + phase)
                             * height * amp * 0.5
                             + math.sin(x / width * math.pi * 9 * freq + phase * 3)
                             * height * amp * 0.12)
                    if y >= crest:
                        # A little vertical shading inside each band, so
                        # it is not a flat cut-out.
                        k = min(1.0, (y - crest) / (height * 0.30))
                        r, g, b = (colour[0] * (1 - 0.35 * k),
                                   colour[1] * (1 - 0.35 * k),
                                   colour[2] * (1 - 0.35 * k))
            # Enough grain to break up the banding a pure gradient shows
            # on a large flat sky, and no more: grain is nearly
            # incompressible, and this file's size *is* the payload the
            # demo measures crossing into the document. At 7.0 the same
            # picture was 294 KB.
            n = grain(x, y) * 2.5
            rows += bytes((clamp(r + n), clamp(g + n), clamp(b + n)))

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
    the photograph beat 4 is about.

    Staging a *file tree* is not the same as fabricating content: there
    is no invented correspondence here and nobody's name on anything.
    The documents carry the few lines the demo itself writes and reads.
    """
    for folder in ("Documents", "Downloads", "Pictures", "Projects"):
        os.makedirs(os.path.join(path, folder), exist_ok=True)
    # The photograph beat 4 is about, written twice.
    #
    # In `Pictures`, because that is where it belongs and where a person
    # would look for it — and at the top of the home, because that is
    # the directory Files is *showing* when the beat starts, and a
    # right-click can only land on a row that is on screen. Navigating
    # there first would be two more client-aimed clicks before the
    # gesture the beat is actually about, each of which can miss.
    #
    # Both are the same bytes, so whichever is dragged produces the same
    # payload and the same document delta.
    photo = a_real_png()
    for where in (os.path.join(path, "Pictures", PHOTO_NAME),
                  os.path.join(path, PHOTO_NAME)):
        with open(where, "wb") as fh:
            fh.write(photo)
    with open(os.path.join(path, "Documents", "Launch plan.md"), "w") as fh:
        fh.write("# Launch plan\n\n- Venue - The Hall\n- Date - 3 Sep\n")
    with open(os.path.join(path, "Documents", "Checklist.md"), "w") as fh:
        fh.write("# Checklist\n\n- Doors\n- Sound\n- Seating\n")
    with open(os.path.join(path, "Projects", "README.md"), "w") as fh:
        fh.write("# Projects\n")
    return path


class Registry:
    """A signed registry on loopback, serving one real `.qpkg`.

    The `serve-test-registry` example packs a real binary into a real
    package, signs it, and writes the `registries.toml` the catalogue
    reads — so a fetch through the palette verifies an ed25519 signature
    and a BLAKE3 digest for real. Nothing in the five-beat film fetches
    an application, so absent binaries cost this run nothing.
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
                  through: Session | None = None,
                  timeout: float = 120.0) -> bool | None:
    """Let the agent join the room and carry out one instruction.

    It connects to a session's broker, which is how its edits reach the
    others: the same libp2p republish every peer's edits take.

    **Which** session is a decision about the film, not about the
    protocol, and `through` is where it is made. Two of the things the
    agent sends are addressed to the session hosting its connection
    rather than to the room: opening an application, and reporting that
    it carried content from one application into another. The second is
    the crossing beat 5 exists to show — `collab_broker` hands it to
    `action_flight`, which queues it to be drawn between the two
    windows on *that* session's screen.

    Connected to its own session, as it was, the crossing therefore
    drew on the agent's desktop: the one screen this film never shows,
    and the one the run asserts zero frames came from. Everything could
    work end to end and a viewer would see nothing.

    So it is pointed at the session the camera is on. That is a
    workaround for a real gap and should be removed rather than kept:
    the report is addressed to one session because it was built that
    way, and qOS replicates a workspace's windows, so more than one
    machine legitimately holds both windows and could draw it. Fanning
    it to the room needs a new `CollabEvent`, peer-sync delivery and
    cross-machine attribution; until that exists, this line decides who
    sees it.

    It joins the **room** rather than one application, and that is what
    the film's instruction requires. One sentence covers two apps —
    square the photograph, put it in the open document — so the agent
    has to hold the conversation it was addressed in, the image editor
    it opens, and the word processor it writes to, at the same time. An
    `--app` that names one of them narrows the broker's fan-out to that
    one, and the other two clauses then resolve against nothing.

    It runs **with** the model, because the instruction is not a
    keyword. `Recipe::from_keywords` knows `table`, `caption` and
    `heading`; it does not know that "make square" is a crop with a
    centred unit rectangle, nor that "put in open document" is a
    placement whose subject is whatever the crop just produced. If the
    model is unreachable the agent says on the page that it cannot do
    what was asked, which is honest, and is not the beat.

    Returns None when the binary is not built, so a caller can say that
    rather than report a failure it did not observe.
    """
    binary = agent_binary()
    if binary is None:
        return None
    log_path = os.path.join(shots, "agent.log")
    with open(log_path, "w") as log:
        # `--app ""` joins the room rather than one app. The broker
        # narrows its fan-out to an application only when both ends name
        # one, so a connection announcing nothing is handed every app's
        # state — which is exactly what an instruction spanning two of
        # them needs.
        #
        # This used to name the word processor, for a real reason: with
        # no address to narrow on the agent was offered every document
        # open in the room and took whichever arrived first, and in one
        # run that was the file explorer's, whose actor id then refused
        # to merge with the word processor's. What makes the room safe
        # to join now is that the subject is carried rather than
        # inferred — the compose surface sends the photograph's address
        # with the sentence, so the agent is not guessing which of the
        # documents on offer was meant.
        #
        # `--record` is deliberately *not* passed: it names the file a
        # document was opened from, and this document has none. It is
        # the untitled document the word processor created when beat 2
        # launched it, so its record is empty, and `--record ""` would
        # be a different address from "no record" rather than the same
        # one.
        #
        # The grants are the list quinten granted on camera in beat 3,
        # not a second one written here — see `AGENT_NAMESPACES`.
        grants: list[str] = []
        for namespace in AGENT_NAMESPACES:
            grants += ["--grant", namespace]
        proc = subprocess.Popen(
            [binary, "--workspace", str(room),
             "--name", AGENT_ROSTER_NAME,
             "--app", "", *grants,
             "--socket", (through or agent).collab_json_sock, "--once",
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


def beat_one(quinten: Session, shots: str) -> dict:
    banner("1.  boot — a cold machine reaching a usable desktop")
    # The host's screen. A first boot really is empty here — one
    # workspace, no strips, no windows — so "empty" is read off the tree
    # rather than asserted about a backdrop.
    cut("quinten")

    # Only what has to be true *before* anything opens, because
    # everything asked here is a round trip into a compositor the camera
    # is already most of the way through, and the answer is seconds of
    # empty desktop in front of the first window. The rest of this run's
    # bookkeeping is as true afterwards as before, and is printed there.
    host_ws = quinten.active_workspace()
    claim("his desktop starts with no windows on it",
          len(quinten.tabs_here()), 0)
    quinten.park_pointer()
    quinten.snapshot(os.path.join(shots, "01-empty-desktop.png"))
    # The film opens on an empty desktop, and the boot bumper is cut to
    # end where the desktop arrives rather than after it — so this hold
    # is the far side of that join and the same picture is on both sides
    # of it. See `docs/boot-bumper-cut.md`.
    read_hold(EMPTY_DESKTOP_HOLD_S)

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
    return host_ws


def beat_two(quinten: Session, shots: str) -> dict[str, dict]:
    banner("2.  an empty workspace, and three applications opened into it")
    cut("quinten")

    # The order is the point, and it is the film's order rather than a
    # convenience: chat, then Files, then the word processor. It is how
    # somebody actually sets a workspace up. An earlier cut had it
    # backwards — one application, then invitations into a room with
    # nothing in it, then everything else arriving later with no
    # explanation of why.
    #
    # Every one of them through the palette. Opening by address is a
    # window appearing out of nothing and a viewer has to be told what
    # happened; the search — the key, a few letters, the row — *shows*
    # it, and shows that the desktop has one way in rather than a menu
    # per application. `via="palette"` takes that route deliberately
    # rather than as a fallback, so all three are opened the same way
    # and none of them is opened the way none of the others were.
    opened: dict[str, dict] = {}
    for n, app_id in enumerate(BEAT_TWO_ORDER, start=1):
        search = BEAT_TWO_SEARCH[app_id]
        step(f"he opens {BEAT_TWO_TITLE[app_id]} — the key, {search!r}, the row")
        # The camera gets out of the way while the app starts.
        # Snapshotting is most of a filmed compositor's event loop, and
        # the compositor is what spawns this client, hands it its
        # configure and takes its first buffer — so filming a launch at
        # full rate is the camera competing with the thing it is
        # filming.
        with easing(4.0):
            painted = quinten.launch(app_id, search=search, via="palette")
        claim(f"{BEAT_TWO_TITLE[app_id]} opened and painted a frame",
              painted is not None, True)
        if painted is None:
            gap(f"{app_id} never painted, so the workspace this film is "
                f"about is a window short. See {quinten.log_path}")
            continue
        opened[app_id] = painted
        note(f"surface {painted.get('surface_id')} at frame "
             f"{painted.get('frame_seq')}")
        claim(f"and his desktop now holds {n} window(s)",
              len(quinten.tabs_here()), n)

    # A group takes about three quarters of the strip by default, so a
    # third window opens mostly past the right edge — the strip working
    # exactly as designed, and the beat's own subject half out of frame
    # while it does. An even share puts all three in the picture that
    # claims all three are there.
    #
    # Framed by the windows `launch` handed back rather than by app id.
    # A surface id is the compositor's own handle on one client and is
    # never ambiguous; an app id is what a client announced, and this
    # beat has just opened three of them at once.
    frame_windows(quinten, list(opened.values()), VIEWPORT)
    read_hold()
    quinten.snapshot(os.path.join(shots, "02-three-applications.png"))
    claim("three applications, one workspace, opened one way",
          sorted(opened), sorted(BEAT_TWO_ORDER))
    return opened


def beat_three(quinten: Session, xaver: Session, agent: Session,
               host_ws: dict, shots: str) -> None:
    banner("3.  inviting people — xaver, then Claude")
    cut("quinten")

    step("he opens the room's people")
    # Through the chrome, at a pace a viewer can follow, and every step
    # of it a click on something painted. This used to pass in under a
    # second because it was invoked as an action: every claim held and
    # the collaboration UI never appeared, which is the one thing the
    # beat exists to put on screen.
    via = show_collaborators(quinten)
    note(f"opened from {via}")
    claim("the collaborators panel is up", quinten.overlays()["collaborators"],
          True)
    read_hold()
    quinten.snapshot(os.path.join(shots, "03a-collaborators-empty.png"))

    step("xaver first")
    token = invite_from_the_panel(quinten, xaver.user, shots, "03b")
    claim("quinten raised an invite from the panel he opened", bool(token),
          True)
    if not token:
        raise SystemExit("no invite token: the rest cannot run")
    note("the token is a signed blob carrying the room id, the owner's "
         "address and an expiry. The name is not in it: it waits on this "
         "machine until somebody redeems the token and is written into "
         "the room document then, by the room's own authority")
    with off_camera("xaver redeeming the invitation on his own machine — "
                    "quinten's roster gains him on screen shortly"):
        claim("xaver joined",
              join_room(xaver, token, via="panel", shots=shots, tag="03c"), True)
        claim("xaver's own state holds quinten's room",
              xaver.active_workspace()["room"], host_ws["room"])
        # And admitted as an editor, which is a separate act from being
        # let in. Redeeming a token puts somebody in the room; it does
        # not decide what they may do there, and without this xaver
        # holds no capability at all — the room's own list named
        # quinten and the agent, and the beat that claims both guests
        # are in it the same way failed on the guest who is a person.
        #
        # The same grant the agent's session gets below, so "admitted
        # the same way" is something this run does rather than
        # something it says.
        admitted = quinten.grant(f"person:{xaver.user}", "workspace", "write")
        claim("quinten admits xaver as an editor", "err" not in admitted, True)
        if "err" in admitted:
            note(json.dumps(admitted["err"])[:200])

    step("then Claude — admitted the same way, and marked as an agent")
    # The panel again, because raising the invite closed it: the card
    # carries the question and then the token, and a menu over that is a
    # menu over the thing the click was for. Re-opening it is also the
    # honest picture of the second invitation — the same panel, now with
    # xaver in it, and one more name going in the same way.
    via = show_collaborators(quinten)
    note(f"opened from {via} — and xaver is in it now")
    claim("the collaborators panel is up again",
          quinten.overlays()["collaborators"], True)
    quinten.snapshot(os.path.join(shots, "03c2-collaborators-xaver.png"))
    token = invite_from_the_panel(quinten, agent.user, shots, "03d")
    claim("quinten raised a second invite from the same panel", bool(token),
          True)
    if not token:
        raise SystemExit("no second token: the agent cannot join")
    # No cut. The agent has no screen worth filming and showing one
    # inverts the claim: it is a participant in somebody else's room, so
    # it appears as a row in a roster and a caret in a document, never
    # as a desktop of its own.
    claim("the agent joined the room", join_room(agent, token, via="action"),
          True)
    claim("the agent's own state holds quinten's room",
          agent.active_workspace()["room"], host_ws["room"])

    with off_camera("what the agent may do, asked of the authority gate — "
                    "a question with no picture"):
        def may_write(app: str) -> bool:
            answer = quinten.check_action(AGENT_SUBJECT, app, "SpliceText") \
                .get("answer", {}) or {}
            return bool(answer.get("permitted"))

        claim("the agent may not write to the word processor yet",
              may_write(WORD_PROCESSOR), False)
        # Two grants rather than one broad one, because that is the
        # claim. A grant names an application, not the workspace, so an
        # agent handed a photograph to crop cannot thereby start
        # anything it likes — and these two are exactly the two windows
        # it touches in beat 5.
        for scope in AGENT_SCOPES:
            granted = quinten.grant(AGENT_SUBJECT, scope, "write")
            claim(f"quinten grants it write on {scope}", "err" not in granted,
                  True)
            if "err" in granted:
                note(json.dumps(granted["err"])[:200])
        claim("now it may — the same question, the same gate",
              bool(wait_for(lambda: may_write(WORD_PROCESSOR),
                            timeout=SYNC_TIMEOUT_S)), True)
        claim("and still may not write to an app it was not granted",
              may_write("dev.qos.terminal"), False)
        note("a capability is a verb on a scope; the scope here is one app")
        admitted = quinten.grant(f"person:{agent.user}", "workspace", "write")
        claim("quinten admits the agent's session as an editor",
              "err" not in admitted, True)

    step("both of them in the room's own list — which is what membership is")
    # Read off the room's capability list, which *is* the membership
    # (`state.rs`: "Room membership lives here"), and is the same list
    # the panel on screen is drawn from. Read here rather than from
    # `peers`, which carries pointers and nothing else.
    #
    # This is also the honest place to look for the agent. Claude has no
    # mouse — a pointer that has never moved publishes nothing, by
    # design — so asserting a cursor for it would be asserting something
    # that should not exist. What makes it a participant is that it
    # holds a capability in the room.
    via = show_collaborators(quinten)
    note(f"opened from {via}")
    caps = quinten.active_workspace().get("capabilities") or []
    members: dict[str, list[str]] = {}
    for cap in caps:
        subject = json.dumps(cap.get("subject"))
        for name in (quinten.user, xaver.user, agent.user):
            if f'"{name}"' in subject or f":{name}" in subject:
                members.setdefault(name, []).append(str(cap.get("verb")))
    if not caps:
        gap("the active workspace reported no capability list, so who is "
            "in this room could not be read off it")
    else:
        note(f"the room's own capability list names {sorted(members)} "
             f"across {len(caps)} capabilities")
        claim("xaver is in it", xaver.user in members, True)
        claim("and the agent is in it the same way the people are — by "
              "holding a capability in it", agent.user in members, True)

    # And that the roster on screen *says* which is which. Admitted the
    # same way is the claim; indistinguishable afterwards is not, and a
    # viewer who had to infer which of two rows is a person would
    # sometimes infer wrong.
    #
    # Read off `roster`, which the compositor derives from the same
    # `collaborator_roster_rows` the panel is drawn from — so what this
    # asserts and what is on screen cannot drift apart. `is_agent` is
    # kept beside the label rather than folded into it for that reason.
    roster = (quinten.peers() or {}).get("roster") or []
    if not roster:
        gap("the roster came back empty, so whether Claude is marked as an "
            "agent on screen could not be read. The panel is up in this "
            "shot, so this is the reader looking in the wrong place rather "
            "than a room with nobody in it.")
    else:
        note(f"the roster draws {len(roster)} row(s): "
             + ", ".join(f"{r.get('label')!r}"
                         f"{' [agent]' if r.get('is_agent') else ''}"
                         for r in roster))
        agents = [r for r in roster if r.get("is_agent")]
        claim("exactly one row on the roster is marked as an agent",
              len(agents), 1)
        if agents:
            claim("and it is Claude", AGENT_ROSTER_NAME.lower()
                  in str(agents[0].get("label", "")).lower(), True)
        claim("the people are not marked as agents",
              any(r.get("is_agent") for r in roster
                  if str(r.get("label", "")).lower()
                  in (quinten.user.lower(), xaver.user.lower())), False)
    read_hold(READ_HOLD_S * 1.5)
    quinten.snapshot(os.path.join(shots, "03e-roster.png"))
    close_collaborators(quinten)

    # The cut the rest of the film is held on. Taken here rather than at
    # the top of beat 4 because the beat that earns it is this one: a
    # second machine that joined a room and now holds what is in it.
    #
    # And taken *late*. A cut the instant he joins shows an empty
    # desktop, which argues the opposite of what the beat claims — what
    # it claims is that the workspace is the shared thing, so joining it
    # brings what is in it. So his windows are waited for off camera,
    # and the camera arrives to find them already there.
    with off_camera("xaver's machine opening, for itself, the applications "
                    "the room told it about — waited for rather than filmed, "
                    "because a desktop filling up is the cause and his "
                    "screen holding them is the effect"):
        landed = wait_for_xavers_windows(xaver)
    cut("xaver")
    step("his screen — three applications he did not open")
    if len(landed) < len(BEAT_TWO_ORDER):
        gap(f"only {len(landed)} of {len(BEAT_TWO_ORDER)} applications "
            f"reached xaver's machine ({sorted(landed)}), so his screen is "
            f"not yet showing the room he joined. A peer window opens a "
            f"local instance of the app (shell/src/shell/peer_windows.rs); "
            f"one that is not installed on his machine says so on the card "
            f"instead of opening.")
    claim("what the room holds is what he holds",
          sorted(landed), sorted(BEAT_TWO_ORDER))
    frame_windows(xaver, list(landed), VIEWPORT)
    read_hold(READ_HOLD_S * 1.5)
    xaver.snapshot(os.path.join(shots, "03f-xaver-joined.png"))
    note("he is not the host, and he opened none of these")


def beat_four(xaver: Session, shots: str) -> bool:
    banner("4.  xaver: one instruction, from the person holding the thing")
    # No cut. Beats 4 and 5 are one held shot on his screen — he
    # instructs, and he watches it happen. An earlier version cut back
    # to the owner for beat 5, which asked a viewer to change seats in
    # the middle of the payoff.

    # His own file manager, on his own machine — a real local client the
    # room opened for him, not a picture of somebody else's window. That
    # is what makes the next gesture his: `peer_windows::open_for` runs
    # `launcher::launch_in_workspace` on this side, so the rows under
    # his pointer are being drawn by a process on his machine, reading
    # his home.
    tile = best_tile(xaver, FILES_APP, VIEWPORT)
    if tile is None:
        gap("xaver has no file manager window on screen, so there is no "
            "image for him to right-click. Without it this beat cannot "
            "run at all.")
        return False
    frame_one_window(xaver, FILES_APP)
    # Re-read *after* framing. `frame_one_window` moves and resizes the
    # group, so a rect taken before it is the placement of a window that
    # is no longer there — and every press below is aimed at a pixel,
    # which is exactly the aiming that a stale rect gets wrong silently.
    tile = best_tile(xaver, FILES_APP, VIEWPORT) or tile
    rect = tuple(tile["rect"])

    log = LogTail(xaver.log_path)
    log.catch_up()
    listing = expected_listing(xaver.home)

    # The gesture the whole desktop hangs its argument on, driven rather
    # than simulated — every step of it something somebody could have
    # done. What crosses is the photograph's **address**, so the agent
    # never has to work out what "this" was, and the sentence still
    # means the same thing in a session opened tomorrow.
    said = say_something_about(xaver, log, rect, PHOTO_NAME, listing,
                               AGENT_INSTRUCTION, shots, "04")
    claim("xaver sent one instruction about the photograph", said, True)
    if not said:
        return False
    note(f"one instruction, covering two applications: "
         f"{AGENT_INSTRUCTION!r}. Not one per app — the agent resolves "
         f"each clause against what the room exports")
    read_hold(READ_HOLD_S * 1.5)
    xaver.snapshot(os.path.join(shots, "04-instruction-sent.png"))
    return True


def beat_five(xaver: Session, agent: Session, host_ws: dict,
              shots: str) -> None:
    banner("5.  still on xaver's screen: and he watches it happen")
    # Still no cut. This is the second half of one held shot.
    #
    # On *xaver's* screen, not the owner's. He asked for it, so he sees
    # it land, on a machine where he owns nothing and opened nothing.
    # That is a stronger claim than watching it arrive where it was
    # already going, and it costs a cut rather than adding one.
    room = host_ws["room"]
    baseline = agent.input_ops

    # Watched on xaver's log because that is the session the agent is
    # pointed at, and the crossing is reported to whichever session
    # hosts the connection — see `run_the_agent`.
    log = LogTail(xaver.log_path)
    log.catch_up()

    before = set(xaver.tabs_here())
    step("Claude opens the image editor — a window nobody at this desk asked for")
    # The best seconds in the film when it works, and the worst two
    # minutes when it does not. Bounded accordingly.
    with patience(14.0):
        ran = run_the_agent(agent, room, shots, through=xaver)
    if ran is None:
        gap("qos-agent is not built, so nothing could act on the "
            "photograph (cargo build --profile demo -p agent --bin qos-agent)")
        return
    claim("the agent carried out the instruction", ran, True)
    claim("and no synthetic input reached its session to do it",
          agent.input_ops, baseline)

    # Read off *xaver's* tree. The editor is a window the agent opened,
    # in a room xaver is in, so his machine opens its own instance the
    # same way it opened the other three — and that is the picture.
    editor = wait_for(lambda: best_tile(xaver, IMAGE_EDITOR, VIEWPORT),
                      timeout=DOC_SYNC_TIMEOUT_S)
    claim("the image editor is open on xaver's screen too", bool(editor), True)
    if editor:
        fresh = [t for t in xaver.tabs_here() if t not in before]
        note(f"{len(fresh)} window(s) new on his machine since he sent it: "
             f"{fresh or 'none'}")
        frame_windows(xaver, [IMAGE_EDITOR, WORD_PROCESSOR], VIEWPORT)
        read_hold()
        xaver.snapshot(os.path.join(shots, "05a-image-editor.png"))
    else:
        gap("no image editor window reached xaver's machine, so the "
            "crossing has no near end to be watched from")

    step("and the picture crosses into the document")
    # The crossing is the beat: a smooth hand-off, not two windows
    # independently changing. So it is asserted *as a crossing*, and
    # not by reading the document.
    #
    # Those are different claims and the difference is the whole beat. A
    # document that gained an image says the work landed; only this says
    # a viewer saw it travel. Nothing in this rig used to check the
    # second one at all, which is how the report came to be addressed to
    # a session nobody films without anyone noticing — every claim held
    # and the screen was blank.
    #
    # `drawn` is `action_flight::note_reported_flight`'s own answer: true
    # when the crossing was queued to be painted on the next frame. A
    # report it declined — content placed back into the app that produced
    # it, which has not crossed an application boundary — comes back
    # false, and false must not read as success here.
    crossed = log.await_line(
        "a participant reported carrying content between applications",
        timeout=DOC_SYNC_TIMEOUT_S)
    claim("a crossing between two applications was reported to the "
          "session on camera", crossed, True)
    if crossed:
        fields = LogTail.CARRIED_FIELDS.findall(log.text)
        if fields:
            from_app, into_app, who, drawn = fields[-1]
            note(f"{who} carried content from {from_app} into {into_app}")
            claim("and the compositor accepted it as something to draw",
                  drawn, "true")
            claim("it crossed between two different applications, which is "
                  "what makes it a crossing", from_app != into_app, True)
        else:
            gap("the crossing was reported but its fields could not be read, "
                "so which two applications it was between is unknown")
    else:
        gap("no crossing was reported to the session being filmed. The "
            "report is addressed to whichever session hosts the agent's "
            "connection, so either the agent carried nothing between two "
            "applications, or it was pointed somewhere else — see "
            "`run_the_agent`, where that choice is made.")
    read_hold(READ_HOLD_S * 2.0)
    xaver.snapshot(os.path.join(shots, "05b-crossing.png"))

    # The picture, in the document, on his screen. The last frame of the
    # film, and it is taken *before* the readback below rather than
    # after: reading the document attaches a late joiner to his broker,
    # and a collaborator arriving is something a viewer can see — a
    # phantom participant appearing in the last shot, put there by the
    # rig checking its own work.
    frame_one_window(xaver, WORD_PROCESSOR)
    read_hold(READ_HOLD_S * 2.0)
    xaver.snapshot(os.path.join(shots, "05c-in-the-document.png"))
    note("the video is finished after this")

    with off_camera("the document itself, asked of the app holding it — a "
                    "late joiner attaches to do it, and an extra "
                    "collaborator arriving is a thing on screen the film "
                    "did not do"):
        step("read back off xaver's own word processor — a session that "
             "made none of this")
        # Asked of the app running on his machine, which did not perform
        # the crop, did not open the editor and did not send the
        # instruction. A claim read out of the session that made a change
        # proves that session agrees with itself; this one is the room
        # agreeing.
        #
        # The photograph as it sits in his home gives the negative half
        # something real to look for. Its first bytes are the PNG
        # signature and the IHDR the agent cropped, so a document that
        # embedded the *original* is caught; one that embedded the
        # cropped bytes is not, and the positive claim rests on the
        # reference rather than on this guard.
        try:
            with open(os.path.join(xaver.home, PHOTO_NAME), "rb") as fh:
                original = fh.read()
        except OSError as e:
            gap(f"the photograph could not be read back off xaver's home, "
                f"so the document check has nothing to compare against: {e}")
            return
        try:
            carried, size_bytes = image_is_in_the_document(
                xaver, room, original, timeout=DOC_SYNC_TIMEOUT_S)
        except CannotDecode as e:
            gap(f"the document could not be read, so whether the picture "
                f"landed in it is unknown rather than false: {e}")
            return
        claim("the photograph is in the document on his machine", carried,
              True)
        if carried:
            note(f"the document carries a {AGENT_IMAGE_PROOF} reference; "
                 f"{size_bytes}B of document reached this session, and the "
                 f"bytes themselves live in the content store beside it "
                 f"rather than in the change")
        else:
            gap("the word processor on xaver's machine holds no image "
                "reference, so the picture did not cross into the document "
                "even if something appeared on screen")


#: Where the file view puts its rows, straight off the app's own
#: constants (`views/file_view.rs:63`): a 30px column header, then rows
#: of 34px. Used only to convert a press that landed on the *wrong* row
#: into an aimed one — never to guess where a row is in the first place.
LIST_ROW_H = 34.0


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


def press_the_photo_row(quinten: Session, log: LogTail, tile: tuple,
                        photo_name: str, listing: list[str]) -> tuple | None:
    """Press on the photograph's row and *keep the button down*.

    The whole difficulty of this beat is in this function. A file row
    belongs to a client, and a client's widgets are not in the
    compositor's registry, so there is no name to click and no rect to
    read — the press has to be aimed at a pixel. Aiming at a remembered
    pixel is what went wrong repeatedly, so nothing here is remembered:

    - the tile's rect comes from `foreign_frames`, which is the
      placement the compositor is painting right now;
    - every press is checked against the app's own account of what it
      hit (`drag armed path=… at=…`), so a miss is *known* to be a miss
      rather than assumed to be a hit;
    - a press that lands on the wrong row is corrected using the
      client-space y the app just reported, not using arithmetic about
      where the tile's chrome ends. One press measures the mapping
      between screen and client coordinates; the second uses it.

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

    def press(at_y: float) -> tuple[str, float] | None:
        """Press at `at_y`; report what the app says it grabbed."""
        quinten.pointer_move(x, at_y)
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

    # Where to try first. Rows start below the app's toolbar and the
    # column header, and how much chrome sits above them depends on what
    # the app stacked there — so this sweeps a band rather than trusting
    # a single offset. Coarse, because it only has to land on *any* row;
    # the correction below does the aiming.
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


def run(quinten: Session, xaver: Session, agent: Session, shots: str,
        registry) -> None:
    """The five beats of `docs/demo-script.md`, in order.

    Everything below is what a viewer sees, and anything not here is not
    in the film. That is a change in kind from what this function used
    to be: it ran nine beats and chose a cut over the top of them, so
    the film's shape was a decision taken here about which of nine
    things to show. The script settled the shape instead, and what is
    left is five beats and two cuts.

    The cuts, and why there are only two. Quinten's screen for the first
    half — the desktop being set up and the room being filled — then one
    cut to xaver, held for the whole of the second half. He instructs,
    and he watches it happen. An earlier version switched at nearly
    every step, and a viewer spends each of those working out whose
    machine they are on rather than watching what happens on it; a later
    one cut back for beat 5, which asked them to change seats in the
    middle of the payoff.

    The agent is never cut to and has no shot of its own. It appears as
    a chip in a roster, a window it opened on somebody else's desktop
    and a picture landing in somebody else's document — which is the
    claim. `cut` refuses its name outright, and the run fails at the end
    if a single frame turns out to have come from its session.

    `registry` is accepted and unused. The machine that fetches an
    application it does not have is a real capability and was a beat of
    its own in the nine-beat cut; the five-beat script does not list it,
    and a beat that adds a second argument is cut rather than kept
    because the machinery for it happens to be standing.
    """
    host_ws = beat_one(quinten, shots)
    beat_two(quinten, shots)
    beat_three(quinten, xaver, agent, host_ws, shots)
    # Beats 4 and 5 are one held shot on xaver's screen. The cut into it
    # is taken at the end of beat 3, once his machine is holding the
    # applications, so neither of these takes one.
    if beat_four(xaver, shots):
        beat_five(xaver, agent, host_ws, shots)
    else:
        gap("beat 4 did not send an instruction, so there was nothing for "
            "beat 5 to watch happen. Beat 5 is not run: a beat that "
            "reports on work nobody asked for would be reporting on "
            "whatever the room happened to contain.")



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
    for short, name in zip("qxa", ("quinten", "xaver",
                                   AGENT_ROSTER_NAME)):
        run_dir = os.path.join(args.out, name)
        apps_dir = os.path.join(run_dir, "qos-apps")
        # Every machine holds every app. Quinten's used to be missing
        # the word processor so a beat could film it being fetched, and
        # the five-beat film has no such beat — which mattered rather
        # than merely going unused: beat 2 opens the word processor
        # through the palette, so on a machine without it that open
        # would silently become a fetch, and one of three supposedly
        # identical gestures would take ten seconds longer than the
        # others for a reason nothing on screen explained.
        compose_registry(apps_dir, entries)
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
    print(f"apps:          {len(entries)} installed on each of the three "
          f"machines")
    for var, path in sorted(tools.items()):
        print(f"  {var:20} {path}")
    for var in ("QOS_CATALOGUE_BIN", "QOS_INSTALL_BIN", "QOS_LAUNCHER_BIN"):
        if var not in tools:
            print(f"  {var:20} MISSING — no app can be fetched")

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
        # Served but unused: no beat in this film fetches an
        # application. Kept standing because the capability is real and
        # the next cut may ask for it again, and because serving it
        # costs one process that nothing queries.
        with Registry(
            os.path.join(TARGET, "debug", "examples",
                         "serve-test-registry"),
            os.path.join(LIVE_APPS, "payloads", WORD_PROCESSOR, "bin",
                         "word-processor"),
            os.path.join(quinten.dir, "store-config"),
            os.path.join(args.out, "registry.log"),
        ) as registry:
            if registry is None:
                print("no local registry: nothing in this film needs one")
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
        if RECORDER.trimmed > 0.5:
            span = RECORDER.stopped_at - RECORDER.started_at
            print(f"             {RECORDER.trimmed:.0f}s of still picture "
                  f"trimmed from {span:.0f}s of demo — waits on sandbox "
                  f"spawns and network hops, held at most "
                  f"{args.record_max_hold:.1f}s each")
    return rc


if __name__ == "__main__":
    sys.exit(main())
