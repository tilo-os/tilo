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

Budget: boot bumper ~3.2s + the driven take. **Hard cap 1:50**, which
is a requirement and not an aspiration. The bumper is not recorded here
and is gitignored, so a fresh checkout has no copy:
`films/qos-boot-bumper.mp4`, joined on the front by
`scripts/demo-assemble.py` (which `scripts/demo.sh` runs for you).

Two things this cut is built around, both of them findings from takes
rather than plans.

Setup used to cost forty seconds against fifteen planned, and over half
a budget spent on setup is where a demo loses the room. So the opening
is three applications opened the way a person opens them and two
invitations, and nothing else: no fourth search, no notes, no
instruction typed on a page.

Claude's working period is model latency and will not shrink. It is
dead air unless something else is on screen, and the answer is not to
interleave somebody else's typing over it -- that was tried, and it
made the film cut between two machines while both were mid-gesture.
What is on screen instead is the agent saying what it is doing, in
chat, in the room, as it does it.

MEASURED, `--dry-run --self-test`, 2026-08-25, on the first take of this
shape. 114.1s of picture for 114.4s of take -- the film runs on the wall
clock, so these are film seconds and take seconds alike. The take was
stopped by hand once the press failed, so the last row is a still page
and not the agent working, and beats 12-14 are not in it.

     3.0   an empty desktop            (uncharged in the take; the mark
                                        was laid after the hold, fixed)
    11.2   chat opens
     7.4   Files opens
     8.6   the word processor opens
     9.0   inviting xaver
     7.6   inviting Claude
     7.9 \
    19.4 -- the image editor, and the two of them in one tab bar
    11.5 /
     7.0   quinten types a few words
     8.2   the handover: the crossing, the cut, the hold
     8.4   he right-clicks the photograph
    24.3   [a still page: the press never landed, so nothing was asked]
   -----
    86.8   through the right-click, which is beats 2-11 entire

Two seconds worth somebody's attention. The tab-bar beat is 19.4s over
two marks and is the largest thing here that is not the agent -- it is
one script beat and it costs more than the three application launches
put together. And the three launches are 27.2s between them, which is a
quarter of the film spent opening things.

    BEAT 1  the machine boots                                   ~3.2s
        Not recorded here. `films/qos-boot-bumper.mp4`, concatenated
        in front by `scripts/demo-assemble.py`. Console, greeter,
        filled form, cut before the desktop -- see
        docs/boot-bumper-cut.md. Not under `target/`: it lived there
        once and a routine reclaim deleted it, and a recorded boot is
        the one asset here that cannot be regenerated on demand.

    BEAT 2  an empty desktop
        Nothing open, so the strip has somewhere to come from.

    BEATS 3-5  three applications, each opened through the search
        YOU     chat, then Files, then the word processor. Conversation
                first, because that is where work is agreed.
        SHOWS   the desktop has one way in rather than a menu per
                application. The first search is the one a viewer reads;
                the rest are the same gesture by somebody who now knows
                where it is.

    BEATS 6-7  inviting xaver, then inviting Claude
        YOU     the collaborators panel, twice, on the same card.
        SCRIPT  xaver redeems ~8s later, on his own machine, off camera.
                Claude's session redeems its own.
        SHOWS   an agent admitted the way a person is -- the same card,
                the same list, holding a capability -- and marked as an
                agent rather than arriving as a second person. Both
                invitations outstanding at once; this used to lose the
                second name (fixed).

    BEAT 8  the image editor opens, and two applications share one tab bar
        SHOWS   tabs are a frame primitive the compositor owns, so one
                tab bar holds *different applications*. Every other
                desktop has per-app tabs; this is the one gesture that
                cannot be mistaken for a browser.

    BEAT 9  quinten types a few words
        YOU     write anything. `yc.SHARED_TEXT` when this is driving.
        SHOWS   the document is shared. That is the entire claim, and a
                few words make it exactly as well as a page does -- so
                what used to be typed here (prose, a run of bullets and
                a paragraph addressed to the agent by name) is gone,
                along with the thirteen seconds of typing it cost.

    BEAT 10  cut to xaver -- and stay there
        The only cut in the film. It is taken on the frame his own app
        answers with quinten's words in it, so what a viewer sees when
        the camera arrives is a page he did not open carrying a sentence
        he did not type. Everything after this is his screen; the film
        does not come back.

        There were three cuts here once -- he joins, he gives the
        instruction, the work reaches him -- with quinten's screen
        between each pair. Six times over, a viewer who has never seen
        this system had to work out whose desktop they were looking at,
        for a story that changes hands exactly once.

    BEAT 11  he right-clicks harbour.png and says what he wants
        SCRIPT  xaver, in **Files**, on his own machine:
                    "Claude: make this square and put it in the open
                     document"
        SHOWS   an instruction that arrives as (text, address,
                capability) rather than as a description of a screen. He
                is not the host, he opened none of this, and he can
                still hand an object to an agent.

                It is a remark on the photograph and nothing types it
                anywhere. Addressing an agent by writing its name into a
                shared document is a thing this rig used to do, and it
                reads as a workaround for not having a way to ask.
        NEEDS   right-click compose in the file explorer  (landed)
                one instruction, several actions  (in flight)

    BEATS 12-13  Claude works, and it lands where he is watching
        SHOWS   the agent narrates itself in chat -- `Taken`, `Doing`,
                `Done` -- as each thing actually happens, so the wait is
                filled by the thing doing the waiting. Then it **opens
                the image editor itself**, crops the photograph, and the
                result travels into the word processor's document.

                This is the whole film. Not that an agent edited a
                document -- every word processor now ships something
                that does. That an agent *used the desktop*: it opened
                an application because it decided it needed one, acted
                on an object in it, and put the result in a second
                application, owning neither, having been granted the
                right to touch each. Two applications that contain no
                AI, addressable because the system made them so.

                The motion matters and is not decoration. An action that
                only appears as a changed document has to be narrated;
                one you can watch does not. It is also what makes
                attribution legible -- the object travels in the acting
                participant's colour, so it reads as *Claude did this*.
        NEEDS   the agent may open an application  (in flight)
                an agent's cross-app action is visible  (in flight)
                crop by address  (landed, proven to the pixels)

    BEAT 14  done, on the screen he was already watching
        No cut, because there is nowhere to cut from: he asked for it,
        and it arrived on a machine where he owns nothing and opened
        nothing. There is no more, on purpose -- the point has been made
        and every further scene dilutes it.

    CUT, and not to be rebuilt: undo. `undo_for_author` walking back one
    actor's last change is the strongest architectural claim in the
    stack and it wants thirty seconds this film does not have.

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

