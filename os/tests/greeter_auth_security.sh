#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
app="$repo_root/crates/session/greeter/src/greeter_app.rs"
policy="$repo_root/os/etc/polkit-1/rules.d/49-tilo-greeter-fingerprint.rules"
container="$repo_root/os/Containerfile"
lock_auth="$repo_root/crates/session/shell/src/compositor/lock_auth.rs"
pam="$repo_root/os/etc/pam.d/greetd"

ok() { printf 'ok - %s\n' "$1"; }
bad() { printf 'not ok - %s\n' "$1" >&2; exit 1; }

if grep -Fq 'bounded_command::run_prepared(' "$lock_auth" \
   && grep -Fq 'Zeroizing::new(Vec::with_capacity' "$lock_auth" \
   && ! grep -Eq 'cmd\.spawn\(\)|child\.kill\(\)|wait_with_timeout' "$lock_auth"; then
  ok 'session unlock credentials use zeroized bounded process execution'
else
  bad 'session unlock credentials cross an unbounded or non-zeroized helper boundary'
fi

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
   && grep -Fq 'bounded_command::run_with_env(' "$app" \
   && grep -Fq '&[username]' "$app" \
   && grep -Fq '&[("LC_ALL", "C")]' "$app" \
   && ! grep -Fq 'Command::new("fprintd-list")' "$app"; then
  ok 'multi-user fingerprint enrollment is account-bound and resource-bounded'
else
  bad 'multi-user fingerprint authentication is misrouted or trusts an unbounded helper'
fi

if grep -Fq 'subject.user === "greetd"' "$policy" \
   && grep -Fq 'net.reactivated.fprint.device.setusername' "$policy" \
   && ! grep -Eq 'action\.id === "net\.reactivated\.fprint\.device\.(enroll|verify)"' "$policy" \
   && grep -Fq '49-tilo-greeter-fingerprint.rules' "$container"; then
  ok 'image grants only fprintd account selection to the greeter service'
else
  bad 'greeter fprintd policy is missing, broad, or absent from the image'
fi

# The session leaves greetd's xdm_t for the user's context: pam_selinux close
# comes first, pam_loginuid before open, and the rest of the session after it.
session_rule() { grep -n "^session[[:space:]]\+required[[:space:]]\+$1\$" "$pam" | cut -d: -f1; }
first_session_line=$(grep -n '^-\?session' "$pam" | head -1 | cut -d: -f1)
selinux_close_line=$(session_rule 'pam_selinux\.so close')
loginuid_line=$(session_rule 'pam_loginuid\.so')
selinux_open_line=$(session_rule 'pam_selinux\.so open')
include_line=$(grep -n '^session[[:space:]]\+include[[:space:]]\+password-auth$' "$pam" | cut -d: -f1)
if [ -n "$selinux_close_line" ] && [ -n "$loginuid_line" ] && [ -n "$selinux_open_line" ] && [ -n "$include_line" ] \
   && [ "$selinux_close_line" = "$first_session_line" ] \
   && [ "$selinux_close_line" -lt "$loginuid_line" ] \
   && [ "$loginuid_line" -lt "$selinux_open_line" ] \
   && [ "$selinux_open_line" -lt "$include_line" ]; then
  ok 'a greetd session runs in the user SELinux context with its login uid'
else
  bad 'a greetd session can keep the display manager domain or lack a login uid'
fi

auth_keyring_line=$(grep -n '^auth[[:space:]]\+optional[[:space:]]\+pam_gnome_keyring\.so$' "$pam" | cut -d: -f1)
password_auth_line=$(grep -n '^auth[[:space:]]\+substack[[:space:]]\+password-auth$' "$pam" | cut -d: -f1)
session_include_line=$(grep -n '^session[[:space:]]\+include[[:space:]]\+password-auth$' "$pam" | cut -d: -f1)
session_keyring_line=$(grep -n '^session[[:space:]]\+optional[[:space:]]\+pam_gnome_keyring\.so[[:space:]]\+auto_start$' "$pam" | cut -d: -f1)
if [ -n "$auth_keyring_line" ] \
   && [ -n "$password_auth_line" ] \
   && [ "$auth_keyring_line" -lt "$password_auth_line" ] \
   && [ -n "$session_include_line" ] \
   && [ -n "$session_keyring_line" ] \
   && [ "$session_include_line" -lt "$session_keyring_line" ] \
   && grep -Eq '^[[:space:]]+gnome-keyring([[:space:]\\]|$)' "$container" \
   && grep -Eq '^[[:space:]]+gnome-keyring-pam([[:space:]\\]|$)' "$container"; then
  ok 'greetd password login unlocks the Secret Service used by Tilo identity storage'
else
  bad 'greetd can authenticate without making the existing Tilo identity key available'
fi
