# Changes to tilo-ui-app

## Unreleased

### The default build leaves out Tilo's host crates

`default` is now `["native"]`; it was `native` and `broker-transport`. The native runner and
its window stay in the default build. Running inside Tilo moves behind a new `host` feature:
launch through Tilo's launcher, following the workspace's settings, and the WebView frame
trace. `model` no longer pulls in tilo-settings-store.

Turn on the parts you use:

| Feature | What it brings back |
| --- | --- |
| `webview` | tilo-ui's WebView widget |
| `content` | tilo-ui's content module |
| `local-daemons` | Clients of the local session daemons |
| `broker-transport` | The collaboration manager's built-in transport; turns on `local-daemons` |
| `host` | Everything an app running inside Tilo uses: `native`, `model`, all of the above, launch, the runtime portal, the settings store and the content device |

```toml
tilo-ui-app = { version = "…", features = ["host"] }
```

An app built on `tilo-app` needs no change: `tilo-app` turns on `model`, `webview` and
`content` itself, and its native runner (`tilo-runner-native`, which `tilo-app`'s default
`native` feature selects) turns on `host`.
