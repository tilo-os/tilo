# CI completion notifications

The watcher queues results to the **pushing task's session ID**. A child push does
not target the parent session. It does not register with Omnigent's CI endpoint.

Supported events are `Bash`, `exec_command`, and a single literal
`tools.exec_command` call wrapped by `exec` (also `functions.exec`). The nested
`workdir` wins over the outer session directory. Dynamic JavaScript, multiple
calls, shell directory changes and pushes to other refs are intentionally not
interpreted. This is a notification adapter, not a JavaScript or shell runtime.
An output-only `text(result.output)` wrapper is accepted only after the outer
exec cell completes; the exact remote HEAD check remains the push-success proof.

The process checks the exact HEAD against the origin branch before watching.
A nonblocking kernel-held lock in the repository's common Git directory under
`codex-ci-watch/` deduplicates the session/commit across repository and global
hooks. Receipts retain the intended task, worktree, commit and reported result.
They contain no credentials. A later matching invocation can recover a crashed
watcher only after obtaining its released kernel lock, only before delivery began,
and for at most three attempts. It retains the original discovery timestamp.
Elapsed time alone never permits takeover. Crashes during delivery remain
ambiguous and need inspection; they are not retried automatically. Corrupt
receipts also cannot trigger automatic retries, and reported receipts suppress
duplicates during their retention period.
Reported receipts and their locks are removed after seven days; active,
interrupted, failed-delivery and delivery-ambiguous receipts are retained for
recovery or inspection.

## Installation scope

The repository `.codex/hooks.json` only covers sessions loading this repository's
hook configuration. It cannot make promises for infra or another checkout.
In particular, the observed Windows user's global hooks configuration was empty;
the infra push had no local hook. No global hook has been enabled by this change.

For cross-repository coverage, install a reviewed, fixed copy of `watch_ci.py`
and its `push_event.py` and `watch_receipt.py` modules outside mutable checkouts,
and add one global
`PostToolUse` entry using the matcher in the repository configuration. Its command
must use the installed Python executable and an absolute path to that reviewed
copy. Do not resolve the script from an untrusted repository in a global hook.
Preserve other hooks and use normal hook trust; do not bypass it. Existing
repository hooks share the same deduplication receipts with that global copy.

Before describing installation as working, inspect one real hook invocation:
verify its input envelope, target session/worktree/commit, and receipt, then the
single queued completion message. Do not create a second watcher to test it.
`test_push_event.py` preserves the actual PR157 push code and child identity from
the local execution transcript, but reconstructs the hook envelope because no
raw PostToolUse input was retained. Its integration fixture mocks Forgejo and
delivery; passing it is not proof of a deployed hook.

The current watcher discovers runs for 90 seconds and waits at most 25 minutes.
It reports missing runs or timeout explicitly; it does not promise eventual final
delivery after that timeout. Extending this to durable server-side completion is
separate from correcting the observed tool/worktree routing failure.
