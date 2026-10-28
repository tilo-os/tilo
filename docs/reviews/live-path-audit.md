# Live-path audit: composer and Containers

This is a bounded audit, not certification that every app works against its real backend.
Capture fixtures prove layout; typed action tests prove admission and dispatch; neither proves
successful transport to a live service. The remaining fleet needs the same three checks.

## Coverage

| Surface | Evidence | Limit |
| --- | --- | --- |
| Chat message and thread composer | Canonical rich-text draft resources, native input/IME and action tests; actual editor geometry at both text scales, multiline wrapping and scroll preservation | No live peer transport or media upload exercised here |
| Books annotation composer | Same shared composer over the existing typed annotation field; existing app tests | No new book import/backend audit |
| Containers resource/action surface | Existing hosted row effects and permission/refusal tests; real subprocess stream tests; real Podman command refusal checks | Actual builder-host list/create-volume/remove-volume audit passed; packaged app execution remains unverified |

The first scoped remote run passed 371 tests with no skips across these apps and `tilo-rich-text`.
A subsequent real-host investigation added a regression for Podman’s nullable ports field.
The separate `containers/examples/live_engine_audit.rs` is deliberately fail-closed. It probes
real Podman, creates one uniquely named owned volume, observes it through the production
projection, removes it, and proves removal. It uses no sample data, image pulls, foreign
volumes, or successful-result mocks. Its cleanup guard removes only its own volume.

## Containers findings and repairs

Live launch already probes Podman and publishes typed host-local container, image, volume
and distrobox resources. Sample engine state is restricted to capture fixtures; the live
path does not seed it. Engine commands retain their permissions and canonical effect doors.

The driver previously returned success when its preliminary engine probe failed. Lifecycle,
image and volume commands now use the actual command result, so an engine disappearing
between publication and invocation produces a refusal rather than an accepted toast.
Real Podman also exposed a parsing defect: its `ps` JSON emits `Ports: null` for containers
without published ports. The parser now accepts those rows while retaining mapped ports,
instead of rejecting the whole live collection.

Streaming commands now drain both actual output readers before publishing their terminal
result; output emitted immediately before exit is no longer silently lost.

Failed CPU/memory sampling is distinct from measured zero usage. Log command failures are
distinct from a successful empty log. The app shows those states truthfully and retains the
error instead of repeatedly probing an unchanged failed log on every layout.

Actual rootless Podman answers on the builder host. The audit passed there using the compiled
production driver: live list, owned volume creation, live projection, removal and confirmed
absence. The pinned app build container lacks the Podman executable (`No such file or
directory`), so its audit failed before creating a volume. This does not establish a
transport or sandbox defect. The supported `tilo device doctor` found the configured VM’s daemon
SSH connection refused. No foreign VM was started and no sandbox was loosened. Backend-host
proof is distinct from a successful packaged app action in a real OS slot.

## Remaining functional work

- Move the synchronous initial probe and lifecycle commands off the UI thread using the
  existing effect/job boundary. The command timeout is bounded, but a command can still
  block responsiveness while running.
- Distinguish image/volume list and inspect failures from successfully empty collections;
  these subsidiary readers still use best-effort defaults.
- Implement or remove the promised Podman-label fallback when the distrobox CLI is absent.
  The current implementation returns an empty collection instead.
- Run the fail-closed audit inside an available supported OS slot, then exercise real
  container start/stop, logs and stats through the published resource effects. The volume
  audit alone would not prove those capabilities.
- Audit the remaining apps' live services, permissions, transport failures and successful
  effects individually. Passing fixture/native admission tests is insufficient evidence.
