use super::*;

#[test]
fn attachment_save_offers_only_the_downloaded_bytes_and_reports_truthfully() {
    let (mut app, _doc) = app();
    let (message_id, index, filename, bytes) = {
        let state = app.shared.borrow();
        let (message_id, (index, attachment)) = state
            .mailbox
            .messages
            .values()
            .find_map(|message| {
                message
                    .attachments
                    .iter()
                    .enumerate()
                    .find(|(_, attachment)| attachment.is_downloadable())
                    .map(|found| (message.id, found))
            })
            .expect("sample has a hydrated attachment");
        (
            message_id,
            index,
            attachment.filename.clone(),
            attachment.content.clone(),
        )
    };

    // What the host is asked to save is exactly the downloaded bytes,
    // under a name that cannot reach outside the chosen folder.
    let (name, offered) = app
        .attachment_to_save(&message_id.to_string(), index)
        .expect("a downloaded attachment can be saved");
    assert_eq!(offered, bytes);
    assert_eq!(name, MailApp::attachment_save_name(&filename));
    assert!(
        app.attachment_to_save(&message_id.to_string(), index + 100)
            .is_err(),
        "an attachment that is not loaded cannot be saved"
    );
    assert!(
        app.start_effect(
            tilo_actions::InvocationId::new(),
            &crate::actions::effect_payload(&crate::actions::MailEffect::Save {
                message: message_id.to_string(),
                index,
            }),
        )
        .is_err(),
        "only the surface, with the host's chooser, saves an attachment"
    );

    // A save is reported only once the host wrote it.
    let resource = format!("mail.attachment:{message_id}:{index}");
    let saved = app.finish_attachment_save(true, &name, resource.clone());
    assert!(matches!(
        saved,
        ui::framework::Effected::Done(output) if output.resources == vec![resource.clone()]
    ));
    assert_eq!(
        app.shared.borrow().status.as_deref(),
        Some(format!("Saved a copy as {name}").as_str())
    );
    let cancelled = app.finish_attachment_save(false, &name, resource);
    assert!(matches!(
        cancelled,
        ui::framework::Effected::Done(output) if output.resources.is_empty()
    ));
    assert_eq!(
        app.shared.borrow().status.as_deref(),
        Some("Attachment save cancelled")
    );

    assert_eq!(
        MailApp::attachment_save_name("../../secret.pdf"),
        "secret.pdf"
    );
    assert_eq!(MailApp::attachment_save_name(".."), "attachment");
    assert_eq!(MailApp::attachment_save_name("\0bad"), "bad");
}
