//! Sheets the compositor raises for a request a `--shots` render has no
//! source of: a drive to open, an agent's ask to confirm. Each is queued as
//! its source builds it, on the portal dialog queue the compositor pumps.

use super::super::driver::ShellDriver;

/// `Action::SeedDraftConfirmation`: the sheet an agent's ask raises.
pub(super) fn seed_draft_confirmation(driver: &mut ShellDriver, preset: &str) {
    use tilo_actions::{Draft, DraftField, DraftLayout, DraftValue};
    let field = |name: &str, label: &str, value: DraftValue, editable: bool| DraftField {
        path: vec![name.into()],
        label: label.into(),
        value,
        editable,
        multiline: false,
        role: None,
    };
    let text = |text: &str| DraftValue::Text { text: text.into() };
    let mut body = field(
        "body",
        "Body",
        text(
            "Hi Ada,\n\nThe release notes for 2.4 are ready for your review: the new sync engine, \
              the faster search and the fixes from the beta. Could you look before Friday?\n\nThanks",
        ),
        true,
    );
    body.multiline = true;
    let draft = Draft {
        action_label: "Send message".into(),
        resource_label: "Mail".into(),
        description: None,
        layout: DraftLayout::Message,
        fields: vec![
            field("to", "To", text("ada@example.org"), true),
            field("subject", "Subject", text("Release notes for 2.4"), true),
            body,
            field(
                "remind_after_days",
                "Remind me after (days)",
                DraftValue::Number { text: "3".into() },
                true,
            ),
            field(
                "urgent",
                "Mark as urgent",
                DraftValue::Flag { on: false },
                true,
            ),
            field(
                "attachments",
                "Attachments",
                DraftValue::List {
                    count: 2,
                    labels: vec!["release-notes.pdf".into(), "changelog.md".into()],
                },
                false,
            ),
        ],
        undoable: false,
    };
    let handle = format!("confirm:shots-{preset}");
    let request = shell::shell::app_action_consent::AppActionConsentRequest {
        handle: handle.clone(),
        room: uuid::Uuid::nil().to_string(),
        person: "person:ada".into(),
        app_id: "Scribe".into(),
        target_app: "so.tilo.mail".into(),
        resource: "mail.account".into(),
        action: "send".into(),
        resource_label: draft.resource_label.clone(),
        action_label: draft.action_label.clone(),
        risk: tilo_actions::RiskLevel::Sensitive,
        needs_confirmation: true,
        confirming: true,
        moderation: None,
        preview: None,
        scope: Some("Prepare release notes".into()),
        draft: Some(draft),
    };
    let queue = shell::shell::portal_dialog::PortalDialogQueue::default();
    driver.host_mut().set_portal_dialog_queue(queue.clone());
    // Nobody answers a screenshot; the dropped receiver reads as Deny.
    let _ = queue.enqueue_action_grant(request);
    driver.host_mut().pump_portal_dialogs();
    let act = |driver: &mut ShellDriver, part: &str, action: ui::a11y::Action| {
        let target = format!("portal-sheet:{part}#{handle}");
        driver
            .host_mut()
            .perform_portal_dialog_action(&target, &action);
    };
    match preset {
        "edited" => {
            act(
                driver,
                "draft/1",
                ui::a11y::Action::SetValue("Release notes for 2.4 (final)".into()),
            );
            act(driver, "draft/4", ui::a11y::Action::ToggleCheck);
        }
        "refused" => {
            act(driver, "draft/3", ui::a11y::Action::SetValue("soon".into()));
            act(driver, "confirm", ui::a11y::Action::Activate);
        }
        _ => {}
    }
}

/// `Action::SeedDriveConsent`: the sheet the drive worker raises.
#[cfg(target_os = "linux")]
pub(super) fn seed_drive_consent(driver: &mut ShellDriver, preset: &str) {
    use ntfs_mount::protocol::{Drive, Mode, Placement};
    let windows = preset == "windows";
    let drive = Drive {
        id: if windows { "nvme0n1p3:1" } else { "sdb1:7" }.into(),
        label: if windows { "Windows" } else { "" }.into(),
        size_bytes: if windows {
            475_000_000_000
        } else {
            64_000_000_000
        },
        placement: if windows {
            Placement::Internal
        } else {
            Placement::External
        },
        hot_added: !windows,
        remembered: None,
        needs_consent: true,
        preselect: if windows {
            Mode::Open
        } else {
            Mode::OpenSafely
        },
        can_remember: if windows {
            ntfs_mount::protocol::RememberOffer::AnyMode
        } else {
            ntfs_mount::protocol::RememberOffer::OpenSafely
        },
        mounted: None,
    };
    let queue = shell::shell::portal_dialog::PortalDialogQueue::default();
    driver.host_mut().set_portal_dialog_queue(queue.clone());
    let request = shell::shell::drives::consent_request(&drive, format!("shots-{preset}"));
    // Nobody answers a screenshot; the dropped receiver reads as "Not now".
    let _ = queue.enqueue_drive_consent(request);
    driver.host_mut().pump_portal_dialogs();
}
