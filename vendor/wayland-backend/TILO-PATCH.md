# Tilo patch: tolerate a client disappearing during object creation

The pure-Rust server dispatch path creates the protocol object before invoking
the compositor callback, then attaches the callback's `ObjectData` after the
callback returns. A callback can legitimately kill the client by posting a
protocol error. That removes the just-created child object, so the former
unconditional `ObjectMap::with(...).unwrap()` panicked the whole compositor.

This patch treats a missing child at that point as a disconnected-client race.
It deliberately changes no behavior for a live client: the sibling branch
still panics when a live client creates an object but the callback returns no
object data.

Observed on 2026-09-20 when ChatGPT OAuth launched a browser helper. The helper
connected to Tilo's Wayland socket and disappeared during initial protocol
setup; `wayland-backend-0.3.15/src/rs/server_impl/common_poll.rs` panicked at
the child-map `unwrap`, terminating the desktop and then Xwayland.

# Tilo patch: connection errors go to `log`, never straight to standard error

Without its `log` feature this crate reports every connection error with
`eprintln!`, which panics when standard error is a pipe nobody reads any more.
An app keeper's standard error is such a pipe once the session that read it has
restarted: the desktop going away made the keeper print `Io error: Broken pipe`,
which ended the keeper and, with it, the app it kept. `log` is a default feature
here, so the errors reach the program's own logging (`tracing`'s log bridge),
whose writes fail quietly.
