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
#   guest.sh logout            end that session and wait for its compositor
#                              to exit; exits 100 when it does not
#   guest.sh session-autologin SESSION  autologin into SESSION on the next
#                              login, as a person switching in the picker
#   guest.sh session-default           which session an ordinary login starts
#   guest.sh previous-available whether the previous desktop's packages and
#                              session entry are still installed (states where
#                              this VM has no GNOME to return to)
#   guest.sh previous-probe    launch the previous desktop on a free virtual
#                              console and require it to show a frame; exits
#                              100 when it does not, 3 when unusable here
#   guest.sh uninstall         remove the Tilo packages and their files and
#                              return the ordinary login to what it was
#   guest.sh files DELIVER TAKE  two files in the login user's home, written
#   guest.sh files-check DELIVER TAKE  before the switch and checked after it
#   guest.sh record LABEL      record the packages and login sessions
#   guest.sh record-check LABEL EARLIER [--kept N]... [--gone N]...
#                              prove a change touched only what it owns
#   guest.sh update RPM_DIR    upgrade the Tilo packages in place, as a newer
#                              release does
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
# The desktop stock Fedora offers before Tilo is added and the one a person
# returns to afterwards: what the live image's own session entry starts.
previous_session=gnome
tilo_session=tilo.desktop

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
    # Keep one display manager for both desktops: a person signing in to Tilo
    # does not expect GDM beside it.
    dnf -y --setopt=retries=5 --setopt=install_weak_deps=True install \
        "${session[0]}" "${apps[0]}" sddm gnome-keyring libsecret mesa-vulkan-drivers
    rpm -q tilo-session tilo-session-apps sddm gnome-keyring
    step_record installed
    if (( reboot )); then
        echo 'guest: installed a newer kernel with its modules; reboot required'
        exit 100
    fi
    modprobe virtio_gpu || true
}

# The installed package set and the login session entries, so a later step can
# prove that an update, an uninstall and the return to the previous desktop
# added and removed exactly what they should and nothing else.
step_record() {
    local label=$1
    rpm -qa --qf '%{NAME}\n' | sort >"/var/tmp/tilo-e2e/packages.$label"
    (cd /usr/share/wayland-sessions 2>/dev/null && ls *.desktop | sort) \
        >"/var/tmp/tilo-e2e/sessions.$label"
    echo "guest: recorded $(wc -l <"/var/tmp/tilo-e2e/packages.$label") packages and" \
"$(tr '\n' ' ' <"/var/tmp/tilo-e2e/sessions.$label")as $label"
}

# step_record_check LABEL EARLIER [--kept NAME]... [--gone NAME]...
#
# Compare this point with an earlier record. A difference that was not asked
# for fails the step, in both sets: an update, an uninstall or a returned
# desktop must change only what it owns. A `--gone` entry names something that
# must be there before and absent now; `--kept` names something that must
# still be there, so only a difference that was asked for is tolerated.
step_record_check() {
    local label=$1 earlier=$2
    shift 2
    local kept=() gone=()
    while (( $# )); do
        case $1 in
            --kept) kept+=("$2"); shift 2 ;;
            --gone) gone+=("$2"); shift 2 ;;
            *) die "step_record_check: unexpected argument $1" ;;
        esac
    done
    local set added removed item
    for set in packages sessions; do
        added=$(new_entries "$set" "$earlier" "$label")
        removed=$(dropped_entries "$set" "$earlier" "$label")
        for item in $added; do
            in_list "$item" "${kept[@]}" \
                || die "$label added $set $item, which this change does not own"
        done
        for item in $removed; do
            in_list "$item" "${gone[@]}" \
                || die "$label removed $set $item, which this change does not own"
        done
        for item in "${gone[@]}"; do
            in_list "$item" "$removed" \
                || die "$label kept $set $item, which it should have removed"
        done
        for item in "${kept[@]}"; do
            [[ -n $(recorded "$set" "$label" "$item") ]] \
                || die "$label lost $set $item, which it should have kept"
        done
    done
    echo "guest: $label changed only what it owns"
}

recorded() {
    grep -Fx -- "$3" "/var/tmp/tilo-e2e/$1.$2" 2>/dev/null | head -n 1
}

new_entries() {
    comm -13 "/var/tmp/tilo-e2e/$1.$2" "/var/tmp/tilo-e2e/$1.$3" || true
}

dropped_entries() {
    comm -23 "/var/tmp/tilo-e2e/$1.$2" "/var/tmp/tilo-e2e/$1.$3" || true
}

in_list() {
    local wanted=$1
    shift
    printf '%s\n' "$@" | grep -Fxq -- "$wanted"
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
    test -f "/usr/share/wayland-sessions/$tilo_session"
    install -d -m 0755 /etc/sddm.conf.d
    cat >/etc/sddm.conf.d/tilo-e2e.conf <<EOF
[Autologin]
User=$user
Session=$tilo_session
Relogin=false
EOF
    systemctl set-default graphical.target
    systemctl enable sddm.service
    systemctl start sddm.service
}

