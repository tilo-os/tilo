#!/usr/bin/env bash
# A development VM of this tree, booted on a t node with KVM and reached through t: `t test
# os-vm-x86_64` boots it as a job of the session, and `t vm console|ssh|display` attach to it
# for as long as its lifetime lasts (`t vm extend` adds to it, `t vm stop` ends it).
#
#   os-vm.sh ARCH DISK   DISK: os-dev-disk-ARCH's disk directory (disk.qcow2, revision)
#
# The disk is this tree's dev image as os-dev-disk-ARCH installed it, never booted itself: the VM
# runs on a throwaway overlay of it, so nothing it does outlives the job. QEMU runs it through
# os/dev/vm.sh, the same harness the gates boot with, serving its serial console and its display
# (VNC) on sockets (through a QEMU wrapper) and its SSH on a forwarded port inside the job; t's attach agent (served by
# the t server that ran the job, so it speaks its protocol) registers those three channels and
# relays each connection the session makes. Nothing listens on the node's network. As root in a
# micro-VM step with a /dev/kvm of its own; nothing here holds a secret or publishes.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/publish/lib.sh"
cd "$repo_root"

arch=${1:?usage: os-vm.sh ARCH DISK}
disk_dir=${2:?}
[[ $(uname -m) == "$arch" ]] || die "the $arch VM runs natively, not on $(uname -m)"
case $arch in
  x86_64) qemu=(edk2-ovmf qemu-system-x86-core qemu-device-display-virtio-vga) ;;
  aarch64) qemu=(edk2-aarch64 qemu-system-aarch64-core qemu-device-display-virtio-gpu qemu-device-display-virtio-gpu-pci) ;;
  *) die "unknown architecture $arch" ;;
esac
[[ -c /dev/kvm ]] || die 'no /dev/kvm: the check needs requires = ["/dev/kvm"]'
[[ -s $disk_dir/disk.qcow2 && -s $disk_dir/revision ]] \
  || die "no installed dev disk in $disk_dir: the check needs os-dev-disk-$arch"
[[ -n ${T_ATTACH_URL:-} && -n ${T_ATTACH_TOKEN:-} ]] \
  || die 'no T_ATTACH_URL/T_ATTACH_TOKEN: this t server gives jobs no attach token, so `t vm` cannot reach the VM'

missing=()
for p in socat qemu-img ipxe-roms-qemu sshpass openssh-clients python3 procps-ng "${qemu[@]}"; do
  rpm -q "$p" >/dev/null 2>&1 || missing+=("$p")
done
((${#missing[@]} == 0)) || dnf install -y --setopt=install_weak_deps=False "${missing[@]}" >/dev/null

# The agent the server that ran this job serves, so it speaks that server's protocol.
agent=$scratch/t-attach-agent.py
curl -fsS --retry 3 -H "authorization: Bearer $T_ATTACH_TOKEN" -o "$agent" \
  "${T_ATTACH_URL%/}/v1/attach-agent/agent.py" \
  || die "the t server at $T_ATTACH_URL served no attach agent"

revision=$(cat "$disk_dir/revision")
export TILO_DISTROBOX= TILO_VM_ARCH=$arch TMPDIR=/var/tmp TILO_USER=tester TILO_PASS=tilo
export TILO_RUN=/var/tmp/tilo-os-vm-$arch TILO_SSH_PORT=2222
export TILO_DISK=$TILO_RUN/disk.qcow2
export TILO_SERIAL_SOCKET=$TILO_RUN/serial.sock TILO_VNC_SOCKET=$TILO_RUN/vnc.sock
rm -rf -- "$TILO_RUN"
install -d -m 0700 "$TILO_RUN"
os/dev/vm.sh tool qemu-img create -q -f qcow2 -F qcow2 -b "$disk_dir/disk.qcow2" "$TILO_DISK"

# vm.sh starts QEMU as $TILO_QEMU_SYSTEM: this wrapper serves the serial console on a socket
# (still logging to the file `vm.sh wait` reads) and the display as VNC on another, so the
# agent can relay both, without changing how the harness boots every other VM.
export TILO_QEMU_SYSTEM=$scratch/qemu-system-attachable
cat > "$TILO_QEMU_SYSTEM" <<EOF
#!/usr/bin/env bash
args=()
while ((\$#)); do
  if [[ \$1 == -serial && \$2 == file:* ]]; then
    args+=(-chardev "socket,id=tiloserial,path=$TILO_SERIAL_SOCKET,server=on,wait=off,logfile=\${2#file:}"
      -serial chardev:tiloserial)
    shift 2
    continue
  fi
  args+=("\$1")
  shift
done
exec qemu-system-$arch "\${args[@]}" -vnc unix:$TILO_VNC_SOCKET
EOF
chmod 0755 "$TILO_QEMU_SYSTEM"

# What the agent runs for the VM's lifetime: boot it, wait for its greeter, then stay while
# QEMU runs. The agent stops this when the lifetime ends or the session stops the VM; the trap
# then stops QEMU too.
runner=$scratch/os-vm-run.sh
cat > "$runner" <<'EOF'
set -euo pipefail
trap 'os/dev/vm.sh stop >/dev/null 2>&1 || true' EXIT TERM INT
os/dev/vm.sh boot --2d
os/dev/vm.sh wait || echo 'os-vm: no greeter took input yet; the VM keeps running for inspection' >&2
echo "os-vm: up (user $TILO_USER, password $TILO_PASS): t vm console, t vm ssh, t vm display" >&2
while kill -0 "$(cat "$TILO_RUN/qemu.pid")" 2>/dev/null; do sleep 5; done
echo 'os-vm: QEMU exited' >&2
EOF

lifetime=${T_INPUT_LIFETIME:-3600}
[[ $lifetime =~ ^[0-9]+$ ]] || die "lifetime must be seconds, not $lifetime"
exec python3 -I "$agent" --label "tilo dev image ${revision:0:12} ($arch)" \
  --lifetime "$lifetime" --max 13500 \
  --console "unix:$TILO_SERIAL_SOCKET" --ssh "tcp:127.0.0.1:$TILO_SSH_PORT" \
  --display "unix:$TILO_VNC_SOCKET" \
  -- bash "$runner"
