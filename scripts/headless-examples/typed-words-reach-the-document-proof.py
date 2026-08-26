#!/usr/bin/env python3
"""A sentence typed at a document must not land in another window.

The demo's typing beat aims its keystrokes at a *window* and then checks
a *document*, and between the two sits a question neither of them asks:
which window holds the keyboard. When the answer is "not that one" the
sentence is delivered to whatever the answer really is, and the only
symptom at the far end is a document that stayed empty — which reads as a
slow page or a broken room rather than as a sentence typed into somebody
else's window. Two remedies have been aimed at the strip's own idea of
focus, which is the tab that is *drawn*; neither could see this, because
the tree agreed with them both times.

So this stages the failure rather than waiting for it. Chat and a word
processor stand side by side, the keyboard is parked on chat, and the
shell is then made to hold the keys outright — the command palette is up,
which is `chrome_captures_input`, and a press while it is up is taken by
the shell instead of raising the window under it. That is the shape of
every way the beat's one click can fail to move the keyboard: the click
happens, the window does not get the keys, and the beat has no way of
knowing.

What must be true afterwards, whichever way the beat goes:

  * **chat's document does not carry the sentence.** Nothing typed at a
    document may end up in the window beside it. `TEXT` ends in a
    newline, so if the keystrokes reach chat's composer the message is
    sent and chat's own document holds it — this is chat saying what it
    received, not a screenshot being interpreted.
  * and if the beat reported success, the word processor really holds
    the words.

Recovering is the better outcome and is allowed: clicking into the page
dismisses the palette, and clicking again — now that nothing is
intercepting it — raises the window and hands it the keyboard, so the
sentence lands where it was aimed. Refusing is allowed too. Typing blind
is not.

Usage:
  ./typed-words-reach-the-document-proof.py [--keep] [--out DIR]

Exits non-zero when the sentence reached a window it was not addressed
to. Exits 2 when the run could not be staged at all — a proof that could
not look must not report a pass.
"""
from __future__ import annotations

import argparse
import importlib.util
import os
import shutil
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))


def _load_yc():
    """The demo rig itself, imported rather than reimplemented.

    The subject here is `yc.type_into_the_document` — the function the
    film calls — so a copy of it in this file would prove something about
    the copy.
    """
    path = os.path.join(REPO, "scripts", "yc-demo.py")
    spec = importlib.util.spec_from_file_location("yc_demo", path)
    module = importlib.util.module_from_spec(spec)
    sys.modules["yc_demo"] = module
    spec.loader.exec_module(module)
    return module


yc = _load_yc()

#: What gets typed. The trailing newline matters: in the word processor
#: it breaks a paragraph, and in a chat composer it *sends the message* —
#: which is what makes "did this land in the wrong window" a question
#: chat's own document can answer.
TEXT = "Some shared text\n"
PROOF = "Some shared text"


def windows(q) -> list[tuple[int, str]]:
    """Every foreign surface on the strip, with what it announced."""
    out = []
    for group in yc.strip_order(q):
        for w in group["windows"]:
            if w["surface_id"] is not None:
                out.append((int(w["surface_id"]), w["app_id"] or "?"))
    return out


