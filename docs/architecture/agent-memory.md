# Content is memory

Tilo has no memory store for agents. An agent learns what it needs the way
everyone else does: by reading the workspace's resources. What it keeps
durable it writes where a person would put it, through the ordinary typed
action for that place. There is nothing to migrate, export or delete beside
the resources themselves.

Two consequences follow, and both are the point:

- **What an agent knows is what the person can read.** Every durable thing an
  agent learned is a resource the person opens in an app, with the agent's
  change attributed to it in the record's history, undoable like any other
  change and subject to the same roles, confirmation and approval.
- **Forgetting is deleting.** Revoking a grant, ending a task or removing an
  app's record removes the information with it. No second copy survives.

## Where a durable fact goes

An agent that learns something durable ([`ROLE`] in a task's prompt below) puts
it in the place a person would, with that place's action:

| What it learned | Where it goes | Action it runs |
|---|---|---|
| A preference about how the desktop behaves — appearance, display, sound, keyboard, power, notifications, accessibility, language | Settings | the pane's typed effect (`settings.effect`, e.g. `ChooseWallpaper`, `SetNightLight`) |
| A fact about a person — a phone number, an address, a birthday, a mailing group | Contacts | the `contacts.book` actions: `add_contacts`, `new_contact`, the contact's field edits (`Contacts::save`) |
| A decision about the work: what was agreed, why, what is next | the project's note in Notes | the `notes.notebook` actions: `create_page`, `append_paragraph`, `rename`, `tag` |
| A fact with no natural home — a reference, a one-off finding, context for later | the workspace's notes folder in Files (`~/Tilo/<workspace>/notes`) | the same `notes.notebook` actions, on the notebook that folder shows |

The order of the table is the order to try: a preference is a setting before it
is a note, and a fact about a person is a field on their contact before it is a
paragraph. A note is the fallback for what has no home, not the default.

Each of these places is a resource with typed actions, so an agent writes there
exactly as it writes anywhere: by naming a resource and one of its actions. The
write is an action invocation like any other, which is what makes it visible in
the task's step trace, attributed to the agent in the record's history,
undoable with the record's undo, and subject to the person's roles,
confirmation policy and approval.

## The only agent state is the task

The runtime keeps no facts about the person or the work. What it does keep is
the task's own working record: the request as given, the steps performed and
their answers, the content the steps produced, the records the task touched,
the waits it is in, and the conversation around it. That record is what the
Tasks app shows for the run and what the person reads to know what an agent
did; it ends with the task. Nothing in it is read as knowledge by a later task.

The host's own state directory
(`crates/agent/agent-participant/src/host.rs`) holds the agents' device keys,
grants and each room's content bytes — credentials and content, never learned
facts. `AGENTS.json` is owner-only. See the audit in
[crates/agent/agent-participant/src/lib.rs](../../crates/agent/agent-participant/src/lib.rs).

## A preference stated once is read later, not remembered

A preference told to an agent in one task is honoured in a later task because
the agent reads it: it searches the workspace, finds the setting or the contact
or the note, reads it, and plans with it. Two rules keep that honest:

- An agent writes a preference to the setting, then reads it from the setting
  when it needs it. It never keeps a private copy to skip the read.
- The prompt says where durable facts belong and that nothing carries over
  between tasks (the `ROLE` paragraph below). A plan that states a preference
  without having read it is visible in its steps: there is no step that read it.

## The paragraph a plan is given

[`ROLE`] is the standing instruction in
`crates/agent/agent-participant/src/model.rs`, carried in every plan's briefing.
It is the whole of the convention as the model sees it:

```
Nothing you learned in an earlier task carries over to this one. To know a
preference, a person's details, or a decision already made, read it: search the
workspace and read the record that holds it. What you learn that must last, write
where a person would with that place's action: a preference about the desktop in
Settings; a fact about a person on their contact in Contacts; a decision in the
project's note in Notes; anything else in the notes folder in Files. Never keep a
note about the person or the work outside those.
```
