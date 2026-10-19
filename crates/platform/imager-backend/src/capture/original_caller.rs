//! Required requested original-owner facts. This representation validates
//! shape and binding only; it does not certify source-peer group authority.
//! It must not be synthesized from target NSS or source inode group metadata.
use serde::{Deserialize, Serialize};
use std::io;
const MAX_GROUPS: usize = 65_536;
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
/// Included inside the password-authenticated request, never inferred from
/// destination NSS names. A separate trusted source broker must independently
/// certify actual peer credentials before privileged projection is possible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalCallerFacts {
    schema: u32,
    uid: u32,
    primary_gid: u32,
    supplementary_gids: Vec<u32>,
}
impl OriginalCallerFacts {
    pub fn primary_gid(&self) -> u32 {
        self.primary_gid
    }
    pub fn supplementary_gids(&self) -> &[u32] {
        &self.supplementary_gids
    }

    pub fn uid(&self) -> u32 {
        self.uid
    }

    pub fn validate(&self, original_home_uid: u32) -> io::Result<()> {
        if self.schema != 1
            || self.uid == 0
            || self.uid == u32::MAX
            || self.uid != original_home_uid
            || self.primary_gid == u32::MAX
            || self.supplementary_gids.len() > MAX_GROUPS
            || self.supplementary_gids.contains(&u32::MAX)
            || self
                .supplementary_gids
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err(refused(
                "original caller facts are not canonical or bound to original Home",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn caller_facts_round_trip_and_validate_without_native_authority() {
        let wire = serde_json::json!({
            "schema": 1, "uid": 1000, "primary_gid": 1001,
            "supplementary_gids": [1001, 1002]
        });
        let facts: OriginalCallerFacts = serde_json::from_value(wire.clone()).unwrap();
        facts.validate(1000).unwrap();
        assert_eq!(facts.uid(), 1000);
        assert_eq!(facts.primary_gid(), 1001);
        assert_eq!(facts.supplementary_gids(), &[1001, 1002]);
        assert_eq!(serde_json::to_value(&facts).unwrap(), wire);
        assert!(facts.validate(1003).is_err());
        for (field, invalid) in [
            ("schema", serde_json::json!(2)),
            ("uid", serde_json::json!(0)),
            ("primary_gid", serde_json::json!(u32::MAX)),
            ("supplementary_gids", serde_json::json!([1002, 1001])),
            ("supplementary_gids", serde_json::json!([1001, 1001])),
        ] {
            let mut invalid_wire = wire.clone();
            invalid_wire[field] = invalid;
            let facts: OriginalCallerFacts = serde_json::from_value(invalid_wire).unwrap();
            assert!(facts.validate(1000).is_err());
        }
    }
}

#[cfg(target_os = "linux")]
mod native;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{net::UnixStream, process::CommandExt},
        },
    };
    const CHILD: &str = "TILO_ORIGINAL_CALLER_FIXTURE_FD";
    const CHILD_TEST: &str = "capture::original_caller::tests::original_caller_fixture_child";

