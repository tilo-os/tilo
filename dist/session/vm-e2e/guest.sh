#!/usr/bin/env bash
# Guest side of the host-desktop session VM test (dist/session/vm-e2e/run.py).
# Runs as root inside the throwaway Fedora VM, one step per invocation:
#
#   guest.sh install RPM_DIR   install the tilo-session RPMs the way a user
#                              does (dnf, weak dependencies on), plus SDDM and
#                              gnome-keyring; exits 100 when the VM must reboot
#                              into a kernel whose DRM modules are installed
#   guest.sh user              the login user, with its user manager running
#   guest.sh gnome-input       the user's GNOME input sources (German, no dead
#                              keys; Compose on Right Alt), as a GNOME user
#                              who then tries Tilo already has them
#   guest.sh keyring           an unlocked login keyring on that user's bus, so
#                              org.freedesktop.secrets answers as on a desktop
#   guest.sh identity          provision the Tilo identity on that bus and
#                              require its secret to be in the Secret Service
#   guest.sh login             SDDM autologin into the packaged tilo.desktop
#   guest.sh settings          the login user's Tilo settings.json (empty if none)
#   guest.sh status            compositor pid, crash records, panics (key=value)
#   guest.sh collect DIR       logs and records for the report
#
# It refuses to run anywhere but a VM the test's cloud-init marked, because it
# creates a user, enables a display manager and starts a graphical login.
set -euo pipefail

user=tilo-e2e
password=tilo-e2e-password
marker=/etc/tilo-host-session-e2e

die() { printf 'guest: %s\n' "$*" >&2; exit 1; }

[[ $(id -u) == 0 ]] || die 'run as root'
systemd-detect-virt --vm --quiet || die 'refusing to run outside a virtual machine'
[[ -f $marker ]] || die "refusing to run without $marker (written by the test's cloud-init)"

uid() { id -u "$user"; }

# A command as the login user on its own systemd user manager and session bus.
as_user() {
    local id
    id=$(uid)
    runuser -u "$user" -- env -i \
        HOME="/home/$user" USER="$user" LOGNAME="$user" \
        PATH=/usr/local/bin:/usr/bin:/bin \
        XDG_RUNTIME_DIR="/run/user/$id" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$id/bus" \
        "$@"
}

