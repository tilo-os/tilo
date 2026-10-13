# Working with agent tasks

The task owns the work and its conversation. Resources are the outputs; the
system action log records effects. Presence uses existing collaborator colours
for this first version. Task-label styling is a later experiment, not a
prerequisite for using tasks.

## Start, steer and stop

Open an enrolled agent from Chat's Agents section, choose **New task**, and send
an instruction. The full instruction is stored privately by the workspace task
owner; the short title is only a display label. Queuing does not start execution.
Select the queued task and press **Start** to approve that execution consciously.
Approval is not a claim that an agent has begun running. The task shows the
actual accepted/planning/executing phase when the executor reports it.

The conversation names the selected task and run. Sending a message changes its
instructions, including when the message is phrased as a question. **Saved**
means the owner accepted that revision; **Applied** means this exact execution
incorporated it. Reopening reads the durable instructions, directions and actual
public execution updates. Hidden model reasoning is not published. Each native
execution starts with isolated conversation memory; another task's private
instructions and results are not silently injected into it.

**Stop** fences admission of new actions for that execution. The first receipt
says **Stop requested**, not that every in-flight operation has disappeared.
Previously committed resources remain; cancellation of in-flight external
operations depends on their declared cancellation support.

Click a painted agent action marker to open the nearby private composer. Its
messages address the same task-owner direction action as Chat. Open the task's
full conversation to start queued work or inspect it. Float and return the
conversation using the shared floating surface; its draft, selection and IME
state travel with the retained editor, not through shared workspace text.

## Authority in this version

Task creation records its creator as the responsible person; the existing
assigned-agent field routes execution. Reassignment alone transfers no execution
authority.

Starting freezes a ceiling from the starter's admitted permissions. An attached
resource narrows the ceiling to that resource; a task without an attachment uses
its workspace as the scope. Every action also rechecks the starter's live
permissions and the execution agent's current admission. Privileged actions are
not admitted through this task path. An execution cannot gain authority by
inventing grant arguments, using another task's grants, or calling an ordinary
agent invocation RPC instead of the task admission path.

To steer an active task, a person must currently hold every permission in its
execution ceiling. Inviting someone to the workspace does not delegate the
starter's account. Private conversation reads remain person-authenticated and
owner-gated in this version; public task listings do not contain instructions.
Automatic task-specific grant proposals and richer assignment UI are not
implemented here; this version uses the deterministic scope rule above.

## Verification and development

`apps/chat/examples/README.md` describes the offscreen captures and semantic
navigation. The broker's `native_cross_app_approval_steering_stop_and_grants`
regression drives the enrolled owner and native task RPCs into actual Writer and
Settings resource actions. Its planner and worker mounting are deterministic
test adapters, not a live model or a mounted desktop session. Do not present its
saved CRDT assertions as proof of preference-file projection or desktop document
replication. Use `tilo-control` against a development session for installed
acceptance; never capture the user's physical desktop.
