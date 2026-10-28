# Contributing to Tilo

Tilo is very early alpha software. You (and your agents) are welcome to
contribute: code, bug reports, accessibility testing, translations, design
feedback and documentation.

[Forgejo](https://code.tilo.so/tilo/tilo) is the main repository, the
[GitHub repository](https://github.com/tilo-os/tilo) is just a mirror; please
open issues and pull requests on Forgejo.

## Report a bug or propose a change

[Open an issue](https://code.tilo.so/tilo/tilo/issues/new/choose). Tell us what
you did, what happened and what you expected (instead). Reproduction steps, your
commit or image version, and relevant logs help when available. You do not need
to diagnose the problem or propose a fix. Please remove private information from
logs; report security problems privately as described in
[SECURITY.md](SECURITY.md).

Search existing issues if you can. For a substantial feature or refactor, ask
before investing a lot of time; we can help find the right place and avoid
overlapping work. Small fixes do not need an issue first, we are very open to
PRs.

## Get a checkout

Install Rust through [rustup](https://rustup.rs) and Python 3.11+ for the
repository tools. The checkout selects the right Rust toolchain. Apps and system
components also need native dependencies for your platform:
[Linux](docs/development/platforms/linux.md),
[macOS](docs/development/platforms/macos.md) or
[Windows](docs/development/platforms/windows.md).

Fork the repository on Forgejo and work on a branch:

```sh
git clone https://code.tilo.so/YOUR_USERNAME/tilo.git
cd tilo
git remote add upstream https://code.tilo.so/tilo/tilo.git
git switch -c fix-your-change
```

Install the Git hooks once to catch formatting and commit-message mistakes:

```sh
python3 tools/dev/install-git-hooks.py
```

## Change one app

For example, to work on Notes, edit `apps/notes/` and run it in its own window:

```sh
cargo run --locked -p notes
```

Make your change and rerun to try it. Before sending the PR:

```sh
cargo test --locked -p notes
cargo check --locked -p notes --tests
```

For a different app, replace `notes` with the package `name` in its
`Cargo.toml`. You don't need to build the other apps or install Tilo OS.

## Change the shell or another crate

Edit the crate you're working on. For the shell, that's `crates/session/shell/`:

```sh
cargo check --locked -p shell --tests
cargo test --locked -p shell
```

For another crate, replace `shell` with its package `name` from `Cargo.toml`.
The [code map](docs/codemap.md) can help you find it. Append a test name to
`cargo test` to run just that test.

To try a shell UI change,
[run the desktop in a window](docs/development/run-desktop.md). Linux's packaged
development desktop currently needs hosted [t](docs/t.md) access; ask us if you
need it. Building and testing a crate doesn't require it. For boot or hardware
changes, use a [development VM](docs/development/dev-vms.md).

## Open a PR

Stage the files you changed, then commit and push your branch:

```sh
git add PATHS_YOU_CHANGED
git commit -m "fix(notes): describe your change"
git push -u origin fix-your-change
```

Use your actual paths and a commit message that describes the change; subjects
use `type(scope): description`, such as `feat(notes): add a word count`.

[Open a pull request on Forgejo](https://code.tilo.so/tilo/tilo/pulls), from
your fork's branch into `tilo/tilo`'s `main`. Tell us what changed, why, and
what you tested. A screenshot helps for visual changes. If something wouldn't
build or you couldn't test it, say so—we can help. A maintainer will review and
integrate the PR.

The [development reference](docs/development/README.md) has more testing,
platform and runtime details when you need them.

## Branch policy

`main` is a rather unstable integration branch during the alpha, not a release.
Trusted collaborators can integrate directly there; release testing happens on
`stable`. This workflow is described in
[Testing and integrating changes](docs/t.md).

If you use an agent, you remain responsible for understanding the change, having
the right to contribute it and accurately reporting what was tested. Agents
follow [AGENTS.md](AGENTS.md); humans do not need to reproduce the agent
workflow.

## Licensing

See [LICENSING.md](LICENSING.md) for the licenses used in this repository. Only
contribute code and assets you have the right to share, and preserve third-party
license notices.
