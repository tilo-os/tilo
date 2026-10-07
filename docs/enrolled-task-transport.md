# Enrolled native task transport

`CapnpBrokerState::connect_enrolled_worker` consumes a managed connection from
the existing signed-package/runtime launcher. The retained kernel process
identity, admitted workspace route, agent attestor and current native membership
authorize task delivery and updates. Caller-supplied actor names, process IDs or
ordinary registered-worker descriptors cannot substitute for that custody.

The connection carries one bounded task run. Host admission creates the native
run lease; the enrolled agent acknowledges acceptance before progress or finish.
Exact retries retain their original results, while conflicting or stale epochs
are refused. After starting the run, admission compares its canonical owner
budget with the budget delivered to the agent. A changed queued policy is
refused before agent acceptance. Active-run policy is immutable.

Generic progress records acceptance without inferring tool execution from
prose. Private model reasoning and runtime data are not published as workspace
task progress. Finishing a task does not claim that its process has exited.

Disconnect, broker shutdown and current agent membership revocation retire
custody and abandon the exact active epoch through a host-attributed native
owner commit. Admission is fenced against shutdown. Restart cannot recreate a
live connection from serialized task metadata.

## Reproducible qualification

The opt-in integration target launches a real signed native fixture through the
production launcher, transfers its actual admitted descriptor, exchanges Cap'n
Proto messages, and asserts on the native owner's persisted task state:

```sh
cargo test --locked -p tilo-action-broker --features enrolled-task-test --test enrolled_transport
cargo clippy --locked -p tilo-action-broker --features enrolled-task-test --all-targets --no-deps -- -D warnings
cargo check -p shell --tests
```

The fixture covers acceptance and terminal retries, exact-process disconnect,
membership removal while the process remains alive, ordinary-person refusal,
and owner policy changes after delivery. This broker transport is a component
of native execution. Installing a production launch controller in the shell,
exposing the public launch action, running model inference and independently
grading a coding task remain separate integration requirements.
