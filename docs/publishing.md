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

To build and publish from your forge's CI instead, see [From CI](#from-ci). The two SDKs are
packages on PyPI and npm rather than apps in the registry: [Publishing the
SDKs](#publishing-the-sdks).

The registry's owner reviews every application and every release before it
reaches users. [Decisions](#decisions) lists the choices behind this flow;
each one can still change.

## Your publisher key

`tilo-app keys new` creates a new Ed25519 key in your secret store (the Secret
Service on Linux, the login Keychain on macOS, or sealed files in
`$TILO_SECRETS_DIR` on a machine without one) and prints its public half. It
never replaces an existing key. Back it up with `tilo-app keys export`, which
prints it as a key file, `{"secret_key_hex": ...}`: the key is your identity as
a publisher, and an update signed by any other key is refused.

The same key signs your packages and the registry requests `tilo-app publish`
sends: `tilo-app package` uses it whenever it can find it, and a throwaway key
otherwise (`--identity FILE` picks another). `tilo-app keys show` prints the
public key and the proof line described below. Every command that needs the
key reads `--key FILE` (a key file), then `$TILO_PUBLISHER_KEY` (the key
file's contents, which is how CI passes it), then the secret store. A key file
for a build that never touches a secret store comes from
`tilo package runtime key generate --out FILE`.

On Tilo OS, the App Publisher app does the same with buttons and actions:
it creates the key, shows the public key and proof line, copies the key for a
CI secret, applies for namespaces and checks the application. It uses the
same key: the secret store holds it under App Publisher's app id, which
`tilo-app` reads too.

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

## Your Store page

Write a `store.json` alongside your app before publishing. The same signed
metadata feeds the Store's cards, detail page and semantic inspection:

```json
{
  "name": { "en": "Reading List" },
  "tagline": { "en": "Keep your next good read close." },
  "description": {
    "en": "Collect books, keep notes and share your reading list with a workspace."
  },
  "supported_languages": ["en"],
  "images": {
    "en": [
      {
        "kind": "screenshot",
        "url": "https://example.com/reading-list.png",
        "alt": "A reading list with books grouped by reading progress"
      }
    ]
  },
  "source_url": "https://github.com/your-org/reading-list",
  "homepage": "https://example.com/reading-list"
}
```

Replace the example URLs with your project's credential-free HTTPS URLs.
Use a short tagline for catalogue browsing; put the full explanation in
`description`. The detail page shows previews and the description before
package facts. `images` also accepts `icon` and `banner`; image descriptions
(`alt`) are required by the registry. The Store shows up to three screenshots.

Each localized text map needs an `en` fallback. Add translations using
canonical language tags such as `es` or `pt-BR`. `supported_languages` names
the languages your **app UI** actually supports, independently of the Store
page's translations; keep the list sorted and unique. The Store displays that
list, and an agent can read it by inspecting the specific app.

Submit it with the builds:

```bash
tilo-app publish --metadata store.json
```

Publication still queues an owner review. A missing field or invalid locale
is refused rather than silently dropping your presentation.

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

Add one secret, `TILO_PUBLISHER_KEY`, holding what `tilo-app keys export`
prints. Builds without it, such as pull requests from forks, are
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

### Reuse a GitHub build workflow

On GitHub, `add ci` writes two files: `.github/workflows/tilo.yml` handles
pushes, pull requests and tags; `.github/workflows/tilo-build.yml` is a
[reusable workflow](https://docs.github.com/en/actions/how-tos/reuse-automations/reuse-workflows).
The caller passes the app directory and lockfile, explicitly names the three
signing secrets, and requests publication only for `v*` tags. `--dry-run`
lists both files without writing them; `--force` replaces both. Existing
files are preserved unless you request replacement.

To use the build from another GitHub repository, commit the generated
`tilo-build.yml` to a shared repository and call its exact commit:

```yaml
name: Tilo app
on: [push, pull_request]
permissions:
  contents: read
jobs:
  app:
    uses: your-org/app-builds/.github/workflows/tilo-build.yml@<full-commit-sha>
    with:
      working-directory: .
      lockfile: Cargo.lock
```

Replace the repository and commit with your actual shared workflow. GitHub
must allow the caller to access that repository. This is a callable workflow
you own; it does not require an assumed Tilo repository on GitHub.
The workflow checks out **the caller's app**, builds the existing ten
platform/architecture targets, and installs the CLI from **the caller's SDK
pin**, rather than the shared workflow repository's SDK.

Publication defaults to false. For a release caller, set `publish: true`
and explicitly pass `TILO_PUBLISHER_KEY` under the job's `secrets`; Android
release signing also uses `TILO_ANDROID_KEYSTORE` and
`TILO_ANDROID_KEYSTORE_PASS`. `registry` optionally selects another registry;
`linux-runner` overrides the default Linux x86 runner. Pull request events
never run the publish job, even if the caller requests it. Publication still
submits a request for owner review; it does not approve a release.

Forgejo and Gitea keep their existing single-file workflow.

## Publishing the SDKs

The SDKs are not apps and do not go through the registry: they are two packages, both named
`tilo-sdk`, one on [PyPI](https://pypi.org/project/tilo-sdk/) for Python and one on
[npm](https://www.npmjs.com/package/tilo-sdk) for TypeScript. The Python *distribution* is
`tilo-sdk` because plain `tilo` on PyPI belongs to an unrelated project; what it installs is the
module `import tilo`. npm has no `@tilo` scope to publish under, so the package there is
unscoped. One release tag publishes both: `v0.4.0` publishes `tilo-sdk` 0.4.0 to each registry,
and nothing in the tree carries a version — the tag is the one place it is decided. A
pre-release goes to npm under the `next` dist-tag, so `bun add tilo-sdk` keeps getting the
newest final release.

The tag's run builds both packages, reads the artifacts back (the name, the version and the
module the wheel carries) and only then sends them; `tools/ci/publish/sdks.sh` is the whole of
it. `main` runs the same build with no credentials, on a stand-in version, so a manifest that
stops being publishable breaks long before a tag does.

Re-running a release's SDK step finishes a release that stopped halfway instead of failing on
what it already sent: a version already on a registry is skipped, and on PyPI every file is
checked against the project's index before it is uploaded, so a run that stopped between the
wheel and the sdist uploads only the sdist. Dispatch it on the tag:

```sh
t dispatch publish-sdks --ref tag:v0.4.0 --publish
```

### The two registry credentials

Neither registry can be published to by trusted publishing from here. PyPI's trusted publishing
takes an OpenID Connect identity from GitHub Actions or GitLab CI, and knows neither Forgejo's
nor t's own issuer, so the upload is an API token. npm's provenance attestation has the same
restriction, so these packages carry no provenance from this CI.

| Secret | Registry | What the value is |
| --- | --- | --- |
| `PYPI_TOKEN` | PyPI | An API token that may upload `tilo-sdk` |
| `NPM_TOKEN` | npm | A token npm takes with read and write on `tilo-sdk` that publishes without a second factor: an automation token is the kind made for that, and a granular one only if it carries the same exemption |

The first upload of `tilo-sdk` to PyPI is the special case: a project-scoped token can only be
made for a project that exists, and a pending publisher only works through trusted publishing, so
the first release needs an account-scoped token. Narrow it to `tilo-sdk` once the project is up.
The npm token's package list grows with the native packages below.

Both live in the t server's config, never in this repository: the `t-config` secret in the infra
repository, file `clusters/prod/apps/t-staging-aws/config.sops.yaml`, in the `config.toml` key of
its `stringData`, as `[[secret]]` entries:

```toml
[[secret]]
name = "NPM_TOKEN"
value = "…"
refs = ["tag:v*"]
repos = ["tilo"]

[[secret]]
name = "PYPI_TOKEN"
value = "…"
refs = ["tag:v*"]
repos = ["tilo"]
```

`refs` is what makes them safe to hold: only a trusted run of a release tag is given one, and a
secret that names no `nodes` reaches `release` nodes alone, which run trusted work only. A run of
a branch, of a pull request or of someone's working tree publishes nothing, and a check that would
need the token there is skipped with that as its reason. What a leaked token could do is the
scope above, so who may publish is who may push a `vX.Y.Z` tag.

### Native packages

Both SDKs will gain a native half over the one Rust client, so a program can reach a Tilo host
directly instead of through a browser or a relay: Python's `tilo._native`, a PyO3 extension
module, and the npm package `tilo-sdk-native`, built with napi-rs as one package per platform.
Neither crate exists yet, so nothing publishes them, and the pure packages above release on their
own until they do. What follows is the step that switches on, as a check of its own beside
`publish-sdks`, so that landing the crates is the only change it needs.

The Python wheels join the release `publish-sdks` made: they are files of the same `tilo-sdk`
project at the same version, and the pure `py3-none-any` wheel stays published for platforms
without one. `sdk/python/pyproject.toml` gains the crate as the project's native part:

```toml
[tool.maturin]
manifest-path = "native/Cargo.toml"
python-source = "src"
module-name = "tilo._native"
```

The wheels are built per platform, then sent with the release's other files:

```sh
# From sdk/python. manylinux's own images carry the toolchain and the glibc floor; the arm64
# wheel builds under qemu where no arm64 node runs it.
podman run --rm -v "$PWD:/io:z" -w /io --platform linux/amd64 \
  quay.io/pypa/manylinux_2_28_x86_64 bash -ec \
  'pip install maturin && maturin build --release --compatibility manylinux_2_28 --out dist'
podman run --rm -v "$PWD:/io:z" -w /io --platform linux/arm64 \
  quay.io/pypa/manylinux_2_28_aarch64 bash -ec \
  'pip install maturin && maturin build --release --compatibility manylinux_2_28 --out dist'

# macOS arm64 and x86_64 on a Mac, Windows x86_64 on Windows, once those lanes exist.
maturin build --release --target aarch64-apple-darwin --out dist
maturin build --release --target x86_64-apple-darwin --out dist
maturin build --release --out dist

# Every wheel, with the pure wheel and the sdist of the same version, to PyPI.
uv publish --trusted-publishing never --check-url https://pypi.org/simple/tilo-sdk/ dist/*
```

The npm side is one package per platform, laid out by napi-rs, and the package that names them:

```sh
# From sdk/typescript/native, per platform.
bunx @napi-rs/cli build --platform --release --target x86_64-unknown-linux-gnu
bunx @napi-rs/cli build --platform --release --target aarch64-unknown-linux-gnu
bunx @napi-rs/cli build --platform --release --target aarch64-apple-darwin
bunx @napi-rs/cli build --platform --release --target x86_64-apple-darwin

# The platform packages first, then tilo-sdk-native, which lists them as optional dependencies.
(cd npm/linux-x64-gnu && bun publish --ignore-scripts)
(cd npm/linux-arm64-gnu && bun publish --ignore-scripts)
(cd npm/darwin-arm64 && bun publish --ignore-scripts)
(cd npm/darwin-x64 && bun publish --ignore-scripts)
bun publish --ignore-scripts
```

The names are `tilo-sdk-native` and `tilo-sdk-native-linux-x64-gnu`,
`tilo-sdk-native-linux-arm64-gnu`, `tilo-sdk-native-darwin-arm64`,
`tilo-sdk-native-darwin-x64`, and later `tilo-sdk-native-win32-x64-msvc`; every one of them
belongs in `NPM_TOKEN`'s package list. `tilo-sdk` loads the native package when it is installed
(`await import("tilo-sdk-native")` inside a `try`, with the pure client when that fails), and
Python's `from tilo import _native` does the same, so neither package breaks where the native
half has not been built.

The check that publishes them, added to `t.toml` when the crates land:

```toml
[[check]]
name = "sdk-native-python-{arch}"
run = "bash tools/ci/publish/sdk-native.sh python {arch} /target/publish/sdk-native/python-{arch}"
requires = ["arch=x86_64"]
outputs = ["/target/publish/sdk-native/python-{arch}/*"]
default = false
[check.matrix]
arch = ["x86_64", "aarch64"]

[[check]]
name = "sdk-native-typescript-{arch}"
run = "bash tools/ci/publish/sdk-native.sh typescript {arch} /target/publish/sdk-native/typescript-{arch}"
requires = ["arch=x86_64"]
outputs = ["/target/publish/sdk-native/typescript-{arch}/*"]
default = false
[check.matrix]
arch = ["x86_64", "aarch64"]

[[check]]
name = "publish-sdks-native"
run = "bash tools/ci/publish/sdk-native.sh publish /target/publish/sdk-native"
needs = ["publish-sdks", "sdk-native-python-x86_64", "sdk-native-python-aarch64", "sdk-native-typescript-x86_64", "sdk-native-typescript-aarch64"]
requires = ["arch=x86_64"]
on = ["tag:v[0-9]*.[0-9]*.[0-9]*", "dispatch"]
secrets = ["NPM_TOKEN", "PYPI_TOKEN"]
```

It needs `publish-sdks` because it adds files to that release rather than making one, and it
builds every platform on the x86_64 lane the SDK checks already run on, the arm64 wheel under
qemu, until there are macOS and Windows lanes for the last two. Like the pure release it skips
what is already on a registry, so it can be dispatched on a tag on its own.

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
   until review approves the entry that names it. The registry then also
   serves a compressed copy at `<url>.zst`, which clients download and
   decompress before checking it against your signed hash; your signature
   covers only the package.
5. **One key per publisher.** The same Ed25519 key signs packages and registry
   requests, kept in your secret store or, in CI, in
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
