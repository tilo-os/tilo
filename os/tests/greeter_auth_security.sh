#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
app="$repo_root/crates/session/greeter/src/greeter_app.rs"
policy="$repo_root/os/etc/polkit-1/rules.d/49-tilo-greeter-fingerprint.rules"
container="$repo_root/os/Containerfile"

ok() { printf 'ok - %s\n' "$1"; }
bad() { printf 'not ok - %s\n' "$1" >&2; exit 1; }

if grep -Fq 'with_external_buffer(Arc::clone(&self.password_buffer))' "$app" \
   && grep -Fq 'TextInput::local()' "$app" \
   && grep -Fq 'Zeroizing<String>' "$app" \
   && ! grep -Fq 'const PASSWORD_PATH' "$app" \
   && ! grep -Fq 'with_on_change_path(PASSWORD_PATH)' "$app"; then
  ok 'greeter passcodes stay in zeroized process-local input state'
else
  bad 'greeter passcodes can enter Automerge or lack zeroized local storage'
fi

if grep -Fq 'FingerprintAvailability::CheckNamedAccount' "$app" \
   && grep -Fq 'self.fingerprint_available_for(&username)' "$app" \
   && grep -Fq '.arg(username)' "$app"; then
  ok 'multi-user fingerprint enrollment follows the exact normalized username'
else
  bad 'multi-user fingerprint authentication is not routed by username'
fi

if grep -Fq 'subject.user === "greetd"' "$policy" \
   && grep -Fq 'net.reactivated.fprint.device.setusername' "$policy" \
   && ! grep -Eq 'action\.id === "net\.reactivated\.fprint\.device\.(enroll|verify)"' "$policy" \
   && grep -Fq '49-tilo-greeter-fingerprint.rules' "$container"; then
  ok 'image grants only fprintd account selection to the greeter service'
else
  bad 'greeter fprintd policy is missing, broad, or absent from the image'
fi