step_session_autologin() {
    local session=$1
    [[ -f /usr/share/wayland-sessions/$session ]] \
        || die "no /usr/share/wayland-sessions/$session to choose"
    sed -i "s/^Session=.*/Session=$session/" /etc/sddm.conf.d/tilo-e2e.conf
    grep -Fxq "Session=$session" /etc/sddm.conf.d/tilo-e2e.conf \
        || die "the picker choice $session did not reach the display manager"
}

step_session_default() {
    # What an ordinary login starts, which is the display manager's own
    # configuration and not the test's autologin file: the setting a package
    # must never change behind the person's back.
    local found
    found=$(grep -rhs '^Session=' /etc/sddm.conf /etc/sddm.conf.d/ \
        --exclude=tilo-e2e.conf 2>/dev/null | head -1 || true)
    echo "${found:-Session=unset}"
}

# End the running session the way signing out does, without going through the
# desktop's own logout UI: stop the display manager, which ends the user
# session and every process in it.
step_logout() {
    if systemctl is-active --quiet sddm.service; then
        systemctl stop sddm.service
    fi
    local i
    for i in $(seq 60); do
        [[ -z $(compositor_pid) ]] && return 0
        sleep 1
    done
    die "a Tilo compositor outlived the session it was started in: $(compositor_pid)"
}

