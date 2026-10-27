//! Extensions' notifications (`chrome.notifications`), shown by Tilo's
//! notification service.
//!
//! The engine keeps what each extension shows and tells the window the
//! person used last of every change (`WebViewEvent::ExtensionNotification`);
//! this shows it as the Browser's notification, named for the extension,
//! with its buttons. Choosing it, or one of its buttons, goes back to the
//! engine (`WebViewCommand::ExtensionNotificationChosen`), which tells the
//! extension. Its tag is the same in every Browser process, so whichever
//! window is told of a change replaces or takes away what another showed.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, Weak};

use ui::notify::{Notification, Notifier};
use ui::webview::{ExtensionNotice, SharedEngine, WebViewCommand, WebViewEngine};

/// The scheme of a notification's activation: the Browser's choices alone.
pub const SCHEME: &str = "tilo-browser-extension";

/// The tag the notification `notification` of the extension `extension` is
/// shown under: 15 characters, the same in every process.
pub fn tag(extension: &str, notification: &str) -> String {
    // FNV-1a over both, apart: stable across processes and builds.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in extension.bytes().chain([0]).chain(notification.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("x{:014x}", hash >> 8)
}

/// What choosing the notification, or its button at `button`, hands back.
pub fn activation(extension: &str, notification: &str, button: Option<u8>) -> String {
    let part = button.map_or_else(|| "body".to_owned(), |button| button.to_string());
    let mut encoded = String::with_capacity(notification.len());
    for byte in notification.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            // Writing to a String cannot fail.
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    format!("{SCHEME}:notice/{extension}/{part}/{encoded}")
}

/// What the person chose: the extension, its notification, and the button,
/// if a button.
pub type Choice = (String, String, Option<u8>);

/// The extension, notification and button an activation names.
pub fn chosen(uri: &str) -> Option<Choice> {
    let rest = uri.strip_prefix(SCHEME)?.strip_prefix(":notice/")?;
    let (extension, rest) = rest.split_once('/')?;
    let (part, encoded) = rest.split_once('/')?;
    if extension.len() != 32 || !extension.bytes().all(|byte| (b'a'..=b'p').contains(&byte)) {
        return None;
    }
    let button = match part {
        "body" => None,
        index => Some(index.parse::<u8>().ok().filter(|index| *index < 2)?),
    };
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut input = encoded.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = char::from(input.next()?).to_digit(16)?;
            let low = char::from(input.next()?).to_digit(16)?;
            bytes.push(u8::try_from(high * 16 + low).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    Some((extension.to_owned(), String::from_utf8(bytes).ok()?, button))
}

/// The notification the person sees: the extension's title, its message,
/// items, context and how far along it is, then which extension it is from.
pub fn notification(
    name: &str,
    extension: &str,
    id: &str,
    notice: &ExtensionNotice,
) -> Notification {
    let mut lines: Vec<String> = Vec::new();
    lines.push(notice.message.clone());
    for (title, message) in &notice.items {
        lines.push(format!("{title} {message}").trim().to_owned());
    }
    lines.push(notice.context.clone());
    if let Some(percent) = notice.progress {
        lines.push(crate::CATALOG.t_args(
            "browser-extension-notification-progress",
            &[("percent", ui::i18n::ArgValue::Number(f64::from(percent)))],
        ));
    }
    lines.push(crate::CATALOG.t_args(
        "browser-extension-notification-from",
        &[("name", ui::i18n::ArgValue::Text(name.to_owned()))],
    ));
    lines.retain(|line| !line.trim().is_empty());
    let title = if notice.title.trim().is_empty() {
        name
    } else {
        &notice.title
    };
    let mut shown = Notification::new(tag(extension, id), title, lines.join("\n"))
        .with_activation(activation(extension, id, None));
    if notice.silent {
        shown = shown.quietly();
    }
    for (index, label) in notice.buttons.iter().enumerate() {
        // A button's name is one line: a control character in it (a line
        // break, a tab) would have the whole notification refused.
        let label = label
            .split(char::is_control)
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if label.trim().is_empty() {
            continue;
        }
        if let Ok(index) = u8::try_from(index) {
            shown = shown.with_button(label, activation(extension, id, Some(index)));
        }
    }
    shown
}

/// The engine a choice goes back to: the one that told of a notification
/// last, while its window lives. Any of the profile's engines reaches the
/// extensions.
fn engine() -> &'static Mutex<Option<Weak<dyn WebViewEngine>>> {
    static ENGINE: Mutex<Option<Weak<dyn WebViewEngine>>> = Mutex::new(None);
    &ENGINE
}

/// Hand the engine the person's choice of a notification carrying `uri`.
fn choose(uri: &str) {
    let Some((id, notification, button)) = chosen(uri) else {
        return;
    };
    let live = engine()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(Weak::upgrade);
    if let Some(engine) = live {
        engine.send(WebViewCommand::ExtensionNotificationChosen {
            id,
            notification,
            button,
        });
    }
}

