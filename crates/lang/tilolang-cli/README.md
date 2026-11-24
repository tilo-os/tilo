# tilolang-cli

The `tilolang` command: it creates, checks, runs, builds, packages and
publishes [Tilo](https://code.tilo.so/tilo/tilo) apps, whether you write them in
[tilolang](https://tilolang.dev) or in Rust with
[`tilo-app`](https://crates.io/crates/tilo-app).

## Install

```sh
curl -fsSL https://tilolang.dev/install | sh
```

The installer checks the release's signature before it installs anything. To
build the command from source with Rust instead:

```sh
cargo install tilolang-cli
```

Either way, you run it as `tilolang`.

## Use it

```sh
tilolang guide            # how to start, in the terminal
tilolang check            # a package's errors; a Rust app's lints and tests
tilolang dev              # run the app and apply each change you save to the running window
tilolang build            # a signed .tapp, or a Rust app's build for a --target
tilolang publish          # send a release to the app registry
tilolang repl             # type tilolang in one of your workspaces
```

`tilolang --help` lists every command, and `tilolang COMMAND --help` explains
each one. A command works out what kind of app it's looking at: a directory of
`.tl` files is a tilolang package, and a Cargo crate is a Rust app.

`dev` runs a tilolang app's window on the tilolang runtime of the Tilo
installed on your computer.

`repl` and `push` reach your workspace through the `tilo` command, as you, so
they need it installed and signed in.

## Learn more

- [tilolang docs](https://tilolang.dev/docs/): the language, its components and
  layout
- [Develop an app](https://tilolang.dev/docs/develop)
- [Rust apps](https://tilolang.dev/docs/rust-apps)
- [Publishing](https://tilolang.dev/docs/publishing)

## License

MIT
