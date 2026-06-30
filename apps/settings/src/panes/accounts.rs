//! External Accounts pane — the integration hub.
//!
//! This panel is the editor for the shared account stores
//! ([`crate::accounts`]) that Mail, Calendar, and the file apps consume.
//! It groups the user's accounts by kind (Email, Calendar, Files,
//! Nextcloud), shows each account's connection summary, and offers
//! add/edit/remove plus an "Add account" affordance per kind.
//!
//! ## The seam these panels edit
//!
//! Each kind maps to a concrete on-disk store:
//! - Email → `~/.config/qos/mail/accounts.json` ([`crate::accounts::MailStore`],
//!   the exact shape Mail reads).
//! - Calendar → `~/.config/qos/calendar/accounts.json`.
//! - Files → `~/.config/qos/files/accounts.json`.
//! - Nextcloud → `~/.config/qos/nextcloud/accounts.json`.
//!
//! ## Live vs. sample
//!
//! On launch the pane loads the real stores via
//! [`AccountDirectory::load`]. When every store is empty (a fresh install,
//! or `--shots` on a CI box) it renders
//! [`crate::accounts::sample_directory`] so the surface always reads as
//! designed. The Add/Edit/Remove buttons carry CRDT keys; wiring them to
//! mutate-and-persist the relevant store is the documented next step.

use crate::accounts::{AccountDirectory, AccountKind, AccountView, MailStore};
use crate::widgets::{card, card_with_action, caption, rows, setting_row, value_row, El};
use ui::widget::{Button, Column, Row, Spacer, Text, Widget};

/// Load the real directory; fall back to the populated sample when empty.
fn directory_for_display() -> AccountDirectory {
    let real = AccountDirectory::load();
    if real.is_empty() {
        crate::accounts::sample_directory()
    } else {
        real
    }
}

pub fn build() -> Box<dyn Widget> {
    build_with(&directory_for_display())
}

/// Build the pane from a caller-supplied directory — used by tests and the
/// deterministic `--shots` render.
pub fn build_with(dir: &AccountDirectory) -> Box<dyn Widget> {
    crate::widgets::cards(vec![
        card(
            "External accounts",
            rows(vec![
                Box::new(
                    Text::new(
                        "Accounts you add here are shared across the system. \
                         Mail reads your email accounts, Calendar reads your \
                         CalDAV calendars, and the file apps mount your WebDAV \
                         and Nextcloud shares.",
                    )
                    .with_size(12.0),
                ),
                caption(&format!(
                    "Email accounts: {}",
                    MailStore::path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "~/.config/qos/mail/accounts.json".into())
                )),
            ]),
        ),
        kind_card(dir, AccountKind::Mail),
        kind_card(dir, AccountKind::CalDav),
        kind_card(dir, AccountKind::WebDav),
        kind_card(dir, AccountKind::Nextcloud),
    ])
}

/// A card listing every account of one kind, with an "Add" button in the
/// header and per-account rows below.
fn kind_card(dir: &AccountDirectory, kind: AccountKind) -> El {
    let title = format!("{}  {}", kind.glyph(), kind.label());
    let add_id = format!("settings.accounts.add.{}", kind.slug());
    let add_key = format!("settings.accounts.add.{}", kind.slug());
    let add_btn: El = Box::new(Button::new(&add_id, "\u{f0415}  Add", Some(&add_key)));

    let views = dir.views_of(kind);
    let body: El = if views.is_empty() {
        rows(vec![caption(&format!(
            "No {} accounts yet. Use Add to connect one.",
            kind.label().to_lowercase()
        ))])
    } else {
        rows(views.iter().map(account_row).collect())
    };

    card_with_action(&title, add_btn, body)
}

/// One account: label + connection summary on the left, status + Edit +
/// Remove on the right.
fn account_row(view: &AccountView) -> El {
    let status = if view.enabled { "Connected" } else { "Disabled" };
    let status_color = if view.enabled {
        ui::Theme::dark().status_success
    } else {
        ui::Theme::dark().text_dim
    };

    let text_block = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(Text::new(&view.label).with_size(13.0)))
        .push(Box::new(
            Text::new(&view.summary)
                .with_size(11.0)
                .with_color(ui::Theme::dark().text_dim),
        ));

    let edit_id = format!("settings.accounts.edit.{}", view.id);
    let remove_id = format!("settings.accounts.remove.{}", view.id);
    let edit_key = format!("settings.accounts.edit.{}", view.id);
    let remove_key = format!("settings.accounts.remove.{}", view.id);

    let controls = Row::new()
        .push(Box::new(
            Text::new(status).with_size(11.0).with_color(status_color),
        ))
        .push(Box::new(Button::new(&edit_id, "Edit", Some(&edit_key))))
        .push(Box::new(Button::new(&remove_id, "\u{f01b4}", Some(&remove_key))));

    Box::new(
        Row::new()
            .push(Box::new(text_block))
            .push(Box::new(Spacer::new()))
            .push(Box::new(controls)),
    )
}

/// An add/edit form for an email account, rendered as a card of labelled
/// inputs. Exposed so a future Add flow (and tests) can build it directly;
/// the live flow writes through [`MailStore::save`]. Pre-filled inputs use
/// `TextInput::local` so the `--shots` render shows realistic values
/// without CRDT binding. Passwords use a secret field and are never
/// written to the JSON store (see [`crate::accounts`]).
pub fn mail_account_form(prefill_email: &str) -> El {
    use ui::widget::TextInput;
    let field = |label: &str, value: &str| -> El {
        let input = TextInput::local().with_initial_value(value).with_font_size(13.0);
        setting_row(label, Box::new(input))
    };
    card(
        "New email account",
        rows(vec![
            field("Display name", ""),
            field("Email address", prefill_email),
            value_row("IMAP server", "imap.example.com : 993 (TLS)"),
            value_row("SMTP server", "smtp.example.com : 465 (TLS)"),
            field("Username", prefill_email),
            {
                let pw = TextInput::local().with_font_size(13.0).with_secret(true);
                setting_row("Password", Box::new(pw))
            },
            caption(
                "Passwords are stored in the OS secret store, never in the \
                 shared accounts file.",
            ),
        ]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::sample_directory;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    fn count_named(w: &dyn Widget, name: &str) -> usize {
        let mut n = if w.name() == name { 1 } else { 0 };
        for c in w.children() {
            n += count_named(c, name);
        }
        n
    }

    #[test]
    fn renders_a_card_per_kind_and_builds_layout() {
        let dir = sample_directory();
        let mut pane = build_with(&dir);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        // 4 add buttons + (2 mail + 1 cal + 1 nextcloud) * 2 controls = 12.
        let buttons = count_named(pane.as_ref(), "Button");
        assert!(buttons >= 12, "expected >=12 buttons, got {buttons}");
    }

    #[test]
    fn empty_directory_still_renders_four_add_buttons() {
        let empty = AccountDirectory::default();
        let mut pane = build_with(&empty);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        let buttons = count_named(pane.as_ref(), "Button");
        assert_eq!(buttons, 4, "exactly the four Add buttons");
    }

    #[test]
    fn mail_form_builds() {
        let mut form: Box<dyn Widget> = Box::new(
            ui::widget::Column::new()
                .with_intrinsic_height()
                .push(mail_account_form("ada@example.com")),
        );
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = form.build_layout(&mut taffy, &theme, &mut node_map);
    }
}
