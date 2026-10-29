#!/bin/busybox sh
# The init of a micro-VM step (see vm/mod.rs). It runs as the guest's pid 1 from the initramfs
# `Vm::prepare` writes, with busybox as its only tool, and talks to the host through two
# virtio-fs shares and two disks:
#
#   share     (virtio-fs, tag T_SHARE_TAG) read-only: one directory per layer, each announced as
#             a mount of its own; the root and every input are overlays of some of them
#   rw        (virtio-fs, tag T_RW_TAG, when set) writable: directories bound where the step
#             writes what the host reads as it is (a build directory)
#   scratch   (disk) an empty ext4 for everything else the step writes: the root's and the
#             inputs' upper layers, /tmp, /var/tmp, /var/lib/containers
#   output    (disk) raw: a tar of the step's outputs from 1 MiB on and, written last, a status
#             line in its first sector
#
# /t-vm/job (generated) defines what runs: `t_layers` (the root's layers), `t_setup` (the
# inputs and writable directories), `t_command` (the command, in its own environment) and
# `t_outputs` (the output patterns), and sets T_CWD, T_HOSTNAME, T_NET, T_MEMORY_MIB,
# T_SHARE_TAG and T_RW_TAG.
#
# Stage 1 assembles the root and switches to it; stage 2 (the same script, from the new root's
# /run/t-vm) mounts the rest, runs the command with its output on the `t.out` port, writes the
# outputs and then the status, and resets the machine, which ends QEMU (-no-reboot).
BB=/bin/busybox
T=/run/t-vm

say() { echo "t-vm: $*" >/dev/hvc0; }

# The block device whose serial is $1.
by_serial() {
	for b in /sys/block/vd*; do
		[ "$(cat "$b/serial" 2>/dev/null)" = "$1" ] && { echo "/dev/${b##*/}"; return 0; }
	done
	return 1
}

# Report $1 as the status and stop the machine. Everything the step wrote (the outputs, the
# writable share) is synced first: a written status means the rest is complete, so the host
# may take the result while the machine is still going down.
finish() {
	sync
	out_disk=$(by_serial output) && printf 'T-VM-STATUS %s\n' "$1" | dd of="$out_disk" conv=notrunc,fsync 2>/dev/null
	sync
	reboot -f
	sleep 5
}

die() {
	say "$*"
	finish error
}

# `t_layer NAME` (in t_layers): the share's layer NAME, below those before it. Each layer is an
# announced submount, which the kernel mounts on first access: overlayfs refuses an automount
# point as a lower, so each is looked at first.
lowers=
t_layer() {
	d=/run/t-share/$1
	ls "$d" >/dev/null 2>&1 || die "the share has no layer $1"
	lowers=${lowers:+$lowers:}$d
}

if [ "$1" != stage2 ]; then
	$BB mkdir -p /proc /sys /dev /bin
	$BB mount -t devtmpfs dev /dev
	$BB mount -t proc proc /proc
	$BB mount -t sysfs sys /sys
	$BB --install -s /bin
	export PATH=/bin
	mkdir -p /scratch /newroot /run/t-share /run/t-rw
	. /t-vm/job || die "the job file is broken"
	mount -t virtiofs "$T_SHARE_TAG" /run/t-share -o ro || die "cannot mount the shared inputs"
	if [ -n "$T_RW_TAG" ]; then
		mount -t virtiofs "$T_RW_TAG" /run/t-rw || die "cannot mount the writable share"
	fi
	scratch=$(by_serial scratch) || die "no scratch disk"
	mount -t ext4 -o noinit_itable "$scratch" /scratch || die "cannot mount the scratch disk"
	t_layers
	[ -n "$lowers" ] || die "the root has no layers"
	mkdir -p /scratch/root/upper /scratch/root/work
	# userxattr: the layers' opaque directories are user.overlay.opaque, as the host's
	# overlays wrote and read them.
	mount -t overlay overlay -o "userxattr,lowerdir=$lowers,upperdir=/scratch/root/upper,workdir=/scratch/root/work" /newroot \
		|| die "cannot assemble the root"
	mkdir -p /newroot/run /newroot/dev /newroot/proc /newroot/sys /newroot/tmp
	mount -t tmpfs -o mode=755 run /newroot/run
	mkdir -p /newroot$T/scratch /newroot$T/share /newroot$T/rw
	cp -a /t-vm/. /newroot$T/
	cp /bin/busybox /init /newroot$T/
	mount --move /scratch /newroot$T/scratch
	mount --move /run/t-share /newroot$T/share
	if [ -n "$T_RW_TAG" ]; then
		mount --move /run/t-rw /newroot$T/rw
	fi
	umount /proc /sys
	mount --move /dev /newroot/dev
	exec switch_root /newroot $T/busybox sh $T/init stage2
	die "cannot switch to the step's root"
fi

