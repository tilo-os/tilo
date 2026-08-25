#!/usr/bin/env python3
"""The demo where only the other people are scripted.

`yc-demo.py` performs all three participants. This performs two of them.
Quinten's session opens in a window on your desktop and nothing in this
file ever touches it: you open Files, chat and the word processor, you
open the collaborators panel, you mint the invitations, you type. Every
beat that was synthesised — the clicks, the typing rhythm, the framing,
the dwells — stops being a thing that can look staged, because it is not
staged.

What is left to script is only what a second person and an agent would
really do, and each of them reacts to something you did rather than to a
step number:

  xaver   redeems the invitation a few seconds after the code exists.
          Not after a beat: the token is polled for, off the card and
          off your clipboard, and the delay is the time it takes to read
          a code out and cross the room with it.
  Claude  is launched pointed at the room and then waits, holding no
          screen, until the document itself says something addressed to
          it. `qos-agent` already does that; there is no beat here that
          makes it act.

THE FILM THIS DRIVES
====================

An earlier cut was two people, one shared document, and an agent that
answered one instruction in it. That proves collaboration in a word
processor, and a word processor is the one place a viewer has already
seen collaboration — it invites a comparison with a fifteen-year-old
product and loses it.

This one argues the thing that has no equivalent: **work moving between
different applications, between different people, with an agent acting
across both.** The beats below are what this file now drives; where one
depends on work still in flight, the dependency is named under NEEDS.

`docs/demo-script.md` is the same film in the form somebody films from,
and is the one to read at the camera. This block is the same beats with
their reasons attached, for whoever is changing the rig.

Budget: boot bumper ~3.2s (cut, exists) + ~75s driven. Hard cap 1:40.

MEASURED, from a full three-session take on 2026-08-24. The per-scene
budgets below were written without measuring, and two of them are fiction:

    0-15s    minting two invitations
    15-22s   xaver crosses the room
    22-37s   xaver pastes the code and joins
    37-40s   the agent admitted
    40-62s   the word processor opens, the owner types
    62-90s   Claude waits for its line, then works
    90-106s  the edit crossing, and the excursion
                                    -> 106.6s, 6.6s over the cap

Two findings worth acting on rather than re-budgeting around.

Scene 1 costs 40s against 15s planned. Two invitations and two
redemptions are more real gesture than 15s holds, and spending over half
the budget on setup is where a demo loses the room.

Claude's 28s is model latency and will not shrink. It is dead air unless
something else is on screen, which is what "the owner keeps typing
throughout" is for -- but in this take the typing finished at 62s and
Claude worked from 62 to 90, so the overlap the plan claims did not
happen. Interleaving those is worth more than any second trimmed from a
hold, because it removes a wait rather than shortening one.

    BEAT 0  the machine boots                                   ~3.2s
        Not recorded here. `films/qos-boot-bumper.mp4`, concatenated
        in front by `scripts/demo-assemble.py`. Console, greeter,
        filled form, cut before the desktop -- see
        docs/boot-bumper-cut.md. Not under `target/`: it lived there
        once and a routine reclaim deleted it, and a recorded boot is
        the one asset here that cannot be regenerated on demand.

    BEAT 1  a workspace, and the people in it                    ~15s
        YOU     open the collaborators panel, invite xaver, invite
                Claude. Both invitations outstanding at once -- this
                used to lose the second name (fixed).
        SCRIPT  xaver redeems after ~8s; Claude joins when launched.
        SHOWS   an agent admitted the same way a person is: by holding
                a capability in the same list, marked as an agent so
                nobody has to infer which is which.

    BEAT 2  four applications, two in frame                      ~10s
        YOU     open Files, chat, the word processor. The image editor
                arrives in beat 3.
        YOU     scroll the strip; put two of them in one tab group.
        SHOWS   tabs are a frame primitive the compositor owns, so a
                tab group holds *different applications*. Every other
                desktop has per-app tabs; this is the one gesture that
                cannot be mistaken for a browser.

    SCENE 3  one instruction, from the person holding the thing   ~15s
        SCRIPT  xaver, in **Files**, right-clicks harbour.png and writes:
                    "@claude make square and put in open document"
                One sentence, from him, carrying the file's address. He
                does not describe the photograph and the agent does not
                go looking for it.
        YOU     keep typing in the word processor throughout.
        SHOWS   an instruction that arrives as (text, address, capability)
                rather than as a description of a screen.
        NEEDS   right-click compose in the file explorer  (landed)
                one instruction, several actions  (in flight)

    SCENE 4  and he watches it happen                             ~20s
        HOLD    on xaver. He asked for it, so he sees it land -- on a
                machine where he owns nothing and opened nothing,
                which is a stronger claim than watching a result
                arrive where it was already going. This said `CUT
                back to your screen` while the code stayed where it
                was; `his_instruction` never cuts back, and
                docs/demo-script.md agrees with the code.
        SHOWS   Claude **opens the image editor itself**, crops the
                photograph, and the result travels visibly into the word
                processor's document.

                This is the whole film. Not that an agent edited a
                document -- every word processor now ships something that
                does. That an agent *used the desktop*: it opened an
                application because it decided it needed one, acted on an
                object in it, and put the result in a second application,
                owning neither, having been granted the right to touch
                each. Two applications that contain no AI, addressable
                because the system made them so.

                The motion matters and is not decoration. An action that
                only appears as a changed document has to be narrated; one
                you can watch does not. It is also what makes attribution
                legible -- the object travels in the acting participant's
                colour, so it reads as *Claude did this*.

        NEEDS   the agent may open an application  (in flight)
                an agent's cross-app action is visible  (in flight)
                crop by address  (landed, proven to the pixels)

    SCENE 5  and it is undoable                          ~5s, CUTTABLE
        YOU     Ctrl+Z.
        SHOWS   `undo_for_author` walks back the last change belonging to
                *that* actor and leaves everyone else's alone -- which is
                why an agent may be allowed to write at all.
        This scene is deliberately expendable. It is the strongest
        architectural claim in the stack and it wants thirty seconds it
        does not have here, so it is filmed last and cut without
        argument if it does not read clearly.

    CLOSE   there is no more, on purpose: the point has been made and
            every further scene dilutes it.

WHERE YOU ARE ON THE STRIP, AND HOW YOU GOT THERE
--------------------------------------------------
The strip is longer than the screen, so at every moment a viewer must be
able to say which part of it they are looking at and what moved them
there. That is a hard requirement of this film and not a polish item: a
desktop whose defining idea is a strip cannot afford to look like windows
jumping about.

Two rules.

**Motion is motivated.** The strip moves because something asked it to --
an application opening past the edge, a scene needing two named windows
in frame, a cut to somebody else's screen. It never moves to tidy itself
up, and two moves never happen back to back without a reason between
them.

**Motion is continuous.** Getting from one part of the strip to another
is a travel a viewer can follow, not a cut. `frame_windows` narrows and
scrolls to bring named windows into view; anything that repositions
without animating is a jump, and a jump loses the thread of where things
are.

Worth watching for in every take: a scroll immediately after a launch
(the launch already moved it), a frame call that shifts the strip for a
scene that then does not use the framing, and any reposition during the
agent's latency, when nothing on screen explains it.

CUT, AND WHY -- so nothing is rebuilt by mistake
------------------------------------------------
Scenes that existed, worked, and are deliberately not in this film:

  * Four applications in one tab group. The most distinctive thing this
    desktop does, and it argues about window management while the
    audience is waiting for the point.
  * The agent disagreeing usefully -- reading the run sheet and saying
    sound check at 18:30 falls after doors at 18:00. The best sentence
    anything here has produced. It is a second idea, and a film with two
    ideas makes neither.
  * The run sheet, the table drafting, its ordering, the registry fetch,
    the strip pan as a scene of its own.

None of these are wrong and all of them are built. They are cut because
the film is one argument long.

WHAT EACH BEAT HAS TO SURVIVE
-----------------------------
A beat earns its seconds only if a viewer can *see* the thing it
claims. Two of these have already been rebuilt for that reason: an app
fetched over the network looked exactly like an app already installed
until the row said so, and an agent in the roster looked exactly like a
person until it carried a mark. Before filming any beat above, ask what
is on screen that would be different if the capability were absent. If
the answer is "nothing", the beat is not ready.

The camera is on your screen throughout and steps out to xaver's twice
— once when he arrives, once when the agent's work lands on it — for a
few seconds each.

    scripts/yc-demo-live.py                # you drive; it records
    scripts/yc-demo-live.py --dry-run      # stage it all headless, prove the wiring
    scripts/yc-demo-live.py --self-test    # it drives your half too, no hands
    scripts/yc-demo-live.py --xaver-delay 12 --excursion 3

Runs against target/demo/shell, the same as `yc-demo.py`:

    cargo build --profile demo -p shell --bin shell --bin qos-control
    cargo build --profile demo -p agent --bin qos-agent
    scripts/demo-apps-live.sh

`--release` is not the profile: the control socket this records through
is compiled out of a shipped build on purpose.

  Nothing here opens a window on anybody but you
  ----------------------------------------------
  xaver's and Claude's compositors are headless. Claude's is additionally
  never cut to, and the run fails at the end if a single frame turns out
  to have come from it — an agent with a desktop of its own is the one
  shot that would argue against everything the rest of this shows.

  Two staleness guards, both learned the hard way
  ----------------------------------------------
  A frame cannot tell you which build it came from, so a compositor older
  than the sources under test is refused rather than warned about. So is
  a word-processor payload older than *its* sources: a rebuild breaks the
  link the registry serves through and the old app goes out silently,
  which is worse, because the run is green and the app in the picture is
  last night's.

  The private bus gets a runtime dir of its own
  ---------------------------------------------
  It has to. A session bus activates `xdg-document-portal`, which mounts
  a FUSE filesystem at `$XDG_RUNTIME_DIR/doc` — over the one the real
  portal is serving, if it inherits the real runtime dir. This run's
  teardown then unmounts the desktop's, and every flatpak on the machine
  fails to start until `xdg-document-portal` is restarted. Observed once,
  on this machine, during filming. `private_session_bus` gives the bus a
  runtime dir of this run's own, which is why it is reused rather than
  re-implemented here.
"""
from __future__ import annotations

import argparse
import base64
import importlib.util
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)


