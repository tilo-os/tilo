#!/bin/busybox sh
# The init of a micro-VM step (see vm/mod.rs). It runs as the guest's pid 1 from the initramfs
# `Vm::prepare` writes, with busybox as its only tool, and talks to the host through disks
# alone, found by their serial numbers:
#
#   root      the step's image, a read-only filesystem: the guest's `/`, copy-on-write
#   scratch   an empty ext4 for everything the step writes: the root's upper layer, /tmp,
#             /var/tmp, /var/lib/containers and the upper layers of copy-on-write mounts
#   disk<N>   read-only filesystems the job file mounts where the step expects them
#   output    raw: a status line in its first sector, then a tar of the step's outputs from
#             1 MiB on
#
# /t-vm/job (generated) defines what runs: `t_setup` (the disks' mounts), `t_command` (the
# command, in its own environment) and `t_outputs` (the output patterns), and sets T_CWD,
# T_HOSTNAME, T_NET and T_MEMORY_MIB.
#
# Stage 1 assembles the root and switches to it; stage 2 (the same script, from the new root's
# /run/t-vm) mounts the rest, runs the command with its output on the `t.out` port, writes the
# status and outputs, and resets the machine, which ends QEMU (-no-reboot).
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

# Report $1 as the status and stop the machine. Only the output disk is flushed, outputs and
# status alike (dd's fsync of the device): nothing else outlives the machine. A global sync
# also syncs every filesystem shared with the step over virtio-fs, which the host serves by
# syncing its own filesystem as a whole: on a busy node that waited behind all its writeback
# for longer than t lets a step make no progress, and a step that had passed was ended as hung.
finish() {
	out_disk=$(by_serial output) && printf 'T-VM-STATUS %s\n' "$1" | dd of="$out_disk" conv=notrunc,fsync 2>/dev/null
	reboot -f
	sleep 5
}

die() {
	say "$*"
	finish error
}

if [ "$1" != stage2 ]; then
	$BB mkdir -p /proc /sys /dev /bin
	$BB mount -t devtmpfs dev /dev
	$BB mount -t proc proc /proc
	$BB mount -t sysfs sys /sys
	$BB --install -s /bin
	export PATH=/bin
	mkdir -p /lower /scratch /newroot
	root=$(by_serial root) || die "no root disk"
	scratch=$(by_serial scratch) || die "no scratch disk"
	mount -o ro "$root" /lower || die "cannot mount the step's image"
	mount -t ext4 -o noinit_itable "$scratch" /scratch || die "cannot mount the scratch disk"
	mkdir -p /scratch/root/upper /scratch/root/work
	# Every change copies up in full: what a step changed is read back as the upper directory
	# (a check's setup becomes a layer of it), and with redirect_dir a moved directory or with
	# metacopy a chmod would leave its change in overlay xattrs that reading it doesn't keep.
	mount -t overlay overlay \
		-o lowerdir=/lower,upperdir=/scratch/root/upper,workdir=/scratch/root/work,redirect_dir=off,metacopy=off \
		/newroot || die "cannot assemble the root"
	mkdir -p /newroot/run /newroot/dev /newroot/proc /newroot/sys /newroot/tmp
	mount -t tmpfs -o mode=755 run /newroot/run
	mkdir -p /newroot$T/scratch /newroot$T/lower
	cp -a /t-vm/. /newroot$T/
	cp /bin/busybox /init /newroot$T/
	mount --move /scratch /newroot$T/scratch
	mount --move /lower /newroot$T/lower
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

# `t_disk SERIAL N`: the read-only disk SERIAL, mounted at $T/disk/N.
t_disk() {
	dev=$(by_serial "$1") || die "no disk $1"
	mkdir -p $T/disk/$2
	mount -o ro "$dev" $T/disk/$2 || die "cannot mount disk $1"
}

# `t_mount N PATH DST ro|cow`: PATH of disk N at DST, read-only or copy-on-write (its changes on
# the scratch disk).
t_mount() {
	from=$T/disk/$1/$2
	[ -d "$from" ] || die "disk $1 has no $2"
	mkdir -p "$3" || die "cannot make the mount point $3"
	if [ "$4" = cow ]; then
		layer=$S/cow/$1-$(echo "$2" | tr / _)
		mkdir -p "$layer/upper" "$layer/work"
		mount -t overlay overlay -o "lowerdir=$from,upperdir=$layer/upper,workdir=$layer/work" "$3" \
			|| die "cannot mount $3"
	else
		mount --bind "$from" "$3" && mount -o remount,bind,ro "$3" || die "cannot mount $3"
	fi
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
	# passt's MTU (Passt::start): the guest has no DHCP to learn it, and at 1500 an upload
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
if [ "$code" -gt 128 ] && dmesg | grep -q 'Out of memory'; then
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
