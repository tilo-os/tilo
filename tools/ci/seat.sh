#!/usr/bin/env bash
# The seat a check's headless session runs on, as a Tilo device has it: sourced by a check that
# runs as root in a VM of its own (tools/ci/app-walk.sh, tools/ci/key-latency.sh), with
# `$build` the session build and `$repo_root` the tree. `seat_up` makes the machine; `seat_down`
# ends what a session left running on it. A caller that is not root (a developer's own
# desktop) gets neither: its session is its own.
#
# The walk's session runs as an ordinary account, as a person's desktop does on
# their own machine: Chromium refuses its sandbox as root, and the shell runtime
# is the person's user service. This script itself runs as root in the check's
# VM, where the walk stages the mail provider's host names, trust bundles and
# mail ports; the session and everything it starts are the account's
# (scripts/headless-examples/real_apps.py, $TILO_SESSION_USER).
seat_up() {
    [[ $(id -u) == 0 ]] || return 0
    walk_account=${TILO_SESSION_USER:-walker}
    id "$walk_account" >/dev/null 2>&1 \
        || useradd --create-home --shell /bin/bash "$walk_account"
    export TILO_SESSION_USER=$walk_account
    # The seat's runtime directory, which logind gives a login at /run/user/<uid>: the launcher's
    # system grant, the compositor's display snapshot and the session's own sockets are all named
    # from it, as on a device.
    walk_uid=$(id -u "$walk_account")
    install -d -m 0700 -o "$walk_account" -g "$(id -g "$walk_account")" "/run/user/$walk_uid"
    export TILO_SESSION_RUNTIME="/run/user/$walk_uid"
    # The machine's authorities Settings reads: a system bus of the session's own, with the power
    # services a device's systemd starts on it.
    export TILO_SESSION_SYSTEM_BUS=1
    # What checks the person's password at the lock screen, where the Tilo session on another
    # distribution installs it: the session locks only where a password can be checked, so
    # without it the screen never locks.
    install -D -m 0755 "$build/bin/tilo-unlock-helper" /usr/libexec/tilo-unlock-helper
    install -D -m 0644 dist/session/tilo-unlock.pam /etc/pam.d/tilo-unlock
    # The machine the session runs on, as a Tilo device presents it: the device's own group and
    # directories (`os/usr/lib/tmpfiles.d/tilo.conf`), the account in that group, and the FUSE
    # device node and setuid mount helper its fuse3 package installs. The files daemon is the
    # machine's and runs as root (scripts/headless-examples/app-walk.py), so the person's own
    # FUSE mounts — the document portal's and the session's document store — are the only users
    # of the node and the helper. `/var/workspace` is root's, 0755, as the image makes it: every
    # account traverses it into the folders the daemon presents as theirs.
    walk_group=tilo-identity
    getent group "$walk_group" >/dev/null 2>&1 || groupadd --system "$walk_group"
    id -nG "$walk_account" | tr ' ' '\n' | grep -qx "$walk_group" \
        || usermod -aG "$walk_group" "$walk_account"
    install -d -m 0755 -o root -g root /var/workspace
    install -d -m 2755 -o root -g "$walk_group" /var/cache/quick-fs
    install -d -o root -g "$walk_group" /run/tilo /run/tilo/fs-storage
    chown root:"$walk_group" /run/tilo /run/tilo/fs-storage
    chmod 3770 /run/tilo
    chmod 2750 /run/tilo/fs-storage
    # The device daemon is the system's, as Tilo OS runs it: its unit file is the marker every
    # process reads to find the system daemon (`tilo_paths::machine::system_daemon`), and the
    # runtime directory the unit makes, with the socket in it, is open to every account.
    install -D -m 0644 "$repo_root/os/usr/lib/systemd/system/tilo-runtime.service" \
        /usr/lib/systemd/system/tilo-runtime.service
    install -d -m 0755 -o root -g root /run/tilo-runtime
    # A device node is the kernel's, and a device's is open to every account; the image's fuse3
    # package installs fusermount3 setuid. A job sandbox that copies neither leaves a person's
    # FUSE mount with "failed to open /dev/fuse: Permission denied".
    [[ ! -e /dev/fuse ]] || chmod 0666 /dev/fuse
    if [[ -x /usr/bin/fusermount3 ]] && [[ ! -u /usr/bin/fusermount3 ]]; then
        chmod u+s /usr/bin/fusermount3 2>/dev/null || true
    fi
    # A mount a walk that ended badly left behind answers nothing, and the directory cannot be
    # mounted again until it is gone.
    if mountpoint -q /var/workspace 2>/dev/null; then
        umount --lazy /var/workspace 2>/dev/null || true
    fi
    rm -f /run/tilo/fs.pid /run/tilo/fs.failed
    # The machine's shape is this script's, so its teardown is too: a walk on a developer's own
    # desktop (not root) never reaches here, and never unmounts that desktop's workspace.
    machine_ready=1
}

# The machine's files daemon goes with the session: it is root's, started beside it
# (scripts/headless-examples/app-walk.py), and a session that ended early leaves neither it nor
# its mount behind.
seat_down() {
    if [[ ${machine_ready:-0} == 1 ]]; then
        if [[ -r /run/tilo/fs.pid ]]; then
            local fs_pid tries=50
            fs_pid=$(< /run/tilo/fs.pid)
            kill -TERM "$fs_pid" 2>/dev/null || true
            while ((tries--)) && kill -0 "$fs_pid" 2>/dev/null; do sleep 0.1; done
            kill -KILL "$fs_pid" 2>/dev/null || true
            rm -f /run/tilo/fs.pid /run/tilo/fs.failed
        fi
        mountpoint -q /var/workspace 2>/dev/null && umount --lazy /var/workspace 2>/dev/null || true
    fi
}
