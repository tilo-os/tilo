# Bluetooth and audio Settings architecture

## Bluetooth

Settings uses the shared `qos-bluez` client through `os-actions::bluez`; it
does not invoke `bluetoothctl`. The process-wide client deliberately keeps one
system-bus connection alive because BlueZ binds discovery to the connection
that started it. All device actions use the BlueZ object path, not a MAC
address. This remains correct with multiple adapters.

Settings may start pairing, but never draws or answers a passkey prompt. The
compositor is the registered BlueZ Agent and owns that trusted UI. Pairing is
ordered pair → trust → connect; a connect failure retains the valid bond and
is presented as a recoverable Connect action. Forget is a two-step action and
calls Adapter1.RemoveDevice, which removes keys and trust.

Rename writes Device1.Alias, reconnect policy writes Device1.Trusted, and
battery percentage is Battery1.Percentage. No parallel preferences database is
kept. In particular, `Some(0)` battery is distinct from “not reported”.

## Audio

Simple controls operate on PipeWire/WirePlumber defaults and application
streams. The last selected input and output are preferences, not assumed
hardware state: at startup Settings applies one only if the matching node is
currently present. Hotplug enumeration remains authoritative.

The advanced device-profile picker is capability driven. `pw-dump` Device
objects supply `EnumProfile`; only available profiles are shown and changes use
`wpctl set-profile <device-id> <profile-index>`. A Bluetooth codec is shown
read-only when the node exposes `api.bluez5.codec`. A separate codec picker is
not shown unless the backend exposes codecs as selectable profiles.

Balance, channel playback tests, microphone loopback/monitoring and mono
routing are intentionally not represented by inert controls. They require a
session audio service with owned process/node lifecycle (especially to prevent
feedback and to tear capture down when Settings closes). The current graph
abstraction does not yet provide that lifecycle, so these remain explicit
follow-up work rather than UI that only stores a value.
