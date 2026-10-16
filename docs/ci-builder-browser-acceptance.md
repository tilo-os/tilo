# Native builder browser acceptance

The builder uses Chromium from the existing Fedora RPM repositories and package
signature trust. It adds no browser download channel or signing key. The image
exports the RPM's executable rather than its desktop launcher, so isolated test
homes do not affect browser lookup. The native test wrapper supplies this default
only on Linux, retaining nonempty `CHROME` overrides; Darwin and other platforms
keep their existing discovery, including an unset or empty override.

## Guard ordering

`build-ci-builder.yml` keeps the existing publication model:

1. Build, push a uniquely named native candidate, sign its digest, verify its
   signature, and upload its child descriptor. This is not shared-alias adoption.
2. Resolve the exact successful producer descriptors into native job images.
3. In each `cache-cold` job, check native architecture, prepare the compiler
   cache, launch Chromium over CDP, evaluate JavaScript and close the owned
   browser. Populate and verify the cold compiler-cache namespace.
4. In that same candidate container, run the existing native peer `end_to_end`,
   `passkey_root`, `node_workspace` and `voice_browsers` integration binaries, then
   the three `host_agents` cases covering hosted-agent restart, task reload and
   creator/joiner live visibility. Use the production test wrapper, CI nextest
   profile and existing limits. A failure prevents the successful cold receipt
   upload. An empty selection fails; no test is skipped or quarantined here.
5. Only after both cold jobs succeed, fresh warm jobs require remote cache hits.
   `accept-builder` requires both matrices to succeed and verifies their exact
   image/cache receipts.
6. `promote-builder` requires acceptance. Only main advances the shared revision,
   toolchain and `latest` aliases. Legacy floating consumers can adopt the builder
   only after these checks accept it. Feature refs retain candidate-only tags.

The immutable builder pin in `t.toml` is unchanged. This change introduces neither
an alternate approval mechanism nor new signing permissions.

## Architecture feasibility and remaining proof

Both x86_64 and aarch64 use the existing native runner matrix and its exact image
digests, without emulation or architecture exclusions. Fedora supplies native
Chromium on both. The peer and participant tests build for the host; their Node
CDP drivers are architecture-independent. Voice uses Chromium's test microphone
and the existing headless audio environment. Its MoQ JavaScript dependency is built
through the existing `npm ci` lockfile, not a floating browser download. The three
participant cases use the native simulated browser session, complementing the
real Chromium peer/passkey/workspace/voice cases.

These are feasibility arguments, not image execution results. Hermetic tests
exercise the image recipe with package operations intercepted, platform/default/
override handling in the production wrapper, workflow command failure propagation,
and acceptance ordering. Actual RPM availability, browser codecs, native CPU
compatibility, integration deadlines and cold/warm cache receipts still require
the trusted main builder workflow after review under the existing process. Record
that run and its accepted immutable index before claiming image verification.
No feature-branch signing dispatch or production image-pin update is needed here.
