# qOS Windows compatibility broker

This crate implements the security-sensitive broker core and its testable
boundary contracts. It currently provides:

- deterministic catalogue resolution;
- persistent isolated Wine/UMU capsule state and registered-program lookup;
- a fixed Bubblewrap, Landlock, network-gate, Wayland, GPU, and brokered-audio plan;
- a real no-shell Bubblewrap executor with the qOS seccomp baseline for Wine;
- a trait boundary to the singleton `windows-vm` appliance;
- explainable status DTOs for the Windows Apps UI; and
- a versioned JSON protocol *contract*.

It does **not** currently bind a daemon socket or provide an end-to-end live
provider. `apps/windows` therefore keeps
its live `EmptyProvider` and does not claim that example applications are
installed. Wiring requires a session daemon that authenticates peer
credentials, obtains `CapOverride` records from the compositor (never from the
client message), owns an output/capture-filtered audio proxy and hostname
egress-gate leases, and adapts the authenticated Windows guest bridge to
`VmDelegate`. The protocol module deliberately describes this boundary without
pretending the daemon exists.

Also not implemented here: prefix snapshots, runner migration, component
installation, repair/reset, failure-triggered fallback ordering, or automatic
VM fallback. Existing recipe changes fail with `RecipeChanged`; VM selection
requires both a matching VM catalogue recipe and an explicit trusted caller
action. These operations need durable transaction and recovery semantics before
they should appear as live UI actions.