/// Show, change or take away the extension's notification, as the engine
/// told `from` of it. `name` is the extension's, as installed.
pub fn announce(
    notifier: &dyn Notifier,
    from: &SharedEngine,
    name: &str,
    extension: &str,
    (id, notice): (&str, Option<&ExtensionNotice>),
) {
    static HANDLED: std::sync::Once = std::sync::Once::new();
    *engine()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::downgrade(from));
    HANDLED.call_once(|| ui::notify::set_activation_handler(SCHEME, choose));
    let outcome = match notice {
        Some(notice) => notifier.show(&notification(name, extension, id, notice)),
        None => notifier.withdraw(&tag(extension, id)),
    };
    if let Err(error) = outcome {
        tracing::debug!(%error, extension, "an extension's notification was not shown");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::notify::conformance::Recording;

    const ID: &str = "abcdefghijklmnopabcdefghijklmnop";

    #[test]
    fn a_choice_names_its_extension_notification_and_button() {
        for (notification, button) in [("copy", None), ("a b/c%d?é", Some(1)), ("", Some(0))] {
            let uri = activation(ID, notification, button);
            assert!(ui::notify::valid_activation(&uri), "{uri}");
            assert_eq!(
                chosen(&uri),
                Some((ID.to_owned(), notification.to_owned(), button))
            );
        }
        for bad in [
            format!("{SCHEME}:notice/short/body/x"),
            format!("{SCHEME}:notice/{ID}/2/x"),
            format!("{SCHEME}:notice/{ID}/body/%zz"),
            format!("other:notice/{ID}/body/x"),
        ] {
            assert_eq!(chosen(&bad), None, "{bad}");
        }
    }

    #[test]
    fn a_tag_is_short_stable_and_apart_for_each_notification() {
        let tag = tag(ID, "copy");
        assert_eq!(tag.len(), 15);
        assert_eq!(tag, super::tag(ID, "copy"));
        assert_ne!(tag, super::tag(ID, "copy2"));
        assert_ne!(super::tag("ab", "c"), super::tag("a", "bc"));
    }

    #[test]
    fn a_notification_shows_its_text_progress_buttons_and_source() {
        let notice = ExtensionNotice {
            title: "Copying".into(),
            message: "Two files".into(),
            context: "Downloads".into(),
            items: vec![("First".into(), "done".into())],
            buttons: vec!["Pause".into(), "Stop".into()],
            progress: Some(40),
            silent: false,
        };
        let shown = notification("Copier", ID, "copy", &notice);
        assert_eq!(shown.title, "Copying");
        assert_eq!(shown.tag, tag(ID, "copy"));
        let lines: Vec<&str> = shown.body.lines().collect();
        assert_eq!(lines[..3], ["Two files", "First done", "Downloads"]);
        assert!(lines[3].contains("40"), "{lines:?}");
        assert!(lines[4].contains("Copier"), "{lines:?}");
        assert_eq!(shown.activation, Some(activation(ID, "copy", None)));
        let buttons: Vec<(&str, Option<Choice>)> = shown
            .buttons
            .iter()
            .map(|button| (button.label.as_str(), chosen(&button.activation)))
            .collect();
        assert_eq!(
            buttons,
            [
                ("Pause", Some((ID.into(), "copy".into(), Some(0)))),
                ("Stop", Some((ID.into(), "copy".into(), Some(1)))),
            ]
        );
        // Without a title it is named for the extension.
        let untitled = notification("Copier", ID, "copy", &ExtensionNotice::default());
        assert_eq!(untitled.title, "Copier");
        assert!(!shown.silent);
    }

    #[test]
    fn a_change_shows_quietly_and_a_button_name_on_two_lines_still_shows() {
        // Not through `announce`: that sets the engine a choice goes back
        // to, which another test here relies on.
        let notifier = Recording::default();
        // `notifications.update` of a progress: the engine marks it quiet.
        let notice = ExtensionNotice {
            title: "Copying".into(),
            buttons: vec!["\t".into(), "Stop\nnow\t".into()],
            progress: Some(80),
            silent: true,
            ..ExtensionNotice::default()
        };
        notifier
            .show(&notification("Copier", ID, "copy", &notice))
            .expect("a control character refused it");
        let shown = notifier.shown();
        assert_eq!(shown.len(), 1);
        assert!(shown[0].silent);
        let buttons: Vec<(&str, Option<Choice>)> = shown[0]
            .buttons
            .iter()
            .map(|button| (button.label.as_str(), chosen(&button.activation)))
            .collect();
        assert_eq!(
            buttons,
            [("Stop now", Some((ID.into(), "copy".into(), Some(1))))]
        );
    }

    #[test]
    fn a_change_shows_and_a_clearing_withdraws_under_one_tag() {
        let notifier = Recording::default();
        let window = ui::webview::test_engine::TestEngine::new();
        let engine: SharedEngine = window.clone();
        let notice = ExtensionNotice {
            title: "Hello".into(),
            ..ExtensionNotice::default()
        };
        announce(&notifier, &engine, "Greeter", ID, ("hi", Some(&notice)));
        announce(&notifier, &engine, "Greeter", ID, ("hi", None));
        assert_eq!(notifier.shown().len(), 1);
        assert_eq!(notifier.shown()[0].tag, tag(ID, "hi"));
        assert_eq!(notifier.withdrawn(), [tag(ID, "hi")]);
        // The person chooses its second button: the engine is told.
        choose(&activation(ID, "hi", Some(1)));
        assert!(
            window
                .commands()
                .contains(&WebViewCommand::ExtensionNotificationChosen {
                    id: ID.into(),
                    notification: "hi".into(),
                    button: Some(1),
                })
        );
    }
}
