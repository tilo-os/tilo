# Shell Action capabilities: implementation and qualification

The approved specification was implemented by GPT-5.6 Sol at high reasoning,
with implementation decisions delegated to the agent. Independent scoped
reviews and a combined final review are clean after their corrections.
Final native acceptance passed on rebuilt binaries with stable relevant source inputs.

## User-visible behavior

- JSON and native socket invocations retain asynchronous admission tickets,
  return the broker's real identity, preserve response order, and propagate
  failure. Slow admission and partial writes do not block other connections.
- Shell contracts enforce current authority and explicit confirmation for
  consequential operations. Queued prompts remain pending until applied;
  duplicate-tab failures reach callers.
- External text edits name a canonical record target. Focus changes cannot
  redirect them, and denied/stale targets do not mutate records. Allowed edits
  use the record-owned action frame, attribution, and undo boundary.
- Brightness, volume, Wi-Fi enabled state, power profile, and named VPN state
  share typed dispatch between UI and external invocation. Mechanism absence
  and provider failures are explicit.
- Photos rotation/crop remain deterministic document edits. Explicit export
  pins the addressed source/edit snapshot, renders and verifies immutable PNG
  bytes, and returns exactly one canonical content resource. Writer consumes
  that actual next-step output; invalid or intervening results cannot reuse
  an old image.
- Both immediate broker mutations and later asynchronous lifecycle updates
  refresh discoverable action surfaces without unrelated native input.
- Standalone runtime receipt writes retain their persistence destination;
  restart/readback preserves redacted receipts separately from live output.

## Verification

The implementation report records exact commands, intermediate failures,
corrections, process sessions, and scoped source ownership. Accepted coverage:

| Path | Evidence |
| --- | --- |
| Linux native socket | Real broker directory admission, ordering, timeout/full disconnect, write half-close, and partial response framing regressions passed |
| Shell contracts | Provider suite 27/27 on host and Fedora; actual palette confirmation, delayed prompt, and record authority-revocation regressions passed |
| Addressed edits | 11/11 on host and Fedora, including exact grants, attribution, generation/undo and changed focus |
| Quick Settings | Shared UI/external dispatch and independent availability checks passed; controlled providers used |
| Photos export | Edited-content Effect tests 2/2; actual broker next-step Agent integration 1/1 |
| Agent handoff | Runtime-owned real-session sequence suite 9/9 |
| Receipt persistence | Live broker redaction and fresh-session restart/readback passed on host/Fedora |
| Action publication | Actual collaboration-event Accepted-to-terminal regression 1/1; semantic UI suite 31/31 |
| Build | Host shell library check passed; final Fedora `cargo check -p shell --no-default-features --tests` passed in 22.84 seconds with matching final app source |

## Final native acceptance

`target/edited-handoff-proof/native-typed-20260916-final4-manifest.json`
records runtime success, unchanged binary hashes, and all 1,122 relevant build
inputs unchanged through the build and run. A separate successful source gate
preceded GUI launch. The proof uses typed SDK invocation and verifies the exact
clockwise pixels, immutable PNG bytes/BLAKE3 hash, signed Writer destination,
real producer/consumer denials, missing bytes rejection, and persisted receipt
output redaction. Two signed document events were independently checked.

The 48×32 fixture becomes 32×48. Its source content hash is
`ce93243f53be9df13c7a1df4bc8d6b53630844187158a5e02b631fa3914424e1`;
the exported edited hash is
`ad9c247dd0ebc0ca17aa8a8ad122f707d17242ed2851e074106b515acb17717e`.

Earlier failed or qualified runs remain preserved: one exposed the stale
publication defect; later runtime passes had concurrent source changes and
were not accepted as stable-source qualification. Final4 supersedes those
qualification gaps without weakening assertions.

## Boundaries and ruling

Ruling: qualify the existing production headless Agent with actual broker
sequence/integration tests, and Photos/Writer through native production App
execution. Photos `--headless-drive` is a finite UI harness without the shared
broker lifecycle. Adding another interpreter would contradict the spec;
extracting the shared host lifecycle is separate architecture work. Cost if
wrong: consumers requiring a headless Photos service need that additional work.
No Photos headless parity is claimed.

The evidence does not establish Windows portability, cross-sandbox isolation,
exactly-once external effects, invocation-ID retry, or power-loss durability.
The default-feature host build remains limited by the existing FFmpeg API
mismatch. The full shell tests target Linux-only compositor code and do not
compile as all-target tests on macOS; Fedora supplies that qualification.
Unsupported macOS Quick Settings mechanisms are unavailable rather than
simulated. No actual user hardware settings were changed for verification.