The camera is on your screen for the first half and on xaver's for the
second, and it changes hands once: on the frame where what you wrote
turns up in his document. It does not come back.

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
import contextlib
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
XAVER_DELAY_S = 6.0
# How long a hold on xaver's screen lasts, at the handover and again at
# the payoff. Not an excursion any more: the film cuts to him once and
# stays, so these are pauses to read what is on his screen rather than
# interruptions of somebody else's — and a pause to read one screen is
# three seconds, not four.
EXCURSION_S = 3.0
# Given to the network between the agent saying it applied something and
# the camera being pointed at the machine that has to have received it.
# It is a grace, not a beat: the crossing has usually already happened
# by the time it is asked about, and the readback below fails if it has
# not, so the seconds bought here are seconds of a screen with nothing
# on it.
SYNC_GRACE_S = 2.0
# Held after the last scripted thing happens, before the take ends by
# itself. Room either side of the payoff rather than one frame of it —
# three seconds is about two long looks at a finished page, and six was
# a third of it spent after the film had stopped saying anything.
DEFAULT_DWELL_S = 3.0
# The outside limit on one unattended take.
DEFAULT_DURATION_S = 900.0
# How long the film waits for somebody to write something in the document
# before handing over without it. The handover is the one place a take
# can wait for ever -- everything after it hangs off the writing having
# crossed -- and a rig that stalls silently there reads exactly like a
# rig that has hung.
HANDOVER_GRACE_S = 120.0
# Longest one unchanged frame is shown for. Trimming is the one edit the
# composer makes, and it is why a narrated take cannot have it: see the
# note in `main` where the two are reconciled.
DEFAULT_MAX_HOLD_S = 1.2
# How long an application is filmed opening before the camera comes down
# for the rest of it.
#
# Long enough to see that something was asked for and is coming: the
# palette closes, the toast says which app, the strip makes room. Past
# that the picture stops changing in any way that means anything — a
# static toast over an unchanged desktop, then a dark tile with a
# spinner — while the sandbox spawns and a toolkit starts. Three
# launches were costing twenty-seven seconds of a hundred-second film
# between them, and almost none of it was a thing to watch.
#
# The window arriving is filmed either way: the camera comes back up the
# moment the compositor says the window is on screen, which is the frame
# the beat is about.
WATCH_IT_OPEN_S = 1.6
# How long a stretch in which nothing happens is filmed before the camera
# comes down for the rest of it.
#
# The agent's latency is the longest thing in this film and nothing will
# make it shorter -- but it does not follow that all of it has to be
# filmed. `Recorder.max_hold` was supposed to take care of it and cannot:
# it collapses runs of byte-identical frames, and a page with a caret
# blinking in it is a different page every half second, so a take that
# waited thirty-nine seconds for a model kept every one of them. Measured
# on the take of 2026-08-25: 1612 distinct frames over a thirty-nine
# second stretch whose own transcript called it a still page.
#
# `Recorder.patience` is the answer and was written for exactly this,
# and until now no scene in this file used it. Five seconds is long
# enough to establish that the machine has been asked for something and
# is thinking about it; past that a viewer has understood, and every
# further second is a second of a screen that is not changing. The camera
# comes back up the instant the agent says or does anything, so what it
# leaves out is only the silence between.
FILM_THE_WAIT_S = 5.0
# How often the watcher looks at anything. Every probe is a `state_dump`
# and blocks the session it is asked of — including, while recording,
# the session being filmed — so this is deliberately unhurried. Only one
# thing is ever polled per tick.
#
# It is also the film's latency floor. Nothing in this file happens the
# instant it becomes possible; it happens at the next tick, and the run
# reaches its end through some ten of those — xaver seen, the agent
# admitted, the agent started, the instruction given, the work noticed,
# the take stopped. At nine tenths of a second each that is most of a
# beat spent waiting for a poll to come round, on a screen where the
# thing being waited for has already happened.
POLL_S = 0.5


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

