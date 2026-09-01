# qOS Windows compatibility broker

This crate implements the security-sensitive broker core and its testable
boundary contracts. It currently provides:

- deterministic catalogue resolution;
- persistent isolated Wine/UMU capsule state and registered-program lookup;
- a fixed Bubblewrap, Landlock, network-gate, Wayland, and GPU plan;
- a real no-shell Bubblewrap executor with the qOS seccomp baseline for Wine;
- a trait boundary to the singleton `windows-vm` appliance;
- explainable status DTOs for the Windows Apps UI;
- a per-launch PipeWire/Pulse proxy: playback is always tunnelled to the
  session sink, while a microphone source exists only for a canonical qOS
  microphone grant; and
- an authenticated, bounded session protocol with server-owned entry IDs; and
- descriptor-anchored, no-follow capsule storage.

The compositor binds the live session service and authenticates the launcher
with a one-use PID/start-time/app/workspace registry before passing its
connected descriptor into `apps/windows`. Requests contain only opaque managed
entry IDs. The compositor reloads current `CapOverride` records for every
launch; the client cannot supply grants, executable or host paths, runner
settings, or VM intent. The manager uses an empty unavailable state when the
optional service is absent, while sample applications remain screenshot-only.

Filtered network access uses the existing qOS egress gate. Audio uses a
private, short-lived PipeWire graph per launch. The graph contains no device
discovery: its output tunnel is the only playback path, and its input tunnel
is created only after the compositor has granted `Device("microphone")`.
Wine/Proton sees the graph's private native/Pulse sockets, never the session
socket or `/dev/snd`; revocation terminates the graph. The live VM route is
disabled until a privileged authenticated guest-bridge adapter implements
`VmDelegate`.

Also not implemented here: prefix snapshots, runner migration, component
installation/import, repair/reset, failure-triggered fallback ordering, or
automatic VM fallback. Existing recipe changes fail with `RecipeChanged`.
These operations need durable transaction and recovery semantics before they
should appear as live UI actions.
