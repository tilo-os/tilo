# Installed data consumer capabilities

The installed consumer producers and checks are compiled into the
`prepared-setup` helper (`crates/platform/imager-backend/src/installed_consumers`).
The image build runs each producer once as root; admission
(`home_admission`, `applications_admission`, `account_admission`) runs the
matching read-only check in-process before any target custody write. No mode
accepts paths, alternate helpers, saved evidence inputs, passwords or
environment configuration. Standard output is the closed Rust
`home_admission` / `applications_admission` JSON object, below 4096 bytes
(the account receipt stays below 16 KiB). Failures print one diagnostic line
to standard error and exit with status 1.

* `/usr/libexec/tilo-prepared-setup --verify-home-consumer-capability` produces
  the Home receipt; with `--check` it executes the installed helper's
  `--home-consumer-capability` probe and the installed seed helper's
  `--capabilities`, rehashes the actual helper, firstboot-user, seed helper and
  GNU tar, and compares the protected receipt.
  This is **consumer protocol admission**, not a claim of end-to-end native Home
  restoration qualification. The helper's tar-version probe is not such a test.
  Production image assembly runs this producer and read-only check.
* `--verify-applications-consumer-capability` runs the helper's existing
  `--applications-consumer-capability` factory. It requires the actual
  owned disposable source6 scenario, authentic original-user grants, signed
  worker/runtime cohort, completed source capsule, real receiving UID restore
  and retry, and successful receiving namespace retirement. A plain image build
  without that scenario **cannot** produce application admission. The producer
  never substitutes artifact hashes or unit-test reports for this execution.
  It publishes the original-source receipt, canonical native qualification JSON
  (without a newline, matching Rust's exact digest), then the app capability as
  the final completion marker. Existing receipts are refused, not overwritten.
  An interrupted publication remains unavailable and must be investigated.
  The capability's `qualification_script_sha256` binds the producer entry
  point, which is the helper itself.
* Applications `--check` is rootless/read-only: it verifies protected receipt
  shapes, native report and artifact digests, then uses the helper's existing
  `--reattest-original-source-consumer-capability` entry to independently verify
  the complete signed source6 cohort. It does not recapture the source or rerun
  receiving writes. Native execution is attested by the image-produced,
  root-owned receipt; no untrusted caller can supply one.
* `--verify-account-consumer-capability` executes the installed
  `tilo-account-credential capabilities` probe and the SDK-linked account probe
  from the verified signed runtime, through that runtime's own loader, and
  atomically replaces `/usr/share/tilo/account-consumer-capabilities.json`.
  When the optional service-session qualification fixtures are installed it
  also qualifies the receiving Browser/CEF and the owned receiving route; a
  saved service proof can never silently disappear. `--check` revalidates all
  of it read-only.

Namespace edges are checked for root ownership, foreign writability and symlink
redirection. Reads and subprocess stdout/stderr are bounded; commands have
deadlines and precisely owned process groups are retired. Receipts are fsynced
and atomically published without replacement. The tests' root, owner and runner
seams are not command-line interfaces or qualification evidence.

## Authenticated release carriage

`/usr/libexec/tilo-prepared-setup --data-consumer-catalog-facts` rechecks
installed consumers and exports `home_archive`, plus `applications` and
`original_source` only when app qualification exists. Partial app receipt
publication fails closed.
`media-provenance.py create` executes this exporter in the immutable native OCI
image selected by the existing final-installer build pipeline. Its facts are
bound alongside the exact qcow2 digest. `package-install-medium.sh` validates
that binding and merges the fields into the media catalog fragment. The existing
release-catalog merge and signing path authenticates these facts together with
the medium's exact compressed/uncompressed hashes. Intermediate provenance is
build input, not a freestanding authenticated capability.

Images without the prepared setup helper carry no facts: none are synthesized.
An installed exporter that fails is a build error, never silently omitted.

Contract mutations run with `t cargo test -p imager-backend installed_consumers`.
These are unit tests of production code against fixture namespaces, **not** a
native qualification result. Native source/receiving qualification needs its
actual disposable scenario and installed candidate; no such successful run is
claimed here.