BB=$T/busybox
$BB mkdir -p $T/bin
$BB --install -s $T/bin
export PATH=$T/bin
mount -t proc proc /proc
mount -t sysfs sys /sys
mkdir -p /dev/pts /dev/shm /sys/fs/cgroup
mount -t devpts -o gid=5,mode=620,ptmxmode=666 devpts /dev/pts
mount -t tmpfs -o mode=1777 shm /dev/shm
mount -t cgroup2 cgroup2 /sys/fs/cgroup
[ -e /dev/fd ] || ln -s /proc/self/fd /dev/fd
S=$T/scratch
# What tools write in bulk lands on the scratch disk itself, not in the overlay: podman's
# overlay storage can't sit on overlayfs.
for d in tmp var-tmp containers; do mkdir -p $S/$d; done
chmod 1777 $S/tmp $S/var-tmp
mkdir -p /tmp /var/tmp /var/lib/containers
mount --bind $S/tmp /tmp
mount --bind $S/var-tmp /var/tmp
mount --bind $S/containers /var/lib/containers

# `t_input DST LAYER...`: the share's LAYERs (top first) stacked at DST, copy-on-write (its
# changes on the scratch disk).
t_input() {
	dst=$1
	shift
	in_lowers=
	for name in "$@"; do
		d=$T/share/$name
		ls "$d" >/dev/null 2>&1 || die "the share has no layer $name"
		in_lowers=${in_lowers:+$in_lowers:}$d
	done
	layer=$S/cow/$(echo "$dst" | tr / _)
	mkdir -p "$layer/upper" "$layer/work" "$dst" || die "cannot make the mount point $dst"
	mount -t overlay overlay -o "userxattr,lowerdir=$in_lowers,upperdir=$layer/upper,workdir=$layer/work" "$dst" \
		|| die "cannot mount $dst"
}

# `t_writable NAME DST`: the writable share's directory NAME at DST, written through.
t_writable() {
	from=$T/rw/$1
	[ -d "$from" ] || die "the writable share has no $1"
	mkdir -p "$2" || die "cannot make the mount point $2"
	mount --bind "$from" "$2" || die "cannot mount $2"
}

# `t_which PROGRAM`: PROGRAM's path, looked up in the step's own PATH (T_PATH) where it has no
# slash, in the step's root.
t_which() {
	case $1 in */*) echo "$1"; return 0 ;; esac
	old_ifs=$IFS
	IFS=:
	for d in $T_PATH; do
		[ -n "$d" ] && [ -f "$d/$1" ] && [ -x "$d/$1" ] && { IFS=$old_ifs; echo "$d/$1"; return 0; }
	done
	IFS=$old_ifs
	echo "t-vm: $1: not found in $T_PATH" >&2
	return 1
}

# `t_output PATTERN`: every file matching PATTERN (a glob over absolute paths, where `*` also
# matches `/`), listed for the output tar.
t_output() {
	prefix=${1%%[*?[]*}
	prefix=${prefix%/*}
	[ -n "$prefix" ] || prefix=/
	[ -e "$prefix" ] || return 0
	find "$prefix" -type f -path "$1" >>$T/outputs
}

. $T/job || die "the job file is broken"
t_setup
ip link set lo up
hostname "$T_HOSTNAME"
grep -q localhost /etc/hosts 2>/dev/null || printf '127.0.0.1 localhost\n::1 localhost\n' >>/etc/hosts
if [ "$T_NET" = 1 ]; then
	# passt's MTU (passt_argv): the guest has no DHCP to learn it, and at 1500 an upload
	# of tens of MiB through passt stalled for minutes.
	ip link set eth0 mtu 65520 2>/dev/null || :
	rm -f /etc/resolv.conf
	echo "nameserver $T_DNS" >/etc/resolv.conf
fi

port=
for p in /sys/class/virtio-ports/*; do
	[ "$(cat "$p/name" 2>/dev/null)" = t.out ] && port=/dev/${p##*/}
done
[ -n "$port" ] || die "no output port"

(cd "$T_CWD" && t_command) </dev/null >"$port" 2>&1 &
wait $!
code=$?
# The OOM killer may end any process under the command (rustc under cargo, which then fails with
# its own code), and its lines never reach the console (loglevel=1): the host reads them here,
# `Out of memory: Killed process ...` as the kernel wrote them.
if [ "$code" -ne 0 ] && dmesg | grep -i 'out of memory: killed process' >$T/oom; then
	cat $T/oom >"$port"
	echo "t-vm: the command ran out of the VM's memory ($T_MEMORY_MIB MiB)" >"$port"
fi

: >$T/outputs
t_outputs
out_disk=$(by_serial output) || die "no output disk"
if [ -s $T/outputs ]; then
	# Relative to /, and sorted, so the same files make the same archive.
	sed 's|^/*||' $T/outputs | sort -u >$T/outputs.rel
	# As arguments: not every busybox's tar reads a list of names (-T).
	set --
	while IFS= read -r f; do set -- "$@" "$f"; done <$T/outputs.rel
	{ tar -C / -cf - "$@" 2>$T/tar-errors || echo failed >>$T/tar-errors; } \
		| dd of="$out_disk" bs=1M seek=1 conv=notrunc 2>/dev/null \
		|| { echo "t-vm: the outputs don't fit on the output disk" >"$port"; finish error; }
	[ -s $T/tar-errors ] && { cat $T/tar-errors >"$port"; finish error; }
fi
finish "$code"
