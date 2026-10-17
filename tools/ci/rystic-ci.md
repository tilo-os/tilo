# Native cloud-adapter checks

`rystic_ci.py` separates free preparation from metered execution. It does not
schedule builds, acquire credentials, or invoke the local Cargo routing shim.
The calling CI check should be targeted and non-blocking. A separate manual
strict diagnostic uses the same runner with `--strict`; failures are real
failures, not expected successes.

## Builder and VM contract

Run these operations in order in the native Linux/amd64 test VM. The first
operation runs **real Cargo inside the builder**, with its normal native system
libraries available, not on the submitting desktop:

```sh
python3 tools/ci/rystic_ci.py build --artifacts /target/rystic-ci
python3 tools/ci/rystic_ci.py install-cli --artifacts /target/rystic-ci
```

All three feature-enabled test targets compile before a manifest is written.
Each executable is copied from the exact `compiler-artifact.executable` in Cargo
JSON, not a glob of old binaries. `manifest.json` records executable SHA-256s,
compiler paths, the runner's `harness_sha256`, test inventories and provider pins.
The runner digest is a harness compatibility check, **not** a hash of production
sources. Runtime verifies it against the current runner, checks inventories and
pins against current metadata, and verifies each exact executable's digest.

T owns source binding through source-bound Cargo graph inputs and dependency
outputs. Both the build check and final checks must declare the same transitive
`cargo_packages` closure, with the final checks depending on the build outputs.
Relevant runner, validator and wrapper files must also be explicit inputs. No
whole-repository hash, Git command or expected source SHA is used here. An optional
`T_COMMIT` is recorded as `build_commit` provenance only: unchanged dependency
inputs may validly reuse an artifact built on a different main commit. Main checks
use T's default caching; the weekly sibling disables caching to refresh external
service evidence.
Retain the checkout at its builder path: the strict DOCX test embeds the absolute
path of `crates/system/cloud-source-gdrive/tests/validate_docx.py`. Its digest is
also recorded and checked. Runtime must have the builder's compatible libc and
native shared libraries; these executables are not portable static binaries.
Runtime needs Python 3 and curl (the filesystem fixture uses curl).

CLI installation resolves the official HTTPS release metadata for **0.0.28**,
checks the pinned Linux/amd64 archive SHA-256, and copies only the regular CLI
member without executing an installer or extracting arbitrary paths. It writes
`rystic-cli` and `cli.json`. A parent may stage these files from an already
verified official archive instead; `cli.json` must contain `version`,
`archive_sha256` and the staged executable's `binary_sha256` as emitted by the
installer. Never stage a developer CLI just because it is on PATH.

The caller supplies a fresh directory owned by the VM user, mode 0700, through
`RYSTIC_HOME`, with that check's leased token in `RYSTIC_HOME/token`, mode 0600.
Authentication/lease acquisition and destruction of this private directory belong
to the caller. Do not reuse a developer's home or token. The script never prints
CLI output, token values, license identities, session metadata or test output.
It passes no token in argv and removes `RYSTIC_HOME` from test-process environments.

```sh
RYSTIC_HOME="$private_home" python3 tools/ci/rystic_ci.py run \
  --artifacts /target/rystic-ci --provider all
```

`--provider` accepts only `all`, `gdrive`, or `dropbox`. Preparation validates all
three binary digests and exact test inventories even for a single provider.
Every required test must exist; additions, removals, duplicates, zero tests,
ignored tests and partial artifact sets fail closed. Pinned twins are installed
with actual `rystic pull PRODUCT --version VERSION`; this command verifies the
release download's checksum. All selected pulls finish before any paid start.
Drive is pinned to **0.0.5**, Dropbox to **0.0.4**. No update or latest resolution
is performed. Each loopback HTTP version handshake must match product, version
and the pinned model hash before running tests.

Providers run sequentially, one UUID-named native instance at a time. Startup is
bounded to 60 seconds; each provider's total start/test budget is 240 seconds;
stop/remove share a 30-second cleanup budget. Timed-out subprocess groups are
killed and reaped on timeout or interruption. SIGINT and SIGTERM unwind through
`finally`, attempting both stop and remove for only that owned UUID, even on failed
starts. Cleanup defers cancellation until its shared 30-second budget finishes;
repeated cancellation cannot interrupt it. Cancellation stops provider scheduling,
emits sanitized JSON with `status: interrupted`, and exits 130 (SIGINT) or 143
(SIGTERM). Ordinary assertion failures still allow the next provider to run;
failed cleanup does not.

`run-rystic-ci.sh` forwards cancellation to Python and waits for it before removing
the private credential directory. The caller must also destroy the disposable VM
on cancellation or runner loss: SIGKILL, forced cancellation deadlines and host
failure cannot run Python or shell cleanup. In-process cleanup alone cannot
guarantee that no paid instance survives a hard kill.

The supported inventory is six Drive tests (four adapter and two real filesystem
checks) and three Dropbox tests. Only the two explicitly named native DOCX and
Dropbox ID-replacement contracts are excluded. `--strict` includes both and
returns nonzero for any assertion failure. Nothing is marked skipped or converted
to an expected failure. New tests require an explicit inventory review.

Output is one JSON object with overall status, mode and per-provider
version/model identity, per-test exit/status, and cleanup status. Exit status is
nonzero for failed preparation, assertions, timeouts, identity mismatch, missing
execution evidence or cleanup failure. Preserve this JSON as check evidence;
do not upload the private Rystic home or raw CLI logs.

## Free regression tests

```sh
python3 -m unittest discover -s tools/ci -p test_rystic_ci.py -q
```

These stdlib-only tests fake the subprocess and version-handshake boundaries.
They compile nothing and never start, download or lease a real twin. Existing
manual usage in `scripts/test-cloud-rystic.py` is unchanged. Prior observed test
results and unsupported contract details remain in
[`docs/testing/cloud-rystic-results.md`](../../docs/testing/cloud-rystic-results.md).
