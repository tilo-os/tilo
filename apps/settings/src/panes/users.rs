//! Users pane — the accounts that exist on this device and their roles.
//!
//! User rows are sampled from `/etc/passwd` (regular login users only:
//! UID in the human range), and each user's administrator status is read
//! from `/etc/group` (membership of `wheel` or `sudo`) — so on a real
//! device the panel reflects the actual accounts and who can administer the
//! system. When those reads yield nothing useful (a minimal CI container
//! under `--shots`) a realistic sample is shown.
//!
//! Creating and editing accounts needs privileged `useradd`/`usermod` and a
//! credential flow that lives outside this app, so the panel is a truthful
//! read-only roster rather than offering buttons that can't act. The role a
//! user has is authored in Permissions & Roles.

use std::collections::HashSet;
use std::fs;

use ui::widget::{Column, Text, Widget};

use crate::widgets::{El, caption, card, rows};

/// A login account on this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceUser {
    pub username: String,
    pub full_name: String,
    /// Whether this account is an administrator (in `wheel`/`sudo`).
    pub admin: bool,
    /// Whether this is the currently logged-in user.
    pub current: bool,
}

/// Collect the usernames that are members of an admin group (`wheel` or
/// `sudo`) from `/etc/group` contents. Each line is
/// `name:passwd:gid:member,member,…`.
pub fn admins_from_group(contents: &str) -> HashSet<String> {
    let mut admins = HashSet::new();
    for line in contents.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 4 {
            continue;
        }
        if fields[0] != "wheel" && fields[0] != "sudo" {
            continue;
        }
        for member in fields[3].split(',') {
            let m = member.trim();
            if !m.is_empty() {
                admins.insert(m.to_string());
            }
        }
    }
    admins
}

/// Parse regular login users from `/etc/passwd` — UID 1000..60000 and a
/// real shell. `admins` marks which usernames are administrators. Returns
/// an empty vec on read failure.
fn read_passwd_users(
    contents: &str,
    current_user: Option<&str>,
    admins: &HashSet<String>,
) -> Vec<DeviceUser> {
    let mut out = Vec::new();
    for line in contents.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 7 {
            continue;
        }
        let name = fields[0];
        let uid: u32 = fields[2].parse().unwrap_or(0);
        let gecos = fields[4];
        let shell = fields[6];
        let is_login = (1000..60000).contains(&uid)
            && !shell.ends_with("nologin")
            && !shell.ends_with("false");
        if !is_login {
            continue;
        }
        let full_name = gecos.split(',').next().unwrap_or("").trim();
        out.push(DeviceUser {
            username: name.to_string(),
            full_name: if full_name.is_empty() {
                name.to_string()
            } else {
                full_name.to_string()
            },
            admin: admins.contains(name),
            current: current_user == Some(name),
        });
    }
    out
}

fn users_for_display() -> Vec<DeviceUser> {
    let current = std::env::var("USER").ok();
    let admins = fs::read_to_string("/etc/group")
        .map(|c| admins_from_group(&c))
        .unwrap_or_default();
    // Real login accounts from `/etc/passwd`. We deliberately do **not**
    // substitute an illustrative sample: a machine that parses no login users
    // shows an honest empty roster rather than fabricated people (the same
    // no-fabrication rule the Printers pane follows).
    fs::read_to_string("/etc/passwd")
        .map(|c| read_passwd_users(&c, current.as_deref(), &admins))
        .unwrap_or_default()
}

/// A realistic roster used only by tests / deterministic `--shots`, never as a
/// production fallback.
#[cfg(test)]
fn sample_users() -> Vec<DeviceUser> {
    vec![
        DeviceUser {
            username: "ada".into(),
            full_name: "Ada Lovelace".into(),
            admin: true,
            current: true,
        },
        DeviceUser {
            username: "charles".into(),
            full_name: "Charles Babbage".into(),
            admin: false,
            current: false,
        },
        DeviceUser {
            username: "guest".into(),
            full_name: "Guest".into(),
            admin: false,
            current: false,
        },
    ]
}

