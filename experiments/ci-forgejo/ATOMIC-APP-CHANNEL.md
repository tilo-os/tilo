# Atomic application channel pilot

The current application publisher changes architecture URL/digest repository
variables in separate API calls. A reader can observe mixed old/new values, and
an older publisher can overwrite a newer result. This experiment replaces that
logical operation with one Git ref pointing to one complete JSON manifest.

`tools/atomic_app_channel.py` is **private-fixture only**. It hardcodes repository
`qos/ci-workflow-pilot-20260918-9245-repeat` and the separate branch
`probe-atomic-app-channel-20260919`; it cannot target product refs. Both fetch and
push URLs must match. Every mutation rechecks repository privacy. Ordinary
non-force pushes use an explicit refspec and disable following tags. No package
uploads, main edits, repository-variable writes or Actions jobs are involved.

## Contract

Each channel commit contains only `app-channel.json`. Its sole parent is the
expected old channel commit. The manifest includes generation, source commit,
previous channel commit and per-architecture immutable artifact URL, SHA256,
artifact commit, source SHA and runtime version/root/publisher identity.

Preparing a candidate validates each receipt against actual immutable Git blob
bytes, including unchanged architecture entries. The pilot's tiny JSON payloads
are explicitly marked `nonpublishing-channel-fixture`; they are not real apps.
Artifact commits must descend from their declared source. URLs bind full commit
SHAs; branch names, arbitrary hosts and product package endpoints are rejected.

Promotion validates the candidate again, confirms its sole expected parent and
the current remote head, then sends one ordinary push. Two sibling candidates
cannot both fast-forward the ref. A rejected candidate is never rebased/retried.
A lost response triggers a readback; if that read fails the outcome is uncertain.
`not-current` means only that the candidate is not the head at readback; a later
valid promotion might have followed it. It is not evidence the candidate never
landed. Inspect history before deciding what happened.

A generation must increase **and the new source must descend from the previous
source**. A numerically larger generation cannot authorize a source rollback,
even in a new process after restart. Intentional rollback requires a separate
reviewed policy; this pilot provides no bypass.

Partial architecture promotion preserves every untouched receipt byte-for-byte.
The channel's top-level source describes its latest promotion, not a claim that
all architectures use that source. Each architecture's own source/runtime fields
remain authoritative. Release policy can require a complete matrix before a
stable channel promotion; this prototype does not silently invent that guarantee.

## CLI and verification

Clone the explicitly named private fixture to a separate local bare repository.
By default, a promotion JSON is validated and a candidate commit is prepared only
in that local object database. No remote state changes until the explicit flag:

```sh
python tools/atomic_app_channel.py --git-dir /local/pilot.git \
  --expected-head FULL_CURRENT_CHANNEL_SHA --promotion proposal.json
python tools/atomic_app_channel.py --git-dir /local/pilot.git \
  --expected-head FULL_CURRENT_CHANNEL_SHA --candidate PREPARED_COMMIT_SHA \
  --apply-private-fixture
python -m unittest discover -s tools/tests -p test_atomic_app_channel.py -v
```

Proposal JSON contains `source_sha`, integer `generation`, and `architectures`
mapping the architecture updates to complete immutable receipts. Initialization
requires a deliberately seeded channel branch at the source commit, generation 1.
Five tests use a real temporary bare Git server: concurrent sibling pushes with
exactly one winner, partial architecture preservation, larger-generation stale
source rejection after restart, invalid artifact digest rejection, lost push
response reconciliation without retry, and product push URL rejection.

## Production path

Keep this tool's private gate intact. Production activation needs a separately
reviewed adapter: consume verified immutable package publication receipts, enforce
artifact availability/retention, validate package/runtime signatures and trust,
then construct this manifest and perform the same expected-parent non-force
promotion. Update consumers to read one manifest at its resolved commit, select
their architecture entry and verify its digest; do not split the manifest back
into independently mutable repository variables.

Protect the channel ref from force pushes/deletion and restrict its sole writer.
The prototype's ancestry guarantee assumes other actors do not rewrite history.
The packaging coordinator must record the promotion commit as part of successful
publication before advancing its packaged baseline. Content upload can resume by
digest independently; one final manifest update makes the whole channel visible.
This does not replace dependency planning or introduce a new CI execution engine.
