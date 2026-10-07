# Native task steps

An enrolled Agent can invoke a published action as a numbered task step. The broker uses
its consumed managed connection and exact accepted numeric run and lease; arguments cannot
supply caller authority. The target must be published in the same workspace, have a stable
semantic identity and require no interactive confirmation. Current resource permissions
still apply: workspace task membership alone does not grant tool access.

The native owner stores bounded step metadata on the task control. Only an authenticated
Host can prepare or settle it. Preparation shares the owner command ordering with task
cancellation. New steps are sequential, with at most one unsettled predecessor, up to 256
per run and 1024 retained per task. A cancelled run can settle its already admitted child,
but cannot admit another. An Agent cannot finish a run as Done while its current epoch has
an unsettled step. Waiting and cancellation preserve uncertain metadata.

The child invocation UUID uses a versioned, domain-separated hash of length-prefixed room
and task identities, owner epoch and step index. Ephemeral connection identities do not
change it. The private intent commits to the actual original Agent source route and
certificate ownership, target route and semantic identity, action contract and schema,
arguments, task context and the first kernel-admitted process/package/runtime binding.
Replay checks fresh current authority against this retained intent; it does not replace
original provenance. Object keys are recursively sorted before bounded commitment encoding.

There is one dispatch permit: the existing private invocation journal. The owner commits
PrepareStep before the journal admits the child. The owner ordering lock is released before
waiting on the canonical target terminal. Host settlement commits to the canonical terminal
and retained continuation, rather than an Agent's declaration of success. Identical replay
returns that recorded result without dispatching again; changed intent refuses.

Crash windows remain explicit. A durable preparation without a retained dispatch record is
uncertain and refuses redispatch. An uncertain journal record also refuses. A lost owner
settlement acknowledgement can recover the recorded terminal and idempotent settlement.
Missing private output or inconsistent commitments refuse, rather than inventing success.
The shared task inspect action exposes only epoch, index, invocation, intent/output digests
and owner outcome. Arguments, prompts, private tool results and process metadata stay in
host-private storage.

The signed component tests use genuine enrollment, consumed managed descriptors, actual
StatePeer/native-owner commits, a signed private target worker and the configured production
journal. The target handler counter proves one invocation across identical replay and lost
terminal acknowledgement. A scoped signed grant and its revocation test actual resource
permission enforcement; another test preserves Contributor refusal without that grant.
Cancellation and lost preparation acknowledgement are exercised through real owner commits.
Wire tests retain legacy command/result ordinals and absent-step control JSON compatibility.

These tests qualify dispatch, custody, permission checks, durable metadata and replay within
an admitted live connection. They do not qualify ordinary document readiness, installed
end-to-end coding, model inference, process reconstruction after a broker restart, or a new
connection recovering retired managed custody. Task Done denotes conversation completion,
not an independent coding grader result.
