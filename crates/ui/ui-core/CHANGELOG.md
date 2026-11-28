# Changes to tilo-ui

## Unreleased

### The default build leaves out Tilo's host crates

`default` is now `["native"]`, and `native` no longer turns on `local-daemons`. A plain
`tilo-ui` dependency builds the toolkit and its native window, and no longer builds
tilo-webview, tilo-page-engine, tilo-content-device, runtime-portal, tilo-identity-client or
the collaboration wire's client.

Turn on the parts you use:

| Feature | What it brings back |
| --- | --- |
| `webview` | The WebView widget (tilo-webview, tilo-page-engine) |
| `content` | The content module (tilo-content-device) |
| `local-daemons` | Clients of the local session daemons: the runtime portal and the identity service |
| `broker-transport` | The collaboration manager's built-in transport; turns on `local-daemons` |
| `host` | `native` with all of the above: what an app running inside Tilo builds with |

```toml
tilo-ui = { version = "…", features = ["webview"] }
```

An app built on `tilo-app` needs no change: `tilo-app` turns on what it uses.

The browser build is unchanged: `default-features = false` for `wasm32-unknown-unknown`.
