# Application resource claims

The signed tapp manifest is the only source of truth for resources a packaged
application can handle:

```toml
[providers]
ext = ["html"]
mime = ["text/html"]
schemes = ["http", "https"]
```

Claims are identifiers, never commands. tapp normalizes case, leading dots and
trailing scheme colons before signing, rejects malformed concrete MIME types,
extensions outside the conservative portable alphabet, and malformed or
reserved URI schemes. `file`, `data`, and `javascript` are not claimable URI
schemes: local files use the capability-granting file-open path, while inline
active content must stay inside an already-authorized application.

A manifest containing only `[providers].ext` is valid; `mime` and `schemes`
are optional. `secondary = true` keeps every claim but ranks the app after
other claimants when nobody chose a default, so a video editor that imports
`.mp4` does not open videos ahead of the player.

The launcher exposes validated extension, MIME, and scheme claims when it
enumerates installed GUI apps. Personal Settings and Workspace Settings build
their default-app pickers from that live enumeration. Stored defaults are only
preferences: resolution ignores a removed package or a package that no longer
claims the resource.

File open prefers a concrete MIME claim derived by the shared `mime_guess`
database, then falls back to the legacy extension claim. URI open validates a
bounded scheme-bearing value and sends the complete URI as one opaque argv
item. Immediately before either launch, the shell bypasses its short-lived app
cache, re-reads the installed registry and manifest, and verifies that the
selected package still makes the relevant claim. A claim string is never sent
to a shell, interpreted as an executable, or split into arguments.

Only applications with a startup ingestion path may claim a resource.
The browser currently claims `http` and `https` and consumes the launched URI.
Mail and Calendar keep their existing file-format claims but do not claim
`mailto` or `webcal` until their startup paths can safely populate a composer or
event import. Media and document applications retain only formats already
handled by their concrete open/import code.
