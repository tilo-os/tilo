# Network

`tilo-net` (`crates/platform/tilo-net`) is the network capability for apps on Linux, macOS
and the web. Apps do not build HTTP clients, open sockets or pick TLS roots themselves.

## The API

| Item | Feature | Linux / macOS | Web |
|---|---|---|---|
| `Client`, `Request`, `Response` | `http` | reqwest over rustls (ring) | `fetch`; body streamed from its `ReadableStream` |
| `ws::connect` | `ws` | tokio-websockets | the page's `WebSocket` |
| `webtransport::connect` | `webtransport` | web-transport-quinn | the browser's `WebTransport`, where it exists |
| `status::{current, watch}` | always | a route to the public internet | `navigator.onLine`; metering from Network Information |
| `sockets::{TcpStream, TlsStream, TcpListener, UdpSocket}` | `sockets` | tokio | `Unsupported` |
| `proxy` | always | unused | the host's same-origin fetch proxy |

- Every future is `Send` on every target, so it runs on `tilo_rt::app()` and in
  `LatestRequest::spawn`. In a page the browser objects sit behind `SendWrapper`; a page has
  one thread.
- `Response::chunk` streams the body. `bytes_limited` refuses a body over a limit before
  buffering past it.
- The request timeout covers the body too. On the web it is a `setTimeout` that aborts the
  fetch.
- Bodies are decompressed (gzip, brotli) as a browser does.
- `availability(Feature)` and `CAPABILITIES` answer what a target provides. `CrossOriginHttp`
  and `Sockets` are unsupported on the web, and `Metered` is known only there.
- TLS: one provider (ring). `client_config()` uses the platform verifier, as reqwest does:
  the Tilo bundle on Linux, the Keychain on macOS. WebTransport can pin a server certificate
  by its SHA-256 instead, as a page's `serverCertificateHashes` does.
- Desktop-only code that has not moved yet keeps `tilo_net::http::{client, blocking_client}`,
  the reqwest builders on the same TLS defaults.

Related pieces:

- `tilo_rt::sleep` is the portable pause for retry backoffs.
- `tilo_dav::net` is the async WebDAV/CalDAV client on `Client`.

## Using it from an app

Send requests from the app runtime and hand results to the UI through a channel that wakes
it, or through `LatestRequest`/`LatestTask`. Declare the capability in the manifest:

```toml
[capabilities]
required = ["network.http"]           # or optional, if the app works offline
optional = ["network.fetch-proxy"]    # only if the app opts into the proxy
```

## CORS and the fetch proxy

A page may read a cross-origin response only when the site sends
`Access-Control-Allow-Origin`.

- Open APIs do, so weather, maps and speed-test work directly: Open-Meteo, OSM tiles,
  Nominatim, OSRM and Cloudflare's speed endpoints.
- An HTTP tester, a feed reader and CalDAV servers mostly do not. For them the host that
  serves the page may relay requests through a proxy on its own origin.

**Off by default, twice:**

- The app opts in with `ClientBuilder::fetch_proxy(ProxyUse::CrossOrigin)`, or per request.
  api-tester, feeds and calendar do.
- The host enables the proxy with `tilo_net::proxy::configure(Some(url))`, or by setting
  `globalThis.__tiloFetchProxy = "/_tilo/net/fetch"` before the runtime loads.
- The endpoint must be on the page's origin. Same-origin requests never use it, and the
  desktop ignores it.

When a request fails in a way that may be CORS, `NetError::may_be_cors()` is true. The
browser does not distinguish a CORS refusal from an unreachable host.

### Wire protocol v1

The constants live in `tilo_net::proxy`. `Client` does the client side, so a proxied response
reads like a direct one.

- **Request:** `METHOD {endpoint}?url={percent-encoded absolute http(s) URL}`.
  - Method and body are the target request's.
  - `x-tilo-proxy: 1` is required, so a cross-site form cannot drive the proxy.
  - Headers a browser will not send (`User-Agent`, `Cookie`, `Referer`, `Origin`, `sec-*`, …)
    arrive as `x-tilo-forward-<name>`, and the proxy restores them.
  - The page's own cookies authenticate the page to the host. They are never forwarded.
- **Response:** the target's status, headers and body.
  - Hop-by-hop headers are removed.
  - `Set-Cookie` becomes `x-tilo-forward-set-cookie`, so the host's origin never stores it.
  - `x-tilo-proxy-final-url` names the URL that answered after redirects.
- **Proxy failure:** a status with `x-tilo-proxy-error` and a text body.

  | Code | Status |
  |---|---|
  | `bad-request` | 400 |
  | `unauthorized` | 401 |
  | `blocked` | 403 |
  | `too-large` | 413 |
  | `unreachable` | 502 |
  | `timeout` | 504 |

### What a host's proxy must enforce

- **Authentication:** only signed-in pages of that host may use it. Rate-limit it per user.
- **Destinations:** refuse the proxy's own network: loopback, RFC 1918, link-local,
  unique-local, CGNAT, multicast and unspecified addresses.
  - Resolve the name, check every address, and connect to the checked address, not a fresh
    lookup.
  - Follow redirects in the proxy, and repeat the check on every hop.
  - Drop `Authorization` when a redirect changes origin.
- **Schemes:** `http` and `https` only.
- **Bounds:** request and response size limits (the dev server uses 16 MiB and 64 MiB) and an
  overall deadline (30 s).
- **Logging:** never log request bodies or `Authorization`. They carry users' credentials
  (CalDAV passwords, API keys).

### Development

`crates/browser/tilo-web-desktop/scripts/serve.mjs BUNDLE_DIR --fetch-proxy` serves a built
desktop bundle on loopback and implements v1 at `/_tilo/net/fetch`. It also sets the page
global.

- `--allow-private` lets it reach a local test server.
- It is unauthenticated and resolves names separately from connecting, so it is for
  development only.

Tilo Cloud should implement the same contract behind its session, set `__tiloFetchProxy` in
the page it serves, and advertise `network.fetch-proxy`.

## Not done yet

- **Status:** the desktop answers from routes, not NetworkManager or `NWPathMonitor`, so
  metering is unknown there.
- **Browser tests:** there is no wasm-bindgen-test suite in headless Chrome yet. The web
  backends were verified through the desktop bundle smoke:
  - maps fetches, streams and decodes real tiles in the page;
  - the proxy was exercised through the dev server, not yet from an app in a page.
- **Remaining blocking clients:** in desktop-only apps (store, browser, imager, git-client,
  mail and others), and in the GStreamer media paths of music and tube.
