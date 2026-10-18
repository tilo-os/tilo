//! Kernel interfaces the `libc` crate does not wrap: `clone3`, `mount_setattr`,
//! `open_tree`, `move_mount`, Landlock, and the few structures they take.

use std::os::fd::{FromRawFd, OwnedFd, RawFd};

/// `struct clone_args`, version 2 (with `cgroup`).
#[repr(C)]
#[derive(Default)]
pub struct CloneArgs {
    pub flags: u64,
    pub pidfd: u64,
    pub child_tid: u64,
    pub parent_tid: u64,
    pub exit_signal: u64,
    pub stack: u64,
    pub stack_size: u64,
    pub tls: u64,
    pub set_tid: u64,
    pub set_tid_size: u64,
    pub cgroup: u64,
}

/// `CLONE_INTO_CGROUP`.
pub const CLONE_INTO_CGROUP: u64 = 0x2_0000_0000;

/// What a child started by [`clone3_run`] runs; it never returns.
pub type ChildFn = unsafe extern "C" fn(*mut libc::c_void) -> !;

/// `clone3` whose child calls `child(arg)` on the stack `args` names
/// (`stack`, `stack_size`): the way to start a child that shares this
/// process's memory (`CLONE_VM`), which must not return into the frames of
/// the thread that made it. libc wraps `clone` this way, but not `clone3`,
/// the only call that takes `CLONE_INTO_CGROUP`. Returns what the parent's
/// `clone3` returned: the child's pid, or a negative errno.
///
/// # Safety
/// `args` names a mapped, writable stack, its top 16-byte aligned, used by
/// nothing else until the child has exited or called `execve`; `child`
/// never returns.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub unsafe fn clone3_run(args: &CloneArgs, child: ChildFn, arg: *mut libc::c_void) -> libc::c_long {
    let size = std::mem::size_of::<CloneArgs>();
    let ret: libc::c_long;
    // SAFETY: the parent returns from the system call as from any other.
    // The kernel starts the child at the same instruction, with these
    // registers and its stack pointer at the top of the given stack, so it
    // calls `child` there and never comes back into this frame. The raw
    // system call (not libc's `syscall`) because the child must not return
    // through a frame on the parent's stack.
    #[cfg(target_arch = "x86_64")]
    unsafe {
        std::arch::asm!(
            "syscall",
            "test rax, rax",
            "jnz 2f",
            "xor ebp, ebp",
            "mov rdi, r13",
            "call r12",
            "ud2",
            "2:",
            inlateout("rax") libc::SYS_clone3 => ret,
            in("rdi") std::ptr::from_ref(args),
            in("rsi") size,
            in("r12") child,
            in("r13") arg,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    // SAFETY: as above; the kernel keeps every register but x0 across `svc`.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        std::arch::asm!(
            "svc #0",
            "cbnz x0, 2f",
            "mov x29, xzr",
            "mov x30, xzr",
            "mov x0, x10",
            "blr x9",
            "brk #0x1",
            "2:",
            inlateout("x0") std::ptr::from_ref(args) => ret,
            in("x1") size,
            in("x8") libc::SYS_clone3,
            in("x9") child,
            in("x10") arg,
            options(nostack),
        );
    }
    ret
}

/// `struct mount_attr`.
#[repr(C)]
#[derive(Default)]
pub struct MountAttr {
    pub attr_set: u64,
    pub attr_clr: u64,
    pub propagation: u64,
    pub userns_fd: u64,
}

pub const MOUNT_ATTR_RDONLY: u64 = 0x1;
pub const MOUNT_ATTR_NOSUID: u64 = 0x2;
pub const MOUNT_ATTR_NODEV: u64 = 0x4;
pub const MOUNT_ATTR_IDMAP: u64 = 0x10_0000;
pub const AT_RECURSIVE: libc::c_uint = 0x8000;

/// `open_tree` flags.
pub const OPEN_TREE_CLONE: libc::c_uint = 1;
pub const OPEN_TREE_CLOEXEC: libc::c_uint = libc::O_CLOEXEC.unsigned_abs();