# Whether this host has a desktop to go back to. The Cloud image is minimal
# and may carry no other login at all; a person's Fedora workstation has GNOME.
# The test still records which of the two it checked instead of silently
# skipping the return half of the journey.
previous_state() {
    local missing=() package
    for package in gnome-shell gnome-session-wayland-session; do
        rpm -q "$package" >/dev/null 2>&1 || missing+=("$package")
    done
    [[ -f /usr/share/wayland-sessions/$previous_session.desktop ]] \
        || missing+=("/usr/share/wayland-sessions/$previous_session.desktop")
    printf '%s' "${missing[*]:-}"
    (( ${#missing[@]} == 0 ))
}

step_previous_available() {
    local missing
    missing=$(previous_state) && echo 'guest: a previous desktop is installed' \
        || echo "guest: no previous desktop on this host: $missing"
    return 0
}

# Launch the previous desktop on a virtual console nothing else owns, as a
# person at the login screen does, and require its compositor to start. It is
# started from a logind session on tty5 rather than from this SSH session,
# because Fedora's GNOME asks logind for the seat. There is no GPU in the test
# VM, so a software-rendered frame is the most that can be proven here; a
# host without the nested-VT facility exits 3, which the harness reports
# rather than counting as a pass.
step_previous_probe() {
    previous_state || die "this host has no previous desktop to probe"
    [[ -w /dev/tty5 ]] || { echo 'guest: no free virtual console to probe on'; exit 3; }
    local unit=tilo-e2e-previous.service
    cat >/etc/systemd/system/$unit <<EOF
[Unit]
Description=Throwaway previous desktop probe (Tilo host-session test)
[Service]
Type=simple
PAMName=login
TTYPath=/dev/tty5
StandardInput=tty
StandardOutput=journal
StandardError=journal
User=$user
Environment=HOME=/home/$user
Environment=XDG_SESSION_TYPE=wayland
ExecStart=/usr/bin/dbus-run-session -- /usr/bin/gnome-session --session=gnome
[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl reset-failed $unit 2>/dev/null || true
    local i pid painted=0
    if ! timeout 240 systemctl start $unit; then
        journalctl -u $unit --no-pager -b | tail -n 60 >&2
        systemctl stop $unit 2>/dev/null || true
        systemctl reset-failed $unit 2>/dev/null || true
        rm -f /etc/systemd/system/$unit
        systemctl daemon-reload
        echo 'guest: the previous desktop did not start'
        return 100
    fi
    for i in $(seq 90); do
        pid=$(pgrep -u "$user" -x gnome-shell | head -n 1 || true)
        if [[ -n $pid ]] && journalctl -u $unit --no-pager -b -o cat 2>/dev/null \
            | grep -Eqi 'frame drawn|presented frame|first frame|failed to.*egl|software render'; then
            painted=1
            break
        fi
        sleep 2
    done
    systemctl stop $unit 2>/dev/null || true
    systemctl reset-failed $unit 2>/dev/null || true
    rm -f /etc/systemd/system/$unit
    systemctl daemon-reload
    [[ -n $pid ]] || { echo 'guest: no GNOME Shell process on the probe console'; return 100; }
    if (( ! painted )); then
        echo "guest: GNOME Shell (pid $pid) started but showed no frame"
        return 3
    fi
    echo "guest: GNOME Shell started and showed a frame on the probe console"
}

step_uninstall() {
    # Exactly what a person does to go back: remove the Tilo packages with the
    # host's package manager. Their names are read from the installed set so
    # the test never removes something the packages did not add.
    systemctl stop sddm.service 2>/dev/null || true
    step_logout
    local installed=()
    local package
    for package in tilo-session tilo-session-apps tilo-session-devel; do
        rpm -q "$package" >/dev/null 2>&1 && installed+=("$package")
    done
    (( ${#installed[@]} )) || die 'the Tilo packages are not installed'
    dnf -y --setopt=retries=5 remove "${installed[@]}"
    local leftover
    leftover=$(rpm -qa 'tilo-*' 2>/dev/null | tr '\n' ' ')
    [[ -z $leftover ]] || die "packages remain after uninstall: $leftover"
    if [[ -f /usr/share/wayland-sessions/$tilo_session ]]; then
        die "the Tilo session entry survived its package"
    fi
    sed -i "/^Session=$tilo_session$/d" /etc/sddm.conf.d/tilo-e2e.conf
    echo 'guest: Tilo packages and their session entry are gone'
}

step_files() {
    local deliver=$1 take=$2 dir
    for dir in "$(dirname "$deliver")" "$(dirname "$take")"; do
        as_user mkdir -p "/home/$user/$dir"
    done
    as_user sh -c "printf 'delivered before the switch\n' > /home/$user/$deliver"
    as_user sh -c "printf 'delivered before the switch\n' > /home/$user/$take"
    as_user sh -c "test \$(cat /home/$user/$deliver) = \$(cat /home/$user/$take)" \
        || die 'the delivered files did not read back'
    echo 'guest: the login user files are in place'
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
    local topology
    topology=$(grep -o 'DRM topology .*' "$log" 2>/dev/null | tail -n 1 || true)
    echo "topology=$topology"
    echo "driven_cards=$(sed -n 's/.* driven_cards=\([0-9]*\) .*/\1/p' <<<"$topology")"
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

step_files_check() {
    local deliver=$1 take=$2
    as_user sh -c "test \"\$(cat /home/$user/$deliver)\" = 'delivered before the switch'" \
        || die "the login user's $deliver did not survive the switch"
    as_user sh -c "test \"\$(cat /home/$user/$take)\" = 'delivered before the switch'" \
        || die "the login user's $take did not survive the switch"
    as_user sh -c "test \"\$(cat /home/$user/$deliver)\" = \"\$(cat /home/$user/$take)\"" \
        || die 'the two delivered files no longer agree'
    echo "guest: $deliver and $take are unchanged"
}

# Install the same packages again, as a person does when a newer release
# appears: the surface an update has to keep intact.
step_update() {
    local dir=$1
    shopt -s nullglob
    local session=("$dir"/tilo-session-[0-9]*.rpm) apps=("$dir"/tilo-session-apps-[0-9]*.rpm)
    shopt -u nullglob
    (( ${#session[@]} == 1 && ${#apps[@]} == 1 )) \
        || die "want exactly one tilo-session and one tilo-session-apps RPM in $dir"
    dnf -y --setopt=retries=5 --setopt=install_weak_deps=True upgrade \
        "${session[0]}" "${apps[0]}"
    rpm -q tilo-session tilo-session-apps
    echo 'guest: the Tilo packages were upgraded in place'
}

case ${1:-} in
    install) shift; (($# == 1)) || die 'install RPM_DIR'; step_install "$1" ;;
    update) shift; (($# == 1)) || die 'update RPM_DIR'; step_update "$1" ;;
    user) step_user ;;
    gnome-input) step_gnome_input ;;
    keyring) step_keyring ;;
    identity) step_identity ;;
    login) step_login ;;
    logout) step_logout ;;
    session-autologin) shift; (($# == 1)) || die 'session-autologin SESSION'; step_session_autologin "$1" ;;
    session-default) step_session_default ;;
    previous-available) step_previous_available ;;
    previous-probe) step_previous_probe ;;
    uninstall) step_uninstall ;;
    files) shift; (($# == 2)) || die 'files DELIVER TAKE'; step_files "$1" "$2" ;;
    files-check) shift; (($# == 2)) || die 'files-check DELIVER TAKE'; step_files_check "$1" "$2" ;;
    record) shift; (($# == 1)) || die 'record LABEL'; step_record "$1" ;;
    record-check) shift; step_record_check "$@" ;;
    settings) step_settings ;;
    status) shift; step_status "$@" ;;
    collect) shift; (($# == 1)) || die 'collect DIR'; step_collect "$1" ;;
    *) sed -n '2,23p' "$0" >&2; exit 2 ;;
esac
