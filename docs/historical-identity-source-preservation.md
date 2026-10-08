# Historical identity source preservation

This integration carries the reviewed identity CLI, delegated signing, enrollment
privilege separation, per-person peer storage and private LAN discovery source
onto current main. The original published contributions remain recoverable:

| PR | Original source head | Contribution |
| --- | --- | --- |
| #2466 | `1895af1a1fe66ba5a51f822e6df970a2d5211c2b` | Identity CLI |
| #2746 | `8a61e9f4f1793b2fc74269902d76f261195055af` | Certified device delegation |
| #2764 | `d192c75e4808045e8a60d3ee951ea9a6a64f7f52` | Enrollment privilege separation |
| #2766 | `4ba2869493eb8a60d4f995dc0c8a87534ac2e54a` | Per-person peers, rooms and revocations |
| #2615 | `c2bf5df33a5ad97ef5de2713968d6290653b7d0c` | Private LAN discovery |

All five source heads are by Quik2007 <quinten@daskju.com>. The local recovery merge
`a62b9db257c69b4a77cff0eea2d0da27f3f141d6`, on
`codex/historical-identity-source-chain`, contains all five heads as ancestors and
has tree `ddd41a93f59c4539fbc3807a4096420e25530156`. Its reviewed net delta from main
`caac70cdb154a244559e9e0e02c9b6bbce2edfd2` contains 91 text-only paths, with patch
SHA-256 `76ca695c0cae600e4dc41965f31fe246c0b47f9aa946c5af6b7f51b62270cfb8`.
That delta applies cleanly onto main `00f3841e30d0e37b866060838409481b6491fe81`.

The combined integration adds targeted qualification repairs to this source:
per-person transport fixtures, an enrollment governance refusal control, Android
atomic no-clobber publication, Wasm directory creation, locale completeness, the
exact identity-CLI OS payload contract and the stricter portability count. A
bounded LAN answer table preserves exhausted budgets under source-address floods.
The original branches and local recovery merge are retained; no forced rewrite is
required. Historical non-text fixture changes outside this net source delta are
excluded under the repository's commit-content policy.

The fourteen files evolved between #2764 and #2766 were reviewed for irreversible
UID/GID separation before home access, account-verified supplementary groups,
no-follow descriptor access, bounded single-link files, exclusive staging and
commit claims, person/identity binding and requester liveness. Sticky shared
parents and private per-person file modes remain. Root-only enrollment, boot-key
certification, unlock policy and secret export refuse on joined devices.
Revocation is permanent and scoped to the signing person's identity.

Before closing any source PR, verify the landed combined source on main and keep
these recovery identifiers. The original source heads are evidence pointers;
the combined commit deliberately starts from current main instead of importing
historical commits rejected by current commit-message and content policies.

The Imager restart fixture consumes the existing main-health owner's exact source
from `5f20a0c8c182323009f7709026e2594b007c09b0`
(`fix/native-imager-restart-ownership`), including its ownership-release observer.
This lane does not maintain an independent retry-loop repair. Privileged person
process tests fail if the required runner account cannot be observed.

Main `ab64378df96da444c32bcf54a0480d8353942d52` is merged before
publication, preserving its newer shell authority code. It already includes the
Imager owner's repair, so that repair adds no independent PR delta.

Delegated ownership now reaches the identity, sealing and file helpers from
the shared desktop model. They join `.config/hakari.toml`'s documented
exclusions, so the model built alone does not acquire the generated union's
Tokio dependency. The production dependency boundary checker is unchanged.

Main `7ef34fb12edbb605ff447634dda1e46b3f50031a` is also integrated. Its protected
private messaging transport and account gates are retained, with both newly
added revocation call sites using permanent identity-scoped checks. A production
certificate-verification control refuses even a future-dated root revocation
and foreign-root entries; unrevoked certificates still pass.
