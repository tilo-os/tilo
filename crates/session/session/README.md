# Tilo session in a window

Run the whole desktop — the compositor, the desk's strips, the top bar and every
surface around them — in a window of the desktop you are already in:

```sh
cargo run --locked -p tilo-session -- --backend nested
```

It draws in an X11 window, so it needs one: an X11 desktop, or a Wayland one
with Xwayland (`DISPLAY` set). The window starts at `--size WxH`, by default
1280x720; resizing it resizes the display, and closing it ends the session. An
app command after `--` opens inside it (`cargo build -p calculator` puts one at
`target/debug/calculator`):

```sh
cargo run --locked -p tilo-session -- --backend nested -- target/debug/calculator
```

It prints the `WAYLAND_DISPLAY` and `DISPLAY` of its own; both are fresh names,
beside the host's, so several nested sessions run next to each other.

A nested session leaves the desktop it runs in alone. Its desktop services
serve no bus unless a flag names one, so the host's session bus keeps its
portal and notification names; for services on a bus of their own, start a
private daemon and name it:

```sh
dbus-daemon --session --nofork --address=unix:path="$XDG_RUNTIME_DIR/tilo-nested-bus" &
cargo run --locked -p tilo-session -- --backend nested --session-bus "unix:path=$XDG_RUNTIME_DIR/tilo-nested-bus"
```

It never exports its displays to systemd or D-Bus activation, which would point
the surrounding session's services at its socket.
