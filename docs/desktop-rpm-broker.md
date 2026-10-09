# Desktop RPM isolated signing boundary

Desktop package lanes build and install-test unsigned RPMs without a private key
or OIDC access. A separate protected `desktop-v*` push job requests signed copies
only after both native package lanes succeed. Host-session tests consume those
signed copies on tags; ordinary main/manual host-session verification continues
to consume the original unsigned candidate. Publication requires the same signed
session result. No build, install test or current-head gate is removed.

`dist/rpm/sign-rpms.sh` invokes the public-artifact broker client. The executable
is fixed at `/usr/local/libexec/tilo-desktop-rpm-broker`; it and every parent must
be canonical, root-owned, and not group/world writable, outside the checkout and
job temporary directory. This matches the existing imager broker custody pattern;
it does **not** grant Desktop access to an imager, EFI, PCR or OCI authority.
No private key, key identifier, arbitrary endpoint, signing command or IAM role
is accepted from the job. The native Forgejo OIDC request capability is enabled
only for the separate signing job.

## Operator client protocol

The approved executable receives exactly:

```
tilo-desktop-rpm-broker desktop-rpm REQUEST_JSON RESPONSE_DIRECTORY
```

The request is canonical
JSON plus a newline, schema 1, containing repository `tilo/tilo`, immutable
repository ID 23, protected tag ref, full source SHA, run ID/attempt, architecture,
original same-run `fedora-44-ARCH` artifact name, independently pinned full primary
OpenPGP fingerprint and exactly three RPM names with SHA-256 and immutable-byte
SHA-256. It contains no token or key. The client obtains Forgejo OIDC itself and
transmits it directly to its pinned broker service; stdout must contain no token.

The service must independently verify signature/JWKS, exact issuer/audience,
expiry/not-before, immutable repository and owner identities, protected tag push,
workflow ref/path and source SHA, run ID and attempt, and replay scope. It must
verify authoritative successful same-run native package artifacts and the exact
main verification receipt before signing. Job-supplied context alone is never
signing authorization. Fetch the authoritative artifacts and compare their hashes
to the request; never accept arbitrary caller-uploaded bytes. Maintain an audited,
retry-safe lease scoped to run/attempt/arch/manifest. TLS origin, audience, token
handling, artifact-read credentials and replay persistence belong to the approved
operator client/service, never checkout code.

Only the isolated service holds the dedicated compatible OpenPGP private authority
and runs native RPM signing. Raw EFI/PCR RSA or OCI Cosign signatures cannot stand
in for an RPM/OpenPGP signing identity. Signer provisioning, key creation, key
custody and live server deployment are outside this code change.

The response directory must contain exactly the three signed RPMs,
`tilo-rpm-signing-key.asc`, and `receipt.json`. The receipt has exactly:

```json
{"schema":1,"request_sha256":"SHA256_OF_EXACT_REQUEST_FILE",
 "fingerprint":"FULL_PROTECTED_PRIMARY_FINGERPRINT",
 "signed_sha256":{"RPM_BASENAME":"SHA256_OF_SIGNED_COPY"}}
```

The checkout client rejects symlinks, missing/extra files, oversized metadata,
private-key armor, different manifests/fingerprints, extra primary keys, mismatched
artifact digests and unsigned/wrong-key RPMs. A fresh public-only GnuPG view and
RPM keyring verify every signature. The original RPM lead, immutable header and
payload must be byte-identical: only the aligned signature header may differ.
All three copies qualify before replacing the install-tested RPMs. The public key,
full fingerprint, checksum sidecars and per-architecture request/receipt pairs are retained.
The final release verifier checks their same-source/run binding and every signed
RPM digest again before publication.
No broker stderr is printed because it could contain an OIDC token.

## Exact separate live approval boundary

This code intentionally fails closed until an operator separately approves and
installs the dedicated broker client/service and public verification tools in a
qualified runner image, pins the actual public fingerprint, and protects desktop
release tags. Approval must identify compatible authority/custody/revocation,
exact immutable client image/path, TLS origin/audience, protected-tag OIDC policy,
minimal service-side artifact access, replay storage, audit/cleanup and resource
budget without taking feature-PR capacity. No deployment, credentials, IAM grant,
trust import, key creation, network relaxation or host restart accompanies this PR.

Publish only the approved public certificate and full fingerprint at
`https://tilo.so/security/tilo-rpm-signing-key.asc` and
`https://tilo.so/security/tilo-rpm-signing-key.fingerprint`. A Pi RPM trust import
is another explicit operator action; existing Fedora, OCI, EFI and PCR trust roots
remain intact. Focused tests use synthetic RPM/public-only responses and mocked
cryptographic tool output; they prove control flow and fail-closed checks, not a
live authority or real OpenPGP qualification.
