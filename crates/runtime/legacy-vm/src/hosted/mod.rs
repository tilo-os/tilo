//! Hosted VMs: one VM per hosted workspace, which runs its Linux programs
//! and its Browser.
//!
//! On a host that serves other people's workspaces, the Linux programs a
//! workspace's shell runs (tier 3: `cargo`, `npm`, `python`, …) and the web
//! pages its Browser shows run in a VM of the workspace's own, never in a
//! job sharing the host's kernel. The shell's own commands and Tilo actions
//! need no VM. Its time is billed from the first Linux action until it
//! sleeps; Browser's page actions are counted instead.
//!
//! A hosted VM is the legacy VM's sibling: the same guest agent and
//! protocol over vsock ([`legacy_agent_protocol`]), the same
//! [`Lifecycle`](crate::lifecycle::Lifecycle) deciding when it wakes and
//! sleeps, and its shares served at the same paths. What differs is the
//! hypervisor, chosen for a host packed with many tenants' VMs:
//!
//! - the first tier-3 command boots the VM ([`vm`]); later ones run in it;
//! - the kernel and the root filesystem it boots come from one signed,
//!   digest-pinned image ([`guest`]);
//! - once no command has run for the idle limit, it is snapshotted to disk
//!   and its process ends; the next command restores it;
//! - every VM has a [`size`] and a persistent disk of that size;
//! - the same VM holds the workspace's Browser ([`browser`]): a headless
//!   session the first browser action starts in it, which needs no card;
//! - [`meter`] records each run and each held snapshot, per second, in the
//!   account's ledger, and [`plan`] refuses what the account's plan does not
//!   cover, in words an agent can act on;
//! - one service per host ([`service`]) owns every VM and answers ush over a
//!   Unix socket ([`control`]).
//!
//! # Why Firecracker
//!
//! Measured on one host (Xeon 6975P-C, KVM, a local solid-state disk, page cache dropped
//! before every restore), with a 2 vCPU / 4 GiB guest whose agent answers on
//! vsock, timed from starting the hypervisor process to the agent's answer:
//!
//! | | Cold boot | Restore | Memory after restore | Suspend (8 GiB guest) |
//! |---|---|---|---|---|
//! | Firecracker 1.17 | 294–345 ms | 249–334 ms | 18–20 MB, stays there | 538 ms first, then 42–117 ms (diff) |
//! | cloud-hypervisor 54 | 284–658 ms | 253–312 ms (on-demand) | 75–100 MB, then all 8 GiB within 6 s | 0.8 s per 4 GiB (full) |
//! | QEMU microvm | 661 ms | 751 ms | 224 MB | full migration stream |
//!
//! First answers are close; density is not. Firecracker maps a restored
//! VM's memory file privately and reads a page only when the guest touches
//! it, so a woken VM costs what it uses. cloud-hypervisor's on-demand
//! restore answers as fast but then copies the whole guest RAM in behind
//! it, and its snapshots are always the full RAM: a resumed 8 GiB VM holds
//! 8 GiB again. Firecracker's dirty-page tracking makes each suspend write
//! only the pages written since the last one, into a sparse file that
//! holds what the guest touched (285 MB for an idle 8 GiB guest), which
//! matters on cloud disks with metered throughput. Forty 8 GiB VMs booted at
//! once on the same host all answered within 1.3 s.
//!
//! What Firecracker lacks, virtio-fs and device hot-plug, is replaced by the
//! agent mounting each share over vsock ([`files`], and
//! `legacy_agent::remote_fs` in the guest), each share served by its own
//! process confined with Landlock to that share, as `virtiofsd` is for the
//! legacy VM. The legacy VM itself stays on libvirt and QEMU: it needs a
//! display path, Flatpak and a whole desktop guest, not density.

#![allow(
    clippy::missing_errors_doc,
    reason = "errors are typed enums whose variants document themselves"
)]

pub mod browser;
pub mod control;
pub mod files;
pub mod firecracker;
pub mod guest;
pub mod jail;
pub mod meter;
pub mod network;
pub mod placement;
pub mod plan;
pub mod service;
pub mod size;
pub mod vm;

/// An account, workspace or environment id usable as a file name: ASCII
/// letters, digits, `-`, `_` and `.`, not starting with `.`, at most 128
/// bytes.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_plain_file_names() {
        for good in ["acct-1", "ws_2", "default", "a.b"] {
            assert!(valid_name(good), "{good}");
        }
        for bad in [
            "",
            ".",
            "..",
            ".hidden",
            "a/b",
            "a b",
            "ä",
            &"x".repeat(129),
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
}
