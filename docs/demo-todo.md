# Demo video — the issue list

Every known defect between here and a finished video. Updated 2026-08-23 13:47,
from the 13:31 recorded run plus frames read by hand.

**An item is done only when a frame shows it.** Code landing is not done — that
distinction is the whole reason this file exists.

## Blocking the shoot

| # | Issue | State |
|---|---|---|
| 1 | Beats 2 and 3 do not use the panel's Invite. The script opens the panel, prints a hardcoded gap claiming no Invite row exists, closes it, and invites by address — so the flow the owner asked for is never on camera | fixing |
| 2 | Stale gap: "joining has no semantic op — the token had to be typed". Paste and Join are both addressable and work | fixing |
| 3 | Stale gap: "the agent cannot change the shared document by address". It can; that text describes a different call | fixing |
| 4 | A window sits 788px past the right edge, and "both windows wholly on screen" fails | fixing |
| 5 | Every other `gap()` in the script is unaudited and may be equally stale | fixing |

## Fixed in code, not yet shown on screen

| # | Issue |
|---|---|
| 6 | A session painted its own name as a peer, over its own document |
| 7 | Placeholder read `dev.qos.file-explorer / waiting for first frame` — now a card with the app's name and a quiet indicator |
| 8 | Windows stacked vertically instead of scrolling horizontally |
| 9 | The roster never listed a peer who joined |
| 10 | Invited and joined looked identical — now an empty seat that fills in place |
| 11 | The invite card showed a 900-character base64 wall |
| 12 | The host's roster showed `peer@…` rather than a name |
| 13 | The Files breadcrumb exposed our own source tree |
| 14 | Files reported "0 items" on a populated home |
| 15 | Files rows responded ~17px above where they were drawn |
| 16 | A `ui` app could not start a drag at all |
| 17 | The drag chip said "Image" rather than the filename |
| 18 | The agent adopted the file manager's document and died on a merge collision |
| 19 | Typed capitals were silently lost on the client keycode path |

## Known, deliberately not fixed

| # | Issue | Why |
|---|---|---|
| 20 | Under load a window shows "This app isn't responding", then the app appears | 10s backstop; the final shoot runs on a quiet box |
| 21 | `execute_snapshot_*` aborts the whole test process | wgpu reads a thread-local during teardown after it is gone; pre-existing, unrelated, and it makes a full-suite green unobtainable for anyone |
| 22 | Apps are 4.16 MB against the letter's "<2 MB" | shared toolkit landed (24.8 → 4.36 MB). The rest is spellcheck's 708 KB embedded word list and moving qdoc into the shared object |
| 23 | A peer's *external* window shows a placeholder rather than streamed pixels | out of scope; every app in this demo is ours |

## Needs the owner

| # | Item |
|---|---|
| 24 | The drag take, driven by hand. Staging is verified in dry-run. Ask only once everything above is clear |

## Rules learned the hard way

- **A stale binary invalidates a run.** Five verifications died this way, twice with an agent judging its own fix against a build that predated it. The rig now refuses to record against one.
- **A stale *payload* is worse.** Rebuilding breaks the hardlink the registry relies on, so the old app is served silently. Copy payloads explicitly.
- **The git index is shared.** Never `git commit` without pathspecs — it sweeps up other agents' staged work.
- **`git add <file>` is not isolation** when two agents are inside one file. Split the diff and stage your own hunks.
- **`TaskStop` does not stop a distrobox-routed build.** The wrapper dies; the container-side cargo keeps compiling and holding memory.
- **A gap describing a limitation that has since been fixed is worse than no gap** — it reports a working feature as broken, and has already sent people to "fix" things that worked.
- **Twelve times in two days the test or the claim was wrong rather than the code.** Make a check fail for the right reason before believing it passes.
