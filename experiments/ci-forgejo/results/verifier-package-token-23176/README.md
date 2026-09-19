# Built-in job token cannot read the generic verifier package

Actual repository `qos/qos`, API run **23176**, UI run **9680**, job **85048**,
attempt **1**, task **75965**. The only dispatch used immutable source
`a0bde5a5fcebdf435d6bd58a457ba44f2b198c82` and the manual-only
`qualify-verifier-package-token.yml` workflow. No publication or native build ran.

At `2026-09-19T04:06:02Z`, the fixed-origin package GET returned **HTTP 401**
using only `github.token`. No package bytes were read. The exact URL was:

`https://code.podesta.ai/api/packages/qos/generic/tilo-canonical-verifiers/10ad658a452b21545cf2521fa875536da5d951ee8dd509533251a0a3a0517bde/x86_64.tar.gz`

Expected archive size: **6,113,401 bytes**. The tiny job used an anonymous-pull
public Python image pinned to index digest
`sha256:1a63a53928ce53d2b0baf08092a703f4840ac5dfbd61fd48802dbf48e08c801e`.
It reached its Python step normally. The probe uses the production downloader's
fixed URL and Bearer authentication, but streams into a hash without extracting
the archive or executing its contents.

This disproves built-in job-token generic-package access for the current setup.
The separate nonadmin service-account package-read result and built-in-token
historical Actions-artifact read do not establish this permission. In particular,
PR63's `qualify-collection` job currently supplies `github.token` as
`VERIFIER_BUNDLE_READ_TOKEN`, so its package fetch needs a reviewed solution before
allocating a full app collection build.

There was no retry, alternate credential, permission mutation or second probe.
The adjacent JSON records retain exact run/job identity and the sanitized result;
the marker records the timestamped job output. No response body or token was saved.