/// `move_mount` from the mount a descriptor names.
pub const MOVE_MOUNT_F_EMPTY_PATH: libc::c_uint = 0x4;

/// `struct landlock_ruleset_attr`, the filesystem half only: the kernel
/// accepts a prefix of the structure, so this works on every ABI.
#[repr(C)]
pub struct LandlockRulesetAttr {
    pub handled_access_fs: u64,
}

/// `struct landlock_path_beneath_attr` (packed in the kernel's header).
#[repr(C, packed)]
pub struct LandlockPathBeneath {
    pub allowed_access: u64,
    pub parent_fd: i32,
}

pub const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;
pub const LANDLOCK_RULE_PATH_BENEATH: libc::c_uint = 1;

pub const LL_EXECUTE: u64 = 1 << 0;
pub const LL_WRITE_FILE: u64 = 1 << 1;
pub const LL_READ_FILE: u64 = 1 << 2;
pub const LL_READ_DIR: u64 = 1 << 3;
pub const LL_REFER: u64 = 1 << 13;
pub const LL_TRUNCATE: u64 = 1 << 14;
pub const LL_IOCTL_DEV: u64 = 1 << 15;

/// Every filesystem right Landlock ABI `abi` knows.
pub fn landlock_fs_rights(abi: u32) -> u64 {
    let mut rights = (1 << 13) - 1;
    if abi >= 2 {
        rights |= LL_REFER;
    }
    if abi >= 3 {
        rights |= LL_TRUNCATE;
    }
    if abi >= 5 {
        rights |= LL_IOCTL_DEV;
    }
    rights
}

/// The rights a rule on a non-directory may carry.
pub const LL_FILE_RIGHTS: u64 =
    LL_EXECUTE | LL_WRITE_FILE | LL_READ_FILE | LL_TRUNCATE | LL_IOCTL_DEV;

/// Reading and running: the rights everything outside writable mounts gets.
pub const LL_READ: u64 = LL_EXECUTE | LL_READ_FILE | LL_READ_DIR;

/// The running kernel's Landlock ABI, or `None` without Landlock.
pub fn landlock_abi() -> Option<u32> {
    // SAFETY: the version query reads no memory (a null attribute, size 0).
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0_usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    u32::try_from(abi).ok().filter(|a| *a > 0)
}

/// A new Landlock ruleset handling `fs` rights.
pub fn landlock_ruleset(fs: u64) -> std::io::Result<OwnedFd> {
    let attr = LandlockRulesetAttr {
        handled_access_fs: fs,
    };
    // SAFETY: `attr` is a live structure of the size passed.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &raw const attr,
            std::mem::size_of::<LandlockRulesetAttr>(),
            0_u32,
        )
    };
    let fd = RawFd::try_from(fd).map_err(|_| std::io::Error::other("descriptor out of range"))?;
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the kernel just returned this descriptor; nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `struct __user_cap_header_struct` and `__user_cap_data_struct`.
#[repr(C)]
pub struct CapHeader {
    pub version: u32,
    pub pid: i32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct CapData {
    pub effective: u32,
    pub permitted: u32,
    pub inheritable: u32,
}

pub const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;

/// `SECBIT_NOROOT | SECBIT_NO_SETUID_FIXUP | SECBIT_NO_CAP_AMBIENT_RAISE`,
/// each with its lock, and `SECBIT_KEEP_CAPS_LOCKED` (keep-caps off, locked):
/// an `execve` as uid 0 grants nothing, and no later call can change that.
pub const SECUREBITS_LOCKED_DOWN: libc::c_ulong = 0b1110_1111;

/// `struct ifreq` as `SIOCGIFFLAGS`/`SIOCSIFFLAGS` use it.
#[repr(C)]
pub struct IfreqFlags {
    pub name: [u8; 16],
    pub flags: libc::c_short,
    pub pad: [u8; 22],
}
