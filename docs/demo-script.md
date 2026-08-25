# The demo

The film we are building. Five beats, two machines, one agent. Everything
below is what a viewer sees, in order; anything not here is not in the film.
`scripts/yc-demo-live.py` drives it, and its module docstring carries these
same beats with their reasons attached; `docs/demo-shape.md` is the earlier
seven-beat shape, kept as a record and not to be shot from.

Total: **1:40 maximum.** The boot bumper is 3.2s of it, cut and concatenated
in front rather than recorded with the rest; the driven take is ~75s. A beat
that cannot be done inside its share is cut, not hurried.

The film must be drivable two ways, and both must work:

- **by a person**, live, with the other participants driven for them;
- **by the rig**, unattended, so a take can be recorded authentically and
  a voice-over added afterwards.

---

## 1 — Boot

A cold machine reaching a usable desktop. **3.2s**, and it is not filmed with
the rest: it is a cut from a QEMU recording, `films/qos-boot-bumper.mp4`,
joined on the front by `scripts/demo-assemble.py`. Console, greeter, filled
form, and it ends before the desktop. See `boot-bumper-cut.md` for the cut
and the arithmetic behind the 3.2.

## 2 — An empty workspace, and three applications opened into it

Chat, then Files, then the word processor — **in that order**, each opened
the way a person opens one: the key, a few letters, the row.

The order is the point. It is how somebody actually sets a workspace up,
and the earlier cut had it backwards — one application, then invitations
into a room with nothing in it, then everything else arriving later with
no explanation of why.

Each app is opened through search rather than by address. Opening by
address is a window appearing out of nothing, and a viewer has to be told
what happened. The search shows it, and shows that the desktop has one way
in rather than a menu per application.

## 3 — Inviting people

Xaver, then Claude. Both admitted the same way: by holding a capability in
the same list. Claude is marked as an agent so nobody has to infer which is
which.

Xaver's screen is only shown **once the applications have loaded on it** —
a cut taken the instant he joins shows an empty desktop, which argues the
opposite of what the beat claims. What the beat claims is that the
workspace is the shared thing, so joining it brings what is in it.

## 4 — Cut to xaver: one instruction, from the person holding the thing

Xaver **right-clicks the image in Files** and writes:

> `Claude: make this square and put it in the open document`

One instruction. Not two, not one per app. The rig prints this line for
whoever is at xaver's keyboard, out of `CROP_INSTRUCTION` in
`scripts/yc-demo.py`, so what is on the terminal and what is in this file
cannot drift apart.

`Claude:` rather than `@claude`. Both address the agent; a mention is a
structured range something has to mint at send, while the name prefix is
matched on the text itself and therefore works in a chat message and in a
document paragraph alike. The line a person is told to type has to be the one
that works wherever they type it.

He is not the host, he opened none of these applications, and he is on
another machine. What he sends is the object's **address**, not a
description of where it sat on his screen — so the agent works on the
thing he pointed at, and the sentence still means the same thing in a
session opened tomorrow.

## 5 — Still on xaver's screen: and he watches it happen

Claude opens the image editor, does the work, and the picture makes a
visible crossing into the word processor — a smooth hand-off, not two
windows independently changing.

On **xaver's** screen, not the owner's. He asked for it, so he sees it
land, on a machine where he owns nothing and opened nothing. That is a
stronger claim than watching it arrive where it was already going, and it
costs a cut rather than adding one.

Between the instruction landing and the first change appearing is the model
deciding, and it has been measured at some twenty-eight seconds. That is not
going to get shorter, so the camera moves — inside xaver's strip, not another
cut — to **chat**, where the agent posts its own state as each thing actually
happens: taken, waiting on an app, doing, done. A wait filled by the agent
saying what it is waiting for is not dead air, and it costs nothing to shoot.

The video is finished after this. Nothing more is needed.

---

## Cuttable tail

**Per-actor undo.** Ready to be cut if it does not look or work properly.
Not the main focus.

---

## How it is shot

These decisions outlived the structure they were written for. They were
settled while the film was nine beats, and they are about how a viewer
reads a cut rather than about which beats there are, so they still hold.

**A caption names whose screen this is**, at the bottom of frame, on every
segment: the person and their relationship to the room. Two machines
running the same OS are otherwise indistinguishable across a cut. It is
drawn into the recording, not added to the shell — a caption, not desktop
chrome.

**The agent has no cursor and no cut.** A mouse pointer is an instrument
for pointing at something you are looking at, so giving one to an agent
that never looks at a screen contradicts the claim this film exists to
make. Its presence is a caret in the document at the moment it acts, and
its arrival is a chip in the roster. There is no shot of its "view"
because there is nothing to shoot — and that absence is the demonstration.
The run asserts zero frames come from its session, and fails if any do.

**Cut sparingly, and only where the cut proves something.** Beats 4 and 5
are both on xaver's screen: he instructs, and he watches it happen. One
cut, held. An earlier version switched at nearly every step, and a viewer
spends each of those re-orienting rather than watching; a later one cut
back for beat 5, which asked the viewer to change seats in the middle of
the payoff.

**The boot bumper ends where the desktop arrives**, not after it. The film
opens on an empty desktop, so holding one first shows the same picture on
both sides of a cut. See `boot-bumper-cut.md`.

## Standing constraints

- **Strip legibility.** It must always be clear what part of the
  horizontal strip is on screen, and how we got here: smooth movement at
  sensible moments, never random jiggling.
- **Not overwhelming.** The film makes one argument. A beat that adds a
  second argument is cut.
- Nothing on screen is fake. Apps start empty; no invented content.
- The compositor owns window chrome; apps draw no title bars.

## What the film is arguing

Collaboration lives *below* the applications. Three people-or-agents, four
applications, one workspace — and the thing that crosses between them is a
resource with an address, not a screenshot and not a description. That is
why an agent can be handed an object by someone who does not own it, in an
app they did not open, on a machine they are not sitting at.

That is the claim others cannot make. Everything in the film exists to
show it, and nothing in the film exists for any other reason.