def chat_holds(q, room, needle: str, budget: float = 8.0) -> bool | None:
    """Whether chat's own document carries `needle`.

    `None` when chat never answered, which is not the same as a clean
    document and must not be reported as one.
    """
    reader = None
    try:
        reader = yc.DocumentReader(q.collab_json_sock, str(room), None,
                                   handle="proof-chat",
                                   app_id=yc.CHAT_APP, scope="chat")
        doc, carried = reader.await_document(needle, budget)
    except (OSError, yc.CannotDecode):
        return None
    finally:
        if reader is not None:
            reader.close()
    return carried if doc is not None else None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--keep", action="store_true",
                    help="leave the run directory behind")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    shell_bin = None
    for profile in ("demo", "release", "debug"):
        cand = os.path.join(yc.TARGET, profile, "shell")
        if os.path.exists(cand):
            shell_bin = cand
            break
    if shell_bin is None:
        print("no shell binary under target/; build one first")
        return 2

    out = args.out or tempfile.mkdtemp(prefix="keys-proof-", dir=yc.TARGET)
    os.makedirs(out, exist_ok=True)
    runtime = tempfile.mkdtemp(prefix="qkp-", dir="/tmp")
    run_dir = os.path.join(out, "quinten")
    apps_dir = os.path.join(run_dir, "qos-apps")

    entries = yc.registry_entries(
        os.path.join(yc.MACHINE_APPS, "registry.toml"))
    entries.update(yc.registry_entries(
        os.path.join(yc.LIVE_APPS, "registry.toml")))
    yc.compose_registry(apps_dir, entries)

    # One machine and nobody else in the room: the subject is one
    # session's keyboard, and a peer would only add cursors to it.
    yc.Session.peer_sync = False
    q = yc.Session(
        "quinten", "quinten", run_dir,
        os.path.join(runtime, "q.sock"), shell_bin, yc.RECORD_SIZE,
        os.path.join(runtime, "qrt"),
        yc.stage_home(os.path.join(out, "home")), apps_dir, {})

    failures: list[str] = []
    wrote = False
    try:
        q.start()
        q.settle(1.0)

        chat = q.launch(yc.CHAT_APP,
                        search=yc.STRIP_SEARCH.get(yc.CHAT_APP, "chat"),
                        via="palette")
        wp = q.launch(yc.WORD_PROCESSOR,
                      search=yc.STRIP_SEARCH.get(yc.WORD_PROCESSOR, "word"),
                      via="palette")
        if chat is None or wp is None:
            print("chat or the word processor would not open, so there is "
                  "no pair to stage this on")
            return 2
        q.settle(1.5)

        room = (q.active_workspace() or {}).get("room")
        document = int(wp["surface_id"])
        composer = int(chat["surface_id"])
        print("  on the strip: "
              + ", ".join(f"{sid} ({app})" for sid, app in windows(q)))
        print(f"  the document is {document}, chat is {composer}")

        # Park the keyboard on chat, using the compositor's own
        # navigation. This is the state the strip beat has twice left
        # behind, and it is not itself the bug: a beat that clicks into
        # the document should take the keys back.
        yc.frame_windows(q, [wp, chat], None)
        yc.jump_to_group(q, chat)
        q.settle(0.6)
        if q.keyboard_focus() != composer:
            print(f"  could not park the keyboard on chat "
                  f"(it is on {q.keyboard_focus()}); nothing below is a "
                  f"test of anything")
            return 2

        # And now the shell takes the keys outright, which is what makes
        # this deterministic: while the palette is up every key is the
        # palette's and a press over a window is the shell's, so the
        # beat's one click cannot move the keyboard however well it is
        # aimed.
        q.modifiers(logo=True)
        q.key("k")
        q.modifiers()
        q.settle(0.8)
        if not (q.chrome().get("palette") or {}).get("visible"):
            print("  the palette would not open, so the shell cannot be "
                  "made to hold the keyboard and this proves nothing")
            return 2
        if q.keyboard_focus() is not None:
            print(f"  the palette is up and the seat still hands the keys "
                  f"to {q.keyboard_focus()}; the staging did not take")
            return 2
        print("  staged: the palette holds the keyboard, the document is "
              "the window in front, and chat is the window that had the "
              "keys last")

        shots = os.path.join(out, "shots")
        os.makedirs(shots, exist_ok=True)
        began = time.time()
        wrote = yc.type_into_the_document(q, wp, TEXT, room=room,
                                          proof=PROOF, shots=shots)
        q.snapshot(os.path.join(shots, "after-typing.png"))
        took = time.time() - began
        print(f"  the beat returned {wrote} after {took:.1f}s; the "
              f"keyboard is now on {q.keyboard_focus()}")

        # -- what must hold ------------------------------------------
        query = ((q.chrome().get("palette") or {}).get("query") or "")
        if PROOF.lower()[:11] in query.lower():
            failures.append(f"the sentence was typed into the command "
                            f"palette, whose query reads {query!r}")

        landed_in_chat = chat_holds(q, room, PROOF)
        if landed_in_chat is None:
            failures.append(
                "chat never answered for its document, so whether the "
                "sentence ended up in it was not established — which is "
                "not the same as its not having")
        elif landed_in_chat:
            failures.append(
                f"the sentence reached chat: it was typed at the word "
                f"processor (surface {document}) and chat (surface "
                f"{composer}) holds it")
        else:
            print("  chat's document does not carry the sentence")

        if wrote:
            reader = yc.DocumentReader(q.collab_json_sock, str(room), None,
                                       handle="proof-wp")
            try:
                _doc, carried = reader.await_document(PROOF, 10.0)
            finally:
                reader.close()
            if not carried:
                failures.append(
                    "the beat reported the document was written into and "
                    "the app answers with a document that does not carry "
                    "the words")
            else:
                print("  and the word processor holds them: the beat took "
                      "the keyboard back and the sentence landed where it "
                      "was aimed")
        else:
            print("  the beat refused rather than typing blind")

    finally:
        q.stop()
        shutil.rmtree(runtime, ignore_errors=True)
        if args.keep or not wrote:
            print(f"  run directory: {out}")
        else:
            shutil.rmtree(out, ignore_errors=True)

    if failures:
        print("\nFAILED:")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("\nthe sentence went to the document or nowhere, never to the "
          "window beside it")
    return 0


if __name__ == "__main__":
    sys.exit(main())