# There is one instruction in this film, and it is `yc.CROP_INSTRUCTION`:
# xaver is holding a photograph, and what he asks for takes two
# applications to answer. Composing that is a judgement, so it needs the
# model.
#
# `--no-agent-model` does not have one. Its floor matches the
# instruction's own words against the words the *apps* put on their
# operations, plus a handful of named recipes (`apps/agent/src/plan.rs`)
# — and given "make this square and put it in the open document" it
# finds no clear winner and answers, in the room, in front of
# everybody, that it cannot. Which is honest, is the right behaviour,
# and is not what anybody wants filmed.
#
# That mode used to be handed a different instruction — "turn the
# bullets into a table" — with a readback of its own. Both are gone with
# the notes they acted on: nobody types a run of bullets into this
# document any more, so an instruction about bullets is one no document
# in this film can answer, and a readback asserting a table would be
# asserting a beat the film does not have. The flag still turns the
# model off; what it no longer does is quietly film a different
# argument.

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
        #: Set once the camera has moved to the word processor to watch
        #: the crop arrive, so the reframe happens on the first change
        #: and not on every poll after it.
        self.watched_it_land = False
        #: And separately, whether the closing hold has been played. Kept
        #: apart from the reframe above on purpose: the reframe fires
        #: mid-work, and sharing one flag with the ending meant a take
        #: that filmed the crop arriving never reached its own last beat
        #: and ran to the duration cap.
        self.showed_the_ending = False
        #: Quinten has written something, and it has reached xaver. The
        #: whole second half hangs off this: it is the moment the film
        #: cuts, and after it the camera never comes back.
        self.handed_over = False
        #: So the mark charging the agent's latency is laid once, at the
        #: first tick that finds it working, rather than on every poll.
        self.marked_agent_wait = False
        self.watched_it_think = False
        #: When the agent said it had been spoken to, and every state it
        #: went on to put in the room, as `(at, state)`. The second is
        #: what the camera is pointed at during the wait and the only
        #: measure of whether that was worth doing.
        self.spoken_to_at = 0.0
        self.chat_posts: list[tuple[float, str]] = []
        #: Whose screen chat was framed on, or None if it never was.
        self.framed_chat_on: str | None = None
        #: When the film last had something to show, and the camera it
        #: is holding down until it has something again. See
        #: `FILM_THE_WAIT_S`.
        self.quiet_since: float | None = None
        self.camera_down: contextlib.ExitStack | None = None
        #: The stretches the camera spent down, `(from, to)`. Kept so
        #: `report_the_wait` can tell a still page that was filmed from
        #: one that was not -- the first is a defect of the cut and the
        #: second is the cut working.
        self.dark: list[tuple[float, float]] = []
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
        #: Under `--self-test` the owner's half sets this the moment it
        #: has finished typing, so the handover waits for a fact rather
        #: than for a document poll to notice one. Hands-on there is
        #: nobody to set it and the document is asked instead.
        self.typed = threading.Event()
        #: When the run first had everything the handover needs except
        #: the writing, so waiting for the writing can be bounded rather
        #: than open-ended: a driving half that died leaves this event
        #: unset for ever, and a take that never reaches its second half
        #: is a lost afternoon.
        self.ready_at = 0.0
        self.asked_if_he_wrote = 0.0
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

    # -- xaver's screen, once the film reaches it --------------------

    def look_at_xaver(self, why: str, frame_app: str | None) -> None:
        """Hold on whatever is on xaver's screen, and name why.

        No cut and no coming back: by the time anything calls this the
        film is already on him and stays there. This used to be
        `visit_xaver`, which cut over, held, and cut home again, and was
        called three times — he joins, he gives the instruction, the work
        reaches him — with quinten's screen between each pair. Three
        round trips is six times a viewer who has never seen this system
        has to work out whose desktop they are looking at, for a story
        that changes hands exactly once.

        Only while recording: with no camera there is nobody to show it
        to, and the seconds are the film's rather than the run's.
        """
        if self.recorder is None or self.args.excursion <= 0 or self.x is None:
            return
        self.say(f"holding on xaver — {why}")
        try:
            if frame_app:
                yc.frame_one_window(self.x, frame_app)
            yc.read_hold(self.args.excursion)
            self.x.snapshot(os.path.join(
                self.shots,
                f"xaver-{int(time.time() - self.started):04d}.png"))
        except Exception as exc:  # noqa: BLE001
            # A hold is a shot, not a claim. Losing one costs the cut a
            # few seconds of framing; taking the run down for it would
            # lose the whole film, most of which is of a person who was
            # not doing anything wrong.
            yc.gap(f"the hold on xaver's screen failed "
                   f"({type(exc).__name__}: {exc})")

    # -- the things that fire ----------------------------------------

    def wait_for_his_screen(self, budget: float = 12.0) -> None:
        """Hold until the room's applications have reached xaver.

        Asks his own session what it is showing rather than assuming the
        replication is done: a window group on his side is evidence the
        workspace crossed, and it is what the cut to him is for.
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

    def wait_for_his_files(self, budget: float = 25.0) -> dict | None:
        """Hold until the file manager is on xaver's screen and painted.

        The one window this film's argument needs on the other machine,
        and the last thing to be sure of before a press is aimed into it.
        Two conditions, and they are different: the compositor answers
        `foreign_frames` with a laid-out rect for a window it has not
        started painting, so a beat that took the first rect it was given
        would aim a press into a tile showing nothing yet.

        Returns the tile, or `None` when the budget runs out — the caller
        says what that costs. Bounded because a stalled take is no shot at
        all, and generous because the alternative to waiting here is the
        film's whole second half being empty.
        """
        if self.x is None:
            return None
        due = time.time() + budget
        said = False
        while True:
            tile = yc.best_tile(self.x, yc.FILES_APP, yc.VIEWPORT)
            if tile is not None and tile.get("presented"):
                return tile
            if time.time() >= due or self.stop.is_set():
                # A window that is placed but not painted is a different
                # fact from no window at all, and the caller's gap should
                # not report the second when it is the first.
                if tile is not None:
                    self.say("his file manager is placed but the shell is "
                             "not painting it yet")
                return None
            if not said:
                self.say("waiting for the file manager to reach xaver")
                said = True
            self.waiting = "the file manager reaching xaver's screen"
            self.stop.wait(0.5)

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
            # And that is all that happens here on camera: nothing.
            #
            # His joining used to be a cut of its own -- over to his
            # screen, a hold on the collaborators panel, back to quinten
            # -- placed the instant he redeemed the token. It showed a
            # desktop with nothing on it yet and cost the film a round
            # trip to say something the roster on quinten's own screen
            # already said. The room reaching him is proven at the
            # handover instead, by his own app answering with quinten's
            # words in it, which is the same claim with a picture
            # attached.

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
        self.say(f"Claude is in the room, watching (pid "
                 f"{self.agent_proc.pid}, {decides})")
        # Started early and left waiting, which is the point of
        # `--await-document-ms 0`: connecting, announcing itself and
        # taking the room's state are seconds that would otherwise be
        # spent after xaver has already asked for something. Nothing here
        # gives it anything to do. That happens at the handover, and the
        # handover happens when quinten has written something and it has
        # crossed.
        self.say("Claude is in the room and waiting to be spoken to")

    #: How long the handover waits for quinten's words to turn up on
    #: xaver's machine before cutting anyway. Long enough for a document
    #: to cross a real network twice over; short enough that a take does
    #: not sit on an unchanging page while the person driving wonders
    #: whether the rig has died.
    CROSSING_BUDGET_S = 45.0

    #: The shortest run of characters, with a space in it, that this is
    #: willing to call "something a person wrote". Schema tokens -- block
    #: ids, style names, an app's own keys -- are single words, so a
    #: phrase with a space in it is his and not the document's own
    #: furniture. That distinction is the whole point: a needle taken
    #: from the schema would be found on the far side of any two attached
    #: apps, which is a claim that cannot fail.
    WROTE_MIN_CHARS = 8

    def he_has_written(self) -> str | None:
        """A phrase out of quinten's own document, or None if there is none.

        Hands-on, nothing here saw him type and nothing knows what he
        chose to write, so the phrase the crossing is checked on has to
        come out of his document rather than out of this file. Under
        `--self-test` it is known (`yc.SHARED_PROOF`) and this is not
        used.

        None means "he has not written yet", which is what the handover
        waits on. It is also what a document holding nothing but its own
        schema returns, and that is the same answer for the purpose: an
        empty page has not been written in.
        """
        reader = None
        try:
            reader = yc.DocumentReader(self.q.collab_json_sock,
                                       str(self.room), record=None,
                                       handle="control")
            doc, _ = reader.await_document(
                "", 1.0,
                predicate=lambda blob, _n: bool(self._phrases(blob)))
        except (OSError, yc.CannotDecode):
            return None
        finally:
            if reader is not None:
                reader.close()
        phrases = self._phrases(doc) if doc else []
        return max(phrases, key=len) if phrases else None

    def _phrases(self, blob: bytes | None) -> list[str]:
        if not blob:
            return []
        try:
            strings = yc.document_strings(blob)
        except yc.CannotDecode:
            return []
        return [t.strip() for _p, t in strings
                if " " in t.strip() and len(t.strip()) >= self.WROTE_MIN_CHARS]

    def the_handover(self) -> None:
        """Cut to xaver, once, on the frame where quinten's words reach him.

        The hinge of the film and the only cut in it. Everything before
        this is quinten's screen -- his applications, his invitations,
        his few words -- and everything after it is xaver's, without
        coming back.

        What makes it a cut worth making rather than a scene change is
        *what is already there when the camera arrives*: he opened none
        of these applications and typed none of this, and the sentence
        quinten finished a second ago is on his page. That is the
        collaboration claim, and it is made by looking rather than by
        being narrated -- which is why the wait for it happens on
        quinten's screen, where the typing is, and the cut happens on
        the frame it lands.

        Asked of xaver's *app*, in the app's own protocol, not of the
        wire and not of a screenshot: a late joiner on his broker gets
        the word processor's whole document back, so an answer carrying
        the words is his running app holding them. Three outcomes, kept
        apart, because a rig that reports "not there" for "could not
        look" is the defect this file keeps finding in itself.
        """
        self.handed_over = True
        if self.x is None:
            yc.gap("nobody joined, so there is nobody to hand the film "
                   "over to and no second machine for anything to cross to")
            return

        # What he wrote, and where the rig learned it. Driven, this file
        # typed it and knows it exactly. Hands-on, nobody here watched
        # him type, so it comes out of his own document -- and if his
        # page is still empty there is nothing to hand over yet, which is
        # the state `tick` is holding this call back for.
        if self.typed.is_set():
            proof = yc.SHARED_PROOF
        else:
            proof = self.he_has_written()
            if proof is not None:
                yc.note(f"the words were written by hand, so the crossing "
                        f"is checked on a phrase out of quinten's own "
                        f"document ({proof!r}) rather than on one this rig "
                        f"chose")

        self.waiting = "quinten's words reaching xaver's machine"
        self.status(force=True)
        # The crossing is a wait, not a shot. It runs on quinten's screen,
        # which by now has nothing left to show -- the payoff is the cut
        # that comes after it, onto a page he did not open carrying a
        # sentence he did not type. On the first live take this beat cost
        # 27s of a 110s film, all of it spent watching a document sync
        # that the film never shows. So the camera comes down for it and
        # is back up before the cut. Nothing waits any less; the sync
        # takes exactly as long as it takes and every claim below is
        # still asserted on the real document.
        crossing_dark = self.take_camera_down("the words to cross")
        # The room reaching his machine at all, before anything is asked
        # of the app on it. A cut to a bare desktop argues the opposite
        # of what the cut is for, and an app that has not arrived cannot
        # answer a handshake either.
        self.wait_for_his_screen()

        if proof is None:
            yc.gap("quinten's document holds nothing anyone wrote, so the "
                   "film hands over on a page with nothing of his on it "
                   "and this run establishes nothing about the crossing")
        else:
            carried, doc = False, None
            reader = None
            try:
                reader = yc.DocumentReader(self.x.collab_json_sock,
                                           str(self.room), record=None)
                doc, carried = reader.await_document(
                    proof, self.CROSSING_BUDGET_S)
            except OSError as e:
                yc.gap(f"xaver's session has no JSON broker to ask ({e}), "
                       f"so whether quinten's words reached him was not "
                       f"established — looked at {self.x.collab_json_sock}")
            except yc.CannotDecode as e:
                yc.gap(f"{e} — so whether quinten's words reached him was "
                       f"not established, which is not the same as their "
                       f"not having")
            finally:
                if reader is not None:
                    reader.close()
            # Three outcomes, kept apart. A document that came back
            # without the words is a question asked and answered, and the
            # claim is allowed to fail; silence establishes nothing
            # either way and is a gap. Reporting the second as the first
            # is the defect this file keeps finding in itself.
            if doc is not None:
                yc.claim("what quinten wrote is in xaver's own document, "
                         "on his machine", carried, True)
                if not carried:
                    yc.gap(f"his app answered with {len(doc)} bytes and "
                           f"none of it is {proof!r}: the two apps are "
                           f"attached and answer each other's handshake, "
                           f"and their documents are still two documents. "
                           f"The film hands over to a page with nothing of "
                           f"quinten's on it")
            else:
                yc.gap(f"no app in xaver's session answered the late-join "
                       f"handshake within {self.CROSSING_BUDGET_S:.0f}s, so "
                       f"the crossing was never read back — see "
                       f"{self.x.log_path}")

        # Up before the cut, never after it: the frame the cut is taken on
        # is the one a viewer arrives at, and it has to be a real one.
        if crossing_dark:
            self.camera_back_up()

        mark("cut to xaver — quinten's words are already on his screen")
        yc.cut("xaver")
        try:
            yc.frame_one_window(self.x, yc.WORD_PROCESSOR)
        except Exception as exc:  # noqa: BLE001
            yc.gap(f"xaver's word processor could not be framed for the "
                   f"handover ({type(exc).__name__}: {exc}); the strip is "
                   f"left where it is")
        self.look_at_xaver("he did not open this or type this, and it is "
                           "on his page", None)
        mark("he right-clicks the photograph and says what he wants")
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

        Driven on camera, on the screen the film has just handed over
        to, so the audience sees who did it. Returns False rather than
        raising -- a take that reaches the end and names what was
        missing is worth more than one that dies at its best moment.
        """
        if self.x is None or not self.xaver_in:
            yc.gap("xaver never joined, so nobody was there to give the "
                   "instruction")
            return False
        # Already his screen: `the_handover` cut here and does not cut
        # back. Named anyway, because a `cut` to the session on camera is
        # a no-op and this is the one line in the transcript that says
        # whose hands the next gesture belongs to.
        yc.cut("xaver")
        try:
            # Aimed at a pixel, and the pixel is asked for rather than
            # guessed. A client application's widgets are not in the
            # compositor's target registry -- that registry walks the
            # shell's own tree -- so `named_rect("file_row:harbour.png")`
            # could never once have resolved, and sweeping the window for
            # a row instead is what lost a take. The app answers now:
            # `Widget::resource_rects` says where it drew the row for that
            # file's own address, and `press_the_photo_row` converts it
            # with the window's placement.
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

            # Wait for the file manager to reach him, rather than asking
            # once and giving up.
            #
            # `wait_for_his_screen` holds until *something* from the room is
            # on his desktop, which is what the excursion needed and is not
            # what this needs: the windows arrive one at a time and the one
            # this beat is about is not reliably among the first. A take that
            # asked once found chat and the word processor there and Files
            # still crossing, and reported that he had nothing to right-click
            # on a machine that was about to have it.
            #
            # Bounded, and painted rather than merely placed — the compositor
            # answers with a laid-out rect for a window it is not yet drawing,
            # and a press aimed into one lands on whatever is behind it.
            tile = self.wait_for_his_files()
            if tile is None:
                yc.gap("xaver has no file manager window on screen, so "
                       "there is no image for him to right-click")
                return False
            # How many of them there are, before one of them is picked.
            # A session can be showing two windows that are both the file
            # manager — its own, and a peer's replicated in — and they are
            # different processes writing to different logs, so a beat that
            # aims at one and reads the other's account finds nothing and
            # cannot say why.
            here = yc.tiles_for(self.x, yc.FILES_APP)
            self.say(f"xaver's screen has {len(here)} file-manager "
                     f"window(s): " + ", ".join(
                         f"surface {t['surface_id']} at "
                         f"{[round(v) for v in t['rect']]}" for t in here))
            yc.frame_one_window(self.x, yc.FILES_APP)
            # Re-read after framing: it moves and resizes the group, so a
            # rect taken before is the placement of a window that is no
            # longer there, and every press below is aimed at a pixel.
            tile = yc.best_tile(self.x, yc.FILES_APP, yc.VIEWPORT) or tile
            log = yc.LogTail(self.x.log_path)
            log.catch_up()
            # That his file manager is showing the photograph at all,
            # before anything is aimed at it.
            #
            # Asked of the app, not of the disk. `os.path.exists` would
            # answer for the machine the rig is running on and say
            # nothing about what is on his screen — and it is his screen
            # the beat is about. `Widget::resource_rects` is the app
            # telling the session which addresses it is currently drawing
            # and where, so a row in here is a row a person could point
            # at.
            #
            # The address is checked, not just the name. A row called
            # `harbour.png` proves nothing on its own: that is exactly
            # what a copy in his own home would have been called, and for
            # a while that is what the rig staged. What makes this the
            # beat is *which* `harbour.png` — the one the room carries,
            # reachable because a capability recorded on the room
            # replicated to him, rather than one that happens to sit in a
            # directory of his own.
            # His screen, at the moment the question is asked of it.
            #
            # The claim below reads a log; this is the picture that says
            # whether the log is describing what a person would see. They
            # are different kinds of evidence and the beat has been wrong
            # in both directions — a window the compositor called placed
            # and painted that was a picture of nothing, and a listing
            # that was on screen while the map naming it had not been
            # re-emitted because nothing had moved.
            his_screen = os.path.join(self.shots, "his-listing.png")
            try:
                self.x.snapshot(his_screen)
                self.say(f"his screen, before anything is aimed at it: "
                         f"{his_screen}")
            except Exception as exc:  # noqa: BLE001 — evidence is optional
                yc.gap(f"could not photograph xaver's screen ({exc}), so "
                       f"this run has only the log's account of it")
            drawn = log.resource_rects(yc.FILES_APP)
            rows = [a for a in drawn
                    if os.path.basename(a.split(":", 1)[-1]).lower()
                    == yc.PHOTO_NAME.lower()]
            from_the_room = [a for a in rows
                             if a.split(":", 1)[-1].startswith(yc.WORKSPACE_ROOT)]
            yc.claim(
                f"his file manager is drawing {yc.PHOTO_NAME}, out of the "
                f"room's workspace and not out of a home",
                bool(from_the_room), True,
            )
            if rows and not from_the_room:
                yc.gap(f"the row on his screen is {rows[0]!r}, which is not "
                       f"under {yc.WORKSPACE_ROOT} — he is holding a "
                       f"different file that shares a name, and the beat "
                       f"would pass for the wrong reason")
            elif not rows:
                yc.gap(f"his file manager published {len(drawn)} row(s) and "
                       f"none of them is {yc.PHOTO_NAME}; if it published "
                       f"none at all it was granted nothing and is showing "
                       f"its own sandbox directory. See {self.x.log_path}")
            said = yc.say_something_about(
                self.x, log, tuple(tile["rect"]), yc.PHOTO_NAME,
                # The room's workspace, not his home. That is the whole
                # point of the beat: the object he points at is one the
                # room carries, reachable because he is in the room,
                # rather than a copy of it that happens to sit in his own
                # home under the same name.
                yc.expected_listing(yc.WORKSPACE_ROOT), self.args.instruction,
                self.shots, "his-instruction")
        finally:
            # The camera stays on xaver for the rest of the film. He
            # asked for it, so he sees it land -- on a machine where he
            # owns nothing and opened nothing, which is a stronger claim
            # than watching a result arrive where it was already going.
            self.say("staying on xaver — he watches it happen, and the "
                     "film does not leave him again")
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

    #: The agent, having put a state in the room. `note_work` logs one
    #: of these every time it publishes an `AgentWork` into the
    #: conversation, and only then — a state taken, an operation chosen,
    #: a row appended to the trace, a refusal written. One line per
    #: thing a person reading the thread would watch change, which makes
    #: it both the cue to point the camera at chat and the only honest
    #: measure of whether chat was worth pointing it at.
    POSTED = re.compile(r"put the agent's state in the room.*?state: (\w+)")

    # -- filming a wait without filming all of it ---------------------

    def something_is_happening(self) -> None:
        """The film has something to show again: camera up, clock reset.

        Called from every place the agent changes what is on screen --
        a state published into chat, a change applied to the document,
        its process ending. Ordering matters at the first of those: the
        camera comes up *before* chat is framed, so the reframe is a
        thing a viewer watches happen rather than a cut that lands on a
        window already moved.
        """
        self.camera_back_up()
        self.quiet_since = time.time()

    def mind_the_wait(self) -> None:
        """Take the camera down once a wait has been established.

        Only ever once the instruction has provably gone. Before that
        the person at xaver's keyboard may still be reaching for the
        mouse -- under `--no-instruction` that gesture is a hand, not a
        script -- and a camera that comes down on somebody working is not
        an edit, it is a missing shot.
        """
        if self.recorder is None:
            return
        if self.quiet_since is None:
            # Two ways to know the wait is the agent's and not a
            # person's, and both are needed.
            #
            # `spoken_to_at` is the agent's own word for it and is the
            # better one, but only the summons carried by a message
            # reaches the code that logs it: an instruction written as a
            # line in the document is read by `act_in_document`, which
            # says nothing anywhere. That path would leave the camera up
            # through the whole wait, which is the state this is here to
            # end.
            #
            # So the rig's own gesture counts too — but only when the rig
            # made it. Under `--no-instruction` `instruction_sent` means
            # "xaver's window was handed to a person", who has not typed
            # anything yet, and a camera that comes down on somebody
            # working is a missing shot rather than an edit.
            if not (self.args.instruct and self.instruction_sent):
                return
            self.quiet_since = time.time()
        # Tied to the same switch that already says whether this take may
        # be edited at all. A camera that comes down leaves the picture
        # shorter than the take, which is exactly what `--max-hold 0`
        # exists to forbid: a narrated take records a voice on the wall
        # clock, and a minute taken out of the middle of the picture puts
        # the voice a minute ahead of it for the rest of the film.
        # `Recorder._check_length` would refuse the result; refusing to
        # make it is better than composing it and being told.
        if self.recorder.max_hold <= 0:
            return
        if self.camera_down is not None:
            return
        if time.time() - self.quiet_since < FILM_THE_WAIT_S:
            return
        stack = contextlib.ExitStack()
        stack.enter_context(self.recorder.hold_off())
        self.camera_down = stack
        self.dark.append((time.time(), 0.0))
        self.say(f"nothing has changed for {FILM_THE_WAIT_S:.0f}s — the "
                 f"camera comes down until Claude says or does something")

    def take_camera_down(self, why: str) -> bool:
        """Stop watching a wait that has nothing to show. Did it come down?

        `mind_the_wait` does this for the agent's wait and only for that
        one: it returns early until the instruction has gone. A beat that
        blocks before then -- the crossing does, for as long as a document
        takes to reach another machine -- keeps the camera up on a screen
        with nothing left happening on it.

        Same trade the app launches make, and the same refusal: a take
        that may not be edited at all keeps filming, because a narrated
        take records a voice against the wall clock and a gap cut out of
        the middle puts the two out of step for the rest of the film.

        Pair with `camera_back_up` when this returns True.
        """
        if self.recorder.max_hold <= 0 or self.camera_down is not None:
            return False
        stack = contextlib.ExitStack()
        stack.enter_context(self.recorder.hold_off())
        self.camera_down = stack
        self.dark.append((time.time(), 0.0))
        self.say(f"the camera comes down for {why}")
        return True

    def camera_back_up(self) -> None:
        if self.camera_down is None:
            return
        stack, self.camera_down = self.camera_down, None
        began, _ = self.dark[-1]
        self.dark[-1] = (began, time.time())
        stack.close()
        self.say(f"the camera is back up after {time.time() - began:.0f}s "
                 f"of waiting")

    def filmed(self, a: float, b: float) -> float:
        """How much of `a`..`b` the camera was actually up for."""
        dark = sum(max(0.0, min(b, end or b) - max(a, begin))
                   for begin, end in self.dark)
        return max(0.0, (b - a) - dark)

    def check_the_agent(self) -> None:
        """Whether it has done anything, and whether it has finished."""
        fresh = self.agent_tail.read() if self.agent_tail else ""
        if not self.spoken_to_at and self.SPOKEN_TO.search(fresh):
            self.spoken_to_at = time.time()
            self.say("Claude has been spoken to in the room and is deciding")
            # And the wait starts here, not at process start: everything
            # before this is somebody's hand on a mouse.
            self.quiet_since = self.spoken_to_at
        # Counted off the whole log rather than off this poll's chunk. A
        # read lands wherever the writer had got to, so a line can arrive
        # in two halves — and half of one of these carries neither the
        # message nor the state, so it would be lost rather than counted
        # late. `LogTail.text` is everything read so far, and the line is
        # whole in it by the next poll.
        seen = self.POSTED.findall(self.agent_tail.text) if self.agent_tail \
            else []
        for state in seen[len(self.chat_posts):]:
            # Reframed off the first *publish* rather than off "spoken
            # to", which is a moment earlier and on one path only: the
            # branch that answers a summons naming an app whose state
            # has not arrived writes `Waiting` into the room and returns
            # without ever logging that it was spoken to. Firing here
            # covers that, and buys the stronger property besides —
            # nothing points the camera at chat until chat has something
            # in it that the agent put there.
            self.chat_posts.append((time.time(), state))
            self.something_is_happening()
            self.say(f"Claude put {state} in the room "
                     f"({len(self.chat_posts)} so far)")
            if not self.watched_it_think:
                self.watched_it_think = True
                self.watch_it_think()
        hit = self.APPLIED.search(fresh)
        if hit and not self.agent_applied:
            self.agent_applied = True
            self.something_is_happening()
            self.say(f"Claude acted — {hit.group(0).strip()[:110]}")
            # The change is going into the document *now*, so the
            # document is what the camera should be on. Anything later
            # films a page that is already holding it.
            self.the_work_lands()
        if self.agent_proc.poll() is None:
            return
        code = self.agent_proc.returncode
        self.agent_done_at = time.time()
        self.something_is_happening()
        self.report_the_wait()
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
        it is safe: the trigger is the agent having published a state
        into the room, so the remark has been written, sent, and
        answered. Reframing any sooner would move a window out from
        under whoever was still right-clicking in it -- under
        `--no-instruction` that gesture is a person's hand, and the
        agent's process has been running since before they reached for
        the mouse, so process start is exactly the cue that must not be
        used.

        Whose screen this happens on is asked rather than assumed. It
        was xaver's, because the one path that reaches here cut to him
        and stayed -- but the film is on quinten for every take without
        a second machine in it, and pointing at xaver's chat then
        reframed a window nobody was looking at while the screen that
        was on camera went on showing a still page. So the reframe
        follows the camera, and a screen with no chat window on it is
        said out loud rather than silently skipped.

        Losing this costs the shot its subject and nothing else, so a
        failure here is reported and the take goes on.
        """
        if self.recorder is None:
            return
        on = self.recorder.on_camera()
        if on is None:
            return
        try:
            if yc.best_tile(on, yc.CHAT_APP, yc.VIEWPORT) is None:
                yc.gap(f"no chat window is on {on.name}'s screen, which is "
                       f"the one being filmed, so the agent's account of "
                       f"what it is doing has nowhere to be read and the "
                       f"wait is filmed as a still page")
                return
            yc.frame_one_window(on, yc.CHAT_APP)
            self.framed_chat_on = on.name
            self.say(f"framing chat on {on.name}'s screen — Claude says what "
                     f"it is doing there as it does it")
            on.snapshot(os.path.join(
                self.shots,
                f"claude-thinking-{int(time.time() - self.started):04d}.png"))
        except Exception as exc:  # noqa: BLE001
            yc.gap(f"the camera could not be pointed at chat for the agent's "
                   f"working period ({type(exc).__name__}: {exc})")

    def the_work_lands(self) -> None:
        """Point the camera back at the document as the crop arrives in it.

        `watch_it_think` framed chat, because for most of the agent's
        working period chat is the only thing changing. The last thing
        that happens is not in chat: the photo editor opens in the room,
        the crop is exported, and the picture appears on the page. Framed
        when the agent's log first says it applied something, so the
        arrival is filmed rather than discovered afterwards on a page
        that was already holding it.

        A reframe, not a cut — the camera has been on xaver since the
        handover and does not leave him. Follows whoever is on camera
        for the same reason `watch_it_think` does: a take with no second
        machine in it is filmed on quinten throughout, and reframing the
        screen nobody is looking at is how a beat gets lost.

        Losing this costs the shot its subject and nothing else, so a
        failure is reported and the take goes on.
        """
        if self.watched_it_land or self.recorder is None:
            return
        on = self.recorder.on_camera()
        if on is None:
            return
        self.watched_it_land = True
        try:
            if yc.best_tile(on, yc.WORD_PROCESSOR, yc.VIEWPORT) is None:
                yc.gap(f"no word processor is on {on.name}'s screen, which "
                       f"is the one being filmed, so the picture the agent "
                       f"was told to put in the open document has nowhere "
                       f"to be seen arriving")
                return
            yc.frame_one_window(on, yc.WORD_PROCESSOR)
            self.say(f"framing the document on {on.name}'s screen — the "
                     f"crop arrives there")
            on.snapshot(os.path.join(
                self.shots,
                f"it-landed-{int(time.time() - self.started):04d}.png"))
        except Exception as exc:  # noqa: BLE001
            yc.gap(f"the camera could not be pointed at the document for "
                   f"the crop's arrival ({type(exc).__name__}: {exc})")

    #: The longest the frame may go without changing before the stretch
    #: stops being watchable. Eight seconds is about where a viewer who
    #: has never seen this system decides the picture is frozen; it is
    #: not a measurement, and it is a great deal more generous than
    #: anything a cut would keep.
    LONGEST_STILL_S = 8.0

    def report_the_wait(self) -> None:
        """What was on screen while the agent decided, and how often it moved.

        The longest stretch in this film and the one nothing will
        shorten, so the only question a take can answer about it is
        whether anything changed while it ran. Counted off the agent's
        own publishes and not off the frames: two frames differ whenever
        a caret blinks, and a caret blinking is not the agent saying
        anything.

        Said as a gap rather than a failure when it is thin. The take is
        still worth having and the readback is still the claim -- but a
        still page held for half a minute is a defect of the film, and a
        transcript that does not name it is how it goes on being filmed.
        """
        began = self.spoken_to_at or (self.chat_posts[0][0]
                                      if self.chat_posts else 0.0)
        if not self.chat_posts:
            # Only a summons carried by a message -- the right-click
            # compose surface, which is the gesture this film is built on
            # -- reaches `carry_out`, and only `carry_out` narrates. An
            # instruction written as a line in a document goes to
            # `act_in_document` instead, which opens no record in the
            # room at all, so there is nothing for a camera to find in
            # chat however well it is pointed. Nothing in this film types
            # one any more; a run that lands here without ever being
            # spoken to reached the agent by some other route, and which
            # one is worth knowing before pointing a camera anywhere.
            how = ("it was never spoken to at all, so whatever provoked "
                   "it did not arrive as a message"
                   if not self.spoken_to_at else
                   "it was spoken to and published nothing")
            yc.gap(f"Claude put nothing in the room while it worked, so its "
                   f"whole working period was filmed as a still page — "
                   f"{how}")
            return
        at = [t for t, _s in self.chat_posts]
        span = at[-1] - (began or at[0])
        states = ", ".join(s for _t, s in self.chat_posts)
        self.say(f"Claude's working period carried {len(self.chat_posts)} "
                 f"change(s) over {span:.0f}s: {states}")
        if self.framed_chat_on is None:
            yc.gap("Claude narrated itself in the room and the camera was "
                   "never pointed at it, so the take has the account and "
                   "not the shot")
        # The count is not the question a viewer asks; the longest wait
        # between two of them is. Five changes with four of them in the
        # first second is a still page with a flurry on the front, and
        # counting alone calls that well covered. The tail counts too —
        # the frame goes on being held after the last thing was said.
        edges = [began or at[0]] + at + [self.agent_done_at or at[-1]]
        gaps = list(zip(edges, edges[1:]))
        still = max(b - a for a, b in gaps)
        # Asked of the film rather than of the clock. A stretch the agent
        # did not change is a defect only if it was filmed; the same
        # stretch with the camera down is the cut doing its job, and
        # reporting the two the same way is how the remedy would look
        # like the fault. Both numbers are printed, so a `patience` that
        # silently stopped working shows up as the pair converging rather
        # than as a gap that simply stopped being raised.
        filmed = max(self.filmed(a, b) for a, b in gaps)
        dark = sum((b - a) - self.filmed(a, b) for a, b in gaps)
        self.say(f"the longest stretch it changed nothing was {still:.0f}s; "
                 f"{filmed:.0f}s of it was filmed, {dark:.0f}s of the "
                 f"waiting was not")
        if filmed > self.LONGEST_STILL_S:
            yc.gap(f"the longest the frame went without the agent changing "
                   f"it was {still:.0f}s, out of a working period of "
                   f"{span:.0f}s, and {filmed:.0f}s of that was filmed — "
                   f"that stretch is a still picture")

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

        There is one outcome to look for, because there is one
        instruction. It used to be chosen by `args.outcome`, which
        travelled beside a second instruction picked by
        `--no-agent-model` — and the other half of that pair asserted a
        table carrying a bullet out of notes this film no longer types.
        """
        self.read_back_the_crop()

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
        """The file he right-clicked, as it sits in the room's workspace.

        One place to read it from, because there is one of it. This used
        to try each home in turn and take whichever answered first —
        which was a sensible thing to write when the picture was staged
        into every home, and is exactly the arrangement that made the
        beat provable against the wrong file.
        """
        try:
            with open(os.path.join(yc.WORKSPACE_ROOT, yc.PHOTO_NAME), "rb") as fh:
                return fh.read()
        except OSError:
            return b""

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

        # The film changes hands here, and only here.
        #
        # This used to be inside `start_the_agent`, which fires the
        # moment a word processor appears -- so the cut to xaver happened
        # while quinten was still opening the image editor, and his
        # typing beat was filmed on a machine the story had already left.
        # Waiting for what he wrote to reach xaver puts the cut where the
        # script puts it, and makes the crossing the reason for it.
        if not self.handed_over:
            if not self.ready_at:
                self.ready_at = now
            self.waiting = "he writes a few words in the document"
            written = self.typed.is_set()
            if not written:
                # Driven, the owner's half says so; by hand, his own
                # document is asked, and not on every tick -- each ask is
                # a socket and a second, and this is the one branch that
                # can sit here for a minute while somebody types.
                if self.args.self_test:
                    pass
                elif now - self.asked_if_he_wrote >= 3.0:
                    self.asked_if_he_wrote = now
                    written = self.he_has_written() is not None
            if not written:
                if now <= self.ready_at + HANDOVER_GRACE_S:
                    return
                # Handing over anyway rather than sitting here to the
                # duration cap. A rig that stalls silently at the one
                # place a take can stall for ever reads exactly like a
                # rig that has hung, and the beat it is waiting for is
                # the one nobody can perform on its behalf.
                yc.gap(f"nothing was written in the document within "
                       f"{HANDOVER_GRACE_S:.0f}s of everything else being "
                       f"ready for it, so the film hands over anyway — and "
                       f"the beat it hands over on did not happen")
            self.status(force=True)
            self.the_handover()
            return

        if self.agent_proc is not None and not self.agent_done_at:
            if not self.marked_agent_wait:
                self.marked_agent_wait = True
                mark("Claude reads the room, decides, and works")
            self.waiting = ("Claude is working" if self.agent_applied
                            else "Claude waits to be spoken to")
            self.check_the_agent()
            self.mind_the_wait()
            return

        if self.agent_done_at:
            if now < self.agent_done_at + SYNC_GRACE_S:
                self.waiting = "the edit crossing to the other machine"
                return
            if not self.showed_the_ending:
                self.showed_the_ending = True
                # No cut. The camera has been on him since the handover,
                # so the payoff arrives on the screen he was already
                # watching -- which is the ending the script asks for and
                # is a stronger one than being shown a second machine to
                # prove it happened there too.
                mark("the crop lands, on the screen he was already watching")
                self.the_work_lands()
                self.look_at_xaver("the picture he pointed at, square, in "
                                   "a document he never opened",
                                   None)
                if self.args.stop_after == "agent":
                    self.say(f"holding {self.args.dwell:.0f}s, then stopping")
                    self.stop.wait(self.args.dwell)
                    self.finished = "the agent's work landed on both machines"
                    return

        self.waiting = "you — Ctrl-C in this terminal ends the take"

    def run(self) -> str:
        try:
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
        finally:
            # However this ends, the camera does not stay down. A take
            # abandoned mid-wait would otherwise compose whatever was on
            # screen before the wait as its last frame, and a run that
            # stopped is a different thing from a run that was edited.
            self.camera_back_up()
        if isinstance(sys.stdout, Console):
            sys.stdout.status("")
        return self.finished or "you stopped it"


