# Task conversation acceptance

## Offscreen captures

No root-checkout compilation is needed. With an **existing, current-source**
Chat executable and compatible host libraries/fonts:

```sh
CHAT_BINARY=/path/to/chat bash apps/chat/examples/task-conversation.sh
```

Alternatively, set `TILO_CHAT_CAPTURE_IMAGE` to the **existing builder image
used for that executable**. The script uses rootless, network-disabled Podman
without pulling an image. There is deliberately no hardcoded historical CI
image: its fontconfig ABI may not match the executable. Neither path connects
to a compositor or captures the user's desktop; both use `--shots` offscreen.

If the executable is stale, run these commands sequentially in a compatible
build environment. A remote builder can use any cargo drop-in; retrieve the
resulting PNGs through that builder's artifact mechanism.

```sh
cargo run --locked -p chat --bin chat -- --shots target/chat-task-shots/normal.png --scenario agent-task
cargo run --locked -p chat --bin chat -- --shots target/chat-task-shots/compact.png --scenario agent-task-compact
```

The scenarios draw the production Chat surface at 1280×800 and 420×680.
They are **explicit fixtures**, not evidence that an agent executed work:
task `task:review`, run 7, an executing frontier, one accepted direction at
revision 2, with revision 1 applied. **Direction saved** is therefore correct;
**Stop**, not **Start**, is correct for this already-active run. The fixtures
use the same turn projection as hydrated conversations but do not contact the
backend or prove hydration. Start and reopened history need live acceptance.

### Original capture review

The original `target/chat-task-shots/{normal,compact}.png` captures predate
explicit Start and durable conversation hydration. In both, the direction is
legible, Saved is distinct from Executing, and controls do not overlap. Compact
wraps the direction and the task/run status above the composer correctly.
There is substantial empty timeline space because this fixture contains only
one turn, not because history has been loaded and lost.

Visible limitations: at normal width the agent subtitle is reduced to `Scri…`
despite ample overall header space; compact elides the task title and clips the
long composer hint near the trailing controls. Verify those text-fit issues in
fresh captures before attributing them to current code. They are not evidence
of failed task execution. No colour/UX redesign is required for this MVP.

## Live acceptance

Use a development workspace with an enrolled agent and the backend's task
action grants. Open that agent from Chat's Agents section, choose **New task**,
and send a multi-paragraph instruction. Check that the queued task has the full
instruction, not just its bounded title. Queueing saves the task; it must **not**
start execution. Chat follows the queue receipt's exact task into its queued
frontier (run epoch 0 for a new task).

In **Task conversation**, verify **Start** is offered for the selected queued,
inactive task. Explicitly press Start: Chat invokes the backend's `approve-run`
action with the expected run epoch and revision. **Start approved** means run
approval, not proof of execution. Chat follows the returned new epoch; only the
backend execution frontier may say Accepted, Planning or Executing. Start is
also available through the contextual semantic action locus, not a separate
Chat `start-agent-task` resource action.

Once the selected run is active, ask a question or change course: both send
steering to that same run. The editor clears only after an owner receipt;
Saved changes to Applied only after that run reports applying the revision.
Stop reports **Stop requested**, not an assertion that execution has already
ceased. Task/resource outputs and permission policy remain backend-owned.

Close and reopen the exact task/run conversation. Chat now invokes the task
resource's **`read-conversation`** with `run_epoch`: the original full instruction
(epoch 0) and the selected run's durable directions must reappear, attributed
and without duplicates. `read-task` supplies the frontier, not message text.
A stale or mismatched conversation result must never populate another task/run.
This is durable accepted instruction/direction history, not a claim that agent
output or arbitrary private drafts are replayed.

Float and return the conversation using the shared floating host. Verify that
the selected task/run, pending receipt, private draft, caret/selection and IME
remain with the retained editor. With two tasks assigned to the same agent,
explicitly select one before steering; a replacement run must never silently
inherit the old conversation's authority.

The authenticated `chat.agent:<agent>` row exports `open-agent-conversation`
and `open-task-conversation`. The latter asks only for `task` and explicit
`run_epoch` (including zero for a newly queued task); workspace and agent are
bound by Chat, never chosen by that caller. It can follow the exact saved task
before Chat has projected it. Once its queued frontier arrives, the separate
`start-agent-task` effect provides explicit approval. Opening never starts it.
The root also offers exact navigation with all four source/identity fields.
Backend task actions own `queue`, `approve-run`, `direct`, `stop`, and
`read-conversation`; Chat is not an ordinary-message authority lane.

## Focused verification (on the builder)

```sh
cargo test --locked -p chat --lib agent_
cargo test --locked -p tilo-app --lib framework::rows::tests
cargo check --locked -p chat --tests
python3 apps/chat/tests/test_agent_start_surface.py
bash -n apps/chat/examples/task-conversation.sh
```

The focused tests cover full-instruction queueing, receipt parsing,
revision/run fencing, explicit approval without claiming execution, durable
turn hydration/deduplication, and private editor transfer to/from the shared
floating host. Screenshots complement those checks; they do not replace them.
