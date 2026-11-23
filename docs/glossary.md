# Glossary

The words Tilo's code, docs and UI use. One meaning each. A new concept is added here before it is
used in code.

## People and devices

- **Person**: a human. Identified by their root key.
- **Personal dictionary**: the words a person added. Their setting, so it follows them to their
  devices; every input surface treats the words as spelled right and dictation prefers them.
- **Device**: a machine or browser belonging to one person, with its own key that the person
  certified. Per workspace a device either holds the keys (reads and runs work) or only stores
  ciphertext. A device setting says which work it takes, from whom, and when (charging, energy
  mode, free resources).
- **Server**: a device running Tilo Server, the headless edition of the OS. It has no screen
  session and no local login, joins its person's identity with a ticket, and is configured only
  through its entry in their Servers workspace. It unlocks its disk unattended (TPM, then a
  sibling server, then one of its person's
  devices), updates one server at a time with a health check and rollback, can run workspaces'
  Linux machines, and can serve the API. Per workspace it stores ciphertext or holds the keys and
  takes work; holding a workspace never makes it a member, though the member list names it as
  its operator's device, so what it serves or narrates reads as theirs. One that holds several
  people's workspaces keeps each one's work in its own sandbox.
- **Agent**: a model-driven member. It does what members ask, in the workspace, like a person. A
  person adds it and picks the device it runs on. An outside agent connected over MCP is an agent.
- **Agent session**: one context of one agent: a conversation with a Tilo agent, or one MCP
  connection of an outside agent.
- **Step**: one thing an agent session asks for: an action it calls, or a question it puts to
  its person.
- **Bot**: an installed program with its own identity and role in a workspace, reacting to events
  as itself (a bridge, an integration, moderation).
- **Automation**: saved steps with triggers. Runs for the member who owns it. One execution is an
  invocation. Not a member.
- **Member**: a person, agent or bot in a workspace, with a role.
- **Role**: what a member may do in a workspace.
- **Member access**: who acts in a workspace and what its policy and grants let them do there.
  Every change is checked against it.
- **Approval**: a person's answer, given on their own device, that lets one action run that
  someone acting for them asked for.
- **Presence**: one member in one open view (a window, an agent's private view). A member can have
  several.

Every change records who made it and who they acted for, as a graph:
`(Quinten, Xaver) → Agent 1 → Automation 1 → change`. A change is allowed only if every person in
the graph may make it.

## Workspaces and data

- **Workspace**: the unit of collaboration. Has members, resources and one set of keys. Identified
  by `WorkspaceId` (a UUID) only.
- **Resource**: the unit of data. An app declares resource types; a chat message is a resource.
  Everything a workspace holds is its resources, one set of them for all its members: mail and its
  accounts, the calendar, the vault, models, connections, agents and skills, drafts and histories.
  Its owners restrict any of it with grants.
- **Hybrid logical clock**: the time a device stamps on what it writes where the order matters: its
  wall clock, moved past every time it had seen, so what it writes reads after what it had already
  read however far the devices' clocks are apart. The stamp is made where the action is written and
  every device applies the same one.
- **Target**: the resource an action is offered on: its first parameter.
- **Personal resources**: what belongs to a person's identity rather than to a workspace: their
  identity root, their devices and device keys, their appearance and other settings that are
  theirs everywhere, and each device's settings. Nothing else. Logins are vault items, and history
  and mail are the workspace's. Someone who wants a private mailbox or vault keeps it in a workspace
  of their own.
- **Live session**: one device's sealed `moq-lite` session in one workspace: the presence, call
  audio, pictures, shared windows and pages it publishes and hears. The device serving the workspace
  copies them to the others (its live origin).
- **Peer network**: the devices of a workspace reaching each other directly over their iroh
  endpoints, and what the session says and hears on it.
- **Action service**: how a device answers the action requests other devices of its workspaces
  send it, and sends its own.
- **Action provider**: what answers a device's action calls (`ActionProvider`): a peer's remote
  calls, and a window's own to its app.
- **App action server**: what answers the remote actions that reach a serving device for its apps.
- **App wire**: the socket a session serves its apps' calls on.
- **Document**: internal storage of one or more resources (an automerge document). Apps never see it.
- **App**: what a person installs and runs. Identified by `AppId` (`so.tilo.chat`).
- **App process**: the process an app runs in; it loads the app, gives it the toolkit and the
  action service, and draws its windows.
- **Package**: the signed file an app ships in. `PackageId` names one build.

## Screen

- **Display**: a screen.
- **Display placement**: where each of a person's displays stands and in what order, across their
  devices. One setting of the person's; the Displays pane edits it.
- **Layout**: a workspace's strips, window groups and tabs. Workspace data, shared by its members.
- **Strip**, **window group**, **tab**: the parts of a layout.
- **Part**: a region of a shell surface or of a described component where content goes. A person
  may bind a shell part to a view an app publishes.
- **View**: how the shell shows a layout (strips, canvas, others a shell adds).
- **Shell**: the UI around windows (bar, overview, notifications) and the process that runs it.
- **Desktop widget**: a card on the shell's background (calendar, music).
- **Action API**: how the palette, CLI, scripts and agents call app actions. Agents reach it as
  an MCP server.

## Not used

Old names: room, desk, space, channel (except Chat's channels), principal, subject,
collaborator, participant, host (except a network host name, the verb, and the machine running a
VM), relay (except iroh's NAT and packet relay, and the device relaying a live session), run, seat,
arrangement, record, slot.

Vague metaphors: door, gate, plane, lane, custody, standing, ledger, witness, seam. Name the thing.
