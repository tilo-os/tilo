<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/brand/banner-dark.svg">
  <img src="docs/brand/banner-light.svg" alt="Tilo" width="1200">
</picture>

**The open-source OS for people and agents.**

[tilo.so](https://tilo.so) · [tilo.sh](https://tilo.sh) ·
[Docs](https://docs.tilo.so) · [tilolang](https://tilolang.dev) ·
[Source & issues](https://code.tilo.so/tilo/tilo)

</div>

Tilo is built around shared workspaces. All of your devices work as one computer, every app is multiplayer by default with full history and undo, and people and AI agents work together under the permissions you set.

> ⚠️ **Early alpha:** Expect things to break. **Do not trust Tilo with your only copy of important data yet!**

## Start here

- [**Hang out with us in the Lounge workspace**](https://tilo.sh)
- **Give your agents a workspace:** open [tilo.sh](https://tilo.sh) → *New workspace* → *Setup with agent*
- **Install [Tilo OS](https://tilo.so/os):** try it in a VM, add the desktop session to Linux, or follow the [install guide](docs/getting-started.md)

## What you get

- **Multiplayer for everything:** Invite people or AI agents into a workspace and work together live in notes, files, mail, calendar, terminal, or browser. You see what everyone is doing and decide exactly what they can touch.
- **All your devices, one computer:** Your laptop, desktop, server, and mobile browser sync directly peer-to-peer, end-to-end encrypted, with no account required. Pick up right where you left off across devices: files, photos, contacts, passwords, and open windows.
- **Web access, your own NAS, or cloud:** Open [tilo.sh](https://tilo.sh) in any browser to connect straight to your own devices over iroh without us hosting your data, deploy your own 24/7 Tilo Server/NAS, or add an optional cloud instance.
- **Always-on agents you actually control:** Give agents a sandboxed workspace where they use the same apps you do (via MCP, Python/TypeScript SDKs, CLI, or `tilolang`). Watch what they do live and require approval before anything leaves your workspace.
- **Custom views and new apps on the fly:** Ask your agent to pull data and actions from any app into an interactive view right in chat, or build a brand-new native app in [tilolang](https://tilolang.dev) and share it to your workspace or publicly.
- **Your desktop, your rules:** Ships with a scrolling window manager out of the box, and the entire shell is customizable. Want a 2D canvas, a classic desktop, Windows 95, or Liquid Glass? Switch looks or ask your agent to build one, and every app adapts.
- **Zero-install native apps:** Search for any app and press Enter. Native apps weigh ~1–2 MB (built on **Tilo UI** and **tilolang**), open immediately without installing, and stay sandboxed. Pin the ones you want to keep.
- **History and per-author undo everywhere:** Every edit in every app and in your synced files keeps its full history by author, so you can revert any change or whole agent step in one click.
- **One search for everything:** Find any file, note, email, book highlight, or external result from one search bar and jump straight to it.
- **Accessible by design:** Every app exposes structured data and actions rather than opaque pixels, giving screen readers and assistive technology full access automatically.
- **Native suite plus Linux & Windows apps:** Comes with ~40 native apps (office suite, notes, mail, calendar, browser, maps, photos, video editor, music, terminal), plus Flatpak Linux apps and Windows apps.

## Get Tilo

| Surface | What it is | How to get it |
| --- | --- | --- |
| [**Web & Cloud**](https://docs.tilo.so/cloud) | Zero-install web client (P2P to your own devices) & optional cloud instances | Open [tilo.sh](https://tilo.sh) |
| [**Agents (MCP)**](https://docs.tilo.so/agents-and-code/mcp) | Hosted MCP (`mcp.tilo.sh`) or local MCP | `claude mcp add --transport http tilo https://mcp.tilo.sh` |
| [**SDKs**](https://docs.tilo.so/agents-and-code/sdks) | `tilo-sdk` for Python & TypeScript | `uv add tilo-sdk` / `bun add tilo-sdk` |
| [**CLI**](https://docs.tilo.so/quickstart) | `tilo` command for workspaces, keys & actions | `curl -fsSL https://tilo.sh \| sh` |
| [**Desktop & OS**](https://tilo.so/os) | Tilo Imager (AppImage, DMG, EXE), OS images & `tilo-session` Linux packages | [tilo.so/os](https://tilo.so/os) · [install guide](docs/getting-started.md) |
| [**Self-hosted server / NAS**](https://docs.tilo.so/install/server) | Tilo Server headless OS image | Follow the [server guide](https://docs.tilo.so/install/server) |
| [**tilolang & Tilo UI**](https://tilolang.dev) | Cross-platform app language & native Rust UI library (`crates/ui/ui-core`) | [tilolang.dev](https://tilolang.dev) · [architecture](docs/architecture/README.md) |

## Contribute

Code, bug reports, accessibility testing, translations, docs, and design feedback are all welcome. [CONTRIBUTING.md](CONTRIBUTING.md) covers building from source (`cargo desktop`, `cargo headless`, `cargo web`) and submitting a pull request.

Development happens on [code.tilo.so/tilo/tilo](https://code.tilo.so/tilo/tilo); [GitHub](https://github.com/tilo-os/tilo) is a mirror, so please open issues and pull requests on Forgejo. Coding agents are welcome too: understand what you submit and say what you tested ([AGENTS.md](AGENTS.md)).

## License and security

Everything you need to build and publish an app is MIT; the rest is AGPL-3.0 ([LICENSING.md](LICENSING.md)).
Report vulnerabilities privately per [SECURITY.md](SECURITY.md), not in a public issue.
