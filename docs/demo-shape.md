# The shape of the film

Seven and a half seconds of boot, seventy-five of demo. Under 1:40 with the
title. Everything below is a budget, not a wish: a beat that runs over takes
its seconds from another beat, not from the total.

## What it has to show that nothing else can

Not "a shared document" — every editor on the market has one of those. The
claim is narrower and stranger:

**One workspace, shared. Every app in it collaborative. An agent participating
in it as an ordinary member.**

So the shot that matters is not two people in a word processor. It is two
people and an agent in a *room*, working in **different apps at the same
time**, where the OS — not the app — is what makes that true. A file manager
that nobody wrote collaboration code for is collaborative because it is in the
room.

Everything in the cut earns its place against that sentence or comes out.

## The budget

| | seconds | what is on screen |
|---|---|---|
| **1** | 0-8 | An empty desktop. Files opens on it. Nothing else. |
| **2** | 8-20 | The collaborators panel. Invite. A name. A code you could read aloud. |
| **3** | 20-28 | They arrive. Xaver gets a cursor. The agent gets a chip and **no cursor** — it never looks at a screen. |
| **4** | 28-40 | An app this machine does not have. Searched, fetched, open in about three seconds — and open on Xaver's machine too. One brief cut. |
| **5** | 40-58 | **The shot.** Quinten writes; Xaver is in Files at the same time; his cursor is there, moving. The text lands on his screen. Each person's recent edits are washed in their own colour. |
| **6** | 58-64 | A photograph dragged out of Files into the page. It appears on both machines. |
| **7** | 64-75 | The agent. An instruction, a caret with its name, a run sheet assembling, a note saying the times contradict the prose. Never a cut to it, because it has no screen. |

## The rules that keep it from being overwhelming

**One new idea per beat.** Beat 2 is invitation. Beat 4 is arrival-without-
installation. Beat 5 is simultaneous work in different apps. If a beat needs
two sentences of explanation it is two beats or it is cut.

**Nothing on screen that the beat is not about.** No panels opened to show
they exist. No second workspace. No settings.

**One perspective.** Quinten's screen carries the film. Exactly one excursion
to Xaver, in beat 4, where it proves the window really crossed. Beat 5 shows
his participation *on Quinten's screen* — his cursor, his colour, his edits —
which is stronger than cutting to him, because it is the same frame.

**The agent is never a shot.** Its arrival is a chip. Its work is a caret and
a change. There is no cut to it because there is nothing to cut to, and that
absence is the argument.

## What was cut, and why

- **A second workspace / the wheel disc.** It is a good feature and it is a
  different film. Showing it costs a beat and teaches a viewer a spatial model
  they do not need to understand anything else here.
- **Sandbox prompts and permission grants.** The most technically interesting
  thing in the system and the least legible in seventy-five seconds. The letter
  can carry it.
- **The terminal.** A prompt on screen argues the opposite of the thesis.

## Driving it

Both routes produce the same film:

- `scripts/yc-demo.py --record` — all three participants performed. Reproducible,
  and the one that can run unattended.
- `scripts/yc-demo-live.py` — the owner drives his own session by hand; Xaver
  and the agent react to what he does. Nothing scripted touches his machine.

The second is the more honest artefact and the one to prefer when there is time
to shoot it, because every synthesised gesture is a thing that can look
synthesised — and several did, until somebody watched them.
