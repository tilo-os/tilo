# Current-worker streaming review

The production Whisper worker successfully decoded the complete 17.94-second
public German recording under two-CPU affinity, a 1 GiB worker address-space
limit and a hardware-free, network-denied capsule. The final matches all 18
reference words after case normalization. This qualifies that worker fixture,
not desktop latency or universal installed-app support.

The [receipt](dictation-current-stream-review.json) preserves successful and
failed runs, exact model/audio/source/binary identities, memory observations
and public logs. The first nonempty preview in the refined run arrived at
7.762 seconds and was the hallucination “Danke fürs Zusehen.” The first correct
opening arrived at 13.571 seconds. Final text arrived 6.876 seconds after the
recording's source clock ended. Peak sampled resident memory was 375600 KiB;
this is not a whole-desktop memory measurement.

The review probe now accepts production Whisper's full-context cadence when
actual preparation diagnostics are present. It stops scheduling further drafts
when the original source clock ends, waits for every public PCM byte to be
delivered and records source-end-to-final time. The separate Finish exchange
can reuse a just-computed draft and is insufficient as a user-latency metric.
No worker policy, model, compositor actor or production setting changed.

The direct pipe can block the paced producer while inference runs. It differs
from the compositor's bounded capture queue, so these numbers cannot establish
microphone/desktop latency or prove a production scheduling defect. Different
probe schedules also preclude claiming an engine speedup between these runs.

The initial run correctly failed the worker's parent-PID guard. A private PID1
owner launcher corrected the fixture without weakening that guard. Another
run lost its builder worktree and receipt; its outcome is unknown, never PASS.
The cause is unconfirmed. The refined run held the validated owned worktree
lock and saved byte-verified executable copies and receipts outside the build
cache. Its ephemeral copies were removed and capsule absence was observed.

Independent field tests passed 20 selected cases: two native Notes bridge and
identity cases, one public-transcript Notes selection/Undo/Redo replay and
17 lifecycle/refusal cases. These use synthetic or replayed recognition
results, not new microphone capture. Existing eight probe regressions passed.
No Rust source changed for this review; the previously completed shell test
compile check and strict Clippy remain applicable to the production change.
Generic installed legacy field import, physical microphones, older physical
devices, noisy speech and automatic language transitions remain unqualified.
