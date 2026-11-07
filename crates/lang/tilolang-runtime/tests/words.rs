//! A view's words in the reader's language (i18n.md §2): a literal a component shows is a
//! message of the package's catalogue, drawn through the runtime's words with its
//! interpolations as arguments; a literal that is data, or that a view compares, is not.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, Runtime, SimpleAuthority, Value, Words};

    const SOURCE: &str = r#"app so.tilo.inbox "Inbox"

type Inbox {
    unread: Int = 3
    kind: Text = "work"
}

root Inbox

default view inbox for Inbox

view inbox(i: Inbox) {
    ui.Text("{i.unread} new messages")
    ui.Text(if i.unread > 2 and i.kind != "home" { "At work" } else { "At home" })
    ui.Link("https://example.com/help", label: .some("Help"))
}
"#;

    /// German for the keys it knows, keyed exactly as the catalogue keys them.
    struct German;
    impl Words for German {
        fn add(&self, _: &[(String, String)]) {}
        fn add_used(&self, _: &[(String, String)]) {}
        fn word(&self, key: &str, args: &[(String, String)], source: &str) -> String {
            let arg = |name: &str| {
                args.iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default()
            };
            match key.rsplit_once('.').map(|(_, attr)| attr) {
                Some("w-new-messages") => format!("{} neue Nachrichten", arg("unread")),
                Some("w-at-work") => "Bei der Arbeit".to_owned(),
                Some("w-help") => "Hilfe".to_owned(),
                _ => source.to_owned(),
            }
        }
    }

    #[test]
    fn a_views_words_draw_in_the_readers_language() {
        let app = App::compile(SOURCE, &[]);
        let catalogue = tilolang::messages::source_catalogue(app.meta());
        assert!(
            catalogue.contains(".w-new-messages = { $unread } new messages"),
            "{catalogue}"
        );
        assert!(catalogue.contains(".w-at-work = At work"), "{catalogue}");
        assert!(
            !catalogue.contains("w-home ="),
            "a compared literal is no message: {catalogue}"
        );
        assert!(
            !catalogue.contains("example"),
            "a data prop's literal is no message: {catalogue}"
        );

        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        rt.set_words(German);
        let inbox = rt.create("Inbox").unwrap();
        let mut view = rt
            .open_view(&me(), &app.view("inbox"), vec![Value::resource(&inbox)])
            .unwrap();
        view.poll_patches().unwrap();
        let texts: Vec<String> = of_kind(view.tree(), "ui.Text")
            .into_iter()
            .map(|t| text_prop(t, "value"))
            .collect();
        assert_eq!(texts, ["3 neue Nachrichten", "Bei der Arbeit"]);
        let link = of_kind(view.tree(), "ui.Link");
        assert_eq!(text_prop(link[0], "url"), "https://example.com/help");
        assert_eq!(
            text_prop(link[0], "label"),
            "Hilfe",
            "an option of words is words"
        );
    }
}
