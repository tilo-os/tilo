# The shape of the film — the seven-beat cut, and why it is not the one

> **Superseded. Do not shoot from this file.**
>
> The film is `docs/demo-script.md`: five beats, and `scripts/yc-demo-live.py`
> drives them. Two beats described below — the registry fetch, and the agent
> noticing that the times contradict the prose — were deliberately cut, and a
> third, the tab group, went with them. Somebody reading this as a shooting
> document walks onto a set expecting beats the rig no longer performs.
>
> It is kept because the argument at the top of it is the one the current film
> still makes, and because the reasons a beat was cut are worth more than the
> beat. Where a section still holds it says so.

## What it has to show that nothing else can — still true

Not "a shared document" — every editor on the market has one of those. The
claim is narrower and stranger:

**One workspace, shared. Every app in it collaborative. An agent participating
in it as an ordinary member.**

So the shot that matters is not two people in a word processor. It is two
people and an agent in a *room*, working in **different apps at the same
time**, where the OS — not the app — is what makes that true. A file manager
that nobody wrote collaboration code for is collaborative because it is in the
room.

Everything in the cut earns its place against that sentence or comes out. The
current film sharpens it to one sentence rather than three: an agent *used the
desktop* — opened an application because it decided it needed one, acted on an
object in it, and put the result in a second application, owning neither.

## The seven-beat budget — the earlier shape

Kept as a record of what was tried. Beat 4 and beat 7's second half are the
two that were cut; the rest were compressed rather than dropped.

| | seconds | what was on screen |
|---|---|---|
| **1** | 0-8 | An empty desktop. Files opens on it. Nothing else. |
| **2** | 8-20 | The collaborators panel. Invite. A name. A code you could read aloud. |
| **3** | 20-28 | They arrive. Xaver gets a cursor. The agent gets a chip and **no cursor** — it never looks at a screen. |
| **4** | 28-40 | *Cut.* An app this machine does not have. Searched, fetched, open in about three seconds — and open on Xaver's machine too. Still built, still works: `yc-demo-live.py --fetch-word-processor`. It is a real network beat with a real wait in it, and a live take is not the place to discover that. |
| **5** | 40-58 | Quinten writes; Xaver is in Files at the same time; his cursor is there, moving. The text lands on his screen. Each person's recent edits are washed in their own colour. |
| **6** | 58-64 | A photograph dragged out of Files into the page. It appears on both machines. Now `yc-demo-insert.py`, filmed separately, not part of the live take. |
| **7** | 64-75 | The agent. An instruction, a caret with its name, a run sheet assembling, **and a note saying the times contradict the prose**. That last part is *cut*: it is the best sentence anything here has produced and it is a second idea, and a film with two ideas makes neither. |

Boot in front of all of it was written here as seven and a half seconds. The
cut that exists is **3.2 s** — see `boot-bumper-cut.md`, which does the
arithmetic against the hundred-second cap.

## The rules that keep it from being overwhelming — still true

**One new idea per beat.** If a beat needs two sentences of explanation it is
two beats or it is cut.

**Nothing on screen that the beat is not about.** No panels opened to show
they exist. No second workspace. No settings.

**One perspective, and cut sparingly.** The current film spends its cuts
differently from the plan above — it holds on Xaver for the instruction *and*
for the result, because he asked for it and watching it land on a machine
where he owns nothing is the stronger claim. But the reason is the same one:
every cut asks a viewer who has never seen this system to work out whose
screen they are now looking at.

**The agent is never a shot.** Its arrival is a chip. Its work is a caret and
a change, and its account of what it is doing is a line in chat. There is no
cut to it because there is nothing to cut to, and that absence is the
argument. The run asserts zero frames came from its session and fails if any
did.

## What was cut, and why — still true, and added to

- **A second workspace / the wheel disc.** A good feature and a different
  film. Showing it costs a beat and teaches a viewer a spatial model they do
  not need to understand anything else here.
- **Sandbox prompts and permission grants.** The most technically interesting
  thing in the system and the least legible in seventy-five seconds. The
  letter can carry it.
- **The terminal.** A prompt on screen argues the opposite of the thesis.
- **Four applications in one tab group.** The most distinctive thing this
  desktop does, and it argues about window management while the audience is
  waiting for the point.
- **The agent disagreeing usefully.** See beat 7 above.
- **The registry fetch.** See beat 4 above.

None of these are wrong and all of them are built. They are cut because the
film is one argument long.

## Driving it

- `scripts/yc-demo-live.py` — the owner drives his own session by hand; Xaver
  and the agent react to what he does. Nothing scripted touches his machine.
  **This is the film**, and its module docstring carries the beats with their
  reasons attached.
- `scripts/yc-demo.py --record` — all three participants performed, nine
  beats, unattended and reproducible. It is a *different, longer cut*: it
  still contains the fetch, the tab group and the run sheet. Useful for
  proving the stack end to end; it is not what gets filmed.

The first is the more honest artefact and the one to prefer when there is time
to shoot it, because every synthesised gesture is a thing that can look
synthesised — and several did, until somebody watched them.
