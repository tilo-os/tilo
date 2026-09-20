# CI completion notifications

The watcher queues results to the **pushing task's session ID**. A child push does
not target the parent session. It does not register with Omnigent's CI endpoint.

Supported events are `Bash`, `exec_command`, and a single literal
`tools.exec_command` call wrapped by `exec` (also `functions.exec`). The nested
`workdir` wins over the outer session directory. Dynamic JavaScript, multiple
calls, shell directory changes and pushes to other refs are intentionally not
interpreted. This is a notification adapter, not a JavaScript or shell runtime.

The process checks the exact HEAD against the origin branch before watching.
An exclusive receipt in the repository's common Git directory under
`codex-ci-watch/` deduplicates the session/commit across repository and global
hooks. Receipts retain the intended task, worktree, commit and reported result.
They contain no credentials. A crashed watcher is not silently restarted: inspect
the recorded process state and CI before removing its receipt. Never delete a
receipt merely because observation timed out.

## Installation scope

The repository `.codex/hooks.json` only covers sessions loading this repository's
hook configuration. It cannot make promises for infra or another checkout.
In particular, the observed Windows user's global hooks configuration was empty;
the infra push had no local hook. No global hook has been enabled by this change.

For cross-repository coverage, install a reviewed, fixed copy of `watch_ci.py`
and `push_event.py` together outside mutable checkouts, and add one global
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
