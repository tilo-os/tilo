use super::*;
use crate::shell::portal_dialog::{
    AuthenticationDecision, AuthenticationRequest, NetworkSecretsDecision, NetworkSecretsRequest,
    PortalDialogQueue, ReviewDecision, ReviewRequest, SecretFieldRule, SecretFieldSpec,
    UsbDeviceRequest, UsbDeviceRow,
};

const SCREEN: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

fn node<'a>(dialog: &'a AccessNode, name: &str) -> &'a AccessNode {
    dialog
        .children
        .iter()
        .find(|node| node.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no node named {name:?} in {dialog:#?}"))
}

fn target_of(dialog: &AccessNode, name: &str) -> String {
    node(dialog, name)
        .action_target
        .clone()
        .unwrap_or_else(|| panic!("{name:?} is not operable"))
}

fn authentication(echo: bool) -> AuthenticationRequest {
    AuthenticationRequest {
        handle: "polkit:cookie".into(),
        message: "Authentication is required to install software".into(),
        action_id: "org.freedesktop.Flatpak.app-install".into(),
        identities: vec!["quinn".into()],
        identity: 0,
        prompt: "Password:".into(),
        echo,
        notice: Some("Wrong password".into()),
    }
}

fn masked(key: &str, label: &str) -> SecretFieldSpec {
    SecretFieldSpec {
        key: key.into(),
        label: label.into(),
        masked: true,
        required: true,
        initial: String::new(),
        rule: SecretFieldRule::Any,
    }
}

#[test]
fn portal_sheets_publish_their_parts_and_answer_through_accessibility_actions() {
    fn the_authentication_sheet_is_a_dialog_with_a_password_field() {
        let queue = PortalDialogQueue::default();
        let receiver = queue
            .enqueue_authentication(authentication(false))
            .expect("queued");
        let mut sheet = PortalDialogController::new(queue);
        assert!(sheet.pump());
        let dialog = sheet.accessibility(SCREEN).expect("a sheet is up");
        assert_eq!(dialog.role, Role::Dialog);
        assert!(dialog.state.modal);
        assert_eq!(
            dialog.name.as_deref(),
            Some("Authentication is required to install software")
        );
        let field = node(&dialog, "Password");
        assert_eq!(field.role, Role::TextInput);
        assert!(field.state.protected && field.state.focused);
        assert_eq!(field.value, None);
        assert!(field.bounds.is_some(), "the field sits where it is painted");
        assert_eq!(
            node(&dialog, "Wrong password").live,
            Some(Politeness::Assertive),
            "the failed attempt is announced"
        );

        let secret = target_of(&dialog, "Password");
        assert!(sheet.perform_accessibility_action(&secret, &Action::SetValue("hunter2".into())));
        let dialog = sheet.accessibility(SCREEN).expect("still up");
        assert_eq!(
            node(&dialog, "Password").value,
            None,
            "what was typed is never published"
        );
        assert_eq!(sheet.state_json()["secret_len"], 7);
        let authenticate = target_of(&dialog, "Authenticate");
        assert!(sheet.perform_accessibility_action(&authenticate, &Action::Activate));
        assert_eq!(
            receiver.blocking_recv(),
            Ok(AuthenticationDecision::Answered {
                identity: 0,
                secret: "hunter2".into(),
            })
        );
        assert!(sheet.accessibility(SCREEN).is_none(), "the sheet is gone");
    }

    fn an_echoing_prompt_publishes_what_is_typed() {
        let queue = PortalDialogQueue::default();
        let _receiver = queue
            .enqueue_authentication(authentication(true))
            .expect("queued");
        let mut sheet = PortalDialogController::new(queue);
        assert!(sheet.pump());
        let dialog = sheet.accessibility(SCREEN).expect("up");
        let target = target_of(&dialog, "Password");
        assert!(sheet.perform_accessibility_action(&target, &Action::SetValue("quinn".into())));
        let dialog = sheet.accessibility(SCREEN).expect("up");
        let field = node(&dialog, "Password");
        assert!(!field.state.protected);
        assert_eq!(field.value.as_deref(), Some("quinn"));
    }

    fn a_target_from_an_answered_sheet_never_answers_the_next() {
        let queue = PortalDialogQueue::default();
        let first = queue
            .enqueue_authentication(authentication(false))
            .expect("queued");
        let mut sheet = PortalDialogController::new(queue.clone());
        assert!(sheet.pump());
        let dialog = sheet.accessibility(SCREEN).expect("up");
        let stale_confirm = target_of(&dialog, "Authenticate");
        let cancel = target_of(&dialog, "Cancel");
        assert!(sheet.perform_accessibility_action(&cancel, &Action::Activate));
        assert_eq!(first.blocking_recv(), Ok(AuthenticationDecision::Cancelled));

        let second = queue
            .enqueue_review(ReviewRequest {
                handle: "/r/next".into(),
                app_id: "org.example.App".into(),
                title: "Allow background activity?".into(),
                reason: "Sync".into(),
                resource: "background".into(),
                confirm_label: "Allow".into(),
            })
            .expect("queued");
        assert!(sheet.pump());
        assert!(
            !sheet.perform_accessibility_action(&stale_confirm, &Action::Activate),
            "a confirm aimed at the answered sheet does nothing"
        );
        let dialog = sheet.accessibility(SCREEN).expect("the review is up");
        assert!(
            node(&dialog, "Allow").state.focused,
            "with nothing to choose, the answer has the keyboard"
        );
        assert!(
            sheet.perform_accessibility_action(&target_of(&dialog, "Allow"), &Action::Activate)
        );
        assert_eq!(second.blocking_recv(), Ok(ReviewDecision::Confirmed));
    }

    fn network_secrets_publish_each_field_and_remember() {
        let queue = PortalDialogQueue::default();
        let receiver = queue
            .enqueue_network_secrets(NetworkSecretsRequest {
                handle: "/nm/1".into(),
                title: "Network sign-in".into(),
                detail: "“eduroam” needs your user name and password.".into(),
                fields: vec![
                    SecretFieldSpec {
                        masked: false,
                        initial: "quinn".into(),
                        ..masked("identity", "User name")
                    },
                    masked("password", "Password"),
                ],
                remember: Some(false),
                sim: None,
            })
            .expect("queued");
        let mut sheet = PortalDialogController::new(queue);
        assert!(sheet.pump());
        let dialog = sheet.accessibility(SCREEN).expect("up");
        let user = node(&dialog, "User name");
        assert_eq!(user.value.as_deref(), Some("quinn"));
        assert!(!user.state.protected && user.state.required);
        let password = node(&dialog, "Password");
        assert!(password.state.protected && password.state.focused);
        assert_eq!(password.value, None);
        let remember = node(&dialog, "Remember on this device");
        assert_eq!(remember.role, Role::Checkbox);
        assert_eq!(remember.state.checked, Some(false));

        assert!(sheet.perform_accessibility_action(
            &target_of(&dialog, "Password"),
            &Action::SetValue("s3cret".into())
        ));
        assert!(sheet.perform_accessibility_action(
            &target_of(&dialog, "Remember on this device"),
            &Action::Activate
        ));
        let dialog = sheet.accessibility(SCREEN).expect("up");
        assert_eq!(
            node(&dialog, "Remember on this device").state.checked,
            Some(true)
        );
        assert!(
            sheet.perform_accessibility_action(&target_of(&dialog, "Connect"), &Action::Activate)
        );
        match receiver.blocking_recv() {
            Ok(NetworkSecretsDecision::Answered { values, remember }) => {
                assert!(remember);
                assert_eq!(
                    values
                        .iter()
                        .map(|value| value.as_str())
                        .collect::<Vec<_>>(),
                    ["quinn", "s3cret"]
                );
            }
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    fn a_refused_entry_is_announced_and_marked_invalid() {
        let queue = PortalDialogQueue::default();
        let _receiver = queue
            .enqueue_network_secrets(NetworkSecretsRequest {
                handle: "/nm/2".into(),
                title: "Wi-Fi password required".into(),
                detail: "Enter the password for “Home”.".into(),
                fields: vec![SecretFieldSpec {
                    rule: SecretFieldRule::WpaPassphrase,
                    ..masked("psk", "Password")
                }],
                remember: None,
                sim: None,
            })
            .expect("queued");
        let mut sheet = PortalDialogController::new(queue);
        assert!(sheet.pump());
        let dialog = sheet.accessibility(SCREEN).expect("up");
        assert!(sheet.perform_accessibility_action(
            &target_of(&dialog, "Password"),
            &Action::SetValue("short".into())
        ));
        assert!(
            !sheet.perform_accessibility_action(&target_of(&dialog, "Connect"), &Action::Activate)
        );
        let dialog = sheet.accessibility(SCREEN).expect("still up");
        assert!(node(&dialog, "Password").state.invalid);
        let error = dialog
            .children
            .iter()
            .find(|node| node.live == Some(Politeness::Assertive))
            .expect("the refusal is announced");
        assert!(
            error
                .name
                .as_deref()
                .is_some_and(|name| name.contains("8 to 63"))
        );
    }

    fn usb_devices_are_checkboxes_toggled_one_at_a_time() {
        let queue = PortalDialogQueue::default();
        let receiver = queue
            .enqueue_usb_devices(UsbDeviceRequest {
                review: ReviewRequest {
                    handle: "/usb/1".into(),
                    app_id: "org.example.Flasher".into(),
                    title: "Use USB devices?".into(),
                    reason: String::new(),
                    resource: "usb".into(),
                    confirm_label: "Allow".into(),
                },
                devices: vec![
                    UsbDeviceRow {
                        id: "a".into(),
                        label: "Keyboard".into(),
                        writable: false,
                    },
                    UsbDeviceRow {
                        id: "b".into(),
                        label: "Board".into(),
                        writable: true,
                    },
                ],
            })
            .expect("queued");
        let mut sheet = PortalDialogController::new(queue);
        assert!(sheet.pump());
        let dialog = sheet.accessibility(SCREEN).expect("up");
        let boxes: Vec<_> = dialog
            .children
            .iter()
            .filter(|node| node.role == Role::Checkbox)
            .collect();
        assert_eq!(boxes.len(), 2);
        assert!(boxes.iter().all(|node| node.state.checked == Some(true)));
        let board = boxes[1].action_target.clone().expect("operable");
        assert!(sheet.perform_accessibility_action(&board, &Action::Activate));
        let dialog = sheet.accessibility(SCREEN).expect("up");
        let checked: Vec<_> = dialog
            .children
            .iter()
            .filter(|node| node.role == Role::Checkbox)
            .map(|node| node.state.checked)
            .collect();
        assert_eq!(checked, [Some(true), Some(false)]);
        assert!(
            sheet.perform_accessibility_action(&target_of(&dialog, "Allow"), &Action::Activate)
        );
        let decision = receiver.blocking_recv().expect("answered");
        assert_eq!(decision.approved, ["a".to_string()]);
    }

    the_authentication_sheet_is_a_dialog_with_a_password_field();
    an_echoing_prompt_publishes_what_is_typed();
    a_target_from_an_answered_sheet_never_answers_the_next();
    network_secrets_publish_each_field_and_remember();
    a_refused_entry_is_announced_and_marked_invalid();
    usb_devices_are_checkboxes_toggled_one_at_a_time();
}

#[test]
fn the_drive_sheet_offers_its_remember_row_as_a_checkbox() {
    use crate::shell::portal_dialog::{DriveConsentDecision, DriveConsentRequest};
    let queue = PortalDialogQueue::default();
    let answer = queue
        .enqueue_drive_consent(DriveConsentRequest {
            handle: "tilo-drive-1-sdb1:4".into(),
            title: "Open Stick?".into(),
            drive: "External drive · 64 GB".into(),
            detail: "Choose how to open this drive.".into(),
            choices: vec!["Open safely".into(), "Open".into()],
            preselect: 0,
            remember: Some("Remember this drive and suggest opening it safely".into()),
            confirm_label: "Open drive".into(),
            cancel_label: "Not now".into(),
        })
        .expect("queued");
    let mut sheet = PortalDialogController::new(queue);
    assert!(sheet.pump());
    let dialog = sheet.accessibility(SCREEN).expect("a sheet is up");
    assert_eq!(dialog.role, Role::Dialog);
    let remember = node(&dialog, "Remember this drive and suggest opening it safely");
    assert_eq!(remember.role, Role::Checkbox);
    assert_eq!(remember.state.checked, Some(false));
    let target = target_of(&dialog, "Remember this drive and suggest opening it safely");
    assert!(sheet.perform_accessibility_action(&target, &Action::ToggleCheck));
    // The two ways to open are the sheet's two buttons; each answers.
    assert_eq!(node(&dialog, "Open safely").role, Role::Button);
    assert_eq!(node(&dialog, "Open").role, Role::Button);
    let open_safely = target_of(&dialog, "Open safely");
    assert!(sheet.perform_accessibility_action(&open_safely, &Action::Activate));
    assert_eq!(
        answer.blocking_recv(),
        Ok(DriveConsentDecision::Chosen {
            choice: 0,
            remember: true
        })
    );
}

#[test]
fn the_drive_sheet_answers_with_two_buttons_and_no_choice_rows() {
    use crate::shell::portal_dialog::{DriveConsentDecision, DriveConsentRequest};
    let queue = PortalDialogQueue::default();
    let answer = queue
        .enqueue_drive_consent(DriveConsentRequest {
            handle: "tilo-drive-1-nvme0n1p3:1".into(),
            title: "Open Windows?".into(),
            drive: "Inside this computer, 475 GB".into(),
            detail: "Open it at full speed only for your own drives.".into(),
            choices: vec!["Open safely".into(), "Open at full speed".into()],
            preselect: 1,
            remember: None,
            confirm_label: "Open".into(),
            cancel_label: "Not now".into(),
        })
        .expect("queued");
    let mut sheet = PortalDialogController::new(queue);
    assert!(sheet.pump());
    let layout = sheet.layout(SCREEN).expect("the sheet fits");
    assert!(
        layout.choices.is_empty(),
        "no choice rows above the buttons"
    );
    assert_eq!(layout.answers.len(), 2, "one button per way to open");
    assert_eq!(
        layout.confirm, layout.answers[1],
        "the suggested way is the primary button"
    );
    assert!(
        layout.answers[0].x1 < layout.answers[1].x0,
        "the secondary sits beside the primary, neither over the other"
    );
    assert!(
        layout.answers[0].width() >= 200.0,
        "each button has room for its whole label"
    );
    // "Not now" is the sheet's close button, still one tap away.
    let dialog = sheet.accessibility(SCREEN).expect("a sheet is up");
    assert_eq!(node(&dialog, "Not now").role, Role::Button);
    assert!(sheet.click(layout.answers[0].center(), &layout));
    assert_eq!(
        answer.blocking_recv(),
        Ok(DriveConsentDecision::Chosen {
            choice: 0,
            remember: false
        })
    );
}
