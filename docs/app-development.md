# Build a Tilo app

Most Tilo apps and custom views are written in **[tilolang](https://tilolang.dev)**
([language overview](designs/app-language/README.md)), which compiles to sandboxed
WebAssembly and renders natively with **Tilo UI**.

When you need Rust (for example, wrapping a native library or embedding a custom
renderer), you can also build an app directly on the `tilo-app` Rust SDK and Tilo UI.
Both use the same model: declare your state as a typed resource, the changes it
allows as actions, and views bound to both. From that the system provides storage,
sync between devices and people, history and per-author undo, permissions, search,
the command palette, and the interface scripts and agents use to drive the app
without the screen. The same source works on Tilo, other Linux desktops, macOS,
Windows, Android, and the web (iOS not yet).

Tilo is in alpha: the SDK changes often. An app builds against the exact SDK
revision its `Cargo.lock` pins, so it keeps building while the SDK moves on.

## Install the tool

You need Rust through [rustup](https://rustup.rs) and Python 3.11 or newer as
`python3` (building the SDK stages its pinned fonts with a Python script; on
macOS, `brew install python`). Then:

```sh
cargo install --locked --git https://code.tilo.so/tilo/tilo.git tilolang-cli
```

This installs `tilolang`, the one command for apps written in tilolang and in
Rust alike: `dev`, `check`, `build`, `package` and `run` read which kind of app
they were given from its directory (one holding `.tl` files is a tilolang
package). `tilolang --help` lists its commands; a Rust app's commands accept
`--json` and then write exactly one JSON document to stdout, so scripts and
agents can drive them. `tilolang guide` has short offline workflows
(`start`, `actions`, `collaborate`, `permissions`, `network`, `verify`,
`publish`).

In a Tilo checkout, `cargo run --locked -p tilolang-cli -- <command>` runs the
tool from source instead.

## Create an app

```sh
tilolang new reading-list --id org.example.reading-list
cd reading-list
tilolang dev
```

The id is the app's permanent name; use a namespace you control (a domain you
own, reversed), because publishing checks that you own it. `tilolang new
--list-templates` lists the templates (`starter`, `document`, `collection`,
`graph`).

To learn one idea at a time, start from an example instead. Each app in
[`examples/apps`](../examples/README.md) is complete and tested:

```sh
tilolang examples
tilolang new my-list --from-example shared-checklist
```

`tilolang dev` builds the app, opens it and rebuilds on every save; the open
document, screen and caret carry over, and a failed build leaves the last good
version running. The other everyday commands:

```sh
tilolang run      # build once and open it
tilolang test     # the Rust tests, plus the accessibility and action audit
tilolang check    # the full check: structure, format, Clippy, tests, scenarios
tilolang shot     # render the app to an image without a window
```

### Running a build through this device's daemon

A built app directory (its `tilo-app.toml` and payload) installs as this
computer's own development build with:

```sh
tilo app install --dev path/to/build
```

The daemon marks the package as this device's dev build of the app, packs and
signs it with the device's own key (the key stays in the daemon), and installs
it under an id of its own, marked as a dev build in the shell. It starts with
no grants, asks for what it needs like any new app, and never takes the
installed app's id or grants. Only the person on this computer's own daemon
connection can install one; a key, a visitor or another account is refused.

A new app is a Cargo project: `src/` holds the app, `tilo-app.toml` is the
package manifest (id, entrypoint, sandbox), `surface.toml` records the public
surface, `locales/` holds the text, `tests/` the tests and `AGENTS.md` a short
guide for coding agents.

## The model: resources, actions, bound views

This is the whole of [`examples/apps/counter`](../examples/apps/counter),
slightly shortened. The state is a model:

```rust
use ui::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Counter::within_limit)]
pub struct Counter {
    pub count: i64,
}

impl Counter {
    pub fn within_limit(&self) -> Result<(), Refusal> {
        if self.count.abs() > 1_000_000 {
            return Err(Refusal::new("the count is limited to a million either way"));
        }
        Ok(())
    }
}
```

Every change is an action: a method marked `#[action]` on the resource. Its
doc comment is its label, its parameters are typed, and returning `Err`
refuses it without changing anything. Setting a field directly
(`Counter::paths().count.set(42)`) is an edit at a typed path and goes through
the same checks.

```rust
#[resource(kind = "example.counter", label = "Counter")]
impl Counter {
    /// Add an amount to the count.
    #[action]
    pub fn add(&mut self, #[param("Amount to add")] amount: i64) -> Result<(), &'static str> {
        self.count = self.count.checked_add(amount).ok_or("the count would pass its limit")?;
        Ok(())
    }

    /// Reset the count to zero.
    #[action(destructive)]
    pub fn reset(&mut self) {
        self.count = 0;
    }
}
```

The view binds controls to fields and actions. A bound button is disabled
while its action is not allowed, and pressing it invokes the action, exactly
as the palette, an agent or a script would:

```rust
pub struct App;

impl ResourceApp for App {
    type Resource = Counter;
    const ID: &'static str = "so.tilo.example.counter";
    const TITLE: &'static str = "Counter";

    fn view(cx: &mut ViewCx<Counter>) -> Element {
        let count = cx.text(|counter| counter.count.to_string()).with_role(TextRole::Display);
        let actions = cx.actions();
        let add_one = actions.bind_invocation(Button::new("add-one", "counter-add-one"), Counter::ADD.with(1));
        let reset = cx.button("counter-reset-button", Counter::RESET.invoke());
        Box::new(ui::column![count, add_one, reset])
    }
}
```

`src/main.rs` is one line, `ui::main!(example_counter::App);`, which gives the
same app a window, the headless test modes and registration in the web page.
Button labels are keys into `locales/en-US.ftl`, which also says how each
action reads in history.

What this gives you without more code:

- **Collaboration and sync.** Actions become CRDT changes. Two people (or two
  devices) editing at once merge; see the `shared-checklist` example's tests.
- **History and undo.** Each action is a history entry with its author; undo
  takes back only your own actions. A destructive action that undo can take
  back runs at once and offers Undo; one it cannot take back is confirmed by
  the system, never by an app dialog.
- **Permissions.** Roles decide who may invoke which action; a viewer sees the
  state and cannot change it.
- **Search, palette, scripts and agents.** The resource and its actions are
  published (`tilolang surface` writes them to `surface.toml`), so system
  search, the command palette and agents find and invoke them.

Apps start empty. Sample content belongs in tests, screenshots
(`ResourceApp::scenario`) or explicit demos. Apps contain no platform code:
files, network, processes and other system services come from the crates in
`crates/platform/`, which pick a backend per target.

## Test it

A `Replica` applies actions with no window, which is the fastest way to test
what the model allows:

```rust
#[test]
fn a_refused_action_changes_nothing() {
    let mut counter = Replica::<Counter>::new();
    counter.apply(Counter::ADD.with(10)).expect("add ten");
    assert!(counter.apply(Counter::ADD.with(i64::MAX)).is_err());
    assert_eq!(counter.state().expect("counter").count, 10);
}
```

`Replica::new().as_author("ada")` gives each replica an author for merge and
undo tests, and `HeadlessApp::<App>::new()` runs the real app headlessly so a test
can press controls by id and read the published actions. Prefer semantic
assertions (roles, names, state) over pixels. In the Tilo checkout:

```sh
t cargo test --locked -p example-counter    # on the build server
```

`tilolang check` also fails on actions that read a clock, randomness, files or
the network, on missing translations, on controls bound to something the
resource does not publish, and on a public surface that differs from
`surface.toml`.

## Other platforms

`run`, `dev`, `build`, `check` and `package` take `--platform` (`linux`,
`macos`, `windows`, `android`) and `--arch` (`x86_64`, `aarch64`):

```sh
tilolang check --platform all        # lint every other target too
tilolang package --platform macos --arch aarch64
```

On other systems an app ships as one executable with the toolkit linked in.
[Android](android.md) covers the Android build.

## A program in JavaScript or Python

A package can carry a script that a runtime's interpreter runs, so that an
existing JavaScript or Python program becomes an app:

```toml
[package]
optional-runtimes = ["bun/1.4.5+bun"]       # or "python/3.13.18+python"

[entrypoint]
bin = "bin/bun"                            # the interpreter, in the runtime
runtime = "bun/1.4.5+bun"
script = "main.js"                         # a file of this package

[sandbox.net]
mode = "allowlist"
hosts = ["discord.com", "gateway.discord.gg"]

[sandbox.services]
native = "required@40"
```

The launcher starts it the way it starts any app: in the app's sandbox, with
the package mounted at `$TILO_APP_MODULE` and the script as the interpreter's
first argument. The network is what `[sandbox.net]` declares and the workspace
granted, reached through the proxy that `HTTP_PROXY`, `HTTPS_PROXY` and
`ALL_PROXY` name. Nothing else is reachable.

The declaration goes in the package's `app.toml`, beside its `app.tl`. A
package may carry both: `tilolang build` ships the script's files (everything
in the package directory except the language's sources and what a build writes)
beside the compiled module, so a device runs the program as the app's process —
the held connection when it says `connection = true` — and the module in the
app's tilolang host, where its records, actions and views answer as any
tilolang app's. The SDK's native addon is not something a package vendors: the
bun runtime package carries it at `node_modules/tilo-sdk-native`.

An app that calls adds `calls = true` to `[sandbox.net]`: UDP audio and video to
any public internet address, through the same proxy and only while a call of the
app is live. Its TCP stays what `hosts` names.

`$TILO_SVC_NATIVE_FD` is a socket to the device daemon that the launcher
already connected. It carries action frames, one JSON line each, and the daemon
admits every call on it as this app in `$TILO_WORKSPACE_ID`. A tilolang action
reads the app through `invoker.app()`, so it can admit one app's program and
refuse everything else:

```
fn the_bridge(who: Actor) -> Bool {
    (who.app() ?? "") == "so.tilo.discord"
}

action relay(b: Bridge, text: Text) {
    require the_bridge(invoker) "Only the Discord bridge relays"
    ...
}
```

## Calls

An app that carries an outside service's call into a workspace call asks the
connection it holds for a call lane, once its signed manifest declares the
`calls` capability. `CallCarryRequest` names the service's call and the
workspace's voice channel they are one with; `CallReport` says who is in the
outside call and where the bridge stands; `CallHeard` and `CallSaid` exchange
20 ms Opus frames, one speaker at a time in each direction, untouched. The
device says `CallStatus` on the same connection with the line to show in the
service.

The device holds the lane to the workspace's call rule and carries the frames
under the call's media keys, which the app never sees. A computer that cannot
carry a call there, or a workspace that admitted the app to another call,
refuses the pair with the reason. The app plays the outside call's frames out
and hands what the workspace's people say back as they are: mixing, decoding
and re-encoding are the outside service's own business, not the host's.

The interpreters come as runtime packages, never from the host. Each is
built by `tools/apps/runtime_packages.sh` from a pinned upstream release
(bun's release binary, and a relocatable CPython that carries its own `lib/`)
and offered as `so.tilo.runtime.bun` or `so.tilo.runtime.python`. The token is
the release the package provides plus the runtime it is, so `bun/1.4.5+bun`
and `python/3.13.18+python` are the releases those two packages carry today;
`optional-runtimes` installs the package with the app, and its `bin/bun` or
`bin/python3` is the binary this entrypoint names. A release builds its
package from `os/external-assets/github-assets.tsv`, which pins every asset's
SHA-256.

Each runtime package also carries the SDK's native part, so a program reaches
this computer's daemon without shipping a build of it: bun resolves
`import("tilo-sdk-native")` from the runtime's own modules (the launcher names
that directory in `NODE_PATH` for a script entrypoint), and CPython imports
`tilo` and its `_native` extension from the runtime's own lib directory.

## Publish

```sh
tilolang keys new
tilolang package
tilolang publish
```

[Publishing](publishing.md) covers the publisher key, proving a namespace and
building releases from CI; [the registry](registry.md) describes how packages
are stored, verified and served.

## Reference

- The SDK source: [`crates/sdk/tilo-app`](../crates/sdk/tilo-app); browse the
  API with `cargo doc --locked -p tilo-app --no-deps --open`.
- The [interface guidelines](../crates/ui/ui-core/HIG.md): layout,
  navigation, text and the shared controls.
- [Architecture](architecture/README.md): how apps fit the system.
