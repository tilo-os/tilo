# Publish a Tilo app

Getting an app from `tilo-app package` into the store takes one key, one
application for the namespace your app ids live in, and then one command per
release:

```bash
tilo-app keys new                                  # once
tilo-app apply --domain example.com --contact you@example.com   # once
tilo-app package --identity ~/.config/tilo/publisher-key.json
tilo-app publish
```

The registry's owner reviews every application and every release before it
reaches users. [Decisions](#decisions) lists the choices behind this flow;
each one can still change.

## Your publisher key

`tilo-app keys new` writes a new Ed25519 key to
`$XDG_CONFIG_HOME/tilo/publisher-key.json` (`--key PATH` elsewhere), readable
only by you, and prints its public half. It never replaces an existing key.
Back the file up: the key is your identity as a publisher, and an update
signed by any other key is refused.

The same key signs your packages (`tilo-app package --identity FILE`) and the
registry requests `tilo-app publish` sends. `tilo-app keys show` prints the
public key and the proof line described below. Every command that needs the
key reads `--key`, then `$TILO_PUBLISHER_KEY` (the file's contents or its bare
hex seed, which is how CI passes it), then the default path.

## Your namespace

A package id is lowercase reverse DNS with at least three labels, such as
`com.example.notes`. The registry lets your key publish only under namespaces
you have proven you own. Apply once per set of namespaces:

```bash
tilo-app apply --domain example.com --contact you@example.com
tilo-app apply --forge github:ada --forge codeberg:ada --contact ada@example.com
tilo-app apply --status
```

Before applying, publish the line `tilo-publisher=<your public key>` (from
`tilo-app keys show`) where only the namespace's owner can write:

| You own | Namespace | Put the line in |
| --- | --- | --- |
| the domain `example.com` | `com.example` | a DNS TXT record at `_tilo-publisher.example.com`, or `https://example.com/.well-known/tilo-publisher.txt` |
| the GitHub account `ada` | `io.github.ada` | `tilo-publisher.txt` in a public repository `ada/tilo-publisher` |
| the Codeberg account `ada` | `org.codeberg.ada` | the same file and repository on Codeberg |
| the account `ada` on a Forgejo or Gitea host `git.example.net` | `net.example.git.ada` | the same file and repository on that host (`--forge git.example.net:ada`) |

`apply` prints every place the registry will look. The registry checks them
before it queues the application, and answers with the namespace, the line
and what it found at each place when a proof is missing. The owner then
reviews the application; approval checks the proofs once more, so leave them
in place until `tilo-app apply --status` says `approved`.

## Releasing

Build and sign each platform you ship, then publish everything in `dist/`:

```bash
tilo-app package --identity "$KEY"                                   # Tilo OS, this machine's CPU
tilo-app package --identity "$KEY" --arch aarch64                    # Tilo OS on ARM
tilo-app package --identity "$KEY" --platform macos --arch aarch64   # macOS
tilo-app package --identity "$KEY" --platform windows --arch x86_64  # Windows
tilo-app publish
```

`publish` checks each package the way the registry will, uploads it, and asks
for the catalogue entry that lists it: a new entry the first time, an update
after that. Nothing is public until the owner approves the request; the output
names the entry and where it waits. `--metadata store.json` sets the store
presentation (localized `name`, `tagline`, `description`, `images`,
`supported_languages`, `source_url`, `homepage`); without it a new entry takes
its name from the manifest. `--registry URL` or `$TILO_REGISTRY` targets
another registry, such as one you run yourself (`docs/registry.md`).

A Tilo OS package declares the one architecture it carries: the level
`[package.isa]` names for it, else `v3` on x86_64 and `aarch64` (ARMv8-A) on
ARM, the floor the scaffolded `.cargo/config.toml` builds at.

Web builds are not yet produced by `tilo-app package`.

## Decisions

These are the defaults this flow is built on. Each is a decision, not a
constraint of the design, and may change before Tilo leaves alpha.

1. **One registry.** Alpha has a single registry, `apps.tilo.so`, for Tilo OS,
   Tilo Web, Windows and macOS builds. `--registry` and `$TILO_REGISTRY` exist
   for self-hosted registries and testing.
2. **Namespaces are proven, not granted.** A key may claim a package id only
   inside a namespace it proved: a domain by DNS TXT with `.well-known` as the
   fallback, or a git forge account by a file in a repository named
   `tilo-publisher`. The proof is the line `tilo-publisher=<public key>`.
   `so.tilo` is Tilo's own.
3. **Applications wait for the owner.** The registry verifies proofs when an
   application arrives and again at approval, but only the registry's owner
   approves, on the server. There is no remote approval endpoint.
4. **The registry hosts the bytes.** `publish` uploads each package, signed by
   your key, and the entry points at
   `https://apps.tilo.so/artifacts/<blake3>.tapp`. An upload stays private
   until review approves the entry that names it.
5. **One key per publisher.** The same Ed25519 key signs packages and registry
   requests, kept in `publisher-key.json` or, in CI, in
   `$TILO_PUBLISHER_KEY`. The App Publisher app manages keys on Tilo OS.
6. **Desktop builds are self-contained.** A Windows or macOS build is one
   static executable with the toolkit linked in, up to 64 MiB, using the
   per-user `tilo-runtime` service for collaboration when it runs.
7. **A package claims only what it carries.** A Tilo OS package declares the
   architecture it was built for and no other.
8. **Every release is reviewed.** New entries and updates alike wait for the
   owner; `publish` never makes anything public by itself.
