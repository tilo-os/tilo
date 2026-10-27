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
| `proxy` | always | unused | the workspace host's fetch proxy |

- Every future is `Send` on every target, so it runs on `tilo_rt::app()` and in
  `LatestRequest::spawn`. In a page the browser objects sit behind `SendWrapper`; a page has
  one thread.
- `Response::chunk` streams the body. `bytes_limited` refuses a body over a limit before
  buffering past it.
- The request timeout covers the body too. On the web it is a `setTimeout` that aborts the
  fetch.
- Bodies are decompressed (gzip, brotli) as a browser does.
- `availability(Feature)` and `Feature::targets` answer what a target provides. `CrossOriginHttp`
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
- An HTTP tester, a feed reader, a video directory and CalDAV servers mostly do not. For
  them the workspace host the page is admitted to relays requests through its fetch proxy.

**Off by default, twice:**

- The app opts in with `ClientBuilder::fetch_proxy(ProxyUse::CrossOrigin)`, or per request.
  api-tester, feeds, tube and calendar do.
- The page names the proxy in `globalThis.__tiloFetchProxy`, which tilo-net reads on every
  request:
  - the browser runtime sets it to a function answering `{endpoint, session}` once the page
    is admitted to a host whose session grant carries `FEATURE_FETCH_PROXY`
    (`web/fetch-proxy.mjs`); the endpoint is that host's `/peer/v1/fetch`, and the session
    follows reconnects;
  - a host on the page's own origin that needs no session sets it to the endpoint string
    (the development server does);
  - a host embedding tilo-net calls `tilo_net::proxy::configure(Some(url))` instead.
- Same-origin requests never use it, and the desktop ignores it.

When a request fails in a way that may be CORS, `NetError::may_be_cors()` is true. The
browser does not distinguish a CORS refusal from an unreachable host.

### Wire protocol v1

The constants live in `tilo_net::proxy`. `Client` does the client side, so a proxied response
reads like a direct one.

- **Request:** `METHOD {endpoint}?url={percent-encoded absolute http(s) URL}`.
  - Method and body are the target request's.
  - `x-tilo-proxy: 1` is required, so a cross-site form cannot drive the proxy.
  - `x-tilo-session` carries the page's session at a workspace host on another origin (the
    web entry's page and the host's `/peer/v1/fetch` are different origins).
  - Headers a browser will not send (`User-Agent`, `Cookie`, `Referer`, `Origin`, `sec-*`, …)
    arrive as `x-tilo-forward-<name>`, and the proxy restores them.
  - The page's own cookies and session authenticate the page to the host. They are never
    forwarded.
  - A media element cannot send headers. Its `GET` or `HEAD` carries the marker and the
    session in the query instead: `&tilo-proxy=1&tilo-session={session}`
    (`tilo_net::proxy::media_url`). No other method may, so a cross-site form still cannot
    drive the proxy. An open-ended byte range it asks for (`Range: bytes=N-`) is asked of the
    target 2 MiB at a time; the target answers `206` with the whole length, and the element
    asks for the next piece as it plays, so a long video is many short exchanges within the
    proxy's bounds.
  - tilo-media's page backend loads a cross-origin source the element's CORS request cannot
    read through that form, when the page names a proxy, and again with the current session
    when the page reconnected while it played.
- **Response:** the target's status, headers and body.
  - Hop-by-hop headers are removed.
  - Headers the host's origin would act on reach the app renamed `x-tilo-forward-<name>`:
    `Set-Cookie` (so that origin never stores it), `WWW-Authenticate`, caching
    (`Cache-Control`, `Vary`, …), security policies, `Clear-Site-Data`, `Alt-Svc`,
    `Access-Control-*` and the target's own `x-tilo-*`. The proxy answers
    `Cache-Control: private, no-store`, so no cache in front of the host shares an answer.
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
  | `rate-limited` | 429, with `Retry-After` |

### The host's proxy

There is one implementation, `tilo_net::proxy::host` (feature `proxy-host`). `tilo-relay`
serves it at `/peer/v1/fetch` with `--fetch-proxy`, and the room service turns that on for
its rooms (`TILO_ROOMS_FETCH_PROXY`, on by default). It enforces:

- **Authentication:** the relay takes the page's browser session in `x-tilo-session`, and
  its person must be admitted to a workspace there, as a member or a guest. A room's
  spectators only watch, so they may not. CORS answers only the listener's allowed origins
  (the web entry), never with credentials.
