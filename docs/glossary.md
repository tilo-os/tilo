# Glossary

The words Tilo's code, docs and UI use. One meaning each. A new concept is added here before it is
used in code.

## People and devices

- **Person**: a human. Identified by their root key.
- **Device**: a machine or browser belonging to one person, with its own key that the person
  certified. Per workspace a device either holds the keys (reads and runs work) or only stores
  ciphertext. A device setting says which work it takes, from whom, and when (charging, energy
  mode, free resources).
- **Agent**: a model-driven member. It does what members ask, in the workspace, like a person. A
  person adds it and picks the device it runs on. Claude Code connected over MCP is an agent.
- **Agent session**: one context of one agent: a conversation with a Tilo agent, or one Claude
  Code session (one MCP connection).
- **Bot**: an installed program with its own identity and role in a workspace, reacting to events
  as itself (a bridge, an integration, moderation).
- **Automation**: saved steps with triggers. Runs for the member who owns it. Not a member.
- **Member**: a person, agent or bot in a workspace, with a role.
- **Role**: what a member may do in a workspace.

Every change records who made it and who they acted for, as a graph:
`(Quinten, Xaver) → Agent 1 → Automation 1 → change`. A change is allowed only if every person in
the graph may make it.

## Workspaces and data

- **Workspace**: the unit of collaboration. Has members, resources and one set of keys. Identified
  by `WorkspaceId` (a UUID) only.
- **Resource**: the unit of data. An app declares resource types; a chat message is a resource.
  Per-person data (drafts, recents, view state) is resources in the workspace that belong to a
  person.
- **Personal resources**: resources that belong to no workspace: a person's devices and keys,
  logins, settings that apply everywhere, browsing history, signature.
- **Document**: internal storage of one or more resources (an automerge document). Apps never see it.
- **App**: what a person installs and runs. Identified by `AppId` (`so.tilo.chat`).
- **Package**: the signed file an app ships in. `PackageId` names one build.

## Screen

- **Display**: a screen.
- **Layout**: a workspace's strips, window groups and tabs. Workspace data, shared by its members.
- **Strip**, **window group**, **tab**: the parts of a layout.
- **View**: how the shell shows a layout (strips, canvas, others a shell adds).
- **Shell**: the UI around windows (bar, overview, notifications) and the process that runs it.
- **Desktop widget**: a card on the shell's background (calendar, music).
- **Action API**: how the palette, CLI, scripts and agents call app actions. Agents reach it as
  an MCP server.

## Not used

room, desk, space, channel (except Chat's channels), principal, subject, collaborator, participant,
host, relay (except iroh's NAT relay), run, door, seat, arrangement, record, slot.
