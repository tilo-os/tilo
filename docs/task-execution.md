# Native workspace task coordination

Task coordination belongs to the workspace, rather than to a project app. The
Activity pane and the broker's semantic actions use the same native owner commit
path. Creating a task records the actual device person as its creator. A refused
commit leaves the draft available; the pane does not substitute a local cache
write or report success before the owner's receipt arrives.

## Public actions

The workspace's `create-task` action creates manual coordination work.
`queue-agent-task` queues work for an exact current workspace agent, with a
`model` or `rules_only` planning route. The caller must be an admitted person with
current workspace write authority. A queued task has no active execution run.
Queueing does not launch a process or grant resource, model or check authority.

An optional resource attachment is ordinary shared data. Manual tasks expose
`attach-resource` while their coordination frontier permits it; replacement
requires the observed attachment identity. Managed tasks do not use this manual
attachment action.

Each task exposes `inspect`. Its result describes coordination, the observed run
epoch and active run, and execution policy. Private runtime handles, system
prompts and model reasoning are excluded. Human task instructions remain shared
workspace data.

## Owner execution budgets

Only the current workspace owner can use `set-execution-budget` for a managed,
unfinished task with no active run. The operation requires the exact observed
epoch and budget; a stale observation is refused. An explicit policy admits
`turn_ms` from 1 to 900,000 and `total_ms` from 1 to 7,200,000, with the turn
ceiling no larger than the total. Clearing the policy restores the existing
runner defaults: 30 seconds per turn, 300 seconds total, 30 turns and 64 steps.

Budgets are time ceilings, not authority or evidence of execution. An active
run's policy cannot be changed. The agent runner applies the admitted ceiling to
generation and its overall deadline; deadline exhaustion does not manufacture a
successful task result.

## Qualification boundary

The public queue action and Activity draft handoff are coordination surfaces.
Process launch, authenticated agent acceptance, tool execution, independent
verification and integration require their own production paths and evidence.
This increment does not advertise a public launch action or a completed coding
evaluation.
