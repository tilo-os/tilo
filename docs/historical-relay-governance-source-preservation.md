# Historical relay governance source preservation

This source path depends on #3643 at
`d8de3ab482c82d0ff6a0c2b2b36684f4b31af94d` and integrates main
`404343fc7d0254e73153a8aa832bc77309f4dc37` without replacing its authenticated
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

The only source conflict is resolved by retaining main's authenticated current
admission receipt scenario and all three original multi-member revocation,
refusal and restart scenarios. Focused controls exercise verified bounded
imports, durable restore, expired/revoked media-governor refusal, five real relay
room scenarios and the environment-session observer. No test is ignored or
weakened. All affected peer/relay targets compile; all-target Clippy and
`shell --tests` compile pass on the SSH runner.

#2787 (`19f7982f2afadc2e016a22beddf587fca1e49ac0`) and #2801
(`ac35d7ac12b1f01ca7269d56bdf02c4e25892770`) remain separate unique work.
#2787's previous-governor-actor cutoff needs the arbitrary-actor governance
history preservation from #2801 before a complete takeover can be qualified.
The existing dirty #2801 worktree is untouched pending ownership coordination.
Neither succession PR is established as preserved by this revocation adoption.

Close an original source PR only after verifying its contribution on landed
main and retaining these exact recovery pointers. Qualification changes only
source and tests, never live identities, access grants, credentials or deployed
trust.
