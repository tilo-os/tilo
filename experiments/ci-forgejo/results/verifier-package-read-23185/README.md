# Dedicated read-only verifier package access passes

Actual repository `qos/qos`, [UI run 9689](https://code.podesta.ai/qos/qos/actions/runs/9689),
API run **23185**, job **85073**, attempt **1**, task **75982**.
The single manual dispatch used immutable source
`3fdda564df68190271dc436a0e739ba162f77aac`; the job ran from
`2026-09-19T04:11:01Z` to `2026-09-19T04:11:06Z`.

The probe streamed exactly **6,113,401 bytes** and matched SHA256
`10ad658a452b21545cf2521fa875536da5d951ee8dd509533251a0a3a0517bde`
from the content-addressed package URL:

`https://code.podesta.ai/api/packages/qos/generic/tilo-canonical-verifiers/10ad658a452b21545cf2521fa875536da5d951ee8dd509533251a0a3a0517bde/x86_64.tar.gz`

Authentication used only the newly provisioned `TILO_VERIFIER_BUNDLE_READ_TOKEN`
Actions secret. This reuses existing nonadmin service identity
`qos-packaging-controller`, token **153**, scope **read:package** only. Provisioning
verified its ID/name/scope and matched the decrypted SOPS credential against live
token metadata. Plaintext stayed in process memory and the authenticated HTTPS API
request. No token or credential response body was logged or saved.

This is the same fixed URL, Bearer authentication and hash/size binding as the
production downloader. The public Python container was pinned to
`sha256:1a63a53928ce53d2b0baf08092a703f4840ac5dfbd61fd48802dbf48e08c801e`.
The probe discarded bytes after hashing. No archive extraction, executable launch,
native build, package publication or trust-policy promotion occurred. The
coordinator remains disabled. Historical Actions artifact reads still use the
built-in job token; no broad registry credential fallback was introduced.

The earlier [built-in job-token probe](../verifier-package-token-23176/README.md)
returned HTTP 401 for these same bytes. This follow-up qualifies the narrow service
credential bridge; it does not claim native full-collection acceptance. Production
source change `86bddae5b142eda44d1d99dcd1ec17cbd8312576` has seven passing workflow
contract tests (including negatives for either lane reverting to built-in/broad
credentials), eight passing canonical-bundle tests and passing shell syntax checks.
It remains pending independent review and is not pushed by this probe.
