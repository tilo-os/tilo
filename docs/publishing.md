# Publish a Tilo app

Getting an app from `tilo-app package` into the store takes the CLI, one key,
one application for the namespace your app ids live in, and then one command
per release ([Build a Tilo app](app-development.md) covers the app itself):

```bash
cargo install --locked --git https://code.tilo.so/tilo/tilo.git tilo-app-cli   # once
tilo-app new reading-list --id org.example.reading-list && cd reading-list
tilo-app keys new                                                # once
tilo-app apply --domain example.com --contact you@example.com    # once
tilo-app package
tilo-app publish
```

To build and publish from your forge's CI instead, see [From CI](#from-ci).

The registry's owner reviews every application and every release before it
reaches users. [Decisions](#decisions) lists the choices behind this flow;
each one can still change.

## Your publisher key

`tilo-app keys new` writes a new Ed25519 key to
`$XDG_CONFIG_HOME/tilo/publisher-key.json` (`--key PATH` elsewhere), readable
only by you, and prints its public half. It never replaces an existing key.
Back the file up: the key is your identity as a publisher, and an update
signed by any other key is refused.

The same key signs your packages and the registry requests `tilo-app publish`
sends: `tilo-app package` uses it whenever it can find it, and a throwaway key
otherwise (`--identity FILE` picks another). `tilo-app keys show` prints the
public key and the proof line described below. Every command that needs the
key reads `--key`, then `$TILO_PUBLISHER_KEY` (the file's contents or its bare
hex seed, which is how CI passes it), then the default path.

On Tilo OS, the App Publisher app does the same with buttons and actions:
it creates the key, shows the public key and proof line, copies the key for a
CI secret, applies for namespaces and checks the application. It uses the
same file, which the sandbox keeps private to App Publisher.

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
tilo-app package                                   # Tilo OS, this machine's CPU
tilo-app package --arch aarch64                    # Tilo OS on ARM
tilo-app package --platform linux --arch x86_64    # other Linux desktops
tilo-app package --platform macos --arch aarch64   # macOS
tilo-app package --platform windows --arch x86_64  # Windows
tilo-app package --platform web                    # the browser
tilo-app package --platform android                # Android phones and tablets
tilo-app publish
```

`publish` checks each package the way the registry will, uploads it, and asks
for the catalogue entry that lists it: a new entry the first time, an update
after that. Every build of one app goes into the same request, and a release made while an earlier one still waits for review joins it. Nothing is public until the owner approves the request; the output
names the entry and where it waits. `--metadata store.json` sets the store
presentation (localized `name`, `tagline`, `description`, `images`,
`supported_languages`, `source_url`, `homepage`); without it a new entry takes
its name from the manifest. `--registry URL` or `$TILO_REGISTRY` targets
another registry, such as one you run yourself (`docs/registry.md`).

A Tilo OS package declares the one architecture it carries: the level
`[package.isa]` names for it, else `v3` on x86_64 and `aarch64` (ARMv8-A) on
ARM, the floor the scaffolded `.cargo/config.toml` builds at.

A web package, `dist/<package>-web-wasm32.tapp`, carries the app as a
WebAssembly side module of the browser's toolkit core
([The web](app-development.md#the-web)). Its signed `[target]` names the
`web` platform, `wasm32`, the WebAssembly features the module uses and the
core's runtime ABI id (`tilo-runtime/2026.07+wasm.<id>`). The registry lists
it beside the app's other builds, and a page installs it only into a core with
that id, so a module built against another core is refused before it links.
The core itself is Tilo's to publish (`so.tilo.runtime.web`); an app never
ships one. That core exports what Tilo's own web apps import, so a module
that needs more of a shared crate than they do does not link in it yet.

An Android package, `dist/<package>-android-<arch>.tapp`, carries the
release-signed APK as `bin/<package>.apk`; the `.apk` and the `.aab` beside it
are for sideloading and for Google Play. Its signed `[target]` names the
`android` platform and the architecture. The registry lists it beside the app's other
builds and Tilo OS clients never install it. The APK is signed with your
Android release key, separate from the publisher key and kept beside it
([Android signing](android.md#signing)); keep both, since Android refuses an
update signed with another key.

## From CI

```bash
tilo-app add ci --forge github                            # .github/workflows/tilo.yml
tilo-app add ci --forge forgejo --runs-on codeberg-small  # Codeberg
tilo-app add ci --forge forgejo                           # another Forgejo host
tilo-app add ci --forge gitea                             # .gitea/workflows/tilo.yml
```

The workflow goes at the root of the repository holding the app and runs in
the app's directory. Every push and pull request runs `tilo-app check`, then
packages each platform and uploads the packages as build artifacts. Pushing a
tag named `v*` also runs one `tilo-app publish` over all of them.

Add one secret, `TILO_PUBLISHER_KEY`, holding the contents of your
`publisher-key.json`. Builds without it, such as pull requests from forks, are
signed with a throwaway key and are never published. Set the variable
`TILO_REGISTRY` to publish to another registry.

On GitHub the matrix covers Tilo OS, Linux, Windows and macOS, each on x86_64 and
aarch64, the web and Android (aarch64), using GitHub's hosted runners. Forgejo
and Gitea hosts provide Linux runners, so their workflow builds Tilo OS for
x86_64, the web and Android and lists the other builds commented out: enable
each one you have a runner for. The Android job installs the Android SDK with
`tilo-app setup android` and caches it; set the `TILO_ANDROID_KEYSTORE`
(base64) and `TILO_ANDROID_KEYSTORE_PASS` secrets to sign with your own
release key. The Tilo OS, web and Android jobs run in `registry.tilo.so/tilo/tilo-builder`, the
image Tilo itself builds in, so a package built in CI links the same system
libraries and CPU floor as the OS it runs on, and the web job finds the web
toolchain there. Each job installs `tilo-app` from the Tilo revision your
`Cargo.lock` pins and caches it.

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
9. **Builds happen in your CI.** The registry builds nothing. `tilo-app add
   ci` writes a workflow for GitHub Actions, Forgejo Actions (including
   Codeberg) or Gitea Actions, and the same commands run anywhere else.
10. **Tags publish.** Pushes and pull requests build; a `v*` tag publishes.
    The key lives in one CI secret, and the registry's review, not the
    workflow, decides what users see.
11. **CI builds with Tilo's toolchain.** Tilo OS packages build in the
    published `tilo-builder` image, and `tilo-app` comes from the Tilo
    revision the app's `Cargo.lock` pins, so CI checks and packages the app
    against the same SDK it was written against.
12. **Packages are signed by the publisher key by default.** `tilo-app
    package` signs with the key `publish` would use, so a package built on
    any machine with the key can be published unchanged.
13. **The key stays on the machine.** App Publisher keeps the key in its file,
    never in its synced, versioned resource. Creating or copying the key
    needs the owner's role and a confirmation, whether a person, a script or
    an agent asks.
