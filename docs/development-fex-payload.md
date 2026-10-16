# CI-built FEX candidate for ARM Windows apps

Run `t dispatch publish-fex-payload --ref main`. t builds on native ARM64 and x86-64
nodes and publishes a native FEX toolchain candidate, a complete x86 development SDK
candidate, and the portable ARM FEX payload, then sets main's `fex-payload-candidate`
pointer to the payload's digest (docs/publishing-on-t.md, `tools/ci/products.toml`). It
clones the reviewed FEX revision and recursive submodules from
`os/external-assets/fex-source.env`, applies the checked-in hash-pinned patches,
and validates the installed payload before publishing it. All references use
immutable OCI digests. The final payload is signed with the development key
(`keys/cosign-dev.pub`, KMS `alias/tilo-dev-signing`), which no release verification
accepts; runtime acceptance has **not** been verified.

The SDK and native toolchain still resolve live Fedora RPMs. These artifacts
are development candidates, not frozen production inputs. Their unique tags
are retained for inspection; consumers must use the digest reference printed
by the payload job rather than a tag. A signed payload proves which CI job
built those bytes. It does not prove that Wine, Proton, graphics, or the device
GPU work. The validation limits and known FEX regressions are tracked in
[`os/docs/fex-build-validation.md`](../os/docs/fex-build-validation.md).

The separate retained x86 guest currently available at
`registry.tilo.so/tilo/tilo-development-windows-guest@sha256:592518b08a4c88395b7054c293eab9aeb52750869aba3a59dbe87d98556ae765`
is also a development candidate. Before using it for an ARM OS image, verify
that its traditional Wine pair, source pins, DXVK payload and loader layout
pass the current `windows_provider.py` consumer checks. Its producer records
`runtime_acceptance_verified=false`.

`build-image.yml` normally reads repository variables `TILO_FEX_PAYLOAD_REF`
and `TILO_WINDOWS_GUEST_REF` as a pair. It fails with only one variable and
builds an ARM image without Windows apps if both are absent. An explicit
Lenovo `main` dispatch can instead supply both immutable references as
`fex_payload_candidate` and `windows_guest_candidate`. This confines the
experimental inputs to one build without changing the nightly or tag channels.
The resulting image retains `org.tilo.with-windows=1` and its embedded provider
digest record. Its KMS-signed SLSA release provenance names both input digests
and records `inputClass=development-only` and
`runtimeAcceptanceVerified=false`. Lenovo publication verifies those exact
claims before making the board image tag visible. A production provider
additionally needs frozen input transactions and mixed-width Wine, Proton,
DXVK, graphics, capsule and target-hardware acceptance.
