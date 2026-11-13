"""The person's answers to the questions a session puts on its sheet.

A service or the agent door asks the person a question; the portal sheet
(`so.tilo.portal-sheet`) shows it, and the person presses one of its controls: a
choice ("Allow once", "Allow for this session", "Always allow"), the sheet's
Allow that answers with the choice highlighted, or its Deny. Everything here
presses those controls by the names an assistive technology reads
(`list_targets`, the desk's own named controls), never a provider of the old
shell's: the choice and the Allow are the person's answer, and Deny is their
refusal.

The door's admission is one of these questions (`session.agent-door`), as an
app's access question is (`portal.access`), so every check that needs the person
to answer one answers it here.
"""

from __future__ import annotations

import json

from .core import CheckFailed, Unobservable
from .vm import VmError, wait_until

# The sheet that asks, as the person sees it.
SHEET = "so.tilo.portal-sheet"
# The sheet's choices, as the session numbers them (session questions.rs `admitted`, and
# tilo_portal::consent for an app's access question).
ALLOW_ONCE, ALLOW_SESSION, ALLOW_ALWAYS = 0, 1, 2
CHOICE_LABELS = {ALLOW_ONCE: "Allow once", ALLOW_SESSION: "Allow for this session",
                 ALLOW_ALWAYS: "Always allow"}
# What the sheet's controls are called, in any question it asks.
SHEET_WORDS = ("Allow", "Deny", "Always allow")
# The sheet's refusal, and the words its Allow may go by: a sheet whose choices are
# highlighted answers on its Allow, a review on its Confirm.
DENY_WORD = "deny"
ALLOW_WORDS = ("allow", "confirm", "continue")
# How long the person may take to answer, from the moment the question is asked.
SHEET_S = 60


def sheet_targets(ctx) -> list[dict]:
    """The controls the person can press on the sheet up now (`list-targets`, the desk's
    named controls, as an assistive technology reads them), or [] when no sheet is up."""
    try:
        targets = ctx.vm.targets()
    except VmError:
        return []
    names = [t for t in targets if any(w.lower() in str(t.get("name", "")).lower() for w in SHEET_WORDS)]
    return targets if names else []


def portal_dialog(ctx) -> dict | None:
    """The question on the sheet, as far as the desk shows it: its controls."""
    targets = sheet_targets(ctx)
    return {"targets": [t.get("name") for t in targets]} if targets else None


def sheet_rows(ctx) -> list[dict]:
    try:
        return [r for r in ctx.vm.actions(programmatic=True) if r.get("application") == SHEET]
    except VmError:
        return []


def press(ctx, name: str) -> None:
    reply = ctx.vm.raw({"op": "click_named", "name": name})
    if isinstance(reply, dict) and reply.get("err"):
        raise CheckFailed(f"pressing {name!r} on the sheet was refused: {reply}")


def _up(ctx, what: str) -> list[dict]:
    """The sheet's controls, once the person is asked; what it showed is written and shot."""
    dialog = wait_until(lambda: portal_dialog(ctx), SHEET_S, interval=0.5)
    if dialog is None:
        raise CheckFailed(f"{what}: the person was never asked (no question on the sheet within {SHEET_S} s)")
    targets = sheet_targets(ctx)
    ctx.write(f"sheet-{what}.json", json.dumps({"targets": targets, "rows": sheet_rows(ctx)}, indent=1))
    ctx.shot(f"sheet-{what}")
    return targets


def _gone(ctx, what: str) -> None:
    if wait_until(lambda: portal_dialog(ctx) is None or None, 15, interval=0.5) is None:
        raise CheckFailed(f"{what}: the question stayed up after the person answered it "
                          f"({[t.get('name') for t in sheet_targets(ctx)]})")


def answer_sheet(ctx, choice: int, what: str) -> dict:
    """Answer the question up on the sheet with `choice`, as the person does: press the
    choice, then the sheet's Allow if it asks for one. What the sheet showed."""
    targets = _up(ctx, what)
    label = CHOICE_LABELS[choice]
    chosen = [t["name"] for t in targets if label.lower() in str(t.get("name", "")).lower()]
    if not chosen:
        raise Unobservable(f"{what}: the sheet shows no control {label!r}: {[t.get('name') for t in targets]}")
    press(ctx, chosen[0])
    # A sheet whose choices are highlighted first answers on its Allow.
    gone = wait_until(lambda: portal_dialog(ctx) is None or None, 3, interval=0.3)
    if gone is None:
        confirm = [t["name"] for t in sheet_targets(ctx)
                   if str(t.get("name", "")).strip().lower() in ALLOW_WORDS]
        if confirm:
            press(ctx, confirm[0])
    _gone(ctx, what)
    return {"targets": [t.get("name") for t in targets], "chosen": chosen[0]}


def deny_sheet(ctx, what: str) -> dict:
    """Refuse the question up on the sheet, as the person does: press its Deny."""
    targets = _up(ctx, what)
    deny = [t["name"] for t in targets if str(t.get("name", "")).strip().lower() == DENY_WORD]
    if not deny:
        raise Unobservable(f"{what}: the sheet shows no Deny control: {[t.get('name') for t in targets]}")
    press(ctx, deny[0])
    _gone(ctx, what)
    return {"targets": [t.get("name") for t in targets], "chosen": deny[0]}


def no_sheet(ctx, seconds: float) -> dict | None:
    """The question that came up within `seconds`, or None."""
    return wait_until(lambda: portal_dialog(ctx), seconds, interval=0.5)