    #[test]
    fn original_caller_fixture_child() {
        let Ok(value) = std::env::var(CHILD) else {
            return;
        };
        let fd = value.parse::<i32>().unwrap();
        // SAFETY: parent transfers this exclusive fixture socket descriptor.
        let mut socket = unsafe { UnixStream::from_raw_fd(fd) };
        runtime_portal::process_binding::complete(&mut socket).unwrap();
        let mut byte = [0];
        socket.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [7]);
    }

    struct Owned(std::process::Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn original_credentials_come_from_actual_kernel_admitted_peer_and_expire_on_exit() {
        let (host, app) = UnixStream::pair().unwrap();
        let executable = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
        // SAFETY: reads this fixture process's current effective credentials.
        let (uid, gid) = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        if uid == 0 && !can_set_fixture_credentials() {
            // A capability-free build sandbox may map its worker to UID 0.
            // Run the entire existing peer scenario as an actual non-root
            // user in a nested user namespace, rather than forging facts or
            // treating that sandbox's UID 0 as an original-source owner.
            tilo_test_env::require_capability!(
                UserNamespace,
                tilo_test_env::user_namespace(),
                "capability-free root runs the peer scenario in a user namespace"
            );
            let output = std::process::Command::new("unshare")
                .args(["--user", "--map-user=1000", "--map-group=1001", "--"])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "capture::original_caller::tests::original_credentials_come_from_actual_kernel_admitted_peer_and_expire_on_exit",
                    "--nocapture",
                ])
                .output()
                .expect("non-root credential fixture namespace must start");
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout)
                        .contains("test result: ok. 1 passed;"),
                "non-root credential fixture failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let groups = if uid == 0 {
            vec![1002u32, 1002, 1003]
        } else {
            current_groups()
        };
        let child_uid = if uid == 0 { 1000 } else { uid };
        let primary_group = if uid == 0 { 1001 } else { gid };
        let descriptors = [app.as_raw_fd(), executable.as_raw_fd()];
        let child_groups = groups.clone();
        // The already held executable avoids relying on a foreign UID traversing
        // the parent's private build directory. No source-home file is touched.
        let mut command =
            std::process::Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
        command
            .args(["--exact", CHILD_TEST, "--nocapture"])
            .env(CHILD, app.as_raw_fd().to_string());
        // SAFETY: child-only pre_exec uses fixed inherited descriptors and
        // async-signal-safe credential syscalls; parent credentials stay intact.
        unsafe {
            command.pre_exec(move || {
                for fd in descriptors {
                    if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                if uid == 0 {
                    let checked = |result, operation: &[u8]| {
                        if result < 0 {
                            let error = io::Error::last_os_error();
                            libc::write(
                                libc::STDERR_FILENO,
                                operation.as_ptr().cast(),
                                operation.len(),
                            );
                            Err(error)
                        } else {
                            Ok(())
                        }
                    };
                    checked(
                        libc::setgroups(child_groups.len(), child_groups.as_ptr()),
                        b"fixture setgroups\n",
                    )?;
                    checked(libc::setgid(primary_group), b"fixture setgid\n")?;
                    checked(libc::setuid(child_uid), b"fixture setuid\n")?;
                }
                Ok(())
            });
        }
        let mut child = Owned(command.spawn().unwrap_or_else(|error| {
            let status = std::fs::read_to_string("/proc/self/status").unwrap();
            let capabilities: Vec<_> = status.lines().filter(|line| line.starts_with("Cap") || line.starts_with("Seccomp")).collect();
            panic!("credential fixture spawn: {error}; {capabilities:?}; uid_map={:?}; gid_map={:?}; setgroups={:?}", std::fs::read_to_string("/proc/self/uid_map"), std::fs::read_to_string("/proc/self/gid_map"), std::fs::read_to_string("/proc/self/setgroups"));
        }));
        let mut channel =
            super::super::transport::HostChannel::authenticate(host, child.0.id(), child_uid)
                .unwrap();
        let owner = channel.original_peer();
        let facts = owner.original_caller_facts().unwrap();
        assert_eq!(facts, channel.original_caller_facts().unwrap());
        assert_eq!(facts.uid, child_uid);
        assert_eq!(facts.primary_gid, primary_group);
        let mut canonical_groups = groups;
        canonical_groups.sort_unstable();
        canonical_groups.dedup();
        assert_eq!(facts.supplementary_gids, canonical_groups);
        facts.validate(child_uid).unwrap();
        assert!(facts.validate(child_uid.checked_add(1).unwrap()).is_err());
        channel.stream().write_all(&[7]).unwrap();
        assert!(child.0.wait().unwrap().success());
        drop(channel);
        assert!(
            owner.original_caller_facts().is_err(),
            "exited original peer cannot authorize another capture"
        );
    }

    #[test]
    fn optional_source_facts_never_reject_existing_online_root_channel() {
        let (host, mut app) = UnixStream::pair().unwrap();
        let client = std::thread::spawn(move || {
            runtime_portal::process_binding::complete(&mut app).unwrap();
            let mut command = [0];
            app.read_exact(&mut command).unwrap();
            app.write_all(&command).unwrap();
        });
        // SAFETY: reads this fixture's actual effective UID, not request facts.
        let uid = rustix::process::geteuid().as_raw();
        let mut channel =
            super::super::transport::HostChannel::authenticate(host, std::process::id(), uid)
                .unwrap();
        let facts = channel.original_caller_facts();
        if uid == 0 {
            assert!(
                facts.is_err(),
                "root is refused only as original-source certification"
            );
        } else {
            facts.unwrap().validate(uid).unwrap();
        }
        channel.stream().write_all(&[9]).unwrap();
        let mut reply = [0];
        channel.stream().read_exact(&mut reply).unwrap();
        assert_eq!(
            reply,
            [9],
            "online admitted traffic survives absent source certification"
        );
        client.join().unwrap();
    }

    fn can_set_fixture_credentials() -> bool {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let bits = status
            .lines()
            .find_map(|line| line.strip_prefix("CapEff:"))
            .unwrap();
        let capabilities = u64::from_str_radix(bits.trim(), 16).unwrap();
        // Linux capability numbers: CAP_SETGID = 6, CAP_SETUID = 7.
        let needed = (1u64 << 6) | (1u64 << 7);
        capabilities & needed == needed
    }

    fn current_groups() -> Vec<u32> {
        // SAFETY: queries only this fixture's current supplementary credentials.
        let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        assert!(count >= 0);
        let mut groups = vec![0; usize::try_from(count).expect("nonnegative group count")];
        // SAFETY: live buffer sized from the kernel count; fail on concurrent drift.
        assert_eq!(
            unsafe { libc::getgroups(count, groups.as_mut_ptr()) },
            count
        );
        groups.sort_unstable();
        groups.dedup();
        groups
    }
}