- **Destinations:** the proxy's own network is refused: loopback, RFC 1918, link-local,
  unique-local, CGNAT, multicast, unspecified, documentation and reserved addresses, also
  inside IPv6 (mapped, NAT64, 6to4).
  - Each name is resolved once, every address is checked, and the connection dials the
    checked addresses, never a fresh lookup a rebinding name could answer differently.
  - Redirects are followed in the proxy, each hop checked again.
  - `Authorization` and `Cookie` are dropped when a redirect changes origin.
- **Schemes, ports and methods:** `http` and `https`; not the ports browsers refuse (the
  Fetch standard's bad ports: SMTP, SSH, …); HTTP and WebDAV methods, never `CONNECT` or
  `TRACE`.
- **Credentials:** the page's cookies and session, and the ingress's forwarding headers
  (`X-Forwarded-*`, `Forwarded`, `CF-*`), never reach the target.
- **Bounds:** 16 MiB request and 64 MiB response bodies (decompressed), and 30 s for the
  whole exchange, body included. A body cut at a bound ends in an error, never a short body
  that reads as whole.
- **Rates:** per device, 240 requests a minute (as a burst too) and 128 MiB at once, then
  8 MiB a minute; per destination host, 3000 requests a minute from everyone together;
  16 requests in flight per device and 256 in total. The relay takes other values from
  `TILO_RELAY_FETCH_PROXY_DEVICE_PER_MIN`, `…_DEVICE_MIB_PER_HOUR`, `…_DESTINATION_PER_MIN`,
  `…_DEVICE_IN_FLIGHT` and `…_IN_FLIGHT`.
- **Budgets:** rates smooth bursts out but let a device stream all day, so response bytes
  are also budgeted per window (a day): 2 GiB per visitor (device) and 64 GiB per room, where
  the relay counts a request against every workspace on the host its person is admitted to.
  Spent, the proxy answers `429` with `x-tilo-proxy-error: over-budget` and `Retry-After` set
  to the window's end, and a response it is relaying ends in an error at that point. A media
  element cannot read that answer, so `tilo-media`'s page backend asks the proxy with a `HEAD`
  when a proxied load fails and reports `MediaEvent::LimitReached`; Tube then says "Playback
  limit reached on this host. Try again later." The relay takes other values from
  `TILO_RELAY_FETCH_PROXY_BUDGET_HOURS`, `…_VISITOR_MIB` and `…_ROOM_MIB` (`0` turns a budget
  off).
- **Metrics and logs:** the relay writes `tilo_rooms_fetch_proxy_*` counters (requests by
  outcome, `over-budget` among them, bytes each way, requests in flight, devices) to `<state>/metrics.d/fetch-proxy.prom`,
  which the room service's `/metrics` appends. Refusals are logged under the `fetch_proxy`
  target with the device's first bytes and the destination's origin only: never bodies,
  headers, paths or queries, which carry people's credentials (CalDAV passwords, API keys).

### Development

`crates/browser/tilo-web-desktop/scripts/serve.mjs BUNDLE_DIR --fetch-proxy` serves a built
desktop bundle on loopback, sets the page global to `/_tilo/net/fetch`, and forwards that
path to `tilo-fetch-proxy` (`cargo run -p tilo-net --features proxy-dev-server --bin
tilo-fetch-proxy`): the same implementation, unauthenticated on loopback.

- `--allow-private` lets it reach a local test server.
- `--proxy-bin PATH` (or `TILO_FETCH_PROXY_BIN`) names a built program; otherwise it uses
  `target/{debug,release}/tilo-fetch-proxy` or `cargo run`.

## Not done yet

- **Status:** the desktop answers from routes, not NetworkManager or `NWPathMonitor`, so
  metering is unknown there.
- **Browser tests:** there is no wasm-bindgen-test suite in headless Chrome yet. The web
  backends were verified through the desktop bundle smoke:
  - maps fetches, streams and decodes real tiles in the page;
  - the proxy is tested against a real relay and a page's session natively
    (`tilo-browser-peer`'s `fetch_proxy` test), not yet from an app in headless Chrome.
- **Remaining blocking clients:** in desktop-only apps (store, browser, imager, git-client,
  mail and others), and in the GStreamer media paths of music and tube.
