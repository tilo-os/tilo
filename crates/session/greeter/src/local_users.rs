//! Conservative discovery of the account to present on a personal Tilo device.
//!
//! The greeter may collapse to a one-field, passcode-only form only when the
//! machine has exactly one ordinary interactive local account. With several it
//! lists them to choose from; with none, or an account database it cannot read
//! with certainty, it asks for a username instead of guessing.

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

/// Discover the ordinary interactive accounts from the host account
/// database, in database order. Any read or parsing ambiguity returns none,
/// which keeps the typed-username fallback.
pub fn discover_login_users() -> Vec<LoginUser> {
    discover_in(
        Path::new("/etc/passwd"),
        Path::new("/etc/login.defs"),
        Path::new("/etc/shells"),
    )
}

fn discover_in(passwd: &Path, login_defs: &Path, shells: &Path) -> Vec<LoginUser> {
    let Ok(passwd) = std::fs::read_to_string(passwd) else {
        return Vec::new();
    };
    let login_defs = std::fs::read_to_string(login_defs).unwrap_or_default();
    let shells = std::fs::read_to_string(shells).ok();
    select_login_users(&passwd, &login_defs, shells.as_deref()).unwrap_or_default()
}

/// The account last signed in to on this machine, remembered by the greeter
/// in its own state directory. Only a name that is one of `users` counts.
pub fn remembered_user(users: &[LoginUser]) -> Option<usize> {
    let name = std::fs::read_to_string(last_user_path()?).ok()?;
    let name = name.trim();
    users.iter().position(|user| user.username == name)
}

/// Remember `username` as the account to offer first next time.
pub fn remember_user(username: &str) {
    let Some(path) = last_user_path() else { return };
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_ok()
    {
        let _ = std::fs::write(&path, username);
    }
}

fn last_user_path() -> Option<std::path::PathBuf> {
    Some(
        tilo_paths::xdg::state_home()?
            .join("tilo-greeter")
            .join("last-user"),
    )
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

#[cfg(test)]
fn select_login_user(passwd: &str, login_defs: &str, shells: Option<&str>) -> Option<LoginUser> {
    let mut users = select_login_users(passwd, login_defs, shells)?;
    (users.len() == 1).then(|| users.remove(0))
}

/// Every ordinary interactive account, or `None` when the database is
/// malformed (a reason not to guess at all).
fn select_login_users(
    passwd: &str,
    login_defs: &str,
    shells: Option<&str>,
) -> Option<Vec<LoginUser>> {
    let (uid_min, uid_max) = uid_bounds(login_defs);
    if uid_min > uid_max {
        return None;
    }
    let allowed_shells = login_shells(shells);
    let mut candidates = Vec::new();
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
        candidates.push(LoginUser {
            username: username.to_owned(),
            display_name: safe_display_name(gecos, username),
        });
    }
    Some(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHELLS: &str = "/bin/bash\n/bin/zsh\n";

    #[test]
    fn local_user_discovery_selects_only_one_safe_interactive_user() {
        {
            let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                          greetd:x:977:977::/var/lib/greetd:/usr/sbin/nologin\n\
                          quickuser:x:1000:1000:Quinten Quick,,,:/home/quickuser:/bin/bash\n";
            assert_eq!(
                select_login_user(passwd, "UID_MIN 1000\nUID_MAX 60000\n", Some(SHELLS)),
                Some(LoginUser {
                    username: "quickuser".into(),
                    display_name: "Quinten Quick".into(),
                })
            );
        }
        {
            let none = "root:x:0:0:root:/root:/bin/bash\ndaemon:x:1000:1000::/:/sbin/nologin\n";
            assert_eq!(select_login_user(none, "", Some(SHELLS)), None);

            let multiple = "alice:x:1000:1000:Alice:/home/alice:/bin/bash\n\
                            bob:x:1001:1001:Bob:/home/bob:/bin/zsh\n";
            assert_eq!(select_login_user(multiple, "", Some(SHELLS)), None);
            let listed = select_login_users(multiple, "", Some(SHELLS)).unwrap();
            assert_eq!(
                listed
                    .iter()
                    .map(|u| u.username.as_str())
                    .collect::<Vec<_>>(),
                ["alice", "bob"],
                "several accounts are listed to choose from, in database order"
            );
        }
        {
            let passwd = "service:x:1500:1500::/srv/service:/bin/custom\n\
                          person:x:2000:2000::/home/person:/bin/bash\n";
            assert_eq!(
                select_login_user(passwd, "UID_MIN 2000\nUID_MAX 2999\n", Some(SHELLS))
                    .map(|user| user.username),
                Some("person".into())
            );
        }
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
