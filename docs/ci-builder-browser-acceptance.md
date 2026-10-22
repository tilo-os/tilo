# Native builder browser acceptance

The builder uses Chromium from the existing Fedora RPM repositories and package
signature trust. It adds no browser download channel or signing key. The image
exports the RPM's executable rather than its desktop launcher, so isolated test
homes do not affect browser lookup. The native test wrapper supplies this default
only on Linux, retaining nonempty `CHROME` overrides; Darwin and other platforms
keep their existing discovery, including an unset or empty override.

## Guard ordering

The builder's chain in `t.toml` (generated from the `ci-builder` product in
`tools/ci/products.toml`):

1. `image-ci-builder-<arch>` builds the candidate natively on each architecture
   (`tools/ci/publish/ci-builder.sh`); `image-ci-builder` joins both into one index.
   Nothing is pushed or signed yet.
2. The canaries run inside the exact candidate archive the same run built
   (`tools/ci/publish/ci-builder-canary.sh`): `canary-ci-builder-cache-cold-<arch>`
   checks the build environment and the pinned toolchain, then compiles through t's
   compiler cache under a fresh key and must write; `canary-ci-builder-cache-warm-<arch>`,
   a fresh job, must read back everything the cold one wrote.
3. `canary-ci-builder-browser-<arch>` launches the builder's Chromium over CDP,
   evaluates JavaScript and closes the owned browser, then runs the native peer
   `end_to_end`, `passkey_root`, `node_workspace` and `voice_browsers` integration
   binaries and the three `host_agents` cases covering hosted-agent restart, task
   reload and creator/joiner live visibility, with the production test wrapper, CI
   nextest profile and existing limits. An empty selection fails; no test is skipped
   or quarantined here.
4. The product's `gates` must pass before anything is signed or published.
5. On main, `sign-dev-ci-builder` signs the candidate with the development key and
   `publish-ci-builder` pushes only `dev-<commit>` tags. Stable's `sign-ci-builder`
   signs the index and both images with the release key and `publish-ci-builder`
   publishes `<commit>` and `<rust channel>`.

This change introduces neither an alternate approval mechanism nor new signing
permissions.

## Architecture feasibility and remaining proof

Both x86_64 and aarch64 build natively and run their canaries in their exact
candidate images, without emulation; which canaries gate publication is the
product's `gates` list in `tools/ci/products.toml`. Fedora supplies native
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
compatibility, integration deadlines and cold/warm cache results still require
the canaries to pass on t. Record that run and its accepted immutable index before
claiming image verification.
No feature-branch signing dispatch or production image-pin update is needed here.
