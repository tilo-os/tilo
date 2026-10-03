//! Conservative discovery of the account to present on a personal Tilo device.
//!
//! The greeter may collapse to a one-field, passcode-only form only when the
//! machine has exactly one ordinary interactive local account. Ambiguity is a
//! security boundary: zero or multiple candidates return `None`, leaving the
//! username field visible instead of guessing who is signing in.

use std::collections::HashSet;
use std::path::Path;

const DEFAULT_UID_MIN: u32 = 1_000;
const DEFAULT_UID_MAX: u32 = 60_000;

/// A local account safe to present on the login screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginUser {
    pub username: String,
    pub display_name: String,
}

/// Discover the sole ordinary interactive account from the host account
/// database. Any read or parsing ambiguity keeps the two-field fallback.
pub fn discover_default_login_user() -> Option<LoginUser> {
    discover_login_user(
        Path::new("/etc/passwd"),
        Path::new("/etc/login.defs"),
        Path::new("/etc/shells"),
    )
}

fn discover_login_user(passwd: &Path, login_defs: &Path, shells: &Path) -> Option<LoginUser> {
    let passwd = std::fs::read_to_string(passwd).ok()?;
    let login_defs = std::fs::read_to_string(login_defs).unwrap_or_default();
    let shells = std::fs::read_to_string(shells).ok();
    select_login_user(&passwd, &login_defs, shells.as_deref())
}

fn uid_bounds(login_defs: &str) -> (u32, u32) {
    let mut minimum = DEFAULT_UID_MIN;
    let mut maximum = DEFAULT_UID_MAX;
    for line in login_defs.lines() {
        let mut fields = line
            .split('#')
            .next()
            .unwrap_or_default()
            .split_whitespace();
        let Some(key) = fields.next() else { continue };
        let Some(value) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        match key {
            "UID_MIN" => minimum = value,
            "UID_MAX" => maximum = value,
            _ => {}
        }
    }
    (minimum, maximum)
}

fn login_shells(shells: Option<&str>) -> Option<HashSet<&str>> {
    shells.map(|contents| {
        contents
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('/') && !line.contains(char::is_whitespace))
            .collect()
    })
}

fn known_non_login_shell(shell: &str) -> bool {
    matches!(
        shell.rsplit('/').next(),
        Some("nologin" | "false" | "sync" | "shutdown" | "halt")
    )
}

fn safe_username(username: &str) -> bool {
    !username.is_empty()
        && username.len() <= 32
        && username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn safe_display_name(gecos: &str, username: &str) -> String {
    let name = gecos
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect::<String>();
    if name.is_empty() {
        username.to_owned()
    } else {
        name
    }
}

fn select_login_user(passwd: &str, login_defs: &str, shells: Option<&str>) -> Option<LoginUser> {
    let (uid_min, uid_max) = uid_bounds(login_defs);
    if uid_min > uid_max {
        return None;
    }
    let allowed_shells = login_shells(shells);
    let mut candidate = None;
    for line in passwd.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split(':');
        let username = fields.next()?;
        let _password = fields.next()?;
        let uid = fields.next()?.parse::<u32>().ok()?;
        let _group = fields.next()?;
        let gecos = fields.next()?;
        let home = fields.next()?;
        let shell = fields.next()?;
        if fields.next().is_some() {
            // A malformed account database is not a reason to guess.
            return None;
        }
        if uid < uid_min
            || uid > uid_max
            || uid == 65_534
            || !safe_username(username)
            || !home.starts_with('/')
            || !shell.starts_with('/')
            || known_non_login_shell(shell)
            || allowed_shells
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(shell))
        {
            continue;
        }
        let found = LoginUser {
            username: username.to_owned(),
            display_name: safe_display_name(gecos, username),
        };
        if candidate.replace(found).is_some() {
            return None;
        }
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHELLS: &str = "/bin/bash\n/bin/zsh\n";

    #[test]
    fn local_user_discovery_selects_only_one_safe_interactive_user() {
        // Phase `exactly_one_interactive_user_is_selected`.
        {
            let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                          greetd:x:977:977::/var/lib/greetd:/usr/sbin/nologin\n\
                          tester:x:1000:1000:ada Quick,,,:/home/tester:/bin/bash\n";
            assert_eq!(
                select_login_user(passwd, "UID_MIN 1000\nUID_MAX 60000\n", Some(SHELLS)),
                Some(LoginUser {
                    username: "tester".into(),
                    display_name: "ada Quick".into(),
                })
            );
        }
        // Phase `zero_or_multiple_users_never_guess`.
        {
            let none = "root:x:0:0:root:/root:/bin/bash\ndaemon:x:1000:1000::/:/sbin/nologin\n";
            assert_eq!(select_login_user(none, "", Some(SHELLS)), None);

            let multiple = "alice:x:1000:1000:Alice:/home/alice:/bin/bash\n\
                            bob:x:1001:1001:Bob:/home/bob:/bin/zsh\n";
            assert_eq!(select_login_user(multiple, "", Some(SHELLS)), None);
        }
        // Phase `uid_bounds_and_shell_allowlist_are_enforced`.
        {
            let passwd = "service:x:1500:1500::/srv/service:/bin/custom\n\
                          person:x:2000:2000::/home/person:/bin/bash\n";
            assert_eq!(
                select_login_user(passwd, "UID_MIN 2000\nUID_MAX 2999\n", Some(SHELLS))
                    .map(|user| user.username),
                Some("person".into())
            );
        }
        // Phase `malformed_or_unsafe_records_fall_back`.
        {
            assert_eq!(select_login_user("broken:record", "", Some(SHELLS)), None);
            assert_eq!(
                select_login_user(
                    "bad name:x:1000:1000:Bad\nName:/home/bad:/bin/bash\n",
                    "",
                    Some(SHELLS)
                ),
                None
            );
        }
    }
}
