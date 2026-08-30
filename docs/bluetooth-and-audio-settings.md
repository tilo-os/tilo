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

Diagnostics and persistent transforms live in `media::audio_runtime`, separate
from graph enumeration. Controls appear only when the corresponding standard
host API is reachable:

- stereo balance writes the two channel volumes through the PipeWire Pulse
  server (`pactl`);
- mono loads one temporary `module-remap-sink`, switches the default to it,
  then restores the physical sink and unloads the module on stop or Drop;
- left/right tests send a bounded, low-volume generated PCM tone directly to
  the selected PipeWire node with `pw-play`;
- microphone test captures raw samples from the selected source with
  `pw-record`, reduces them immediately to a peak meter, and retains no audio.

Every diagnostic process is killed and waited and every capture reader is
joined when its bounded test ends, the pane is hidden, or Settings exits. The
microphone path has no playback node, so testing cannot silently become
microphone monitoring or create feedback. The microphone test is capped at
eight seconds in Settings (and the backend rejects unbounded durations).

Balance and mono are persisted by Settings but actuated by exactly one worker
owned by the compositor session. That worker observes PipeWire off the render
thread, reapplies balance after a real volume/default-device change, retains
the physical master when the virtual mono sink becomes the reported default,
and migrates cleanly on hot-plug. Dropping the session owner restores the
physical output and unloads the module. Settings windows therefore never
create competing virtual sinks, and closing or navigating away from Settings
does not undo an accessibility choice. Codec selection remains read-only
unless WirePlumber advertises concrete selectable profiles.
