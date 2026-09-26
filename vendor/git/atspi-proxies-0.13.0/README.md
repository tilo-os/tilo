# `atspi-proxies`

An internal crate used by other `atspi-*` crates.
This handles direct communication with DBus.

## Feature Flags

* `default`: `wrappers`, `async-io`
* `async-io`: use zbus's `async-io` runtime backend
* `tokio`: use zbus's `tokio` runtime backend, spawning zbus's tasks on the application's tokio runtime. One of `async-io` or `tokio` must be enabled.
* `wrappers`: enable the `atspi-common` event wrapper types
