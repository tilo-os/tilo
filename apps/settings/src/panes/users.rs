//! Users pane — the accounts that exist on this device and their roles.
//!
//! User rows are sampled from `/etc/passwd` (regular login users only:
//! UID in the human range), so on a real device the panel reflects the
//! actual accounts. When that read yields nothing useful (minimal CI
//! containers under `--shots`) a realistic sample is shown.
//!
//! The role shown per user ties into the OS permission model — the
//! Permissions & Roles pane is where roles are authored; here each user's
//! current role is summarised with a shortcut into that editor.

use std::fs;

use ui::widget::{Button, Column, Row, Spacer, Text, Widget};

use crate::widgets::{card, card_with_action, caption, rows, El};

pub const ADD_USER_KEY: &str = "users.add";

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

/// Parse regular login users from `/etc/passwd` — UID 1000..60000 and a
/// real shell. Returns an empty vec on read failure.
fn read_passwd_users(contents: &str, current_user: Option<&str>) -> Vec<DeviceUser> {
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
            admin: false, // group membership isn't in passwd; left false here.
            current: current_user == Some(name),
        });
    }
    out
}

fn users_for_display() -> Vec<DeviceUser> {
    let current = std::env::var("USER").ok();
    let parsed = fs::read_to_string("/etc/passwd")
        .map(|c| read_passwd_users(&c, current.as_deref()))
        .unwrap_or_default();
    if parsed.is_empty() {
        sample_users()
    } else {
        parsed
    }
}

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
    let add_btn: El = Box::new(Button::new(
        "settings.users.add",
        "\u{f0415}  Add user",
        Some(ADD_USER_KEY),
    ));

    let mut user_rows: Vec<El> = Vec::new();
    for u in users {
        user_rows.push(user_row(u));
    }

    let list = card_with_action("People", add_btn, rows(user_rows));

    let note = card(
        "Roles",
        rows(vec![caption(
            "A user's capabilities come from the roles assigned in \
             Permissions & Roles. Administrators can manage other accounts \
             and install system updates.",
        )]),
    );

    Box::new(Column::new().with_intrinsic_height().push(list).push(note))
}

fn user_row(u: &DeviceUser) -> El {
    let role = if u.admin { "Administrator" } else { "Standard" };
    let suffix = if u.current { "  (you)" } else { "" };

    let block = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(
            Text::new(&format!("{}{}", u.full_name, suffix)).with_size(13.0),
        ))
        .push(Box::new(
            Text::new(&format!("{} · {}", u.username, role))
                .with_size(11.0)
                .with_color(ui::Theme::dark().text_dim),
        ));

    let edit_id = format!("settings.users.edit.{}", u.username);
    let edit_key = format!("settings.users.edit.{}", u.username);

    Box::new(
        Row::new()
            .push(Box::new(block))
            .push(Box::new(Spacer::new()))
            .push(Box::new(Button::new(&edit_id, "Manage", Some(&edit_key)))),
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
        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                      bin:x:1:1:bin:/bin:/usr/sbin/nologin\n\
                      ada:x:1000:1000:Ada Lovelace,,,:/home/ada:/bin/bash\n\
                      svc:x:980:980:service:/var/svc:/usr/sbin/nologin\n";
        let users = read_passwd_users(passwd, Some("ada"));
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].username, "ada");
        assert_eq!(users[0].full_name, "Ada Lovelace");
        assert!(users[0].current);
    }

    #[test]
    fn empty_passwd_falls_back_to_sample() {
        let users = read_passwd_users("", None);
        assert!(users.is_empty());
        // The display path substitutes the sample.
        assert!(!sample_users().is_empty());
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