pub fn build() -> Box<dyn Widget> {
    build_with(&users_for_display())
}

pub fn build_with(users: &[DeviceUser]) -> Box<dyn Widget> {
    let list = if users.is_empty() {
        // Honest empty state — never fabricated people.
        card(
            "People",
            rows(vec![caption("No user accounts found on this device.")]),
        )
    } else {
        let mut user_rows: Vec<El> = Vec::new();
        for u in users {
            user_rows.push(user_row(u));
        }
        card("People", rows(user_rows))
    };

    let note = card(
        "Roles",
        rows(vec![caption(
            "A user's capabilities come from the roles assigned in \
             Permissions & Roles. Administrators can manage other accounts \
             and install system updates.",
        )]),
    );

    crate::widgets::cards(vec![list, note])
}

fn user_row(u: &DeviceUser) -> El {
    let role = if u.admin { "Administrator" } else { "Standard" };
    let suffix = if u.current { "  (you)" } else { "" };

    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(
                Text::new(&format!("{}{}", u.full_name, suffix)).with_size(13.0),
            ))
            .push(Box::new(
                Text::new(&format!("{} · {}", u.username, role))
                    .with_size(11.0)
                    .with_color(ui::Theme::dark().text_dim),
            )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    #[test]
    fn parses_login_users_and_skips_system_accounts() {
        let admins = HashSet::new();
        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                      bin:x:1:1:bin:/bin:/usr/sbin/nologin\n\
                      ada:x:1000:1000:Ada Lovelace,,,:/home/ada:/bin/bash\n\
                      svc:x:980:980:service:/var/svc:/usr/sbin/nologin\n";
        let users = read_passwd_users(passwd, Some("ada"), &admins);
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].username, "ada");
        assert_eq!(users[0].full_name, "Ada Lovelace");
        assert!(users[0].current);
    }

    #[test]
    fn admin_membership_is_read_from_group_file() {
        let group = "root:x:0:\n\
                     wheel:x:10:ada,charles\n\
                     sudo:x:27:ada\n\
                     users:x:100:\n";
        let admins = admins_from_group(group);
        assert!(admins.contains("ada"));
        assert!(admins.contains("charles"));
        assert!(!admins.contains("guest"));

        // The role column reflects real group membership, not a constant.
        let passwd = "ada:x:1000:1000:Ada,,,:/home/ada:/bin/bash\n\
                      guest:x:1001:1001:Guest,,,:/home/guest:/bin/bash\n";
        let users = read_passwd_users(passwd, Some("ada"), &admins);
        let ada = users.iter().find(|u| u.username == "ada").unwrap();
        let guest = users.iter().find(|u| u.username == "guest").unwrap();
        assert!(ada.admin, "ada is in wheel → Administrator");
        assert!(!guest.admin, "guest is in no admin group → Standard");
    }

    #[test]
    fn empty_passwd_yields_no_login_users() {
        let admins = HashSet::new();
        let users = read_passwd_users("", None, &admins);
        assert!(
            users.is_empty(),
            "no login users parse from an empty passwd"
        );
        // The sample roster exists only for tests / --shots, never as a
        // production fallback.
        assert!(!sample_users().is_empty());
    }

    /// The production roster is the real `/etc/passwd` list (or empty) — it
    /// must never fall back to the illustrative sample. Mirrors the Printers
    /// pane's `production_list_never_fabricates_the_sample` guard.
    #[test]
    fn production_roster_never_fabricates_the_sample() {
        let real = users_for_display();
        assert!(
            !real
                .iter()
                .any(|u| u.full_name == "Ada Lovelace" || u.username == "charles"),
            "the sample must not leak into the production roster: {real:?}"
        );
    }

    /// With no login users, the pane renders an honest empty state rather than
    /// fabricated people.
    #[test]
    fn empty_roster_builds_honest_state() {
        let mut pane = build_with(&[]);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    #[test]
    fn builds_layout_from_sample() {
        let mut pane = build_with(&sample_users());
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }
}
