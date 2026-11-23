//! How the device's shell runtime (`ush`) shows the session that it holds the device identity
//! when the session cannot look at it.
//!
//! The session serves its MCP socket to programs in its own namespaces. The shell runtime runs
//! outside them where systemd starts it (its unit's `PrivateTmp=`), and it reads the device key
//! (the key its tool builder signs with), which makes it non-dumpable: the kernel then refuses its
//! namespaces and its executable to every other process of the user, the session included. So it
//! proves instead what no sandboxed program can: that it holds the device identity. It puts the
//! proof in its request ([`crate::environment_socket::Request`],
//! [`crate::workspace_terminal::Request`]), made over [`statement`] of itself, which the session
//! reads of the process that connected: a proof is its process's alone, and one copied to
//! another process proves nothing there.

/// The bytes a proof is made over for the process `pid`: its pid and when it started, as
/// `/proc/<pid>/stat` says (which the kernel shows of any process). `None` when there is no
/// such process.
#[must_use]
pub fn statement(pid: u32) -> Option<Vec<u8>> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    statement_of(pid, &stat)
}

/// [`statement`] of `pid` from the `fields` of `/proc/<pid>/stat` the kernel gave for it.
#[must_use]
pub fn statement_of(pid: u32, fields: &str) -> Option<Vec<u8>> {
    // The command name, in parentheses, may hold spaces and parentheses itself: the fields
    // after it start past its last `)`. The start time is the 22nd field, the 20th after it.
    let (_, after) = fields.rsplit_once(')')?;
    let started: u64 = after.split_whitespace().nth(19)?.parse().ok()?;
    Some(format!("{pid}:{started}").into_bytes())
}

/// Whether `proof` is written as a proof is: 32 bytes, in hex.
#[must_use]
pub fn well_formed(proof: &str) -> bool {
    proof.len() == 64 && proof.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_statement_names_the_process_and_when_it_started() {
        let stat = "4242 (tilo ush) (x)) S 1 4242 4242 0 -1 4194560 120 0 0 0 1 2 0 0 20 0 3 0 \
                    987654 1000 200 18446744073709551615";
        assert_eq!(statement_of(4242, stat).unwrap(), b"4242:987654");
        assert!(
            statement_of(4242, "4242 (sleep").is_none(),
            "a cut-off stat"
        );
        let own = statement(std::process::id()).expect("this process's own");
        assert!(own.starts_with(format!("{}:", std::process::id()).as_bytes()));
        assert!(statement(u32::MAX).is_none(), "no such process");
        assert!(well_formed(&"ab".repeat(32)));
        assert!(!well_formed(&"ab".repeat(31)));
        assert!(!well_formed(&"zz".repeat(32)));
    }
}