# --------------------------------------------------------------------
# the half a person does, done without one
# --------------------------------------------------------------------


def the_owners_half(q, shots: str, live: "Live",
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
        _the_owners_half(q, shots, live, stop)
    except Exception as exc:  # noqa: BLE001 — a dead thread must be loud
        import traceback
        traceback.print_exc()
        yc.FAILURES.append(
            f"--self-test: the owner's half stopped at "
            f"{type(exc).__name__}: {exc}")


def _the_owners_half(q, shots: str, live: "Live",
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
    # Marked before the hold, not after it. The beat *is* the hold -- an
    # empty strip, so the applications have somewhere to come from -- and
    # laying the mark on the far side of it charged those seconds to
    # nothing: a take measured at 114.1s of picture accounted for 111.1s
    # across its beats, and the missing three were this one.
    #
    # Long enough to establish that the desktop is empty and no longer,
    # which is under a second: there is one thing to see and nothing is
    # moving. Three seconds of it was a thirty-seventh of the whole film
    # spent on a still picture of nothing, before anybody had touched
    # the machine.
    mark("an empty desktop")
    stop.wait(0.8)
    if stop.is_set():
        return
    print("\n  [ --self-test: driving the owner's half ]", flush=True)

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
    chat = q.launch(yc.CHAT_APP, search="chat", via="palette",
                    look=0.9, read=1.1, watch=WATCH_IT_OPEN_S)
    yc.claim("chat opened", chat is not None, True)
    stop.wait(0.15)

    mark("Files opens")
    # Granted before it starts, not asked for after.
    #
    # The file manager's sandbox is built at launch, so a capability
    # granted to a process already running changes the list and not the
    # mount: the prompt appears, the answer is recorded, and the window
    # still says "No such file or directory" about a directory that is
    # right there. Granting first means the bind exists when the process
    # does.
    #
    # And a permission dialog is not this film's subject. The capability
    # model gets its own beat, with a grant somebody actually watches
    # being made; a modal in front of the file manager during a different
    # scene is an interruption that teaches nothing.
    # Every session, not just the one being driven. A capability is
    # per-workspace and each person here has their own, so granting only
    # quinten's left the guest's file manager asking — and his screen is
    # the one the camera cuts to. A finished take spent 204 unbroken
    # seconds, three and a half minutes, framed on an unanswered
    # "read/write access to /var/tmp/qos/home/xaver" that nobody was ever
    # going to click, while the beats it was hiding happened off-camera.
    for s in yc.SESSIONS:
        if getattr(s, "home", None) is None:
            continue
        yc.claim(
            f"{s.name}'s file manager may read the home it is about to show",
            s.grant(f"app:{yc.FILES_APP}", f"fs:{s.home}", "write")
             .get("answer", {}).get("granted") is not None,
            True,
        )
    # And the workspace the *room* carries — granted once, by the person
    # who owns the room, on the room.
    #
    # This one is not a convenience like the homes above, it is the beat.
    # A capability recorded on a room is replicated to whoever was
    # admitted to it (`WorkspaceTreeReplica`), and a peer's window is
    # launched with the room's own capability list
    # (`peer_windows::open_for` → `launcher::launch_in_workspace`). So
    # granting it here, on the host, is what puts the photograph within
    # xaver's reach later — not a second grant made on his machine, which
    # he could not make anyway: `Workspace::grant_as` refuses a grantor
    # who neither owns the room nor holds admin over it, and a guest is
    # neither. He reaches the object by being in the room.
    #
    # Read, not write. Nothing in the film needs to change it, and the
    # narrower grant is the one that proves the point.
    yc.claim(
        "the room carries a workspace its file manager may read",
        q.grant(f"app:{yc.FILES_APP}", f"fs:{yc.WORKSPACE_ROOT}", "read")
         .get("answer", {}).get("granted") is not None,
        True,
    )
    files = q.launch(yc.FILES_APP, search="files", via="palette",
                     look=0.25, read=0.45, watch=WATCH_IT_OPEN_S)
    yc.claim("Files opened", files is not None, True)
    stop.wait(0.15)

    # Its own group, or the placement rule tabs it in behind what is
    # already there and the strip has one window where the scene wants two.
    mark("the word processor opens")
    q.place_next_window(yc.WORD_PROCESSOR)
    wp = q.launch(yc.WORD_PROCESSOR, search="word", via="palette",
                  look=0.25, read=0.45, watch=WATCH_IT_OPEN_S)
    yc.claim("the word processor opened", wp is not None, True)
    if wp is None:
        return
    stop.wait(0.25)

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
    stop.wait(0.3)

    # Now the people, into a workspace that already has something in it.
    mark("inviting xaver")
    yc.show_collaborators(q, settle=0.8)
    token = yc.invite_from_the_panel(q, "xaver", shots, "live-invite")
    yc.claim("an invitation was minted from the panel", bool(token), True)

    # Two, because an invitation names its holder and the agent is a
    # member like any other. Raised again on an empty field -- the
    # composer used to keep the previous answer, so a second invitation
    # came out addressed to "XaverXaver".
    # The same card, held for less time. What the second invitation adds
    # is that an agent is admitted by the mechanism a person is admitted
    # by — one more name in one more field — and that reads at a glance
    # off a card a viewer finished reading fifteen seconds ago. Nothing
    # is skipped: the panel is opened by its own affordance, the composer
    # still asks who, the token is still minted and still proved to be on
    # the clipboard, and the name in the field is held for exactly as
    # long as xaver's was, because that name is what the beat is for.
    mark("inviting Claude — the same card, admitted as an agent")
    yc.show_collaborators(q, settle=0.4)
    agent_token = yc.invite_from_the_panel(q, "Claude", shots, "live-invite-2",
                                           read=0.45)
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
    stop.wait(0.3)

    # Four applications, two in frame, and two of them made one window.
    #
    # What earns its seconds here is the tab bar holding *different
    # applications*. A tab everywhere else belongs to its app, because the
    # app draws it; here it is a frame primitive the compositor owns, so a
    # file manager and a document and a photograph sit on one bar. That is
    # the one gesture in the film a viewer cannot mistake for a browser.
    mark("the image editor opens, and two applications share one tab bar")
    extras: dict[str, dict] = {}
    if photos is None:
        photos = q.launch(yc.IMAGE_EDITOR,
                          search=yc.STRIP_SEARCH.get(yc.IMAGE_EDITOR,
                                                     "photos"),
                          via="palette", look=0.25, read=0.45,
                          watch=WATCH_IT_OPEN_S)
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
    stop.wait(0.3)

    # A few words, and that is the whole beat.
    #
    # What it has to show is that the document is *shared*: he writes,
    # and it is on somebody else's screen. Three lines of prose, a run of
    # bullets and a paragraph addressed to the agent by name showed that
    # no better and took thirteen seconds to type.
    #
    # The instruction used to be typed here too, and that was the film
    # arguing against itself: an agent answering a line in the document it
    # is already watching is what every word processor now ships, and it
    # made the gesture the film is actually about — somebody pointing at
    # an object and saying something about *that* — into a second route to
    # the same place. With two of them the agent answered whichever
    # arrived first, and on a `--once` run that was usually this one, so
    # the beat the camera was on did nothing.
    #
    # It also cost the film the only thing there was to watch during the
    # agent's half-minute of thinking. A summons carried by a remark goes
    # through `carry_out`, which opens an `AgentWork` record and posts
    # Taken/Waiting/Doing/Done into chat as each thing happens; a line in
    # the document goes down a path that publishes none of it. Every take
    # so far filmed a still page while the agent worked, because of this
    # call.
    #
    # So nothing typed here is addressed to anybody. The instruction
    # reaches the agent once, from xaver's hand, carrying the address of
    # the thing he pointed at — see `Live.his_instruction`.
    mark("quinten types a few words")
    typed = yc.type_into_the_document(q, wp, yc.SHARED_TEXT)
    yc.claim("the document was written into", typed, True)
    if typed:
        # The handover waits on this. Set only when the keystrokes
        # actually went somewhere: with no laid-out tile to aim a caret
        # into, nothing was typed, and a cut to xaver claiming quinten's
        # words are on his screen would be a cut to a page that has
        # nothing on it.
        live.typed.set()
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
                    help=f"seconds held on xaver's screen at the handover, "
                         f"and again when the crop lands (default "
                         f"{EXCURSION_S:.0f}; 0 never cuts to him at all)")
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
    ap.add_argument("--allow-resampled", action="store_true",
                    help="film even when the session does not render at "
                         "the video's picture area. Refused by default: "
                         "the composer fits whatever it is given into "
                         "that area, so any other size resamples every "
                         "frame of a film that is almost entirely text")
    ap.add_argument("--no-record", dest="record", action="store_false",
                    help="drive it without a camera")
    ap.add_argument("--keep", action="store_true",
                    help="leave the sessions running afterwards")

    ap.add_argument("--dry-run", action="store_true",
                    help="stage everything headless, check it and stop. No "
                         "window, no X server, no recording — so it runs "
                         "over ssh and cannot put anything on your screen")
    ap.add_argument("--self-test", action="store_true",
                    help="drive the owner's half too: open Files, mint two "
                         "invitations, open the word processor and write a "
                         "few words in it. Proves the whole rig with no "
                         "hands")
    args = ap.parse_args()

    # The sentence xaver says about the photograph. Not typed anywhere:
    # it is what the remark he opens on `harbour.png` carries, and under
    # `--no-instruction` it is what the terminal tells him to write
    # there. Nothing in this film puts an instruction on a page.
    #
    # One instruction, one readback, and no branch between them. There
    # were two of each once, chosen by `--no-agent-model`, and the
    # pairing was the whole difficulty: whichever instruction was picked,
    # the readback asserted a table with a run sheet's words in its
    # cells, so a take of the crop failed a claim about a beat it was
    # not filming.
    args.instruction = yc.CROP_INSTRUCTION
    if not args.agent_model:
        yc.gap("--no-agent-model has no answer for the instruction this "
               "film gives — its vocabulary is table, caption and "
               "heading, and it will say so in the room rather than crop "
               "anything")

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
    #
    # Refusing here is necessary and was never sufficient. Trimming is
    # only one of the ways a picture can stop being the length of its
    # take — a frame floor in `_timeline` doubled one while this check
    # sat satisfied, because the take genuinely had been recorded at
    # `--max-hold 0`. What a setting is at the start of a take is a
    # weaker claim than what the file is at the end of one, so
    # `Recorder._check_length` reads the finished film back and refuses
    # it on the measurement. This check survives because it fails before
    # the countdown instead of after the filming.
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
    elif args.narrate is not None:
        # Typing the right thing used to be the one way to be told
        # nothing. The confirmation above only printed when the setting
        # was *inferred*, so `--max-hold 0` — the value this rig wants,
        # asked for explicitly — was accepted in silence, and the
        # operator got no more acknowledgement of the setting in force
        # than if he had misspelled the flag.
        print("narrating at --max-hold 0: the picture stays on the wall "
              "clock, which is\nwhat the voice needs.")

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
    # And the room's workspace, which lives outside `--out` for the same
    # reason and is staged once for the whole rig rather than once per
    # session — that is what makes the photograph one object two people
    # reach rather than two files that share a name.
    shutil.rmtree(yc.WORKSPACE_ROOT, ignore_errors=True)
    yc.stage_workspace()
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

    # Who types into the window decides its keymap, not who owns it.
    #
    # `--self-test` means the rig writes the notes and the instruction
    # itself, through `type`, which emits one US evdev keycode per
    # character. Pinned to the operator's German layout, keycode 39 with
    # shift is `Ö` and keycode 12 is `ß` — so the document filled up with
    # `ClaudeÖ make this square` and `ß DoorsÖ 18Ö00`, on camera, and the
    # agent (which matches the literal `claude:`) saw nothing addressed
    # to it. Under `--no-instruction`, where a person's hands are on the
    # keys, the operator's layout is exactly right and stays.
    for s in sessions:
        if hasattr(s, "typed_by_a_person"):
            s.typed_by_a_person = not args.self_test

    if not args.dry_run:
        area = ins.work_area()
        if args.windowed:
            quinten.window_size = None
        else:
            # The video's own size, asked for outright, so a frame goes
            # into the film 1:1 and the band sits in empty canvas below
            # it rather than over the bottom of a window.
            #
            # This used to be clamped down to the work area, reasoning
            # that a window manager shrinks a window asking for more and
            # a shrink is a resize. The clamp is what made the ask wrong.
            # A window manager auto-maximizes a *resizable* window whose
            # request already covers most of the work area, so asking for
            # 1920x1014 of a 1920x1048 work area — 97% of it — came back
            # as the whole 1920x1048, granted by being overruled. The
            # composer then fits every frame into the picture area, so
            # thirty-four rows too tall means every frame resampled.
            #
            # The compositor publishes the requested size as fixed now
            # (see `NIRIOS_WINIT_SIZE`), and a window that cannot be
            # resized is neither shrunk nor auto-maximized. So the honest
            # ask is the size the video needs, and it is granted.
            quinten.window_size = yc.RECORD_SIZE
            if area and (area[0] < yc.VIDEO_W
                         or area[1] < yc.VIDEO_H - yc.BAND_H):
                # A screen that genuinely cannot show it still gets the
                # work area: a window the person driving cannot reach is
                # worse than a smaller picture. It is not free — the
                # composer fits whatever it is given into the picture
                # area, so a smaller render is upscaled and every glyph
                # in it softens — which is why it is said here, before
                # the countdown, rather than found in the frames.
                quinten.window_size = (f"{min(area[0], yc.VIDEO_W)}"
                                       f"x{min(area[1], yc.VIDEO_H - yc.BAND_H)}")
                print(f"  ! this screen's usable area is {area[0]}x{area[1]}, "
                      f"smaller than the video's {yc.RECORD_SIZE}. Filming at "
                      f"{quinten.window_size}, which the composer will scale "
                      f"up to fill the frame: this take will be softer than "
                      f"one shot on a screen with room for {yc.RECORD_SIZE}.")

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
        # Said out loud in a way somebody would act on, and before the
        # countdown rather than after the take.
        #
        # Exactly the picture area is the one to want: the composer's
        # fit computes a scale of 1, the frame goes in untouched, and the
        # band sits on canvas below it. Anything else is fitted into that
        # area, which is the right composition and is not free — the fit
        # is a real resample of every frame. A 1920x1048 render, which is
        # what a window manager hands back when it overrules the ask,
        # comes out scaled to 1856x1014: measured, and visible at 4x as
        # soft stems on the panel headings, in a film that is almost
        # entirely text.
        #
        # So it is a refusal, and `--allow-resampled` is there for a take
        # somebody wants anyway. Not a warning: this was a parenthesis at
        # the end of a setup line, printed once, minutes before anybody
        # looked at a frame.
        picture = (yc.VIDEO_W, yc.VIDEO_H - yc.BAND_H)
        fit = min(picture[0] / rendered[0], picture[1] / rendered[1])
        into = (int(rendered[0] * fit) & ~1, int(rendered[1] * fit) & ~1)
        if rendered == picture:
            print(f"  rendering at           {size}")
        elif args.allow_resampled or args.windowed:
            # `--windowed` is somebody asking for a small window on
            # purpose, so it carries its own consent. It still says what
            # that costs, because a take is a take.
            print(f"  rendering at           {size}  (not {yc.RECORD_SIZE}: "
                  f"every frame will be resampled to {into[0]}x{into[1]})")
            yc.gap(f"the session renders {size} rather than the video's "
                   f"picture area {yc.RECORD_SIZE}, so every frame of this "
                   f"take is resampled to {into[0]}x{into[1]} — soft text "
                   f"in a film that is almost all text")
        else:
            # Which of the two it is, because they have different
            # remedies: a window granted something other than it asked
            # for is a window manager overruling a fixed-size request,
            # and a window asked for at less than the video needs is a
            # screen that has no room for it.
            asked = quinten.window_size or "the compositor's own default"
            why = (f"The window was asked for at {asked} and granted "
                   f"something else, which is a window manager overruling "
                   f"a fixed-size request. Close whatever is reserving "
                   f"screen space"
                   if asked == yc.RECORD_SIZE else
                   f"The window was only asked for at {asked}, because "
                   f"this screen's usable area has no room for "
                   f"{yc.RECORD_SIZE}. Film it on a larger screen")
            raise RuntimeError(
                f"the session renders {size}; the video's picture area is "
                f"{yc.RECORD_SIZE}. Every frame would be fitted into it at "
                f"{into[0]}x{into[1]}, which resamples all of them.\n"
                f"  {why}, or pass --allow-resampled to film it anyway.")
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
            print(f"  shared workspace       "
                  f"{yc.expected_listing(yc.WORKSPACE_ROOT)}  "
                  f"({yc.WORKSPACE_ROOT})")
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
            print("  - open the word processor and write a few words in "
                  "it — anything; a sentence is plenty")
            print("      -> the film cuts to xaver the moment they reach "
                  "his machine, and stays there")
            if args.agent:
                print("      -> Claude is already in the room, waiting to "
                      "be spoken to. Nothing is typed to it: xaver "
                      f"right-clicks {yc.PHOTO_NAME} on his own screen "
                      f'and writes "{args.instruction}"')
                if not args.agent_model:
                    print("         (keyword mode: it only knows table, "
                          "caption and heading, so it will decline that "
                          "one — in the room, on camera)")
            if args.stop_after == "agent" and args.agent:
                print("  - the take ends by itself once the crop has "
                      "landed in the document he is watching")
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
                          "collaborators panel, open the word processor "
                          "and write a few words in it.",
                          quiet=args.self_test)
            if wid is not None:
                ins.raise_window(wid)

        # A dry run films too, when it is driving. The camera reads the
        # same `snapshot` op off a headless session as off a windowed
        # one, so recording here is what makes `--dry-run --self-test` a
        # rehearsal of the whole rig rather than of its first half: the
        # handover, the hold on xaver's screen and the compose all run,
        # on a machine with no X server and with nothing on anybody's
        # desktop. Without it, three of the things most likely to be
        # broken on the night are the three never exercised.
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

        # Built before the driving half starts, because that half now
        # reports into it: the film's one cut waits for quinten to have
        # written something, and under `--self-test` the thread doing the
        # writing is the only thing that knows when it has.
        print()
        live = Live(quinten, xaver, agent, args, shots, recorder)
        live.stop = stop

        if args.self_test:
            hands = threading.Thread(
                target=the_owners_half,
                args=(quinten, shots, live, stop),
                name="hands", daemon=True)
            hands.start()

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
        # The number the last broken take never printed. `compose` now
        # refuses a narrated film that is not the length of its take, so
        # reaching this line is itself the proof — but the operator
        # should be able to read the agreement rather than infer it from
        # the absence of a complaint.
        if recorder.trimmed > yc.WALL_CLOCK_TOL_S:
            print(f"             {recorder.trimmed:.1f}s of still picture "
                  f"trimmed from {recorder.wall_s:.1f}s of take")
        else:
            print(f"             {video['duration']:.1f}s of picture for "
                  f"{recorder.wall_s:.1f}s of take"
                  + (f", {video['audio']:.1f}s of voice"
                     if video.get("audio") else "")
                  + ": the film runs on the wall clock")
    return rc


if __name__ == "__main__":
    sys.exit(main())
