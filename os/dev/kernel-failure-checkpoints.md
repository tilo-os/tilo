# Failed kernel make checkpoints

A run-specific BuildKit cache retains scanned public source and object files when
`make` exits unsuccessfully. The compile step keeps its original failure status;
an independent export target publishes a separately named, non-installable partial.
Regular make failures are covered. Cancellation, worker loss, or an outer timeout
can prevent capture/export and are not durability guarantees.

Capture is capped at 8 GiB of regular file bytes, 250,000 entries and a 300-second
scan/archive/readback budget, with 8 GiB scratch reserve. The size cap is an
operational limit, not an estimate that every future kernel tree fits. Exceeding
it reports the bound reason and preserves the original make failure; no ready
marker or successful partial receipt is produced. Blocking filesystem operations
cannot be preempted by the per-chunk deadline checks.

Private module keys, generated certificates and aggregate link outputs are
excluded. Every archive file is independently read back and hashed. The result
remains `resume_admitted=false` and `writer_audit_completed=false`.

Future admission must independently validate the recorded parent approval and
its source-correction lineage. In particular, a checkpoint made after the CN20K
fix already contains that patch; a consumer must not blindly apply it again.
The current resume verifier does not automatically admit these new partials.
