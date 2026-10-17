# Browser production-path proof

Run from the repository root:

```sh
t test model-providers-web
```

This builds the SDK's production **single-app** bundle for
`apps/model-providers`, linking its actual `definition()` and the shared
provider settings UI into `tilo-browser-runtime`. It does not build all desktop
applications. `generate_web_desktop.py --check` separately checks that the
production desktop registration is current.

`run.mjs` starts an HTTPS administration fixture and an owned Chromium instance,
mounts through production `host.mjs`, and explicitly supplies a
`providerManagement` session. The real `FetchTransport` must send authenticated
RPC3 requests; there is no JavaScript administration-client substitute. The
fixture models the HTTP contract, not real gateway credential storage (covered
by the gateway's native HTTP/FD tests).

The test invokes the production controls' semantic routes to add a provider,
edit its name/endpoint/model, choose the default, and save a private key. It
uses the production published action door with confirmation for destructive
key/provider removal. It checks rendered credential status and scans public
resource/action metadata and runtime snapshots for the key. Delayed HTTP
failures exercise both metadata and private-key drafts; retrying the key save
without typing it again proves that its private buffer survived the failure.

For an existing production bundle:

```sh
CHROME=/path/to/chrome node apps/model-providers/web/run.mjs /absolute/path/to/bundle
```

Chrome discovery otherwise uses the existing Playwright cache helper. Missing
Chrome, WebGPU, controls, registration, or HTTP operations fail the check;
nothing is skipped. Headless Linux runs select the same SwiftShader Vulkan/ANGLE
backend as the desktop browser journeys; other platforms retain their default backend.
Failures include browser-process diagnostics as well as runtime and control state.
The HTTPS fixture uses an ephemeral in-memory self-signed certificate
and Chrome's certificate-error override only in this owned test browser.
Both the server and browser are closed even on failure.
