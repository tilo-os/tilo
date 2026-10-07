# Native managed task controller

`tilo_action_broker::managed_host::ManagedHost` owns signed native agent launches.
Trusted installation supplies the package, runtime, provider, private journal,
current native workspace authority and service connections. Its launch input is
an existing task's room and ID. The controller checks the actual queued model
route, assigned Agent, current epoch, execution budget and native host custody.
It does not accept caller-selected process identity or provider authority.

The host must retain the controller and drive its supervisor off the compositor
paint thread. Actual kernel-bound process admission supplies broker custody;
existing enrolled-task transport supplies acceptance, progress and terminal
reports. Finishing a task does not prove its process has retired. `poll` and
`shutdown` retain ownership through observed cleanup.

A create-once, durable private claim precedes process launch. Later journal
entries record the observed signed package/runtime identity and admitted process.
Disk PIDs are observations and never recreate process authority. An ambiguous
or unfinished claim refuses a repeat launch after restart; this component does
not automatically resume an active run after a crash. Symlinks, foreign or
shared scratch, collisions and invalid journal bounds are refused.

`InstalledSessionConnections` uses the existing identity and model-service
socket locations and framing. The integration host must still supply actual
private key/network custody and the native owner's pinned host, or its actual
governor when no host is pinned. A descriptive certificate or a device name is
not sufficient to authorize a launch.

Component tests perform genuine governor-signed Agent/task admission before
signed host/member enrollment. They exercise governor and delegated-host
launches, non-host refusal, separate task processes, restart ambiguity, journal
durability and actual Unix service connections. They do not qualify the installed
shell's public Launch action or model inference; those require a separate
end-to-end integration through the published resource action.
