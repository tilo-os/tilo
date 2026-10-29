<picture>

  <source media="(prefers-color-scheme: dark)" srcset="docs/brand/banner-dark.svg">
  <img src="docs/brand/banner-light.svg" alt="Tilo — a desktop for doing things together." width="1200">
</picture>

[Website](https://tilo.so) · [Contribute](CONTRIBUTING.md) ·
[Build an app](docs/app-development.md) ·
[Source & issues](https://code.tilo.so/tilo/tilo)

Tilo is a Linux desktop built around collaboration and around working across
your own devices. You share a _workspace_ with someone and work together in
every Tilo app, with full history, undo, search and sync between devices and
people. You can add AI agents too, with their own permissions and history.

We build the desktop, the apps and the system underneath them together. Tilo
is **early alpha**: expect things to break, and don't trust it with your only
copy of important data.

## How it works

- **Workspaces are the unit of collaboration.** Each has its own members,
  roles, apps, layout and data, and is on every device of every member.
- **People and devices are separate.** A member is an identity; each of their
  devices has its own key. Devices sync directly over iroh, end to end
  encrypted with MLS. A server is just a device that stores more.
- **All shared state is CRDT.** Edits from several people and devices merge.
- **Apps are resources, actions and views.** An app declares its typed state
  and the actions that change it; collaboration, history and undo,
  permissions, sync and search come from that. Scripts, assistive technology
  and agents use the same actions as the buttons.
- **The shell is built like an app.** Launcher, top bar and settings run on
  the same toolkit and model as every other app. Windows sit in horizontally
  scrolling strips; tabs can group different apps.

The [architecture overview](docs/architecture/README.md) explains the model
and where it lives in the code; [`workspace.md`](docs/architecture/workspace.md)
is the full specification. The [apps directory](apps/README.md) lists the apps.

## Try it

There is no public release download yet. [Getting started](docs/getting-started.md)
covers virtual machines, adding the desktop to an existing Linux system and
installing the OS. To build and run it from source, follow
[CONTRIBUTING.md](CONTRIBUTING.md).

## Help build it

You don't need to understand the whole system to help, or even to program: bug
reports, small fixes, accessibility testing, translations, documentation and
design feedback all count. [CONTRIBUTING.md](CONTRIBUTING.md) takes you from a
checkout to a pull request; [Build an app](docs/app-development.md) covers the
app SDK.

Our home is [code.tilo.so/tilo/tilo](https://code.tilo.so/tilo/tilo).
[GitHub](https://github.com/tilo-os/tilo) is a mirror; please send issues and
pull requests to Forgejo. People and coding agents are both welcome; if you
use an agent, understand the change you submit and say what you tested.
[AGENTS.md](AGENTS.md) is for the agent.

## License and security

Tilo is open source: everything you need to publish an app is MIT, the rest is
AGPL 3.0. See [LICENSING.md](LICENSING.md).

Please report vulnerabilities privately as described in
[SECURITY.md](SECURITY.md), not in a public issue.
