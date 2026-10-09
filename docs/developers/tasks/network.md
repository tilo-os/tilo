# Fetch data without blocking the view

From your app directory, declare the portable HTTP capability:

```bash
tilo-app add capability network.http
tilo-app doctor --json
```

Use `--required` only if the app cannot work without it. A capability declaration
is not a permission grant; the host can still refuse access. Keep a useful
local or unavailable state. Add `tilo-net` with the `http` feature from the
same source and pinned revision as your generated `tilo-app` dependency in
`Cargo.toml`; add `json` only when using JSON bodies.

Reuse a client: its clones share the native connection pool and use browser
fetch on the web. Put requests on the app runtime or inside the appropriate
resource effect, never a blocking client or subprocess in a view callback.

```rust
async fn fetch_text(
    client: &tilo_net::Client,
    url: &str,
) -> Result<String, tilo_net::NetError> {
    client.get(url)
        .timeout(std::time::Duration::from_secs(15))
        .send().await?
        .error_for_status()?
        .text().await
}
```

The timeout includes the body. HTTP error statuses need `error_for_status`;
a successful connection does not mean a successful request. Use streaming
`Response::chunk` for large bodies. Show translated loading/error/retry states,
and discard stale results when the resource, account or request changes.
Browser CORS still applies; a host fetch proxy is an explicit capability,
not a reason to disable TLS checks or embed credentials in URLs.

Network hints live in `tilo_net::status`: `current()`, `watch().last()`, and
`Watch::changed().await`. Online means "worth trying", not server reachability.
Metering is `Option<bool>`: an unavailable/refusing platform can return `None`.
Do not cache a metering answer for the app's lifetime or treat unknown as proof
that speculative downloads are safe. Tilo's native host automatically supplies
lossless hints to app launches with network access: `Some(false)` means known
unmetered. Older hosts and ordinary Linux desktops use the standard portal;
its false answer stays unknown because it can include unknown transport cost.
Host disconnects discard the snapshot and leave the same app on portable
fallback; the inherited host connection is consumed once, without automatic
reconnection. A normal app relaunch receives a fresh launcher-authenticated
connection and snapshot. Explicit user requests still handle actual errors.

For external writes, declare resource effects with the appropriate identity,
role and confirmation. Shared undo does not reverse a request to a server.
Persist the collaborative result through resource actions; keep connections,
tokens, in-flight work and transport caches outside the shared document.
An agent and a button invoke the same permission-filtered action.

Verify offline, refusal, non-2xx replies, timeout, a stale completion and a
metering transition. Use a local test server rather than a live API or a secret.
Run `tilo-app test --json`; real host permission and transport acceptance need
separate integration journeys.

Next: [Actions and effects](actions.md), [Verification](verify.md),
[Collaboration and undo](collaborate.md).