def _load(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(
        name, os.path.join(HERE, filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# The insert take is loaded first and `yc` is taken *from it*, rather
# than loaded again here. `exec_module` does not consult `sys.modules`,
# so loading `yc-demo.py` a second time would produce a second module
# object with its own `RECORDER`, `FAILURES` and `SESSIONS` — and half
# the calls in this file would be setting globals the other half never
# reads. One copy, shared.
ins = _load("yc_demo_insert", "yc-demo-insert.py")
yc = ins.yc
session_exit = _load("session_exit", "session_exit.py")

# How long after the code exists before xaver redeems it. Long enough to
# be somebody reading a short code off one screen and walking to
# another; short enough that nobody watching wonders whether it worked.
XAVER_DELAY_S = 8.0
# How long each excursion to xaver's screen lasts. Two of them, and both
# are an interruption of the screen the story is on, so they are short.
EXCURSION_S = 4.0
# Given to the network between the agent saying it applied something and
# the camera being pointed at the machine that has to have received it.
SYNC_GRACE_S = 3.0
# Held after the last scripted thing happens, before the take ends by
# itself. Room either side of the payoff rather than one frame of it.
DEFAULT_DWELL_S = 6.0
# The outside limit on one unattended take.
DEFAULT_DURATION_S = 900.0
# Longest one unchanged frame is shown for. Trimming is the one edit the
# composer makes, and it is why a narrated take cannot have it: see the
# note in `main` where the two are reconciled.
DEFAULT_MAX_HOLD_S = 1.2
# How often the watcher looks at anything. Every probe is a `state_dump`
# and blocks the session it is asked of — including, while recording,
# the session being filmed — so this is deliberately unhurried. Only one
# thing is ever polled per tick.
POLL_S = 0.9


def mark(title: str) -> None:
    """Charge the film's next stretch of seconds to `title`.

    `yc-demo.py` has had this since it had beats: `banner` lays a mark
    and `Recorder.beat_report` charges every captured frame to the mark
    it was taken after. This file had no beats to hang one off — its
    whole point is that a person drives and there is no step number —
    and so it has been the one rig that films a take and cannot say
    where the take's seconds went.

    Two things need that. A cut over budget needs to know which beat to
    take the seconds out of rather than which one somebody guesses is
    longest; the per-scene budgets in the header above were written
    without measuring and say so. And a narration recorded into the take
    needs a timing sheet to be written against, which is the same table
    read forwards.

    A mark is a point in the take, not a section somebody owns: two
    threads drive this film at once, so what a mark really answers is
    "what was on screen while this was happening".
    """
    yc.BEAT_MARKS.append((title, time.time()))

# What to tell him to write, per agent mode — and they are not
# interchangeable.
#
# The film's instruction is `yc.CROP_INSTRUCTION`: he is holding a
# photograph, and what he asks for takes two applications to answer.
# Composing that is a judgement, so it needs the model.
#
# `--no-agent-model` does not have one. Its floor matches the
# instruction's own words against the words the *apps* put on their
# operations, plus a handful of named recipes (`apps/agent/src/plan.rs`)
# — and given "make this square and put it in the open document" it
# finds no clear winner and answers, on the page, in front of
# everybody, that it cannot. Which is honest, is the right behaviour,
# and is not what anybody wants filmed.
#
# So that mode asks for the thing it can actually do, and the readback
# after it asserts *that* outcome rather than the crop's. One
# instruction per mode, and the mode decides both ends.
KEYWORD_INSTRUCTION = "Claude: turn the bullets into a table"

# What the film calls each session, under its name on the band. Quinten
# is whoever is running this; the band says so in the third person
# because that is what a caption is.
ROLES = {"quinten": "hosts the room", "xaver": "joined by invite"}


# --------------------------------------------------------------------
# one transient line at the bottom of the transcript
# --------------------------------------------------------------------


class Console:
    """`stdout` with a status line that anything else clears first.

    Three things write to this terminal at once — the watcher's counter,
    the assertions the scripted half prints as it goes, and under
    `--self-test` a second thread driving the owner's half. The counter
    is a `\\r` line with no newline on the end of it, so without this
    every one of those lands *appended* to it:

        23s  0 frames  xaver crosses the room    [PASS] xaver joined

    which is the transcript this rig exists to produce, made unreadable
    by the thing that was supposed to prove it was alive. So the status
    line is erased before any other write and re-drawn after it, and the
    lock makes that hold across the threads.
    """

    def __init__(self, stream) -> None:
        self._stream = stream
        self._pending = 0
        self._buf = ""
        self._lock = threading.Lock()

    def write(self, text: str) -> int:
        """Buffer until a line is whole, then emit the whole line.

        `print` writes its text and its newline as two separate calls, so
        locking each call individually still lets the other thread land
        its line in the middle of this one — which is the same
        interleaving one level down. Holding partial lines back until
        their newline arrives makes a line the unit, which is what a
        reader thinks it is anyway.
        """
        if not text:
            return 0
        with self._lock:
            self._buf += text
            if "\n" in self._buf:
                whole, _, self._buf = self._buf.rpartition("\n")
                self._erase()
                self._stream.write(whole + "\n")
                self._stream.flush()
        return len(text)

    def status(self, text: str) -> None:
        with self._lock:
            self._erase()
            self._stream.write(text)
            self._stream.flush()
            self._pending = len(text)

    def _erase(self) -> None:
        if self._pending:
            self._stream.write("\r" + " " * self._pending + "\r")
            self._pending = 0

    def flush(self) -> None:
        with self._lock:
            # Anything still without its newline goes out now: on the way
            # out, a half-written line is better than a lost one.
            if self._buf:
                self._erase()
                self._stream.write(self._buf)
                self._buf = ""
            self._stream.flush()

    def __getattr__(self, name):
        return getattr(self._stream, name)


# --------------------------------------------------------------------
# refusing to film the wrong build
# --------------------------------------------------------------------


def sources_newer_than(artifact: str, roots: tuple[str, ...]) -> list[str]:
    """Every `.rs` under `roots` modified after `artifact` was built.

    A screenshot cannot tell you which build it came from, so this is the
    only place the question can be asked. Newer source is a refusal
    rather than a warning: a warning in a long transcript is a warning
    nobody reads, and this rig has silently filmed a stale build twice.
    """
    try:
        built = os.path.getmtime(artifact)
    except OSError:
        return []
    newer: list[str] = []
    for root in roots:
        for dirpath, _dirs, files in os.walk(root):
            for f in files:
                if not f.endswith(".rs"):
                    continue
                path = os.path.join(dirpath, f)
                try:
                    if os.path.getmtime(path) > built:
                        newer.append(os.path.relpath(path, REPO))
                except OSError:
                    pass
    return sorted(newer)


def refuse_stale(what: str, artifact: str, roots: tuple[str, ...],
                 remedy: str) -> bool:
    newer = sources_newer_than(artifact, roots)
    if not newer:
        return False
    print(f"\nrefusing to record: {what} ({os.path.relpath(artifact, REPO)}) "
          f"is older than {len(newer)} source file(s) under test")
    for f in newer[:8]:
        print(f"    {f}")
    if len(newer) > 8:
        print(f"    ... and {len(newer) - 8} more")
    print(f"\n  {remedy}")
    print("  (or --allow-stale to film it anyway)")
    return True


# --------------------------------------------------------------------
# the invitation, as a fact rather than as a beat
# --------------------------------------------------------------------


# How long to keep watching for a second invitation before admitting the
# agent on xaver's. Long enough that minting two in one visit to the panel
# always lands inside it, short enough that a run where only one was minted
# is not left waiting on a gesture nobody is going to make.
AGENT_INVITE_GRACE_S = 25.0


#: Where finished film goes: `films/` at the repository root, git-ignored.
#:
#: Deliberately not under `target/`. That directory is cargo's, and its
#: whole contract is that everything in it can be deleted and rebuilt — a
#: recorded take cannot. It costs ten minutes of machine time, it is not
#: reproducible byte-for-byte, and one has already been lost to a routine
#: clean. A run's scratch (state, logs, frames) stays in `target/`, because
#: that genuinely is regenerable; only the thing somebody watches moves.
FILMS_DIR = os.path.join(REPO, "films")


def decode_invite(token: str) -> dict | None:
    """What a token says it admits its holder to, or None.

    `make_invite_token` is url-safe base64 of a JSON body carrying the
    room, the owner's peer id and an expiry. Decoding it here is not a
    shortcut around anything — every field is public to whoever holds
    the token — and it buys two things: a shape check strong enough that
    an unrelated clipboard is never mistaken for an invitation, and a
    room to point the agent at that came out of the code *he minted*
    rather than out of a session the harness read behind his back.
    """
    token = (token or "").strip()
    if len(token) < 64:
        return None
    try:
        raw = base64.urlsafe_b64decode(token + "=" * (-len(token) % 4))
        body = json.loads(raw)
    except Exception:  # noqa: BLE001 — anything that is not a token
        return None
    if isinstance(body, dict) and body.get("room_id"):
        return body
    return None


# --------------------------------------------------------------------
# the take
# --------------------------------------------------------------------


class Live:
    """The scripted half, driven by what the unscripted half does.

    Each thing below fires once, when the condition it is watching for
    becomes true, and never because a previous one finished. That is the
    whole difference between this and the nine-beat film: there is no
    order to remember, because there is no order to get wrong.
    """

    def __init__(self, quinten, xaver, agent, args, shots: str,
                 recorder) -> None:
        self.q = quinten
        self.x = xaver
        self.a = agent
        self.args = args
        self.shots = shots
        self.recorder = recorder

        self.token: str | None = None
        self.token_at = 0.0
        # A second invitation, made out to the agent. Separate because an
        # invitation names its holder: `invite_from_the_panel` types a
        # person's name and that name "is what the room will call whoever
        # redeems this token". One token therefore carries one name, and a
        # second participant redeeming it arrives as `peer@<n>` -- which is
        # exactly what the payoff frame showed, on the agent's own chip,
        # beside a `xaver` that resolved.
        self.agent_token: str | None = None
        self.agent_token_at = 0.0
        self.room: str | None = None
        self.xaver_in = False
        #: Whether xaver's gesture actually went out. Distinct from
        #: the agent having acted: the agent can only act on what
        #: reached it, so a run where this is False and the agent
        #: did something is a run where something else provoked it.
        self.instruction_sent = False
        self.agent_session_in = False
        self.wp_seen = False
        self.agent_proc: subprocess.Popen | None = None
        self.agent_tail = None
        self.agent_log = os.path.join(shots, "agent.log")
        self.agent_applied = False
        self.agent_done_at = 0.0
        self.visited_after_agent = False
        #: So the mark charging the agent's latency is laid once, at the
        #: first tick that finds it working, rather than on every poll.
        self.marked_agent_wait = False
        self.watched_it_think = False
        self.finished = ""

        self.started = time.time()
        self.deadline = self.started + args.duration
        self.waiting = "the desktop to settle"
        # Everything scripted hangs off the invitation, because
        # everything scripted is somebody else being in the room. Said in
        # the counter rather than only in the docstring: a run where he
        # opens three apps and wonders why nothing is happening is a run
        # where the terminal should already have told him.
        self.first_wait = "he mints an invite — all of this waits on it"
        self._last_status = 0.0
        self.stop = threading.Event()

    # -- talking to whoever is watching the terminal -----------------

    def status(self, force: bool = False) -> None:
        """One line, rewritten in place, saying what is being waited for.

        A harness that prints nothing for two minutes and a harness that
        has hung look identical, and only one of them is worth waiting
        out.
        """
        now = time.time()
        if not force and now - self._last_status < 1.0:
            return
        self._last_status = now
        frames = len(self.recorder.frames) if self.recorder else 0
        line = (f"  {now - self.started:5.0f}s  {frames:6d} frames   "
                f"{self.waiting:<48.48s} ({self.deadline - now:4.0f}s left)")
        if isinstance(sys.stdout, Console):
            sys.stdout.status(line)
        else:
            sys.stdout.write("\r" + line)
            sys.stdout.flush()

    def say(self, text: str) -> None:
        print(f"  {text}", flush=True)

    # -- an excursion to the other machine ---------------------------

    def visit_xaver(self, why: str, frame_app: str | None) -> None:
        """Put xaver's screen on camera for a few seconds, and come back.

        Only while recording: with no camera there is nobody to show it
        to, and every second spent here is a second the person driving is
        not being filmed.
        """
        if self.recorder is None or self.args.excursion <= 0 or self.x is None:
            return
        self.say(f"cutting to xaver — {why}")
        yc.cut("xaver")
        try:
            if frame_app:
                yc.frame_one_window(self.x, frame_app)
            else:
                yc.show_collaborators(self.x)
            yc.read_hold(self.args.excursion)
            self.x.snapshot(os.path.join(
                self.shots,
                f"xaver-{int(time.time() - self.started):04d}.png"))
            if not frame_app:
                yc.close_collaborators(self.x)
        except Exception as exc:  # noqa: BLE001
            # An excursion is a shot, not a claim. Losing one costs the
            # cut a few seconds of the other machine; taking the run down
            # for it would lose the whole film, most of which is of a
            # person who was not doing anything wrong.
            yc.gap(f"the excursion to xaver's screen failed "
                   f"({type(exc).__name__}: {exc})")
        finally:
            yc.cut("quinten")

    # -- the things that fire ----------------------------------------

    def wait_for_his_screen(self, budget: float = 12.0) -> None:
        """Hold until the room's applications have reached xaver.

        Asks his own session what it is showing rather than assuming the
        replication is done: a window group on his side is evidence the
        workspace crossed, and it is the thing the excursion is for.
        """
        if self.x is None:
            return
        due = time.time() + budget
        seen = 0
        while time.time() < due and not self.stop.is_set():
            try:
                groups = yc.tab_groups_here(self.x)
            except Exception:  # noqa: BLE001
                groups = []
            seen = sum(len(g.get("tabs", [])) for g in groups)
            if seen:
                self.say(f"his screen has {seen} window(s) from the room")
                return
            self.waiting = "the room's windows reaching xaver's screen"
            self.stop.wait(0.5)
        yc.gap(f"xaver's screen still showed nothing after {budget:.0f}s, "
               f"so the cut to it is of an empty desktop")

    def look_for_the_invitation(self) -> None:
        """Whether he has minted one yet.

        The card first, because while it is up that is where the token
        provably is and the harness can say who it was made out to. The
        clipboard second, for the run where he read the code and
        dismissed the card between two polls — the token is on the
        clipboard because the button he pressed put it there, which is
        the same fact arriving by a different route.

        Either way what crosses to the other machine is what his own
        gesture produced. Nothing here mints anything.
        """
        card = self.q.chrome().get("peer_sync") or {}
        minted = card.get("invite_token")
        source = "the card he is looking at"
        if not decode_invite(minted or ""):
            minted = self.q.clipboard_read()
            source = "his system clipboard"
        body = decode_invite(minted or "")
        if not body:
            return
        found = (minted or "").strip()

        # The second invitation is recognised by being a different token,
        # not by counting polls: a person mints the two whenever they get
        # to it, and the card holds the most recent one. Anything already
        # seen is the same card being looked at twice.
        if self.token is not None:
            if found == self.token or found == self.agent_token:
                return
            self.agent_token = found
            self.agent_token_at = time.time()
            who2 = card.get("invite_for")
            self.say("a second invitation exists"
                     + (f", made out to {who2!r}" if who2 else ""))
            return

        self.token = found
        self.token_at = time.time()
        self.room = str(body["room_id"])
        who = card.get("invite_for")
        self.say(f"an invitation exists, from {source}"
                 + (f", made out to {who!r}" if who else ""))
        self.say(f"  it admits its holder to room {self.room}")
        if self.x is not None:
            self.say(f"  xaver reads it and crosses the room — "
                     f"{self.args.xaver_delay:.0f}s")

    def bring_xaver_in(self) -> None:
        """Redeem the invitation on the other machine.

        Off camera, and then cut to. The redemption itself is a person
        pasting a string into a dialog, which is not a shot; his screen
        *once he is in the room* is, and it is the only reason to cross.
        """
        with yc.off_camera("xaver redeems the invitation on his own machine"):
            self.xaver_in = bool(yc.join_room(self.x, self.token, via="panel"))
        yc.claim("xaver joined the room quinten opened", self.xaver_in, True)
        if self.xaver_in:
            yc.claim("and his session holds the room the token named",
                     self.x.active_workspace()["room"], self.room)
            # Not before his screen has something on it.
            #
            # The excursion exists to show that the room reached him, and
            # a cut taken the instant he joins shows an empty desktop --
            # which argues the opposite. The applications are opened
            # before anybody is invited now, so what this waits for is
            # them arriving on *his* machine, which is the claim: the
            # workspace is the shared thing, so joining it brings what is
            # in it. Bounded, because a cut to a bare screen is a poor
            # shot and a stalled take is no shot at all.
            self.wait_for_his_screen()
            mark("cut to xaver: he is in the room")
            self.visit_xaver("he is in the room now", None)

    def bring_the_agent_session_in(self) -> None:
        """Admit the session the agent speaks through, by address.

        Its **own** invitation, not xaver's. A token is a bearer
        credential for one room, so reusing one does admit a second
        participant — but an invitation also carries the name the room
        will call whoever redeems it, and that name is held on the host
        until they do. Two participants on one token means the second has
        no name to be given, and arrives as `peer@<n>`. On camera that
        was the agent's own chip, unresolved, beside a `xaver` that
        resolved from the invitation he had been sent.

        So the agent is invited the way a person is, which is also what
        the beat claims: an agent admitted by holding a capability in the
        same list, marked as an agent so nobody has to infer which is
        which. A member with no name is a weaker version of that shot.

        The route still differs, and that difference is still the point —
        an agent has no top bar to click, so it raises the same card by
        its stable address and redeems with the card's own buttons.

        The grants are the other half. An agent is a subject in the
        permission model rather than a peer, so what it may do is a verb
        on a scope; this asks the production gate before and after rather
        than asserting anything about it.
        """
        with yc.off_camera("the agent's session admitted to the room, and "
                           "what it is allowed to do — a question to the "
                           "authority gate, which has no picture"):
            if self.agent_token is None:
                # Live, this is a person who minted one invitation and
                # not two. The run continues on the shared token rather
                # than stalling, because a nameless agent is a blemish
                # and a halted take is a lost afternoon -- but it is said
                # out loud, because it is the difference between the
                # frame the beat wants and the frame that shipped.
                yc.gap("only one invitation was minted, so the agent "
                       "redeems xaver's; expect its chip to read "
                       "peer@<n> rather than a name")
            joined = yc.join_room(
                self.a, self.agent_token or self.token, via="action")
            yc.claim("the agent's session joined the room", joined, True)
            if joined:
                yc.claim("and it holds the room the token named",
                         self.a.active_workspace()["room"], self.room)

            def may_write() -> bool:
                answer = self.q.check_action(
                    yc.AGENT_SUBJECT, yc.WORD_PROCESSOR,
                    "SpliceText").get("answer") or {}
                return bool(answer.get("permitted"))

            yc.claim("the agent may not write to the word processor yet",
                     may_write(), False)
            # One grant per application it is given leave to work in. The
            # film needs two -- it is told to square a photograph and put it
            # in the open document -- and naming them separately is the
            # claim: an agent handed a photograph cannot thereby start
            # anything it likes, because a grant names an app, not the room.
            refused = [scope for scope in yc.AGENT_SCOPES
                       if "err" in self.q.grant(yc.AGENT_SUBJECT, scope,
                                                "write")]
            yc.claim(f"quinten's grants of write on {len(yc.AGENT_SCOPES)} "
                     f"named apps were accepted", not refused, True)
            if refused:
                yc.gap(f"the room refused: {', '.join(refused)}")
            yc.claim("and now it may — the same question, the same gate",
                     bool(yc.wait_for(may_write, timeout=yc.SYNC_TIMEOUT_S)),
                     True)
            admitted = self.q.grant(f"person:{self.a.user}", "workspace",
                                    "write")
            yc.claim("the agent's session is admitted to the room as an editor",
                     "err" not in admitted, True)
            # And admitted as what it is. The grant above names the
            # *session*, which the sync path needs before this peer may
            # take part at all, and names it a person because that is
            # the only kind a session has. Membership under an agent
            # principal is the separate statement, and it is the one
            # this beat makes out loud — the room's capability list is
            # where "this member is an agent" is recorded, and every
            # surface that marks one reads it there.
            as_agent = self.q.grant(yc.AGENT_SUBJECT, "workspace", "write")
            yc.claim("and admitted to the room as an agent, not as a "
                     "second person",
                     "err" not in as_agent, True)
        self.agent_session_in = True

    def word_processor_is_open(self) -> bool:
        """Whether he has a word processor on screen yet.

        Read off the frame counters rather than off the tree: a tile
        exists and is laid out seconds before it presents anything, and
        an agent pointed at a document that does not exist waits for a
        document that does not exist.
        """
        for f in self.q.foreign_frames():
            if (f.get("app_id") == yc.WORD_PROCESSOR
                    and f.get("frame_seq", 0) > 0):
                return True
        return False

    def start_the_agent(self) -> None:
        """Launch `qos-agent` and point it at the room. Nothing else.

        It is given no instruction and no schedule. It connects to its
        own session's broker, announces itself in the room, and then
        waits — indefinitely, which is what `--await-document-ms 0` is
        for — until the document contains a paragraph addressed to it by
        name. Whether that ever happens is his business, not this file's.
        """
        binary = yc.agent_binary()
        if binary is None:
            yc.gap("qos-agent is not built, so nothing here can act on an "
                   "instruction written to it — cargo build --profile demo "
                   "-p agent --bin qos-agent")
            self.args.agent = False
            return
        argv = [binary, "--workspace", str(self.room), "--name", "Claude",
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
                "--socket", self.a.collab_json_sock,
                "--once", "--pace-ms", str(self.args.agent_pace),
                # A script wants a bound on this; a person at a keyboard
                # is the bound here.
                "--await-document-ms", "0"]
        if not self.args.agent_model:
            argv.append("--no-model")
        self.agent_proc = subprocess.Popen(
            argv, stdout=open(self.agent_log, "w"),
            stderr=subprocess.STDOUT, cwd=REPO,
            # The one process in this file that is spawned outside
            # `Session.start`, so it is the one the session's own pin
            # does not reach: without this it inherits the operator's
            # environment wholesale. It writes into the document that is
            # on camera, and it links the word processor — whose
            # catalogue it would resolve in the operator's language.
            env=yc.pin_locale(dict(os.environ)),
            preexec_fn=yc.die_with_parent(enabled=not yc.KEEP_SESSIONS))
        if not yc.KEEP_SESSIONS:
            yc.track(self.agent_proc)
        self.agent_tail = yc.LogTail(self.agent_log)
        decides = "a model decides" if self.args.agent_model \
            else "keyword vocabulary, no network"
        self.say(f"Claude is in the room, watching the document "
                 f"(pid {self.agent_proc.pid}, {decides})")
        self.say("Claude is watching; xaver is about to give it something "
                 "to do")
        mark("cut to xaver: his instruction")
        self.his_instruction()

    def his_instruction(self) -> bool:
        """Xaver picks the image and says what he wants done with it.

        The argument of the whole film, and the reason it is *his* hand
        rather than yours. He is not the host, he did not open any of
        these applications, and he is on another machine -- and he can
        still take an object out of the shared workspace and hand it to
        an agent, because what he sends is the object's address rather
        than a description of where it was sitting on his screen. The
        agent then works on the thing he pointed at, in a document he
        never opened, and the result arrives on both screens.

        Right-click rather than a chord, because a chord is a thing you
        have to be told about and a right-click is a thing you try. That
        gesture reached the context menu and stopped there until now:
        `open_remark_for_selection` was written for it, said so in its
        own documentation, and was never called by it.

        Driven on camera during an excursion, so the audience sees who
        did it. Returns False rather than raising -- a take that reaches
        the end and names what was missing is worth more than one that
        dies at its best moment.
        """
        if self.x is None or not self.xaver_in:
            yc.gap("xaver never joined, so nobody was there to give the "
                   "instruction")
            return False
        self.say("cutting to xaver — he has the image")
        yc.cut("xaver")
        try:
            # Aimed at a pixel, not at a name. A client application's
            # widgets are not in the compositor's target registry -- that
            # registry walks the shell's own tree -- so the earlier call
            # here asked for `file_row:harbour.png` and could never once
            # have resolved. It had never run, because the compose surface
            # had never opened far enough for anyone to find out.
            if not self.args.instruct:
                # Handed over rather than driven. His window is already
                # in front -- the cut raised it -- so the only thing
                # missing is knowing what to do with it, and that is a
                # line of text rather than a mechanism.
                self.say("")
                self.say("  " + "-" * 58)
                self.say("  XAVER IS YOURS. His window is in front.")
                self.say("")
                self.say("    right-click the image in Files, then write:")
                self.say(f"      {self.args.instruction}")
                self.say("")
                self.say("    the rig picks it up from here and watches "
                         "for the answer")
                self.say("  " + "-" * 58)
                self.say("")
                self.instruction_sent = True
                # Not claimed. Nothing here observed him write anything,
                # and a claim that reports a person's gesture it never saw
                # is worth less than no claim at all.
                yc.gap("the instruction is being written by hand, so this "
                       "run does not assert that it was")
                return True

            tile = yc.best_tile(self.x, yc.FILES_APP, yc.VIEWPORT)
            if tile is None:
                yc.gap("xaver has no file manager window on screen, so "
                       "there is no image for him to right-click")
                return False
            yc.frame_one_window(self.x, yc.FILES_APP)
            # Re-read after framing: it moves and resizes the group, so a
            # rect taken before is the placement of a window that is no
            # longer there, and every press below is aimed at a pixel.
            tile = yc.best_tile(self.x, yc.FILES_APP, yc.VIEWPORT) or tile
            log = yc.LogTail(self.x.log_path)
            log.catch_up()
            said = yc.say_something_about(
                self.x, log, tuple(tile["rect"]), yc.PHOTO_NAME,
                yc.expected_listing(self.x.home), self.args.instruction,
                self.shots, "his-instruction")
        finally:
            # The camera stays on xaver. He asked for it, so he sees it
            # land -- on a machine where he owns nothing and opened
            # nothing, which is a stronger claim than watching a result
            # arrive where it was already going.
            self.say("staying on xaver — he watches it happen")
        self.instruction_sent = said
        yc.claim("the instruction came from the person holding the thing, "
                 "on another machine", said, True)
        return said

    #: The agent, having changed the document: `applied splice text
    #: (+76 -29)`. On a word boundary, because the same log carries
    #: prose about a change being held *unapplied* while history it was
    #: built on is missing — and a plain substring match reported that
    #: sentence as the agent acting, in a run where it had not.
    APPLIED = re.compile(r"\bapplied .*")

    #: The agent, having taken the summons (`session.rs`, "spoken to in
    #: the room"). Between this and the first `applied` is the model
    #: deciding, which is most of the wait and all of the dead air.
    SPOKEN_TO = re.compile(r"\bspoken to in the room\b")

    def check_the_agent(self) -> None:
        """Whether it has done anything, and whether it has finished."""
        fresh = self.agent_tail.read() if self.agent_tail else ""
        if not self.watched_it_think and self.SPOKEN_TO.search(fresh):
            self.watched_it_think = True
            self.watch_it_think()
        hit = self.APPLIED.search(fresh)
        if hit and not self.agent_applied:
            self.agent_applied = True
            self.say(f"Claude acted — {hit.group(0).strip()[:110]}")
        if self.agent_proc.poll() is None:
            return
        code = self.agent_proc.returncode
        self.agent_done_at = time.time()
        if code == 0:
            yc.claim("the agent answered an instruction written to it "
                     "in the document", self.agent_applied, True)
            self.read_back_what_it_wrote()
        else:
            tail = ""
            try:
                with open(self.agent_log) as fh:
                    tail = fh.read()[-300:]
            except OSError:
                pass
            yc.gap(f"qos-agent exited {code} — {tail.strip()[-200:]}")
        self.say(f"Claude has finished (exit {code}); giving the network "
                 f"{SYNC_GRACE_S:.0f}s before looking at xaver's screen")

    def watch_it_think(self) -> None:
        """Point the camera at where the agent is narrating itself.

        Between the instruction landing and the first change appearing
        is the model deciding, and it has been measured at some
        twenty-eight seconds. Nothing about that will get shorter, so
        the only question is what is on screen while it happens -- and
        the answer already exists: the agent posts its own state into
        chat as each thing actually happens, `Taken` then `Waiting` then
        `Doing` then `Done` or `Refused` (`apps/chat/src/model.rs`),
        written never to claim something is happening now that is not.
        A wait filled by the agent saying what it is waiting for is not
        dead air.

        Done here rather than earlier because this is the first moment
        it is safe: the trigger is the agent's own log saying it was
        spoken to, so the remark has been written and sent. Reframing
        any sooner would move a window out from under whoever was still
        right-clicking in it.

        The camera is already on xaver -- `his_instruction` cut to him
        and stays -- so this is a reframe within his strip and not
        another cut. Losing it costs the shot its subject and nothing
        else, so a failure here is reported and the take goes on.
        """
        if self.recorder is None or self.x is None:
            return
        try:
            if yc.best_tile(self.x, yc.CHAT_APP, yc.VIEWPORT) is None:
                yc.gap("no chat window is on screen, so the agent's account "
                       "of what it is doing has nowhere to be read and the "
                       "wait is filmed as a still page")
                return
            yc.frame_one_window(self.x, yc.CHAT_APP)
            self.say("framing chat — Claude says what it is doing there as "
                     "it does it")
            self.x.snapshot(os.path.join(
                self.shots,
                f"claude-thinking-{int(time.time() - self.started):04d}.png"))
        except Exception as exc:  # noqa: BLE001
            yc.gap(f"the camera could not be pointed at chat for the agent's "
                   f"working period ({type(exc).__name__}: {exc})")

    def read_back_what_it_wrote(self) -> None:
        """Ask the document what is in it, rather than the log what happened.

        The claim above is the one that has been passing while the frame
        showed the opposite, and the reason is worth stating where it can
        be seen: it comes off `applied .*` in the agent's log, and a
        refusal is applied too. It is one `SpliceText` writing a sentence
        over the instruction, logged in exactly the same words as the
        table. So "the agent carried out an instruction" was true of a
        refusal, true of a wall of jargon, and true of the best sentence
        in the film, and a sweep reporting that every claim held told
        nobody which of the three had been recorded.

        This asks the app instead, over the same protocol the scripted
        half's readback uses, and asserts on the two things that tell
        them apart: the outcome the instruction asked for is on the page,
        and no refusal is. Both, because either alone can mislead -- a
        page can carry the notes it always carried while the agent
        refused beside them, and an agent that wrote nothing at all
        refuses nothing.

        Which outcome that is comes from `args.outcome`, set beside the
        instruction so the two cannot drift apart.
        """
        if self.args.outcome == "crop":
            self.read_back_the_crop()
        else:
            self.read_back_the_run_sheet()

    def read_back_the_crop(self) -> None:
        """The picture, and whether it is square.

        The instruction asked for two things and this asserts both.

        *In the open document.* A document carries a picture's BLAKE3
        address and a small descriptor, never its bytes -- so the
        reference is the whole of what replicates, and it is what the
        other machine draws from. `picture_references` reads it back out
        of the document the app answers with.

        *Square.* A fact about the picture rather than about the page, so
        it is read off the picture: the bytes that address resolves to,
        out of the content store beside the document, measured at their
        own IHDR. Nothing here needs a before-and-after -- the
        photograph he pointed at is 512x320, so a square picture on that
        page is one somebody made, and an uncropped one fails this by
        being the shape it always was. That also keeps the readback to
        the single attach the run already makes, rather than putting a
        second phantom collaborator on screen mid-take.

        A run that cannot reach the bytes says so. "The shape could not
        be established" and "the picture is not square" are different
        answers, and reporting the first as either is the defect this
        rig keeps finding in itself.
        """
        reader = None
        try:
            reader = yc.DocumentReader(self.q.collab_json_sock, str(self.room),
                                       record=None)
            doc, _ = reader.await_document(
                "image/", 30.0,
                predicate=lambda blob, _n: bool(yc.picture_references(blob)))
        except OSError as e:
            yc.gap(f"the document could not be read back ({e}), so what is "
                   f"on the page was not established")
            return
        except yc.CannotDecode as e:
            yc.gap(f"{e} -- so what is on the page was not established, "
                   f"which is not the same as the page being empty")
            return
        finally:
            if reader is not None:
                reader.close()
        if doc is None:
            yc.gap("no app answered with the document, so what is on the "
                   "page was not established")
            return
        try:
            pictures = yc.picture_references(doc)
        except yc.CannotDecode as e:
            yc.gap(f"{e} -- so whether a picture reached the page was not "
                   f"established")
            return
        yc.claim("the picture the agent worked on is in the open document",
                 bool(pictures), True)
        if not pictures:
            self.no_refusal(doc)
            return

        was = yc.png_size(self.the_photograph()) or (0, 0)
        measured: list[tuple[str, int, int]] = []
        for hash_ in pictures:
            raw = yc.blob_beside_the_document(self.q, str(self.room),
                                              yc.WORD_PROCESSOR, hash_)
            size = yc.png_size(raw) if raw else None
            if size is None:
                yc.gap(f"the page references a picture ({hash_[:12]}) whose "
                       f"bytes are not reachable from here, so its shape was "
                       f"not established -- which is not the same as its not "
                       f"being square")
                continue
            measured.append((hash_, size[0], size[1]))
            yc.note(f"the page holds {size[0]}x{size[1]} at {hash_[:12]}")
        if measured:
            yc.claim(f"and it is square, where the photograph he pointed at "
                     f"is {was[0]}x{was[1]}",
                     any(w == h for _h, w, h in measured), True)
        self.no_refusal(doc)

    def the_photograph(self) -> bytes:
        """The file he right-clicked, as it sits in his home."""
        for home in (self.x.home if self.x else None, self.q.home):
            if not home:
                continue
            try:
                with open(os.path.join(home, yc.PHOTO_NAME), "rb") as fh:
                    return fh.read()
            except OSError:
                continue
        return b""

    def read_back_the_run_sheet(self) -> None:
        """The table, and the bullets' own words in its cells."""
        reader = None
        try:
            reader = yc.DocumentReader(self.q.collab_json_sock, str(self.room),
                                       record=None)
            # In a *cell*, not merely on the page. The phrase looked for
            # is one of the bullets the agent was asked to replace, so
            # the loose test is answered by the agent's own input: it
            # passed on a run where the agent refused outright and built
            # no table, because the bullet saying it was still sitting
            # there untouched.
            doc, carried = reader.await_document(
                yc.AGENT_BULLET_PROOF, 20.0,
                predicate=yc.carries_text_in_a_table)
        except OSError as e:
            yc.gap(f"the document could not be read back ({e}), so what is "
                   f"on the page was not established")
            return
        except yc.CannotDecode as e:
            yc.gap(f"{e} -- so what is on the page was not established, "
                   f"which is not the same as the page being empty")
            return
        finally:
            if reader is not None:
                reader.close()
        if doc is None:
            yc.gap("no app answered with the document, so what is on the "
                   "page was not established")
            return
        yc.claim("and the run sheet's own content is in the table's cells",
                 carried, True)
        self.no_refusal(doc)

    def no_refusal(self, doc: bytes) -> None:
        """That what landed on the page is the work and not a decline.

        An address is how the agent dispatches; a refusal quoting one is
        the frame this looks for.

        Decoded, not grepped. This was a byte search, and a byte search
        of a saved document answers "no refusal" for every document
        including one that is nothing but a refusal: Automerge
        DEFLATE-compresses each column, so the sentence is in the
        document and not in its bytes. The claim beside it proves the
        point -- the agent's own output is likewise unfindable as bytes
        -- and a check that cannot fail is not a check.
        """
        try:
            refused = yc.carries_text(doc, "I can't")
        except yc.CannotDecode as e:
            yc.gap(f"{e} -- so whether the agent refused was not established")
            return
        yc.claim("and it is not a refusal", refused, False)
        if refused:
            yc.gap("the agent answered in the document, and what it wrote "
                   "was that it could not do what was asked -- which is "
                   "honest, and is not the beat")

    # -- the loop ----------------------------------------------------

    def tick(self) -> None:
        """One pass. At most one thing is polled or done per call, so the
        session being filmed is never asked two questions at once."""
        now = time.time()

        if self.token is None:
            self.waiting = self.first_wait
            self.look_for_the_invitation()
            return

        if self.x is not None and not self.xaver_in:
            due = self.token_at + self.args.xaver_delay
            if now < due:
                self.waiting = f"xaver crosses the room ({due - now:.0f}s)"
                return
            self.waiting = "xaver is pasting the code and pressing Join"
            self.status(force=True)
            self.bring_xaver_in()
            return

        if self.args.agent and not self.agent_session_in:
            # Keep looking for the agent's own invitation. The first
            # token stops the poll above, so without this the second one
            # is never seen and the agent falls back to xaver's --
            # arriving nameless, which is the defect this is here to
            # remove. Bounded, because a person may simply not mint a
            # second one and the run should continue rather than stall.
            if (self.agent_token is None
                    and now < self.token_at + AGENT_INVITE_GRACE_S):
                self.waiting = "a second invitation, made out to Claude"
                self.look_for_the_invitation()
                return
            self.waiting = "Claude's session is being admitted to the room"
            self.status(force=True)
            self.bring_the_agent_session_in()
            return

        if self.args.agent and self.agent_proc is None:
            if not self.wp_seen:
                self.waiting = "he opens the word processor"
                self.wp_seen = self.word_processor_is_open()
                return
            self.start_the_agent()
            return

        if self.agent_proc is not None and not self.agent_done_at:
            if not self.marked_agent_wait:
                self.marked_agent_wait = True
                mark("Claude reads the room, decides, and works")
            self.waiting = ("Claude is working" if self.agent_applied
                            else "Claude waits for a line addressed to it")
            self.check_the_agent()
            return

        if self.agent_done_at:
            if now < self.agent_done_at + SYNC_GRACE_S:
                self.waiting = "the edit crossing to the other machine"
                return
            if not self.visited_after_agent:
                self.visited_after_agent = True
                mark("cut to xaver: the agent's work reached him")
                self.visit_xaver("the agent's work is on his screen too",
                                 yc.WORD_PROCESSOR)
                if self.args.stop_after == "agent":
                    self.say(f"holding {self.args.dwell:.0f}s, then stopping")
                    self.stop.wait(self.args.dwell)
                    self.finished = "the agent's work landed on both machines"
                    return

        self.waiting = "you — Ctrl-C in this terminal ends the take"

    def run(self) -> str:
        while not self.stop.is_set() and not self.finished:
            if time.time() > self.deadline:
                self.finished = f"the {self.args.duration:.0f}s cap"
                break
            try:
                self.tick()
            except Exception as exc:  # noqa: BLE001 — the report is the product
                import traceback
                traceback.print_exc()
                yc.FAILURES.append(f"{type(exc).__name__}: {exc}")
                self.finished = f"{type(exc).__name__}: {exc}"
                break
            self.status()
            self.stop.wait(POLL_S)
        if isinstance(sys.stdout, Console):
            sys.stdout.status("")
        return self.finished or "you stopped it"


# --------------------------------------------------------------------
# the half a person does, done without one
# --------------------------------------------------------------------


def the_owners_half(q, shots: str, instruction: str,
                    stop: threading.Event) -> None:
    """Everything you would do, driven from here.

    Wrapped, because it runs on a thread of its own and a thread that
    dies of an exception dies silently: the watcher then sits there
    saying it is waiting for a word processor nobody is ever going to
    open, and the run reads as the reactive half hanging rather than as
    the driving half having crashed. That is exactly what a
    `NameError` in one of `yc-demo.py`'s framing helpers did to the
    first self-test of this file.
    """
    try:
        _the_owners_half(q, shots, instruction, stop)
    except Exception as exc:  # noqa: BLE001 — a dead thread must be loud
        import traceback
        traceback.print_exc()
        yc.FAILURES.append(
            f"--self-test: the owner's half stopped at "
            f"{type(exc).__name__}: {exc}")


def _the_owners_half(q, shots: str, instruction: str,
                     stop: threading.Event) -> None:
    """The steps themselves.

    For `--self-test`: the reactive halves cannot be proven without
    something to react to, and asking a person to sit through the rig
    being tested is how a rig stops being tested. Every step below is
    the same production path the nine-beat film drives, and none of it
    says anything about how easy any of it is with a mouse.

    The drag is deliberately not here. It is the one gesture that is
    really about a hand, and `yc-demo-insert.py --self-test` already
    drives and proves it end to end.
    """
    stop.wait(3.0)
    if stop.is_set():
        return
    print("\n  [ --self-test: driving the owner's half ]", flush=True)
    mark("an empty desktop")

    # The applications first, then the people. That is the order somebody
    # actually sets a workspace up in, and the earlier cut had it backwards:
    # one application, then two invitations into a room with nothing in it,
    # then everything else arriving at 39s with no explanation of why.
    #
    # Each one opened the way a person opens it -- the key, the box, a few
    # letters, the row. Opening by address is a window appearing out of
    # nothing, and a viewer has to be told what happened; the search shows
    # it, and shows that the desktop has one way in rather than a menu per
    # application. Which route an app took used to depend on whether it
    # happened to be in the catalogue an empty query builds, so half the
    # film explained itself and half did not, for a reason nothing on
    # screen conveyed.
    #
    # Chat, Files, the word processor: the order they would be reached for,
    # conversation first because that is where the work is agreed.
    # The first search is the one a viewer has to read — the box
    # appearing, being looked at, being typed into. The rest are the same
    # gesture by somebody who now knows where it is, which is both faster
    # and truer; four full-length searches in a row spends a minute of a
    # hundred-second film on opening things.
    mark("chat opens, through the search")
    chat = q.launch(yc.CHAT_APP, search="chat", via="palette", look=1.0)
    yc.claim("chat opened", chat is not None, True)
    stop.wait(0.4)

    mark("Files opens")
    files = q.launch(yc.FILES_APP, search="files", via="palette", look=0.45)
    yc.claim("Files opened", files is not None, True)
    stop.wait(0.4)

    # Its own group, or the placement rule tabs it in behind what is
    # already there and the strip has one window where the scene wants two.
    mark("the word processor opens")
    q.place_next_window(yc.WORD_PROCESSOR)
    wp = q.launch(yc.WORD_PROCESSOR, search="word", via="palette", look=0.45)
    yc.claim("the word processor opened", wp is not None, True)
    if wp is None:
        return
    stop.wait(0.6)

    # The image editor is deliberately *not* in this run. Three
    # applications is the set somebody opens to start work; a fourth
    # opened before anybody has said why is a fourth identical search,
    # and the opening was already a third of the film. It arrives with
    # the scene that needs it -- somebody opening a photograph in order
    # to look at it, which is a reason a viewer can see.
    photos = None

    # Framing is a nicety and is not what any of this is proving, so a
    # helper that throws costs a tidy strip rather than the take.
    try:
        yc.frame_windows(q, [yc.FILES_APP, yc.WORD_PROCESSOR], yc.VIEWPORT)
    except Exception as exc:  # noqa: BLE001
        yc.gap(f"the two windows could not be framed ({type(exc).__name__}: "
               f"{exc}); the strip is left where it is")
    stop.wait(0.8)

    # Now the people, into a workspace that already has something in it.
    mark("inviting xaver")
    yc.show_collaborators(q)
    token = yc.invite_from_the_panel(q, "xaver", shots, "live-invite")
    yc.claim("an invitation was minted from the panel", bool(token), True)

    # Two, because an invitation names its holder and the agent is a
    # member like any other. Raised again on an empty field -- the
    # composer used to keep the previous answer, so a second invitation
    # came out addressed to "XaverXaver".
    mark("inviting Claude — the same card, a different name")
    yc.show_collaborators(q)
    agent_token = yc.invite_from_the_panel(q, "Claude", shots, "live-invite-2")
    yc.claim("a second invitation was minted, for the agent",
             bool(agent_token), True)
    # Both, then distinct -- in that order, and never `a != b` on its own.
    # Written as one comparison it passed while the first invitation had
    # failed to mint at all: `token` was None, so "different from xaver's"
    # was true of anything. The run reported two invitations, the room got
    # one, and it was made out to the agent, which is why the agent turned
    # up as the first collaborator. A check that cannot fail when its own
    # premise is broken is the exact fault this file keeps finding in
    # other people's tests.
    yc.claim("both invitations exist",
             bool(token) and bool(agent_token), True)
    yc.claim("and they are two different invitations",
             bool(token) and bool(agent_token) and agent_token != token,
             True)
    yc.close_collaborators(q)
    stop.wait(0.8)

    # Four applications, two in frame, and two of them made one window.
    #
    # What earns its seconds here is the tab bar holding *different
    # applications*. A tab everywhere else belongs to its app, because the
    # app draws it; here it is a frame primitive the compositor owns, so a
    # file manager and a document and a photograph sit on one bar. That is
    # the one gesture in the film a viewer cannot mistake for a browser.
    mark("the image editor, and the applications sharing one tab bar")
    extras: dict[str, dict] = {}
    if photos is None:
        photos = q.launch(yc.IMAGE_EDITOR,
                          search=yc.STRIP_SEARCH.get(yc.IMAGE_EDITOR,
                                                     "photos"),
                          via="palette", look=0.45)
        yc.claim("the image editor opened", photos is not None, True)
    if photos is not None:
        extras[yc.IMAGE_EDITOR] = photos
    if chat is not None:
        extras[yc.CHAT_APP] = chat
    if extras:
        try:
            yc.beat_strip(q, extras, wp, shots)
        except Exception as exc:  # noqa: BLE001
            yc.gap(f"the strip scene did not complete "
                   f"({type(exc).__name__}: {exc})")
    stop.wait(0.8)

    # A run of real list items, a line of prose that disagrees with them,
    # and then the instruction — in that order, because the agent
    # converts the bullets immediately above the paragraph addressed to
    # it.
    mark("he writes the notes, and the line addressed to Claude")
    typed = yc.type_into_the_document(
        q, wp, yc.AGENT_PROSE + yc.AGENT_BULLETS + instruction + "\n")
    yc.claim("the document was written into", typed, True)
    print("\n  [ --self-test: the owner's half is done ]", flush=True)


# --------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--shell", default=None,
                    help="compositor binary (default target/demo/shell, then "
                         "target/debug/shell). Must honour QOS_TEST_CONTROL, "
                         "which --release does not")
    ap.add_argument("--out", default=os.path.join(yc.TARGET,
                                                  "yc-demo-live"),
                    help="run directory for state, logs and frames")
    ap.add_argument("--video", default=None,
                    help="where the clip goes (default: a timestamped file "
                         "under films/ at the top of the checkout, which is "
                         "gitignored and is not under target/ — cargo may "
                         "delete that, and a take cannot be rebuilt)")
    ap.add_argument("--allow-stale", action="store_true",
                    help="record against a compositor or an app payload "
                         "older than its own sources (default: refuse)")

    ap.add_argument("--xaver-delay", type=float, default=XAVER_DELAY_S,
                    metavar="S",
                    help=f"seconds between the code existing and xaver "
                         f"redeeming it (default {XAVER_DELAY_S:.0f})")
    ap.add_argument("--no-xaver", dest="xaver", action="store_false",
                    help="nobody joins. Leaves you a single machine with an "
                         "agent in the room")
    ap.add_argument("--no-instruction", dest="instruct",
                    action="store_false",
                    help="do not drive xaver's half. His window is raised "
                         "when the film reaches him and left to you: you "
                         "right-click the image and write the instruction "
                         "yourself, on his machine, and the rig goes back "
                         "to watching for the agent to answer it. Everything "
                         "mechanical still happens -- his session is "
                         "admitted, the agent is started -- so what you take "
                         "over is the gesture the beat is about and nothing "
                         "else")
    ap.add_argument("--no-agent", dest="agent", action="store_false",
                    help="no agent in the room at all")
    ap.add_argument("--no-agent-model", dest="agent_model",
                    action="store_false",
                    help="decide by keyword instead of asking a model: "
                         "reproducible and off the network, at the price of "
                         "a three-word vocabulary. Ask it for a table, a "
                         "caption or a heading in that mode — anything else "
                         "and it says so, on the page, in the shot")
    ap.add_argument("--agent-pace", type=int, default=250, metavar="MS",
                    help="pause between the agent's edits, so its caret can "
                         "be watched moving")

    ap.add_argument("--excursion", type=float, default=EXCURSION_S, metavar="S",
                    help=f"seconds spent on xaver's screen per cut, twice "
                         f"(default {EXCURSION_S:.0f}; 0 never leaves yours)")
    ap.add_argument("--stop-after", choices=("agent", "never"),
                    default="agent",
                    help="'agent' ends the take once the agent's work has "
                         "been shown landing on the other machine; 'never' "
                         "runs until you press Ctrl-C or --duration expires")
    ap.add_argument("--dwell", type=float, default=DEFAULT_DWELL_S, metavar="S",
                    help="seconds held after the last scripted thing happens")
    ap.add_argument("--duration", type=float, default=DEFAULT_DURATION_S,
                    metavar="S", help="outside limit on one take")
    ap.add_argument("--countdown", type=int, default=5, metavar="N",
                    help="seconds counted down before the camera rolls")

    ap.add_argument("--fetch-word-processor", action="store_true",
                    help="take the word processor off your machine and serve "
                         "it from a signed registry on loopback, so opening "
                         "it fetches, verifies and installs it on camera. "
                         "Off by default: it is a real network beat with a "
                         "real wait in it, and a live take is not the place "
                         "to discover that")
    ap.add_argument("--fps", type=int, default=30, metavar="N",
                    help="container frame rate; the capture rate is whatever "
                         "the compositor sustained")
    ap.add_argument("--narrate", nargs="?", const="default", default=None,
                    metavar="SOURCE",
                    help="record spoken narration into the take, from a "
                         "PulseAudio/PipeWire source (default: whatever the "
                         "desktop's sound settings select). The microphone "
                         "opens on the same statement that starts the frames "
                         "and closes on the one that stops them, so the two "
                         "tracks share a start — and they share a length "
                         "only while nothing shortens the picture, so this "
                         "sets --max-hold to 0 and refuses a take that asks "
                         "for both. The boot bumper is prepended separately "
                         "and stays silent.")
    ap.add_argument("--max-hold", type=float, default=None, metavar="S",
                    help=f"longest one unchanged frame is shown for, which "
                         f"trims the stretches where you were thinking "
                         f"(default {DEFAULT_MAX_HOLD_S}; never shows a "
                         f"frame for longer than it was live). 0 keeps the "
                         f"take on the wall clock, which is what --narrate "
                         f"needs")
    ap.add_argument("--windowed", action="store_true",
                    help="do not open the window at the full work area")
    ap.add_argument("--no-record", dest="record", action="store_false",
                    help="drive it without a camera")
    ap.add_argument("--keep", action="store_true",
                    help="leave the sessions running afterwards")

    ap.add_argument("--dry-run", action="store_true",
                    help="stage everything headless, check it and stop. No "
                         "window, no X server, no recording — so it runs "
                         "over ssh and cannot put anything on your screen")
    ap.add_argument("--self-test", action="store_true",
                    help="drive the owner's half too: open Files, mint an "
                         "invitation, open the word processor, write the "
                         "instruction. Proves the whole rig with no hands")
    args = ap.parse_args()

    # The instruction the terminal will tell him to write — and, under
    # `--self-test`, the one that actually gets written. It is only ever
    # advice: the agent reads whatever is in the document, and he can
    # write anything he likes.
    #
    # `args.outcome` travels with it and decides what is read back
    # afterwards. The two were separate once: whichever instruction was
    # picked, the readback asserted a table with the run sheet's words
    # in its cells — so a take of the crop would have failed a claim
    # about a beat it was not filming, and a take of the run sheet
    # passed one about an instruction it had not been given.
    if args.agent_model:
        args.instruction, args.outcome = yc.CROP_INSTRUCTION, "crop"
    else:
        args.instruction, args.outcome = KEYWORD_INSTRUCTION, "table"

    # ---- narration and trimming cannot both be on ------------------
    #
    # The microphone opens on the statement that starts the frames and
    # closes on the one that stops them, so the two tracks begin
    # together. They only *stay* together while nothing shortens the
    # picture. `--max-hold` does: `Recorder._timeline` collapses runs of
    # identical frames and reports what it took out as `trimmed`, so the
    # video comes out shorter than the wall clock while the live mic
    # track is still exactly as long as the take was. The voice then
    # drifts progressively later across the film — silently, discovered
    # after filming, which is the failure this whole rig is built to
    # stop making.
    #
    # So it is not a warning. Left to its default, narrating turns the
    # trim off; asked for explicitly, the run refuses rather than
    # quietly overriding what somebody typed.
    asked_for_hold = args.max_hold is not None
    if args.max_hold is None:
        args.max_hold = DEFAULT_MAX_HOLD_S
    if args.narrate is not None and args.max_hold > 0:
        if asked_for_hold:
            print(f"--narrate and --max-hold {args.max_hold:g} cannot both "
                  f"be on.\n"
                  f"  Trimming shortens the picture and cannot shorten the "
                  f"microphone, so the\n"
                  f"  voice drifts further behind the further into the take "
                  f"you get.\n"
                  f"  Drop --max-hold (it becomes 0 for a narrated take), or "
                  f"drop --narrate\n"
                  f"  and lay the voice over afterwards.", file=sys.stderr)
            return 2
        args.max_hold = 0.0
        print("narrating, so --max-hold is 0 for this take: the picture "
              "stays on the wall\nclock, because trimming it would leave "
              "the voice behind.")

    # ---- preflight -------------------------------------------------
    #
    # Every one of these makes a take pointless rather than merely worse,
    # and every one is cheaper to say now than after a countdown.
    shell_bin = args.shell
    if shell_bin is None:
        for profile in ("demo", "debug"):
            candidate = os.path.join(yc.TARGET, profile, "shell")
            if os.path.exists(candidate):
                shell_bin = candidate
                break
    if not shell_bin or not os.path.exists(shell_bin):
        print("no compositor binary. Build one:\n"
              "  cargo build --profile demo -p shell --bin shell --bin qos-control",
              file=sys.stderr)
        return 2
    # The decoder the document claims are made with. Said here rather
    # than discovered at the end, because a take without it is a take
    # whose two best claims cannot be answered either way -- and, before
    # the harness learned to tell "absent" from "unreadable", a take that
    # reported the agent's finished run sheet as an empty page. Two
    # minutes of filming to find that out is two minutes too many.
    if yc.DOC_TEXT is None:
        print("no qos-doc-text, so nothing could be asked of the documents "
              "this films. Build it:\n"
              "  cargo build --profile demo -p qdoc --bin qos-doc-text",
              file=sys.stderr)
        return 2
    if not args.dry_run and not os.environ.get("DISPLAY"):
        print("no DISPLAY. This take is a window you drive, which needs an X "
              "server (XWayland counts). Run it from a terminal on your "
              "desktop, or --dry-run to check the staging without one.",
              file=sys.stderr)
        return 2
    missing = [] if args.dry_run else [
        t for t in ("xdotool", "wmctrl", "ffmpeg", "ffprobe")
        if shutil.which(t) is None]
    if missing:
        print(f"missing: {', '.join(missing)}", file=sys.stderr)
        return 2

    entries = yc.registry_entries(os.path.join(yc.MACHINE_APPS, "registry.toml"))
    live = yc.registry_entries(os.path.join(yc.LIVE_APPS, "registry.toml"))
    # Every app the live registry serves, not just the one this rig was
    # first written around.
    #
    # Only the word processor was promoted, so `demo-apps-live.sh` built
    # and packaged Photos, the registry served it, and the session was
    # never told — its palette answered `rows: []` for "photos" and the
    # scene that needs four applications quietly filmed three. Nothing
    # failed: the app was absent rather than broken, which is the harder
    # kind to notice.
    for app_id, entry in live.items():
        entries[app_id] = entry

    live_payload = os.path.join(yc.LIVE_APPS, "payloads", yc.WORD_PROCESSOR,
                                "bin", "word-processor")
    if yc.WORD_PROCESSOR in live:
        wp_source = f"{yc.LIVE_APPS} (built from this tree)"
    elif yc.WORD_PROCESSOR in entries:
        wp_source = f"{yc.MACHINE_APPS} (the machine's installed payload)"
    else:
        print("no word processor is installed anywhere this can see, so "
              "there is nothing for anybody to write in.", file=sys.stderr)
        return 2

    if not args.allow_stale:
        if refuse_stale("the compositor", shell_bin,
                        (os.path.join(REPO, "shell", "src"),
                         os.path.join(REPO, "ui", "src")),
                        "cargo build --profile demo -p shell --bin shell "
                        "--bin qos-control"):
            return 2
        # The payload is the worse of the two. A rebuild replaces the
        # binary the registry was serving through and the old app goes
        # out with no error anywhere — the run is green and the app in
        # the picture is the previous one.
        #
        # Every live payload, for that reason -- it does not stop being
        # true of the second app. The binary and the source directory are
        # both the app id's last segment: `dev.qos.photos` is built from
        # `apps/photos` into `bin/photos`, the same shape the packaging
        # script uses to build them.
        for app_id in sorted(live):
            leaf = app_id.rsplit(".", 1)[-1]
            payload = os.path.join(yc.LIVE_APPS, "payloads", app_id,
                                   "bin", leaf)
            if not os.path.exists(payload):
                continue
            if refuse_stale(
                    f"the {leaf} payload", payload,
                    (os.path.join(REPO, "apps", leaf, "src"),
                     os.path.join(REPO, "ui", "src")),
                    "scripts/demo-apps-live.sh"):
                return 2

    # ---- staging ---------------------------------------------------
    args.out = os.path.abspath(args.out)
    # Clean, every run. A session persists its workspaces, so a re-run
    # over the last one's state opens on rooms that are already joined
    # and an invitation that has already been redeemed.
    shutil.rmtree(args.out, ignore_errors=True)
    # The homes live outside `--out`, so they are cleared separately: one
    # left from a previous run carries that run's edits into this one's
    # first frame.
    shutil.rmtree(yc.HOMES_ROOT, ignore_errors=True)
    shots = os.path.join(args.out, "shots")
    os.makedirs(shots, exist_ok=True)

    def tool(name: str) -> str | None:
        for profile in ("demo", "debug"):
            path = os.path.join(yc.TARGET, profile, name)
            if os.path.exists(path):
                return path
        return None

    tools = {var: path for var, name in (
        ("QOS_CATALOGUE_BIN", "qos-catalogue"),
        ("QOS_INSTALL_BIN", "qos-install"),
        ("QOS_LAUNCHER_BIN", "qos-launcher"),
    ) if (path := tool(name))}
    if "QOS_LAUNCHER_BIN" not in tools:
        print("no qos-launcher in target/{demo,debug}: every app launch is "
              "refused without a sandbox to spawn into, so there would be "
              "nothing on screen. Build it:\n"
              "  cargo build --profile demo -p launcher --bin qos-launcher",
              file=sys.stderr)
        return 2

    runtime = os.environ.get("XDG_RUNTIME_DIR") or "/tmp"
    pid = os.getpid()
    yc.KEEP_SESSIONS = args.keep

    wanted = [("q", "quinten")]
    if args.xaver:
        wanted.append(("x", "xaver"))
    if args.agent:
        wanted.append(("a", "Claude"))

    sessions = []
    for short, name in wanted:
        run_dir = os.path.join(args.out, name)
        apps_dir = os.path.join(run_dir, "qos-apps")
        # His machine is the one that does not have it, when the fetch is
        # the beat being shown. Everybody else has it either way.
        yc.compose_registry(
            apps_dir, entries,
            drop=(yc.WORD_PROCESSOR,) if (name == "quinten"
                                          and args.fetch_word_processor) else ())
        # The people get windows; the agent never does. That asymmetry
        # is the mechanical half of "the agent is never on camera" — the
        # other half is asserted against the frames at the end.
        #
        # Xaver's window is not there to be driven. Beats 4 and 5 are
        # shot from his screen and driven over his control socket, so
        # without a window the person narrating is talking over footage
        # they cannot see. It costs nothing to show them: the recording
        # is taken with a `snapshot` op off each session's own
        # framebuffer, never off the screen, so what is stacked in front
        # of what changes no pixel of the film.
        cls = (yc.Session if (args.dry_run or name == "Claude")
               else ins.WindowedSession)
        sessions.append(cls(
            name, name, run_dir,
            # Short: AF_UNIX caps sun_path near 108 bytes, and a run
            # directory deep in a worktree blows past it on its own.
            os.path.join(runtime, f"qos-lv-{short}-{pid}.sock"),
            shell_bin, yc.RECORD_SIZE,
            os.path.join(runtime, f"qlv{short}{pid}"),
            yc.stage_home(os.path.join(yc.HOMES_ROOT, name)),
            apps_dir, tools))

    by_name = {s.name: s for s in sessions}
    quinten = by_name["quinten"]
    xaver = by_name.get("xaver")
    agent = by_name.get("Claude")
    yc.SESSIONS = sessions

    if not args.dry_run:
        area = ins.work_area() or (yc.VIDEO_W, yc.VIDEO_H)
        if args.windowed:
            quinten.window_size = None
        else:
            # The video's own size where the screen has room for it, so
            # the frames go in 1:1 and the band sits in empty canvas
            # below them rather than over the bottom of a window. Where
            # it does not, the largest the window manager will grant
            # without shrinking it — a shrink is a resize, and a resize
            # with a frame in flight takes this backend down.
            quinten.window_size = (f"{min(area[0], yc.VIDEO_W)}"
                                   f"x{min(area[1], yc.VIDEO_H - yc.BAND_H)}")

    print(f"compositor:     {shell_bin}")
    print(f"word processor: {wp_source}")
    print(f"run directory:  {args.out}")
    print("sessions:       " + ", ".join(
        s.name + (" (a window you drive)"
                  if isinstance(s, ins.WindowedSession) else " (headless)")
        for s in sessions))
    for var, path in sorted(tools.items()):
        print(f"  {var:20} {path}")

    # A real take goes where the other demo videos go. A rehearsal's
    # goes in its own run directory: it is proof the composer ran, not
    # something anybody is going to watch, and it should not have to be
    # told apart from a take afterwards.
    video_path = args.video or (
        os.path.join(args.out, "rehearsal.mp4") if args.dry_run else
        os.path.join(FILMS_DIR,
                     f"yc-live-{time.strftime('%Y%m%d-%H%M%S')}.mp4"))
    video_path = os.path.abspath(video_path)
    os.makedirs(os.path.dirname(video_path), exist_ok=True)
    record_dir = os.path.join(args.out, "recording")

    terminal = None if args.dry_run else ins.active_window()
    recorder = None
    wid = None
    bus = None
    registry_ctx = None
    outcome = "not reached"
    session_ends: list[tuple[str, str]] = []
    rc = 0
    hands = None
    real_stdout = None
    stop = threading.Event()

    # First Ctrl-C ends the take and composes what was filmed; a second
    # abandons it. A run that has to be killed twice is a run whose video
    # was on disk the whole time and never got written.
    def interrupt(_sig, _frame):
        if stop.is_set():
            raise KeyboardInterrupt
        stop.set()
        sys.stdout.write("\r" + " " * 100 + "\r")
        print("\n  stopping — the clip will be composed from what was "
              "filmed. Ctrl-C again to abandon it.", flush=True)

    signal.signal(signal.SIGINT, interrupt)

    try:
        # ---- start -------------------------------------------------
        print("\nsetting up:")
        if not args.dry_run:
            raised = ins.private_session_bus(quinten.dir, quinten.runtime_dir)
            if raised:
                quinten.bus_address, bus = raised
                print(f"  private session bus    pid {bus.pid}, with a "
                      f"runtime dir of its own")
            else:
                print("  ! no dbus-daemon; this session shares your "
                      "desktop's bus and its notifications land on it")

        # Cuts now raise the window of whoever the film cut to, so the
        # person narrating always sees the screen being recorded.
        if not args.dry_run:
            yc.SHOW_ON_SCREEN = ins.raise_window

        quinten.start()
        quinten.wait_ready(timeout=180.0)
        if not args.dry_run:
            wid = ins.find_window(quinten.proc.pid)
            quinten.wid = wid
            if wid is None:
                raise RuntimeError(
                    "the compositor started but mapped no X window, so there "
                    f"is nothing for you to drive; see {quinten.log_path}")
            # Out of sight while the rest is staged. It comes back — at
            # the size it already is, never resized — when the camera
            # rolls.
            ins.hide_window(wid)
        print(f"  your session up        pid {quinten.proc.pid}"
              + (f", window {ins.window_size(wid)}" if wid else
                 f", headless at {quinten.size}"))

        # Whatever the window manager actually granted, measured off a
        # render rather than off the X server's opinion — and then given
        # to the other sessions, so every frame in the recording is the
        # same size. The concat demuxer refuses a track whose picture
        # size changes part way through, and a cut to a screen a pixel
        # different is exactly that.
        probe = quinten.snapshot(os.path.join(args.out, "probe.png"))
        rendered = ins.png_size(probe) or (yc.VIDEO_W, yc.VIDEO_H - yc.BAND_H)
        size = f"{rendered[0]}x{rendered[1]}"
        print(f"  rendering at           {size}"
              + ("" if rendered == (yc.VIDEO_W, yc.VIDEO_H - yc.BAND_H) else
                 f"  (not {yc.RECORD_SIZE}: frames are scaled and "
                 f"letterboxed into the video)"))
        for s in sessions:
            if s is not quinten:
                # The agent's screen is never filmed, so it does not have
                # to be the size of the film.
                #
                # Everything with a surface costs GPU memory in proportion
                # to its area, and this machine's window onto that memory
                # is 16404MB of GTT -- shared with the desktop, which
                # holds about 1.9GB before qOS starts. A run reached
                # 15643MB and the machine stopped dead: no GPU reset, no
                # hung task, no OOM, no panic, because an allocation that
                # can neither be served nor fail takes the box with it.
                #
                # Measured at about 600-950MB per client, and a full run
                # is thirteen of them. Nothing here leaks -- an idle
                # application held flat for sixty seconds and a hundred
                # and twenty snapshots moved nothing -- it simply does not
                # fit. So the one session whose pixels nobody will ever
                # see renders small, and the two that are on camera keep
                # every pixel they need.
                s.size = (yc.AGENT_SIZE if s.name == yc.NEVER_ON_CAMERA
                          else size)
                # A window opens at the size it is asked for and is never
                # resized, so the one to ask for is the size quinten was
                # actually granted rather than the one requested of it.
                # Frames from the two have to agree exactly: the concat
                # demuxer refuses a track whose picture size changes part
                # way through, and a cut to a screen one pixel different
                # is exactly that.
                if isinstance(s, ins.WindowedSession):
                    s.window_size = size
                s.start()
        for s in sessions:
            if s is not quinten:
                s.wait_ready()
                if not isinstance(s, ins.WindowedSession):
                    print(f"  {s.name + ' up':<22} pid {s.proc.pid} "
                          f"(headless, no window anywhere)")
                    continue
                s.wid = ins.find_window(s.proc.pid)
                if s.wid:
                    # Staged out of sight. It comes back on its own when
                    # the film reaches the beat that is shot on it.
                    ins.hide_window(s.wid)
                # Measured off a render, not off the X server's opinion,
                # and checked now rather than discovered in the take.
                got = ins.png_size(
                    s.snapshot(os.path.join(args.out, f"probe-{s.name}.png")))
                if got and f"{got[0]}x{got[1]}" != size:
                    raise RuntimeError(
                        f"{s.name} renders {got[0]}x{got[1]} but quinten "
                        f"renders {size}. Cutting between them would change "
                        f"the picture size part way through the film, which "
                        f"the concat demuxer refuses. Close other windows so "
                        f"the window manager can grant both the same size, "
                        f"or run with --windowed.")
                print(f"  {s.name + ' up':<22} pid {s.proc.pid} "
                      f"(window {size}, staged out of sight)")
        yc.VIEWPORT = rendered

        # The strip's proportions, once, before anything is filmed. Every
        # later move then only scrolls it, which reads as navigation;
        # resizing it on camera reads as a fault.
        for s in sessions:
            yc.establish_strip(s)
        # So the word processor opens beside Files rather than as a tab
        # behind it. Staging, not driving: it changes where his next
        # window lands, never whether or when he opens one.
        quinten.place_next_window(yc.WORD_PROCESSOR)

        if terminal:
            ins.raise_window(terminal)

        # ---- the registry, when the fetch is the beat ---------------
        served = None
        if args.fetch_word_processor:
            # Served out of a copy, never out of `target/`. A rebuild
            # while this is running replaces the binary underneath the
            # registry and the old app is served with no error anywhere.
            served = os.path.join(args.out, "served", "word-processor")
            os.makedirs(os.path.dirname(served), exist_ok=True)
            if os.path.exists(live_payload):
                shutil.copy2(live_payload, served)
                print(f"  payload copied         {os.path.getsize(served)} "
                      f"bytes into the run directory — a rebuild cannot "
                      f"change what this take serves")
            registry_ctx = yc.Registry(
                os.path.join(yc.TARGET, "debug", "examples",
                             "serve-test-registry"),
                served,
                os.path.join(quinten.dir, "store-config"),
                os.path.join(args.out, "registry.log"))

        registry = registry_ctx.__enter__() if registry_ctx else None
        if args.fetch_word_processor:
            if registry is None:
                print("  ! no local registry, and the word processor was "
                      "taken off your machine for the fetch beat — you would "
                      "have nothing to open. Re-run without "
                      "--fetch-word-processor.")
            else:
                print(f"  registry               {registry.base_url}")

        # ---- dry run -----------------------------------------------
        if args.dry_run:
            staged = quinten.snapshot(os.path.join(args.out, "staged.png"))
            print("\nstaged, headless:")
            for s in sessions:
                ws = s.active_workspace()
                print(f"  {s.name:8} room {ws['room']}  owner {ws['owner']}  "
                      f"{len(yc.installed_ids(s.apps_dir))} apps installed")
            print(f"  home listed as         "
                  f"{yc.expected_listing(quinten.home)}")
            print(f"  frame                  {staged}")
            if not args.self_test:
                print("\nready: the sessions are up, each has a room of its "
                      "own and the apps are installed. Nothing reactive is "
                      "exercised here — add --self-test to drive the owner's "
                      "half, watch the other two answer it, and get a "
                      "rehearsal clip out of it.")
                outcome = "dry-run"
                return 0
            print("\n  --dry-run --self-test: the whole rig, headless. Both "
                  "halves run, the camera rolls on the same snapshot op a "
                  "take uses, and nothing appears on anybody's screen.")

        # ---- roll ---------------------------------------------------
        #
        # One page saying what the window is for and what will answer
        # each thing done in it. Printed while the terminal is still in
        # front, because in a moment it will not be.
        if not args.dry_run:
            print("\nthe window is yours. There is no order to keep:")
            print("  - open Files, open the collaborators panel, press "
                  "Invite and name somebody")
            if args.xaver:
                print(f"      -> xaver redeems it "
                      f"{args.xaver_delay:.0f}s later, on his own machine")
            print("  - open the word processor and write in it; drag "
                  f"{yc.PHOTO_NAME} out of Files onto the page")
            if args.agent:
                print(f'      -> once it is open, Claude joins the room and '
                      f'waits. Write "{args.instruction}"')
                print("         under a run of bullets and it will do it"
                      + ("" if args.agent_model else
                         " (keyword mode: it only knows"
                         " table, caption and heading)"))
            if args.stop_after == "agent" and args.agent:
                print("  - the take ends by itself once that has landed on "
                      "the other machine")
            else:
                print("  - Ctrl-C in this terminal ends the take and writes "
                      "the clip")
            if args.stop_after == "agent" and not args.agent:
                print("  ! --stop-after agent with no agent in the room: "
                      "nothing can end this take but Ctrl-C or the "
                      f"{args.duration:.0f}s cap")
            if args.self_test:
                print("\n  --self-test: no hands. Your half is driven from "
                      "here so the reactive half has something real to "
                      "answer.")
            ins.countdown(max(0, args.countdown),
                          "Open Files, invite somebody from the "
                          "collaborators panel, open the word processor.",
                          quiet=args.self_test)
            if wid is not None:
                ins.raise_window(wid)

        # A dry run films too, when it is driving. The camera reads the
        # same `snapshot` op off a headless session as off a windowed
        # one, so recording here is what makes `--dry-run --self-test` a
        # rehearsal of the whole rig rather than of its first half: the
        # cuts, the two excursions to xaver's screen and the compose all
        # run, on a machine with no X server and with nothing on
        # anybody's desktop. Without it, three of the things most likely
        # to be broken on the night are the three never exercised.
        if args.record:
            os.makedirs(record_dir, exist_ok=True)
            recorder = yc.Recorder(sessions, record_dir, interval=0.0,
                                   dwell=0.0, cut_dwell=1.0,
                                   max_hold=args.max_hold,
                                   narrate=args.narrate)
            yc.RECORDER = recorder
            # The band's captions. Set on the shared module so the
            # composer draws them, and only for the two sessions that are
            # ever on camera.
            yc.ROLES.update(ROLES)
            recorder.start(quinten.name)

        # From here on three things write to this terminal at once, so
        # the transient counter learns to get out of their way. Put back
        # in the `finally` below, before anything reports.
        real_stdout, sys.stdout = sys.stdout, Console(sys.stdout)

        if args.self_test:
            hands = threading.Thread(
                target=the_owners_half,
                args=(quinten, shots, args.instruction, stop),
                name="hands", daemon=True)
            hands.start()

        print()
        live = Live(quinten, xaver, agent, args, shots, recorder)
        live.stop = stop
        outcome = live.run()

    except KeyboardInterrupt:
        outcome = "abandoned"
        rc = 1
    except Exception as exc:  # noqa: BLE001 — the report is the product
        import traceback
        traceback.print_exc()
        yc.FAILURES.append(f"{type(exc).__name__}: {exc}")
        outcome = f"{type(exc).__name__}: {exc}"
        rc = 1
    finally:
        if real_stdout is not None:
            sys.stdout.flush()
            sys.stdout = real_stdout
        stop.set()
        if hands is not None:
            hands.join(timeout=15)
        # How each session's compositor ended, read before teardown: after
        # `stop()` every status is this script's own SIGTERM, and the
        # question worth answering is whether the compositor was already
        # gone and why. A lost control socket looks the same whether the
        # session was closed on purpose or crashed, and reporting them
        # identically is how an application crash was once read as a
        # compositor crash.
        session_ends = [(sn.name, session_exit.how_it_ended(sn.proc))
                        for sn in sessions]
        # Before teardown either way: the capture thread talks to the
        # sessions, and `--keep` is about the sessions, not the camera.
        if recorder is not None:
            recorder.stop()
        if registry_ctx is not None:
            registry_ctx.__exit__(None, None, None)
        if args.keep:
            print("\n--keep: sessions left running.")
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
            if bus is not None and bus.poll() is None:
                bus.terminate()
        if terminal:
            ins.raise_window(terminal)

    # ---- the clip ---------------------------------------------------
    video = None
    if recorder is not None and recorder.frames:
        recorder.report()
        # Where the take's seconds went. Two threads lay marks, so they
        # are sorted before they are read as edges: a reactive mark and
        # an owner mark can land microseconds apart, and an out-of-order
        # edge charges a whole beat to its neighbour without saying so.
        yc.BEAT_MARKS.sort(key=lambda m: m[1])
        recorder.beat_report(yc.BEAT_MARKS, args.fps)
        # The claim the whole thing rests on, checked against the frames
        # rather than trusted to the cuts. `cut` refuses the name, but
        # `cut` is not the only way the camera could come to point there,
        # and this is the check that still holds when somebody adds
        # another way.
        strays = recorder.frames_from(yc.NEVER_ON_CAMERA)
        if strays:
            yc.FAILURES.append(
                f"{strays} frame(s) came from {yc.NEVER_ON_CAMERA}'s session "
                f"— an agent's desktop is the one thing this must not show")
        else:
            print(f"  no frame came from {yc.NEVER_ON_CAMERA}'s session: what "
                  f"it did is in the film only where the people it did it "
                  f"with can see it")
        # The mechanical half of the same claim, and stated as what it
        # actually forbids: *nobody but the person driving* gets a
        # window. Not "quinten has one" — a rehearsal is headless
        # throughout and would fail an assertion written that way, which
        # is a failure about the harness rather than about the film.
        intruders = [s.name for s in sessions
                     if isinstance(s, ins.WindowedSession)
                     and s.name == "Claude"]
        if intruders:
            yc.FAILURES.append(
                f"a window was opened for {intruders} — an agent with a "
                f"desktop is the wrong claim about what it is")
        first = ins.png_size(os.path.join(record_dir, recorder.frames[0][1]))
        source_size = f"{first[0]}x{first[1]}" if first else yc.RECORD_SIZE
        os.makedirs(os.path.dirname(video_path), exist_ok=True)
        try:
            print(f"\ncomposing {video_path}")
            video = recorder.compose(video_path, args.fps, source_size)
        except Exception as exc:  # noqa: BLE001
            yc.FAILURES.append(f"recording: {type(exc).__name__}: {exc}")
            print(f"  composing failed: {exc}", file=sys.stderr)
            print(f"  the frames are still on disk: {record_dir}/frames",
                  file=sys.stderr)
        for e in recorder.errors:
            yc.FAILURES.append(f"recording: {e}")

    print("\n" + "=" * 70)
    print(f"the take ended because: {outcome}")
    for name, how in session_ends:
        print(f"  {name}'s compositor {how}")
    if yc.GAPS:
        print(f"{len(yc.GAPS)} thing(s) this run could not verify:")
        for g in yc.GAPS:
            print(f"  - {g}")
    if yc.FAILURES:
        rc = 1
        print(f"{len(yc.FAILURES)} claim(s) did not hold:")
        for f in yc.FAILURES:
            print(f"  - {f}")
    elif outcome != "abandoned":
        print("every claim the scripted half made held.")
    print(f"screenshots: {shots}")
    if video:
        print(f"video:       {video['path']}")
        print(f"             {video['width']}x{video['height']} "
              f"{video['codec']} {video['fps']}  {video['duration']:.1f}s  "
              f"{video['frames']} frames  "
              f"{video['bytes'] / 1024 / 1024:.1f} MiB")
        mean_ms, worst_ms = recorder.snapshot_ms()
        print(f"             captured at {recorder.rate():.2f} frames/s; one "
              f"snapshot cost {mean_ms:.0f} ms mean / {worst_ms:.0f} ms worst")
        print("             every frame is a real render of that session, in "
              "capture order; none repeated, none interpolated")
    return rc


if __name__ == "__main__":
    sys.exit(main())
