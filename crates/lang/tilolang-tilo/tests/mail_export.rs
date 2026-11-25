//! Mail publishes each format a message can be written in as an export action on the message:
//! Markdown, JSON and `.eml`. The system's one Export… lists them; Mail keeps no export menu of
//! its own.

#[path = "support/export_probe.rs"]
mod export_probe;

use tilo_render_protocol::patch::{self as wire, ToApp};
use tilolang_runtime::Value;
use tilolang_runtime::value::ResourceRef;
use tilolang_tilo::instance::Instance;

use export_probe::{call, row_export_formats, tell, window};

const MAIL_APP: &str = "so.tilo.mail";

/// The conversation `mail` keeps `message` in: the thread whose messages hold it.
fn holding(instance: &Instance, mail: &ResourceRef, message: &ResourceRef) -> ResourceRef {
    let host = instance.host().expect("the host");
    let runtime = host.runtime();
    let Value::List(threads) = runtime
        .read(mail, "recovered_mail")
        .expect("the mail's threads")
    else {
        panic!("the mail's threads are a collection");
    };
    for held in threads {
        let Value::Ref(thread) = held else { continue };
        let Value::List(messages) = runtime
            .read(&thread.target, "messages")
            .expect("a thread's messages")
        else {
            continue;
        };
        if messages
            .iter()
            .any(|m| matches!(m, Value::Ref(r) if r.target.id_string() == message.id_string()))
        {
            return (*thread.target).clone();
        }
    }
    panic!("no thread holds {}", message.id_string());
}

#[test]
fn a_messages_menu_lists_the_formats_it_exports() {
    static PACKAGES: &[(&str, &str)] = &[crate::fixtures::CONTACTS, crate::fixtures::MAIL];
    let (mut instance, mut far, device) = window(MAIL_APP, PACKAGES);
    let host = instance.host().expect("the host");
    let mail = host.root_of(MAIL_APP).expect("the mail root");
    let message = match call(
        &instance,
        MAIL_APP,
        "ada",
        "deliver_message",
        vec![
            Value::resource(&mail),
            Value::text("Grace Hopper <grace@rivera.example>"),
            Value::text("Launch plan"),
            Value::text("Shipping at four."),
        ],
    ) {
        Value::Ref(made) => (*made.target).clone(),
        other => panic!("a message: {other:?}"),
    };
    instance.update();
    // The conversation the message stands in, as a person opens it from the mailbox.
    let thread = holding(&instance, &mail, &message);
    tell(
        &mut instance,
        &mut far,
        ToApp::Open {
            resource: wire::ResourceRef {
                id: thread.id_string(),
                ty: thread.ty.clone(),
            },
        },
    );

    // The message's own menu, as the conversation shows it, lists the three formats Mail
    // publishes for a message, all under the system's one Export….
    assert_eq!(
        row_export_formats(&mut instance, &mut far, &message),
        ["Markdown", "JSON", ".eml"],
        "the message's menu lists the formats Mail publishes for it"
    );
    assert!(
        device.saved.borrow().is_empty(),
        "listing the formats saves nothing"
    );
}