step_install() {
    local dir=$1
    shopt -s nullglob
    local session=("$dir"/tilo-session-[0-9]*.rpm) apps=("$dir"/tilo-session-apps-[0-9]*.rpm)
    shopt -u nullglob
    (( ${#session[@]} == 1 && ${#apps[@]} == 1 )) \
        || die "want exactly one tilo-session and one tilo-session-apps RPM in $dir"
    # The cloud image carries kernel-core only; virtio-gpu's DRM driver is in
    # kernel-modules, which must match the running kernel.
    local reboot=0
    if ! dnf -y --setopt=retries=5 install "kernel-modules-$(uname -r)"; then
        dnf -y --setopt=retries=5 install kernel kernel-modules
        reboot=1
    fi
    # What a Fedora user adds to try Tilo: the two packages (their Requires
    # and Recommends resolve from Fedora), a display manager and a keyring.
    dnf -y --setopt=retries=5 --setopt=install_weak_deps=True install \
        "${session[0]}" "${apps[0]}" sddm gnome-keyring libsecret mesa-vulkan-drivers
    rpm -q tilo-session tilo-session-apps sddm gnome-keyring
    if (( reboot )); then
        echo 'guest: installed a newer kernel with its modules; reboot required'
        exit 100
    fi
    modprobe virtio_gpu || true
}

step_user() {
    id "$user" >/dev/null 2>&1 || useradd --create-home --comment 'Tilo E2E' "$user"
    echo "$user:$password" | chpasswd
    # Linger starts the user manager (and its bus) now, so the keyring and the
    # identity are set up on the bus the graphical login will use.
    loginctl enable-linger "$user"
    local id
    id=$(uid)
    for _ in $(seq 60); do
        [[ -S /run/user/$id/bus ]] && break
        sleep 1
    done
    [[ -S /run/user/$id/bus ]] || die "the user manager of $user has no session bus"
}

# The GNOME settings the host-desktop session imports on its first login
# (crates/session/shell/src/firstboot.rs, import_host_input_settings). run.py
# then requires the session's settings to carry them.
gnome_sources="[('xkb', 'de+nodeadkeys')]"
gnome_xkb_options="['compose:ralt']"

step_gnome_input() {
    # A GNOME desktop has the schemas and the dconf backend; the Cloud image
    # may not. Without dconf, gsettings falls back to an in-memory backend and
    # nothing reaches the session, so both are required, not optional.
    if ! command -v dconf >/dev/null 2>&1 \
        || ! gsettings list-schemas 2>/dev/null | grep -x org.gnome.desktop.input-sources >/dev/null; then
        dnf -y --setopt=retries=5 install gsettings-desktop-schemas dconf
    fi
    command -v gsettings >/dev/null 2>&1 || die 'gsettings is not installed'
    command -v dconf >/dev/null 2>&1 || die 'dconf is not installed'
    gsettings list-schemas | grep -x org.gnome.desktop.input-sources >/dev/null \
        || die 'the org.gnome.desktop.input-sources schema is not installed'
    as_user gsettings set org.gnome.desktop.input-sources sources "$gnome_sources"
    as_user gsettings set org.gnome.desktop.input-sources xkb-options "$gnome_xkb_options"
    # Read back from new processes, and from the user database itself: what
    # the session's gsettings will read at its first login.
    local sources options stored
    sources=$(as_user gsettings get org.gnome.desktop.input-sources sources)
    options=$(as_user gsettings get org.gnome.desktop.input-sources xkb-options)
    stored=$(as_user dconf read /org/gnome/desktop/input-sources/sources)
    [[ $sources == "$gnome_sources" ]] || die "GNOME input sources read back as $sources"
    [[ $options == "$gnome_xkb_options" ]] || die "GNOME xkb-options read back as $options"
    [[ $stored == "$gnome_sources" ]] || die "the dconf user database holds sources=${stored:-nothing}"
    echo "guest: GNOME input sources $sources, xkb-options $options"
}

step_keyring() {
    # What pam_gnome_keyring does at a password login: unlock the "login"
    # keyring with the login password (creating it on first use) as the
    # default collection, so storing a secret needs no prompt. A blank
    # password does not create it: gnome-keyring asks its prompter instead.
    as_user install -d -m 0700 "/home/$user/.local/share/keyrings"
    as_user sh -c "printf login > /home/$user/.local/share/keyrings/default"
    printf '%s' "$password" \
        | as_user gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null
    printf 'probe' | as_user timeout 30 secret-tool store --label='Tilo E2E probe' tilo-e2e probe \
        || die 'the Secret Service did not store an item (locked or prompting?)'
    [[ $(as_user timeout 30 secret-tool lookup tilo-e2e probe) == probe ]] \
        || die 'the Secret Service did not return the stored item'
    as_user timeout 30 secret-tool clear tilo-e2e probe || true
    echo 'guest: login keyring unlocked; org.freedesktop.secrets answers'
}

step_identity() {
    # The entry point package CI uses for host provisioning; it mints through
    # the same ensure_collaboration_identity_for_legacy_linux_host the
    # host-desktop setup screen calls, with the platform secret store.
    as_user /usr/bin/tilo-shell --host-desktop-session --provision-identity
    as_user /usr/bin/tilo-shell --host-desktop-session --verify-collaboration-identity
    local record=/home/$user/.config/tilo/identity/primary.json
    [[ -s $record ]] || die "no identity record at $record"
    grep -q '"secret-service-v1"' "$record" \
        || die "the identity secret is not in the Secret Service: $(grep -o '"backend":"[^"]*"' "$record" || echo 'no secret reference')"
    echo 'guest: identity provisioned; its secret is in the Secret Service'
}

step_login() {
    test -f /usr/share/wayland-sessions/tilo.desktop
    install -d -m 0755 /etc/sddm.conf.d
    cat >/etc/sddm.conf.d/tilo-e2e.conf <<EOF
[Autologin]
User=$user
Session=tilo.desktop
Relogin=false
EOF
    systemctl set-default graphical.target
    systemctl enable sddm.service
    systemctl start sddm.service
}

step_settings() {
    local file=/home/$user/.config/tilo/session/settings.json
    [[ ! -f $file ]] || cat "$file"
}

compositor_pid() {
    pgrep -u "$user" -x tilo-shell | head -n 1 || true
}

step_status() {
    local home=/home/$user log
    log=$home/.local/state/tilo/session-logs/latest.log
    echo "compositor_pid=$(compositor_pid)"
    echo "modeset=$(grep -c 'DRM scanout: modeset complete' "$log" 2>/dev/null || echo 0)"
    echo "crash_logs=$(find "$home/.cache/tilo/crashes" -type f 2>/dev/null | wc -l)"
    echo "coredumps=$(coredumpctl --no-pager --no-legend list /usr/bin/tilo-shell 2>/dev/null | wc -l)"
    echo "unexpected_exit=$(journalctl -b --no-pager -o cat -t tilo-session 2>/dev/null \
        | grep -c 'compositor exited without a logout request' || true)"
    echo "panics=$( { journalctl -b --no-pager -o cat 2>/dev/null; cat "$log" 2>/dev/null; } \
        | grep -c -E "thread '[^']*' panicked|panicked at " || true)"
    local crash
    crash=$(find "$home/.cache/tilo/crashes" -type f 2>/dev/null | sort | tail -n 1)
    [[ -z $crash ]] || echo "crash_message=$(sed -n 's/^message: *//p' "$crash" | head -n 1)"
    # An app's launch shows in the session log as its own output lines; the
    # compositor logs an app that exits before its first window.
    local app
    for app in "$@"; do
        echo "app.$app=$(grep -c "^\[app $app " "$log" 2>/dev/null || true)"
        echo "app_stopped.$app=$(grep 'application stopped before presenting a window' "$log" 2>/dev/null \
            | grep -c -F "$app" || true)"
        echo "app_error.$app=$(grep "^\[app $app stderr\] error: " "$log" 2>/dev/null | tail -n 1 \
            | sed "s/^\[app $app stderr\] //" || true)"
    done
}

step_collect() {
    local out=$1 home=/home/$user
    install -d -m 0755 "$out"
    journalctl -b --no-pager >"$out/journal.txt" 2>&1 || true
    journalctl -b --no-pager -o cat -t tilo-session >"$out/tilo-session.journal.txt" 2>&1 || true
    coredumpctl --no-pager list >"$out/coredumps.txt" 2>&1 || true
    coredumpctl --no-pager info /usr/bin/tilo-shell >"$out/coredump-tilo-shell.txt" 2>&1 || true
    cp -r "$home/.local/state/tilo/session-logs" "$out/" 2>/dev/null || true
    cp -r "$home/.cache/tilo/crashes" "$out/" 2>/dev/null || true
    cp "$home/.config/tilo/session/settings.json" "$out/settings.json" 2>/dev/null || true
    ps -eo pid,ppid,user,etimes,args --forest >"$out/processes.txt" 2>&1 || true
    rpm -q tilo-session tilo-session-apps sddm gnome-keyring kernel-core mesa-vulkan-drivers \
        >"$out/packages.txt" 2>&1 || true
    chmod -R a+rX "$out"
}

case ${1:-} in
    install) shift; (($# == 1)) || die 'install RPM_DIR'; step_install "$1" ;;
    user) step_user ;;
    gnome-input) step_gnome_input ;;
    keyring) step_keyring ;;
    identity) step_identity ;;
    login) step_login ;;
    settings) step_settings ;;
    status) shift; step_status "$@" ;;
    collect) shift; (($# == 1)) || die 'collect DIR'; step_collect "$1" ;;
    *) sed -n '2,23p' "$0" >&2; exit 2 ;;
esac
