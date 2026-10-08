# Historical relay governance source preservation

This source path depends on #3643 at
`d8de3ab482c82d0ff6a0c2b2b36684f4b31af94d` and integrates main
`90a979d5ae74b11f9c206fcf6e3683d465bedff5` without replacing its authenticated
terminal-audit receipt checks or verified fleet cache. The cache retains full
Person certificate-chain checks and permanent identity-scoped revocation.

| PR | Original source head | Preserved contribution |
| --- | --- | --- |
| #2760 | `b13cbf0b286aad8664502251142c52f25f48258b` | Publish root revocations into room membership; restart catch-up and unsettled-change retries |
| #2775 | `380dfc9f37865511405ce9e444da0c6403624021` | Refuse revoked governance and foreign/member overreach while continuing to host retained history |

Both source heads are authored by Quik2007 <quinten@daskju.com>. #2760 is
ancestral to #2775, which is based on #2746 at
`8a61e9f4f1793b2fc74269902d76f261195055af`. The reviewed net source is 21 text
paths with patch SHA-256
`2d66a993b8edc09377c1c763290b3d639f61a9fae7f02b2b12604c8e672d8aec`.
The adoption commit retains the original author and source trailers; published
original branches remain recovery history. It imports the text source without
bringing historical commits rejected by current commit policy into main's
ancestry. No source branch is force-rewritten.

The governance source conflict is resolved by retaining main's authenticated current
admission receipt scenario and all three original multi-member revocation,
refusal and restart scenarios. Focused controls exercise verified bounded
imports, durable restore, expired/revoked media-governor refusal, five real relay
room scenarios and the environment-session observer. No test is ignored or
weakened. All affected peer/relay targets compile; all-target Clippy and
`shell --tests` compile pass on the SSH runner.

#2787 (`19f7982f2afadc2e016a22beddf587fca1e49ac0`) is preserved on the
dependent `codex/historical-governor-succession` source path described below.
#2801 (`ac35d7ac12b1f01ca7269d56bdf02c4e25892770`) remains separate unique
work under merge-lane ownership coordination. Its existing fourteen dirty
worktree paths remain untouched; this repair does not claim its complete
relay-host takeover or forwarding contribution is preserved.

Close an original source PR only after verifying its contribution on landed
main and retaining these exact recovery pointers. Qualification changes only
source and tests, never live identities, access grants, credentials or deployed
trust.

Exact-head fast quality exposed two obsolete launcher test dependencies,
`getrandom` and `threshold-identity`. Both unused edges are removed from the
launcher manifest and its existing lock entry, with no fresh resolution and all
1,747 locked package versions, sources, checksums and other package entries
preserved. CI's exact analyzer mode reproduces both failures on the old manifest
and passes on the repaired one. Locked all-target launcher compilation, launcher
Clippy, required shell test compilation and pinned Hakari diff/dry-run/verify
pass; no ignored-dependency configuration is added.

Advanced main also caches enrollment and certified recipients. The merged
cache reads this account's private person directory, validates its bounded
typed peer list, and changes scope immediately when the person changes.
Enrollment removal retains main's 750 ms recheck bound. Verified recorded and
in-memory root revocations are checked on every query, invalidate recipient
sets, and exclude revoked registered home devices as well as direct peers.
Directory trust failure refuses cached enrollment. The original read-count,
expiry and real paired-editor restart assertions remain intact; fixtures use
the production person-scoped enrollment format. Warm-cache controls cover
permanent future-dated revocation, foreign roots, recipient refusal and dialing.
Temporarily reusing a warm recipient set without rechecking revocations fails
the production LAN assertion with exit 100. Restoring the exact source makes
the same control pass. Main's isolated retained-proof controls and its audio
pump's no-clone assertion also pass.

The merged OS COPY contract retains both projector user units from main and
the historical identity CLI. Its exact delta is two unit rows over the prior
candidate and one CLI row over main, with no removed rows.

The dependent recovery branch `codex/historical-governor-succession` also
adopts #2787's original ten-path text delta (SHA-256
`b562a866b7b3f9683d432fa4384680bdf48bdf1db7ea18ddeade2d0f3b5be198`). Its source
conflict is resolved by retaining main's single shared room admission and its
capability, topology, immutable receipt, coverage, durability and media checks;
succession detection and actor/contest checks wrap that path. The source
adoption retains Quik2007's authorship, original author date and exact recovery
head. Recovery merge `13b0b5b4e134c13e73f8cca9b6908183895f74dd` keeps the
qualified #3660 parent `079fc60ee5c32efd4a56f40de9b48106b86aa7d6` in its
ancestry. The repair is independent of #2801's dirty source.

The production custody regression
`custody_refuses_governance_written_under_another_actor_after_cutoff` exposed
a defect in that recovery: the revoked governor could hide a governance write
under another Automerge actor. Its strict assertion is retained and now passes.
Custody checks each later change against its causal predecessors using read
isolation of already admitted state, without replaying change bytes in the
host. This catches temporary, reverted and concurrent governance changes even
when the final projection matches the signed cutoff. Member content and
unchanged governance echoes remain admissible. Custody also verifies the root's
succession signature and refuses a known revoked successor.

Following requires a successor-authored transition that descends from every
signed cutoff head; its later governance remains valid. Durable restore
rechecks the recorded history whether the caller pins the previous governor
or the successor, while retaining historical certificates after expiry.
Ten focused peer controls and all six original real relay room scenarios pass.
They cover retained replay and clean fallback, forged records, missing cutoffs,
known successor revocation, atomic refusal, two successive handovers and
expired-history restore. Removing either the custody history defense or the
successor-pinned restore defense makes its strict production assertion fail
with exit 100; restoring the exact source makes the focused suite pass.
Published #3660 remains parent-owned at its revocation-only head.
