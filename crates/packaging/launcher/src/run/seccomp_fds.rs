//! The seccomp programs a sandboxed launch hands bwrap, and the descriptor slots a
//! declared service may never take.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};

use crate::services::{
    ACCOUNT_CHILD_FD, BASELINE_SECCOMP_CHILD_FD, CLONE3_COMPAT_SECCOMP_CHILD_FD,
};

pub(crate) struct SeccompFds {
    pub(crate) clone3_compat: OwnedFd,
    pub(crate) baseline: OwnedFd,
}

impl SeccompFds {
    pub(crate) fn prepend_bwrap_args(argv: &mut Vec<String>) {
        let mut prefix = vec![
            "--add-seccomp-fd".to_string(),
            CLONE3_COMPAT_SECCOMP_CHILD_FD.to_string(),
            "--add-seccomp-fd".to_string(),
            BASELINE_SECCOMP_CHILD_FD.to_string(),
        ];
        prefix.append(argv);
        *argv = prefix;
    }

    pub(crate) fn child_fds(&self, account: RawFd) -> [(RawFd, RawFd); 3] {
        [
            (
                self.clone3_compat.as_raw_fd(),
                CLONE3_COMPAT_SECCOMP_CHILD_FD,
            ),
            (self.baseline.as_raw_fd(), BASELINE_SECCOMP_CHILD_FD),
            (account, ACCOUNT_CHILD_FD),
        ]
    }
}

#[cfg(test)]
mod seccomp_fd_tests {
    use super::*;

    #[test]
    fn both_seccomp_filters_precede_the_sandbox_arguments() {
        let compat = crate::run::stage_seccomp_blob(&[1, 2, 3]).expect("compat filter fd");
        let baseline = crate::run::stage_seccomp_blob(&[4, 5, 6]).expect("baseline filter fd");
        let _fds = SeccompFds {
            clone3_compat: compat,
            baseline,
        };
        let mut argv = vec!["--unshare-all".to_string(), "/app/bin".to_string()];

        SeccompFds::prepend_bwrap_args(&mut argv);

        assert_eq!(
            argv,
            [
                "--add-seccomp-fd",
                "40",
                "--add-seccomp-fd",
                "41",
                "--unshare-all",
                "/app/bin",
            ]
        );
    }

    #[test]
    fn no_handed_over_descriptor_can_replace_a_seccomp_program() {
        use crate::services::RESERVED_CHILD_FDS;
        // Declared services are numbered from 17 upwards around the reserved slots; a filter sharing a
        // slot with one would make bwrap read its program from that service's socket and never start
        // the app.
        for fd in [CLONE3_COMPAT_SECCOMP_CHILD_FD, BASELINE_SECCOMP_CHILD_FD] {
            assert!(RESERVED_CHILD_FDS.contains(&fd), "{fd} must be reserved");
            assert!(fd > 31, "{fd} must stay above bwrap's own descriptors");
        }
        let mut unique = RESERVED_CHILD_FDS.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            RESERVED_CHILD_FDS.len(),
            "reserved slots overlap"
        );
    }
}
