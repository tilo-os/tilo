//! Every app under `apps/tl`, compiled with its committed ids and run end to end: a chat with
//! embedded agent blocks (the fixture `tests/block_chat`), letters, Today's foreign views and flow, lists that open their
//! items in windows of their own, window and lens state, and every root's window at a compact
//! and a wide space.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use serde_json::json;
    use tilolang::IdsLock;
    use tilolang_runtime::{
        AmStore, EditOutcome, FlowOutcome, InvokeOutcome, MemorySink, ModuleHash, PackageId,
        Principal, Resolution, ResourceRef, Runtime, RuntimeError, Space, UiNode, Value,
        ViewInstance,
    };

    /// An app's directory (for messages), source and committed lock, all from one path.
    macro_rules! app_source {
        ($name:literal) => {
            (
                concat!("apps/tl/", $name),
                include_str!(concat!("../../../../apps/tl/", $name, "/app.tl")),
                include_str!(concat!("../../../../apps/tl/", $name, "/app.ids")),
            )
        };
    }

    /// Compile a package with its committed lock, which must already be complete: building
    /// assigns ids to anything new, and a lock that would change is a lock not committed.
    fn build((dir, src, ids): (&str, &str, &str), imports: &[&App]) -> App {
        let lock = IdsLock::parse(ids).unwrap_or_else(|e| panic!("{dir}/app.ids: {e}"));
        let metas: Vec<tilolang::PackageMeta> = imports.iter().map(|a| a.meta().clone()).collect();
        let compiled = tilolang::compile_package(src, &lock, &metas)
            .unwrap_or_else(|d| panic!("{dir}: {d:#?}"));
        assert_eq!(
            compiled.ids.to_text(),
            ids,
            "{dir}/app.ids is out of date: run `tilolang build {dir}`"
        );
        App { compiled }
    }

    struct Apps {
        notes: App,
        todo: App,
        /// The chat of messages with blocks, a fixture.
        chat: App,
        letters: App,
        counter: App,
        today: App,
        /// Chat: channels, replies and threads.
        channels: App,
        board: App,
        views: App,
    }

    fn apps() -> Apps {
        let notes = build(app_source!("notes"), &[]);
        let todo = build(app_source!("tasks"), &[]);
        let chat = build(
            (
                "crates/lang/tilolang-runtime/tests/block_chat",
                include_str!("block_chat/app.tl"),
                include_str!("block_chat/app.ids"),
            ),
            &[],
        );
        let letters = build(
            (
                "crates/lang/tilolang/tests/fixtures/letters",
                include_str!("../../tilolang/tests/fixtures/letters/app.tl"),
                include_str!("../../tilolang/tests/fixtures/letters/app.ids"),
            ),
            &[],
        );
        let counter = build(app_source!("counter"), &[]);
        let channels = build(app_source!("chat"), &[]);
        let today = build(app_source!("today"), &[&notes, &todo, &channels]);
        let board = build(app_source!("board"), &[]);
        let views = build(app_source!("views"), &[&board, &todo, &channels, &notes]);
        Apps {
            notes,
            todo,
            chat,
            letters,
            counter,
            today,
            channels,
            board,
            views,
        }
    }

    struct World {
        rt: Runtime,
        auth: TestAuthority,
        sink: MemorySink,
        ids: Vec<PackageId>,
    }

    fn world(apps: &Apps, store: &AmStore) -> World {
        let auth = TestAuthority::new();
        let rt = Runtime::new(store.clone(), auth.clone()).unwrap();
        let sink = MemorySink::new();
        rt.set_sink(sink.clone());
        let ids = [
            &apps.notes,
            &apps.todo,
            &apps.chat,
            &apps.letters,
            &apps.counter,
            &apps.today,
            &apps.channels,
            &apps.board,
            &apps.views,
        ]
        .iter()
        .map(|a| a.load(&rt))
        .collect();
        World {
            rt,
            auth,
            sink,
            ids,
        }
    }

    impl World {
        fn call(&self, app: &App, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        /// The view `name` with every row of its lists rendered: these checks read the whole
        /// tree, as a host that draws every row does.
        fn view(&self, app: &App, name: &str, args: Vec<Value>) -> ViewInstance {
            let mut view = self
                .rt
                .open_view(&me(), &app.view(name), args)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            view.set_lazy_rows(false)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            view
        }

        fn create(&self, ty: &str) -> ResourceRef {
            self.rt.create(ty).unwrap()
        }

        fn id_of(&self, which: usize) -> &PackageId {
            &self.ids[which]
        }
    }

    fn res(r: &ResourceRef) -> Value {
        Value::resource(r)
    }

    /// Post as `who`: the runtime authenticates the invoker, the message records it.
    fn post(
        w: &World,
        apps: &Apps,
        conv: &ResourceRef,
        who: &str,
        text: &str,
        block: &str,
    ) -> ResourceRef {
        let id = apps.chat.action("post");
        let args = vec![res(conv), Value::text(text), Value::text(block)];
        returned_ref(
            w.rt.invoke_action(&Principal::new(who), (id.as_str(), args))
                .unwrap(),
        )
    }

    fn author_of(w: &World, message: &ResourceRef) -> String {
        match w.rt.read(message, "author").unwrap() {
            Value::Principal(p) => p.0,
            other => panic!("the author is a principal, not {other:?}"),
        }
    }

    fn invocation_of(w: &World, apps: &Apps, message: &ResourceRef) -> ResourceRef {
        let _ = apps;
        let part = items(w, message, "parts").pop().expect("a block part");
        match w.rt.read(&part, "invocation").unwrap() {
            Value::Opt(Some(inner)) => match *inner {
                Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
                other => panic!("not a reference: {other:?}"),
            },
            other => panic!("the host has recorded no invocation: {other:?}"),
        }
    }

    fn start_chat(w: &World, apps: &Apps) -> (ResourceRef, ResourceRef) {
        let chat = w.create("Conversations");
        let conv =
            returned_ref(w.call(&apps.chat, "start", vec![res(&chat), Value::text("Plans")]));
        (chat, conv)
    }

    fn items(w: &World, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
        let Value::List(items) = w.rt.read(at, field).unwrap() else {
            panic!("{field} is not a list");
        };
        items
            .into_iter()
            .map(|v| match v {
                Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
                other => panic!("not a reference: {other:?}"),
            })
            .collect()
    }

    fn text_of(w: &World, at: &ResourceRef, field: &str) -> String {
        w.rt.read(at, field).unwrap().as_text().unwrap().to_owned()
    }

    /// The thread of a conversation: each message's author and its parts as (kind, body).
    fn thread(w: &World, conv: &ResourceRef) -> Vec<(String, Vec<(String, String)>)> {
        items(w, conv, "messages")
            .iter()
            .map(|m| {
                let parts = items(w, m, "parts")
                    .iter()
                    .map(|p| (text_of(w, p, "kind"), text_of(w, p, "body")))
                    .collect();
                (author_of(w, m), parts)
            })
            .collect()
    }

    fn state(n: &UiNode) -> String {
        text_prop(n, "state")
    }

    fn embeds(tree: &UiNode) -> Vec<&UiNode> {
        of_kind(tree, "embed")
    }

    /// The authors of a transcript's messages, as its items name them for the list to draw.
    fn authors_of(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.ListItem")
            .into_iter()
            .filter_map(|item| match item.props.get("author") {
                Some(Value::Principal(who)) => Some(who.0.clone()),
                _ => None,
            })
            .collect()
    }

    /// The values of the texts with the title role.
    fn title_texts(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Text")
            .into_iter()
            .filter(|t| t.props.get("role") == Some(&Value::text("title")))
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    /// The resource of the list item whose row has this title: what activating the row opens.
    fn item_titled(tree: &UiNode, title: &str) -> ResourceRef {
        of_kind(tree, "ui.ListItem")
            .into_iter()
            .find(|item| {
                of_kind(item, "ui.ListRow")
                    .iter()
                    .any(|r| text_prop(r, "title") == title)
            })
            .and_then(|item| match item.props.get("item") {
                Some(Value::Ref(r)) => Some(ResourceRef::clone(&r.target)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no item `{title}` in\n{}", dump(tree)))
    }

    fn row_titles(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.ListRow")
            .into_iter()
            .map(|row| text_prop(row, "title"))
            .collect()
    }

    /// The node of `kind` whose `prop` is `value`.
    fn node_with<'a>(tree: &'a UiNode, kind: &str, prop: &str, value: &str) -> &'a UiNode {
        of_kind(tree, kind)
            .into_iter()
            .find(|node| node.props.get(prop) == Some(&Value::text(value)))
            .unwrap_or_else(|| panic!("no {kind} with {prop} {value} in\n{}", dump(tree)))
    }

    /// What the content column of a window's shell holds.
    fn content(tree: &UiNode) -> &UiNode {
        tree.children
            .iter()
            .find(|child| {
                child.kind == "slot" && child.props.get("name") == Some(&Value::text("content"))
            })
            .unwrap_or_else(|| panic!("no content in\n{}", dump(tree)))
    }

    /// Choose the place `name` of a window's filter, in its sidebar.
    fn choose(view: &ViewInstance, name: &str) {
        let place = node_with(view.tree(), "ui.NavItem", "key", name)
            .key
            .clone();
        view.fire(&place, "select").unwrap();
    }

    fn button_labelled<'a>(tree: &'a UiNode, label: &str) -> &'a UiNode {
        of_kind(tree, "ui.Button")
            .into_iter()
            .find(|b| b.props.get("label") == Some(&Value::text(label)))
            .unwrap_or_else(|| panic!("no button `{label}` in\n{}", dump(tree)))
    }

    fn suggested(out: InvokeOutcome) -> tilolang_runtime::Suggestion {
        match out {
            InvokeOutcome::Suggested(s) => s,
            other => panic!("expected a suggestion, got {other:?}"),
        }
    }

    // ----- the apps compile -----------------------------------------------------------------

    #[test]
    fn every_app_compiles_with_its_committed_ids_and_loads_together() {
        let a = apps();
        let w = world(&a, &AmStore::new());
        assert_eq!(w.ids.len(), 9);
        for app in [
            &a.notes,
            &a.todo,
            &a.chat,
            &a.letters,
            &a.counter,
            &a.today,
            &a.channels,
            &a.board,
            &a.views,
        ] {
            let meta = app.meta();
            let root = meta
                .roots
                .first()
                .unwrap_or_else(|| panic!("{} has no root", meta.app));
            assert!(
                meta.default_view(root).is_some(),
                "{}'s root has no default view",
                meta.app
            );
            assert!(
                meta.views.iter().all(|v| v.name != "main"),
                "{} still has a `main` view",
                meta.app
            );
        }
    }

    // ----- chat -------------------------------------------------------------------------------

    #[test]
    fn two_chat_replicas_posting_concurrently_converge_in_order_with_authors() {
        let apps = apps();
        let (mut a, mut b) = (AmStore::new(), AmStore::new());
        let (wa, wb) = (world(&apps, &a), world(&apps, &b));
        let (_, conv) = start_chat(&wa, &apps);
        a.sync_with(&mut b).unwrap();

        post(&wa, &apps, &conv, "person:ada", "first", "");
        post(&wa, &apps, &conv, "agent:helper", "second", "1 + 2");
        post(&wb, &apps, &conv, "person:bo", "concurrent", "");
        a.sync_with(&mut b).unwrap();

        let (ta, tb) = (thread(&wa, &conv), thread(&wb, &conv));
        assert_eq!(ta, tb, "both replicas read the same thread");
        assert_eq!(ta.len(), 3);
        let authors: Vec<&str> = ta.iter().map(|(a, _)| a.as_str()).collect();
        let at = |who: &str| authors.iter().position(|a| *a == who).unwrap();
        assert!(
            at("person:ada") < at("agent:helper"),
            "one replica's posts keep their order: {authors:?}"
        );
        assert!(authors.contains(&"person:bo"));
        let helper = &ta[at("agent:helper")].1;
        assert_eq!(
            helper,
            &[
                ("text".to_owned(), "second".to_owned()),
                ("block".to_owned(), "1 + 2".to_owned())
            ],
            "a message keeps its parts, in order"
        );

        let view = wb.view(&apps.chat, "thread", vec![res(&conv)]);
        assert_eq!(
            authors_of(view.tree()),
            authors,
            "the view shows the messages in the converged order, with their authors"
        );
    }

    #[test]
    fn typing_in_the_composer_and_sending_posts_a_message_and_clears_the_entry() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let mut view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let inputs = of_kind(view.tree(), "ui.TextInput");
        assert_eq!(
            inputs.len(),
            1,
            "the title is the header's; the one entry is the composer"
        );
        let draft = inputs[0].key.clone();
        assert_eq!(
            view.edit(&draft, &Value::text("hello")).unwrap(),
            EditOutcome::Applied
        );
        // The draft is the person's own state; the Send it shows carries what they typed.
        view.poll_patches().unwrap();
        let send = button_labelled(view.tree(), "Send").key.clone();
        assert!(matches!(view.press(&send).unwrap(), InvokeOutcome::Done(_)));
        assert_eq!(thread(&w, &conv).len(), 1);
        view.poll_patches().unwrap();
        assert_eq!(
            view.bound_value(&draft),
            Some(Value::text("")),
            "the entry is cleared once the message went"
        );
        assert_eq!(
            authors_of(view.tree()),
            ["me"],
            "the composer posts as whoever is looking, as the runtime knows them"
        );
    }

    #[test]
    fn a_press_right_after_a_keystroke_sends_the_keystroke_too() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let mut view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let draft = of_kind(view.tree(), "ui.TextInput")[0].key.clone();
        let send = button_labelled(view.tree(), "Send").key.clone();
        view.edit(&draft, &Value::text("on my wa")).unwrap();
        view.poll_patches().unwrap();
        // The last keystroke and the press arrive together: the view has not drawn since.
        view.edit(&draft, &Value::text("on my way")).unwrap();
        assert!(matches!(view.press(&send).unwrap(), InvokeOutcome::Done(_)));
        assert_eq!(
            thread(&w, &conv)
                .last()
                .map(|(_, parts)| parts[0].1.clone()),
            Some("on my way".to_owned()),
            "the action reads the draft when it is pressed"
        );
        view.poll_patches().unwrap();
        assert_eq!(view.bound_value(&draft), Some(Value::text("")));
    }

    #[test]
    fn an_agents_block_that_is_a_notes_view_renders_that_view_inline_and_stays_live() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let notebook = w.create("Notes");
        let note = returned_ref(w.call(&apps.notes, "new_note", vec![res(&notebook)]));
        w.call(
            &apps.notes,
            "retitle",
            vec![res(&note), Value::text("Groceries")],
        );
        let src = format!("notes.note_editor(@notes.Note:{})", note.id_string());
        post(&w, &apps, &conv, "agent:helper", "Here is your note", &src);

        let mut view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(view.tree());
        assert_eq!(found.len(), 1, "{}", dump(view.tree()));
        assert_eq!(state(found[0]), "view", "{}", dump(view.tree()));
        let title = of_kind(found[0], "ui.TextInput")[0];
        let binding = title.binding.clone().expect("the title input is bound");
        assert_eq!(binding.field().map(|(target, _)| target), Some(&note));
        assert_eq!(
            &binding.author,
            w.id_of(0),
            "the nested view is the notes package's code, not the agent's"
        );
        // A new note's title starts empty: "Untitled" is only what the field prompts with.
        assert_eq!(text_prop(title, "placeholder"), "Untitled");

        let key = title.key.clone();
        assert_eq!(
            view.edit(&key, &Value::text("Shopping")).unwrap(),
            EditOutcome::Applied,
            "the notes view's own input edits even inside an agent's message"
        );
        assert_eq!(w.rt.read(&note, "title").unwrap(), Value::text("Shopping"));
        view.poll_patches().unwrap();
        assert_eq!(
            view.bound_value(&of_kind(embeds(view.tree())[0], "ui.TextInput")[0].key),
            Some(Value::text("Shopping"))
        );
    }

    #[test]
    fn a_block_that_is_a_value_renders_through_the_default_view() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let list = w.create("Tasks");
        let project = returned_ref(w.call(&apps.todo, "new_project", vec![res(&list)]));
        w.call(
            &apps.todo,
            "rename",
            vec![res(&project), Value::text("Groceries")],
        );
        post(&w, &apps, &conv, "agent:helper", "", "1 + 2");
        post(
            &w,
            &apps,
            &conv,
            "agent:helper",
            "",
            &format!("@tasks.Project:{}", project.id_string()),
        );
        let view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(view.tree());
        assert_eq!(
            found.iter().map(|e| state(e)).collect::<Vec<_>>(),
            ["data", "data"]
        );
        assert_eq!(text_prop(of_kind(found[0], "ui.Text")[0], "value"), "3");
        assert_eq!(
            text_prop(of_kind(found[1], "ui.Text")[0], "value"),
            "Groceries"
        );
    }

    #[test]
    fn a_block_that_does_not_compile_renders_its_diagnostics_and_the_chat_goes_on() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        post(&w, &apps, &conv, "agent:helper", "see", "tasks.nothing(1)");
        post(&w, &apps, &conv, "person:ada", "still here", "");
        let view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(view.tree());
        assert_eq!(state(found[0]), "error", "{}", dump(view.tree()));
        let error = of_kind(found[0], "error")[0];
        assert!(!error.children.is_empty());
        let first = text_prop(&error.children[0], "value");
        assert!(
            first.contains("E0"),
            "the diagnostic keeps its code: {first}"
        );
        assert_eq!(authors_of(view.tree()), ["agent:helper", "person:ada"]);
    }

    #[test]
    fn the_same_block_source_is_compiled_once() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let before = w.rt.compiled_modules();
        for who in ["agent:a", "agent:b", "person:c"] {
            post(&w, &apps, &conv, who, "", "40 + 2");
        }
        let view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        assert_eq!(embeds(view.tree()).len(), 3);
        assert_eq!(
            w.rt.compiled_modules(),
            before + 1,
            "three embeds of one source are one module"
        );
        drop(view);
        let _again = w.view(&apps.chat, "thread", vec![res(&conv)]);
        assert_eq!(
            w.rt.compiled_modules(),
            before + 1,
            "and re-rendering adds none"
        );
    }

    #[test]
    fn a_block_applying_letters_send_is_suggested_and_confirming_it_delivers_the_letter() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let desk = w.create("Desk");
        let draft = returned_ref(w.call(&apps.letters, "new_draft", vec![res(&desk)]));
        let mut editor = w.view(&apps.letters, "editor", vec![res(&draft)]);
        let inputs: Vec<String> = of_kind(editor.tree(), "ui.TextInput")
            .iter()
            .map(|i| i.key.clone())
            .collect();
        for (key, text) in inputs
            .iter()
            .zip(["bo@example.org", "Plans", "See you at nine."])
        {
            assert_eq!(
                editor.edit(key, &Value::text(text)).unwrap(),
                EditOutcome::Applied
            );
        }
        editor.poll_patches().unwrap();

        // The person's own Send button also asks: an outside effect always does.
        let own = button_labelled(editor.tree(), "Send").key.clone();
        let own = suggested(editor.press(&own).unwrap());
        assert_eq!(own.chain.len(), 1);

        let src = format!("letters.send(@letters.Draft:{})", draft.id_string());
        post(&w, &apps, &conv, "agent:helper", "Shall I send it?", &src);
        let mut view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(view.tree());
        assert_eq!(state(found[0]), "invocation", "{}", dump(view.tree()));
        let inv = of_kind(found[0], "invocation")[0];
        let Some(Value::Ref(card)) = inv.props.get("invocation") else {
            panic!("the card names its invocation resource: {}", dump(inv));
        };
        let record = card.target.clone();
        assert_eq!(text_prop(inv, "state"), "suggested");
        assert_eq!(inv.props.get("can_resolve"), Some(&Value::Bool(true)));
        assert_eq!(text_prop(inv, "action"), "send");
        assert_eq!(text_prop(inv, "app"), "test.letters");
        let run = text_prop(inv, "run");
        assert_eq!(
            title_texts(inv),
            ["Send to bo@example.org"],
            "the proposal view shows the letter: {}",
            dump(inv)
        );
        assert!(w.rt.pending_outbox().is_empty(), "asking sends nothing");

        // A re-render shows the same suggestion rather than making another.
        post(&w, &apps, &conv, "person:ada", "Looks right", "");
        assert!(view.is_stale(), "a new message re-renders the thread");
        view.poll_patches().unwrap();
        let again = of_kind(view.tree(), "invocation")[0];
        assert_eq!(text_prop(again, "run"), run);

        let state =
            w.rt.resolve_invocation(&record, &me(), Resolution::Approve)
                .unwrap();
        assert_eq!(state, "done");
        assert!(
            view.is_stale(),
            "the invocation resource changed, so the message refreshes"
        );
        view.poll_patches().unwrap();
        assert_eq!(
            text_prop(of_kind(view.tree(), "invocation")[0], "state"),
            "done"
        );
        assert!(
            title_texts(view.tree())
                .iter()
                .all(|h| h != "Send to bo@example.org"),
            "a decided suggestion no longer shows its proposal"
        );

        let pending = w.rt.pending_outbox();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].capability, "post");
        assert_eq!(
            pending[0].payload,
            json!(["bo@example.org", "Plans", "See you at nine."])
        );
        assert_eq!(w.rt.read(&draft, "sent").unwrap(), Value::Bool(true));
        // Marked as being handed over, then handed over.
        assert_eq!(w.rt.settle().unwrap().marked, 1);
        assert_eq!(w.rt.settle().unwrap().delivered, 1);
        assert_eq!(
            w.sink.delivered(),
            vec![(
                "post".to_owned(),
                json!(["bo@example.org", "Plans", "See you at nine."])
            )]
        );
        view.poll_patches().unwrap();
        assert!(w.rt.pending_outbox().is_empty());
    }

    #[test]
    fn declining_a_suggested_block_shows_declined_and_sends_nothing() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let desk = w.create("Desk");
        let draft = returned_ref(w.call(&apps.letters, "new_draft", vec![res(&desk)]));
        let src = format!("letters.send(@letters.Draft:{})", draft.id_string());
        let message = post(&w, &apps, &conv, "agent:helper", "", &src);
        let mut view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let record = invocation_of(&w, &apps, &message);
        assert_eq!(
            w.rt.resolve_invocation(&record, &me(), Resolution::Decline)
                .unwrap(),
            "declined"
        );
        view.poll_patches().unwrap();
        assert_eq!(
            text_prop(of_kind(view.tree(), "invocation")[0], "state"),
            "declined"
        );
        assert!(w.rt.pending_outbox().is_empty());
    }

    #[test]
    fn a_strangers_block_asks_for_a_foreign_action_that_the_apps_own_view_may_run() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let list = w.create("Tasks");
        let first = returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text("milk")]));
        let second = returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text("eggs")]));
        let remove = |item: &ResourceRef| {
            format!(
                "button(\"Remove\", on: tasks.remove(@tasks.Tasks:{}, @tasks.Task:{}))",
                list.id_string(),
                item.id_string()
            )
        };
        post(&w, &apps, &conv, "person:stranger", "", &remove(&first));
        let chat_view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(chat_view.tree());
        assert_eq!(state(found[0]), "view");
        let button = of_kind(found[0], "ui.Button")[0].key.clone();
        let s = suggested(chat_view.press(&button).unwrap());
        assert_eq!(s.action, apps.todo.action("remove"));
        assert_eq!(
            s.chain,
            vec![ModuleHash::principal("person:stranger")],
            "the chain names the principal who wrote the block"
        );
        assert_eq!(items(&w, &list, "items").len(), 2, "asking changed nothing");

        // The same action from the todo app's own surface (a row's menu, which the platform
        // applies as the person) runs without asking.
        let out = w.call(&apps.todo, "remove", vec![res(&list), res(&first)]);
        assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
        assert_eq!(items(&w, &list, "items"), vec![second.clone()]);

        // Trusting the principal covers every block it writes, from the next one on.
        w.auth.trust_principal("person:stranger");
        let third = returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text("bread")]));
        post(&w, &apps, &conv, "person:stranger", "", &remove(&third));
        let chat_view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(chat_view.tree());
        let button = of_kind(found[1], "ui.Button")[0].key.clone();
        assert!(matches!(
            chat_view.press(&button).unwrap(),
            InvokeOutcome::Done(_)
        ));
        assert_eq!(items(&w, &list, "items"), vec![second]);
        w.auth.revoke_principal("person:stranger");
    }

    fn adds(w: &World, apps: &Apps, list: &ResourceRef) -> usize {
        w.rt.history(&list.doc)
            .unwrap()
            .iter()
            .filter(|n| n.action == apps.todo.action("add"))
            .count()
    }

    #[test]
    fn a_trusted_agents_action_block_runs_when_posted_and_never_again_on_render() {
        let apps = apps();
        let (mut a, mut b) = (AmStore::new(), AmStore::new());
        let (wa, wb) = (world(&apps, &a), world(&apps, &b));
        let (_, conv) = start_chat(&wa, &apps);
        let list = wa.create("Tasks");
        wa.auth.trust_principal("agent:helper");
        let src = format!("tasks.add(@tasks.Tasks:{}, \"bread\")", list.id_string());
        let message = post(&wa, &apps, &conv, "agent:helper", "", &src);

        assert_eq!(
            items(&wa, &list, "items").len(),
            1,
            "it ran when the message was posted, before anything rendered it"
        );
        let record = wa
            .rt
            .read_invocation(&invocation_of(&wa, &apps, &message))
            .unwrap();
        assert_eq!(record.state, "done");
        assert_eq!(record.name, "add");
        assert_eq!(record.author, Principal::new("agent:helper"));

        let card_state = |w: &World| {
            let view = w.view(&apps.chat, "thread", vec![res(&conv)]);
            text_prop(of_kind(view.tree(), "invocation")[0], "state")
        };
        let mut view = wa.view(&apps.chat, "thread", vec![res(&conv)]);
        for width in [20.0, 90.0, 20.0] {
            view.set_space("", Space::from_rem(width, 40.0));
            view.poll_patches().unwrap();
        }
        assert_eq!(card_state(&wa), "done");

        // A restart: a new runtime over the same documents.
        let restarted = world(&apps, &a);
        assert_eq!(card_state(&restarted), "done");

        // Another replica renders what the first one recorded.
        a.sync_with(&mut b).unwrap();
        assert_eq!(card_state(&wb), "done");
        assert_eq!(items(&wb, &list, "items").len(), 1);
        for w in [&wa, &restarted, &wb] {
            assert_eq!(adds(w, &apps, &list), 1, "rendering never runs the action");
        }
    }

    #[test]
    fn a_caller_cannot_post_as_an_agent_so_it_cannot_borrow_the_agents_authority() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let list = w.create("Tasks");
        w.auth.trust_principal("agent:helper");
        let src = format!("tasks.add(@tasks.Tasks:{}, \"spam\")", list.id_string());

        let mallory = Principal::new("person:mallory");
        let message = post(&w, &apps, &conv, "person:mallory", "", &src);
        assert_eq!(author_of(&w, &message), "person:mallory");
        let record =
            w.rt.read_invocation(&invocation_of(&w, &apps, &message))
                .unwrap();
        assert_eq!(record.author, mallory);
        assert_eq!(record.state, "suggested", "her code has no grants");
        assert!(items(&w, &list, "items").is_empty());

        // The action has no author to fill in, and a principal is not something a caller
        // can pass in place of text either.
        let id = apps.chat.action("post");
        let forged = Value::Principal(Principal::new("agent:helper"));
        let attempts = [
            vec![
                res(&conv),
                forged.clone(),
                Value::text(""),
                Value::text(&src),
            ],
            vec![res(&conv), forged, Value::text(&src)],
        ];
        for args in attempts {
            let err =
                w.rt.invoke_action(&mallory, (id.as_str(), args))
                    .unwrap_err();
            assert!(matches!(err, RuntimeError::BadArgs(_)), "{err}");
        }
        assert_eq!(thread(&w, &conv).len(), 1, "no forged message was posted");

        // The agent's own post carries the agent's authority.
        let real = post(&w, &apps, &conv, "agent:helper", "", &src);
        assert_eq!(
            w.rt.read_invocation(&invocation_of(&w, &apps, &real))
                .unwrap()
                .state,
            "done"
        );
        assert_eq!(items(&w, &list, "items").len(), 1);
    }

    #[test]
    fn confirming_a_suggestion_on_another_replica_runs_it_once_and_both_replicas_show_done() {
        let apps = apps();
        let (mut a, mut b) = (AmStore::new(), AmStore::new());
        let (wa, wb) = (world(&apps, &a), world(&apps, &b));
        let (_, conv) = start_chat(&wa, &apps);
        let desk = wa.create("Desk");
        let draft = returned_ref(wa.call(&apps.letters, "new_draft", vec![res(&desk)]));
        let editor = wa.view(&apps.letters, "editor", vec![res(&draft)]);
        let inputs: Vec<String> = of_kind(editor.tree(), "ui.TextInput")
            .iter()
            .map(|i| i.key.clone())
            .collect();
        for (key, text) in inputs.iter().zip(["bo@example.org", "Plans", "Nine."]) {
            editor.edit(key, &Value::text(text)).unwrap();
        }
        let src = format!("letters.send(@letters.Draft:{})", draft.id_string());
        let message = post(&wa, &apps, &conv, "agent:helper", "Shall I?", &src);
        let record = invocation_of(&wa, &apps, &message);
        let mut view_a = wa.view(&apps.chat, "thread", vec![res(&conv)]);
        a.sync_with(&mut b).unwrap();

        // On B, a viewer the authority gives no say sees the suggestion but cannot run it.
        let (bo, guest) = (Principal::new("person:bo"), Principal::new("person:guest"));
        wb.auth.deny_invoker(&guest);
        let card_for = |who: &Principal| {
            let mut view = wb
                .rt
                .open_view(who, &apps.chat.view("thread"), vec![res(&conv)])
                .unwrap();
            // The messages are rows of the thread's list: read whole, as a host that draws
            // every row does.
            view.set_lazy_rows(false).unwrap();
            let card = of_kind(view.tree(), "invocation")[0];
            (
                text_prop(card, "state"),
                card.props.get("can_resolve").cloned(),
            )
        };
        assert_eq!(
            card_for(&guest),
            ("suggested".to_owned(), Some(Value::Bool(false)))
        );
        assert_eq!(
            card_for(&bo),
            ("suggested".to_owned(), Some(Value::Bool(true)))
        );
        let err = wb
            .rt
            .resolve_invocation(&record, &guest, Resolution::Approve)
            .unwrap_err();
        assert!(matches!(err, RuntimeError::Denied(_)), "{err}");
        assert_eq!(wb.rt.read_invocation(&record).unwrap().state, "suggested");
        assert!(wb.rt.pending_outbox().is_empty());

        // Someone with authority confirms on B: it runs there, once.
        assert_eq!(
            wb.rt
                .resolve_invocation(&record, &bo, Resolution::Approve)
                .unwrap(),
            "done"
        );
        assert_eq!(wb.rt.read(&draft, "sent").unwrap(), Value::Bool(true));
        assert_eq!(wb.rt.pending_outbox().len(), 1);
        assert_eq!(wa.rt.read_invocation(&record).unwrap().state, "suggested");
        assert!(wa.rt.pending_outbox().is_empty(), "A has not heard yet");

        a.sync_with(&mut b).unwrap();
        assert_eq!(wa.rt.read_invocation(&record).unwrap().state, "done");
        assert_eq!(wa.rt.read(&draft, "sent").unwrap(), Value::Bool(true));
        assert!(
            view_a.is_stale(),
            "the synced invocation refreshes the card"
        );
        view_a.poll_patches().unwrap();
        assert_eq!(
            text_prop(of_kind(view_a.tree(), "invocation")[0], "state"),
            "done"
        );
        for w in [&wa, &wb] {
            assert_eq!(w.rt.pending_outbox().len(), 1, "the letter was queued once");
            let sends =
                w.rt.history(&draft.doc)
                    .unwrap()
                    .iter()
                    .filter(|n| n.action == apps.letters.action("send"))
                    .count();
            assert_eq!(sends, 1);
        }
        assert_eq!(wa.rt.settle().unwrap().marked, 1);
        assert_eq!(wa.rt.settle().unwrap().delivered, 1);
        assert_eq!(wa.sink.delivered().len(), 1);

        let again = wa
            .rt
            .resolve_invocation(&record, &bo, Resolution::Approve)
            .unwrap_err();
        assert!(matches!(again, RuntimeError::NotPending(_)), "{again}");
    }

    #[test]
    fn the_composer_posts_as_the_person_looking_not_as_a_name_in_the_view() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let ada = Principal::new("person:ada");
        let mut view =
            w.rt.open_view(&ada, &apps.chat.view("thread"), vec![res(&conv)])
                .unwrap();
        let draft = of_kind(view.tree(), "ui.TextInput")[0].key.clone();
        view.edit(&draft, &Value::text("hello")).unwrap();
        view.poll_patches().unwrap();
        let send = button_labelled(view.tree(), "Send").key.clone();
        assert!(matches!(view.press(&send).unwrap(), InvokeOutcome::Done(_)));
        assert_eq!(thread(&w, &conv)[0].0, "person:ada");
    }

    // ----- windows: open(r), window state and lenses ------------------------------------------

    // USER REQUIREMENT (2026-10-20): "notes = a notebook list view (`root Notebook`, list rows
    // that `open(n)`) plus a `default view note_editor for Note`; no `open` field in Notebook
    // anymore". In the native vocabulary a list item carries its resource, and activating its
    // row opens it: the platform's list does what `on: open(n)` did.
    #[test]
    fn a_notebook_row_opens_its_note_in_a_window_of_its_own() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let nb = w.create("Notes");
        let note = returned_ref(w.call(&apps.notes, "new_note", vec![res(&nb)]));
        w.call(
            &apps.notes,
            "retitle",
            vec![res(&note), Value::text("Plans")],
        );
        assert!(
            apps.notes
                .meta()
                .types
                .iter()
                .all(|t| t.fields.iter().all(|f| f.name != "open")),
            "the notebook keeps no open note"
        );
        let list = w.view(&apps.notes, "notebook", vec![res(&nb)]);
        assert_eq!(list.window(), nb.id_string(), "a window is its resource's");
        assert_eq!(
            item_titled(list.tree(), "Plans"),
            note,
            "the row opens this note"
        );

        let editor = w.rt.open_resource(&me(), &note, None).unwrap();
        assert_eq!(editor.window(), note.id_string());
        let title = of_kind(editor.tree(), "ui.TextInput")[0];
        // A new note's title starts empty: "Untitled" is only what the field prompts with.
        assert_eq!(text_prop(title, "placeholder"), "Untitled");
        assert_eq!(
            w.rt.default_view(&note.ty).as_deref(),
            Some(apps.notes.view("note_editor").as_str())
        );
    }

    // USER REQUIREMENT (2026-10-20): "window state stored per view instance/window id (shared by
    // all principals viewing it), lens per (principal, window) ... two principals on one view
    // share window state but not lens (test)." The notebook keeps its order and filter as window
    // state; no app keeps a lens any more, so the lens half is
    // `e2e_enums::a_bound_lens_is_the_persons_and_a_bound_window_value_is_everyones`.
    #[test]
    fn two_people_in_one_window_share_its_window_state() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let nb = w.create("Notes");
        for title in ["b", "a"] {
            let n = returned_ref(w.call(&apps.notes, "new_note", vec![res(&nb)]));
            w.call(&apps.notes, "retitle", vec![res(&n), Value::text(title)]);
        }
        let (ada, bo) = (Principal::new("person:ada"), Principal::new("person:bo"));
        let open = |who: &Principal| {
            let mut view =
                w.rt.open_window(
                    who,
                    "window-1",
                    &apps.notes.view("notebook"),
                    vec![res(&nb)],
                )
                .unwrap();
            // The rows are read here as a host that draws every row does.
            view.set_lazy_rows(false).unwrap();
            view
        };
        let (mut va, mut vb) = (open(&ada), open(&bo));
        assert_eq!(row_titles(va.tree()), ["b", "a"]);

        // Window state: Ada sorts the window, and Bo's view of it sorts too.
        let by_title = node_with(va.tree(), "ui.MenuItem", "label", "Title")
            .key
            .clone();
        va.fire(&by_title, "choose").unwrap();
        assert!(
            va.is_stale() && vb.is_stale(),
            "both read the window's order"
        );
        va.poll_patches().unwrap();
        vb.poll_patches().unwrap();
        assert_eq!(row_titles(va.tree()), ["a", "b"]);
        assert_eq!(row_titles(vb.tree()), ["a", "b"]);

        // Another window over the same notebook has state of its own.
        let mut elsewhere =
            w.rt.open_window(
                &ada,
                "window-2",
                &apps.notes.view("notebook"),
                vec![res(&nb)],
            )
            .unwrap();
        elsewhere.set_lazy_rows(false).unwrap();
        assert_eq!(row_titles(elsewhere.tree()), ["b", "a"]);
    }

    #[test]
    fn a_lists_filter_is_window_state_and_changes_nothing_in_the_list() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let list = w.create("Tasks");
        for (t, done) in [("milk", true), ("eggs", false)] {
            let item = returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text(t)]));
            if done {
                w.call(&apps.todo, "check", vec![res(&item)]);
            }
        }
        let mut v = w.view(&apps.todo, "home", vec![res(&list)]);
        // It starts on the inbox: what is in no project, done or not.
        assert_eq!(row_titles(v.tree()), ["milk", "eggs"]);
        let before = w.rt.history(&list.doc).unwrap().len();
        choose(&v, "is:done");
        v.poll_patches().unwrap();
        assert_eq!(row_titles(v.tree()), ["milk"]);
        let tree = v.tree();
        assert_eq!(
            text_prop(of_kind(tree, "ui.NavList")[0], "selected"),
            "is:done"
        );
        assert_eq!(
            (
                &node_with(tree, "ui.NavItem", "key", "is:inbox").props["count"],
                &node_with(tree, "ui.NavItem", "key", "is:today").props["count"],
                &node_with(tree, "ui.NavItem", "key", "is:done").props["count"]
            ),
            (&Value::Int(2), &Value::Int(0), &Value::Int(1))
        );

        assert_eq!(
            w.rt.history(&list.doc).unwrap().len(),
            before,
            "view state is no change to the list"
        );
    }

    #[test]
    fn a_desk_opens_on_its_drafts_as_the_app_chose() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let desk = w.create("Desk");
        let v = w.view(&apps.letters, "desk", vec![res(&desk)]);
        let tree = v.tree();
        assert_eq!(
            text_prop(of_kind(tree, "ui.NavList")[0], "selected"),
            "is:drafts"
        );
        let keys: Vec<String> = of_kind(tree, "ui.NavItem")
            .iter()
            .map(|item| text_prop(item, "key"))
            .collect();
        assert_eq!(keys, ["all", "is:drafts", "is:sent"]);
        assert_eq!(
            text_prop(node_with(tree, "ui.NavItem", "key", "all"), "label"),
            "All letters",
            "the app names the place of every letter"
        );
    }

    // ----- views ------------------------------------------------------------------------------

    /// The view the recorded model answers "what's left for the release?" with in the agent's
    /// end-to-end check.
    fn release_view() -> String {
        let script: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../scripts/headless-examples/agent-tilolang/release.json"
        ))
        .unwrap();
        script["turns"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|turn| turn["blocks"].as_array().unwrap())
            .find_map(|block| block["input"]["view"]["source"].as_str())
            .expect("the script answers with a view")
            .to_owned()
    }

    #[test]
    fn an_answer_draws_its_view_block_live_over_other_apps_as_its_task_wrote_it() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let list = w.rt.create_root("Tasks").unwrap();
        let add = |title: &str| {
            returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text(title)]))
        };
        add("Record the demo video");
        add("Update the changelog");
        let frozen = add("Freeze the strings for translation");
        w.call(&apps.todo, "check", vec![res(&frozen)]);
        let answers = w.rt.create_root("Answers").unwrap();
        // The agent's task answers; the answer records it as the block's author.
        let task = Principal::new("agent-task:release");
        let id = apps.views.action("answer");
        let answer = returned_ref(
            w.rt.invoke_action(
                &task,
                (
                    id.as_str(),
                    vec![
                        res(&answers),
                        Value::text("Left for the release"),
                        Value::text("what's left for the release?"),
                        Value::text(release_view()),
                    ],
                ),
            )
            .unwrap(),
        );
        assert_eq!(author_of(&w, &answer), "agent-task:release");
        let mut view = w.view(&apps.views, "answer_page", vec![res(&answer)]);
        let found = embeds(view.tree());
        assert_eq!(found.len(), 1, "{}", dump(view.tree()));
        assert_eq!(state(found[0]), "view", "{}", dump(view.tree()));
        let shown = row_titles(found[0]);
        assert!(
            shown.contains(&"Record the demo video".to_owned())
                && shown.contains(&"Update the changelog".to_owned()),
            "{shown:?}\n{}",
            dump(view.tree())
        );
        assert!(
            !shown.contains(&"Freeze the strings for translation".to_owned()),
            "{shown:?}"
        );
        // Live: a task added later is in the answer on its next frame.
        add("Screenshots for the docs");
        view.poll_patches().unwrap();
        assert!(
            row_titles(embeds(view.tree())[0]).contains(&"Screenshots for the docs".to_owned()),
            "{}",
            dump(view.tree())
        );
        // As a window draws it, with rows built as they are shown: the answer's rows are there.
        let lazy =
            w.rt.open_view(&me(), &apps.views.view("answer_page"), vec![res(&answer)])
                .unwrap();
        let lazy_rows = of_kind(embeds(lazy.tree())[0], "ui.ListRow").len();
        assert_eq!(
            lazy_rows,
            3,
            "the open tasks' rows are built: {}",
            dump(lazy.tree())
        );
    }

    // ----- today ------------------------------------------------------------------------------

    struct Morning {
        w: World,
        today: ResourceRef,
        list: ResourceRef,
        notebook: ResourceRef,
        chat: ResourceRef,
    }

    fn morning(apps: &Apps) -> Morning {
        let w = world(apps, &AmStore::new());
        // Today reads the other apps' roots: the workspace's one of each.
        let today = w.create("Today");
        let notebook = w.rt.create_root("Notes").unwrap();
        let list = w.rt.create_root("Tasks").unwrap();
        let chat = w.rt.create_root("Channels").unwrap();
        Morning {
            w,
            today,
            list,
            notebook,
            chat,
        }
    }

    fn add_todo(m: &Morning, apps: &Apps, title: &str, done: bool) {
        let item =
            returned_ref(m.w.call(&apps.todo, "add", vec![res(&m.list), Value::text(title)]));
        if done {
            m.w.call(&apps.todo, "check", vec![res(&item)]);
        }
    }

    #[test]
    fn the_boards_cards_show_other_packages_resources_as_rows_with_a_count() {
        let apps = apps();
        let m = morning(&apps);
        add_todo(&m, &apps, "milk", false);
        add_todo(&m, &apps, "bins", true);
        m.w.call(&apps.notes, "new_note", vec![res(&m.notebook)]);
        let view = m.w.view(&apps.today, "today", vec![res(&m.today)]);
        let tree = view.tree();
        assert_eq!(tree.kind, "ui.NavShell", "{}", dump(tree));
        assert_eq!(
            of_kind(tree, "page")[0].props["full"],
            Value::Int(1),
            "Today is a canvas the window's width: {}",
            dump(tree)
        );
        let cards = of_kind(tree, "ui.Card");
        let shown: Vec<&ResourceRef> = cards
            .iter()
            .map(|c| match &c.props["item"] {
                Value::Ref(r) => &*r.target,
                other => panic!("a card shows a resource, not {other:?}"),
            })
            .collect();
        assert_eq!(
            shown,
            [&m.list, &m.notebook, &m.chat],
            "a card per resource Today points at"
        );
        assert_eq!(
            cards[0].props.get("count"),
            Some(&Value::Int(1)),
            "the todo card counts the open items: {}",
            dump(tree)
        );
        let rows: Vec<&UiNode> = of_kind(cards[0], "ui.ListRow");
        assert_eq!(rows.len(), 1, "{}", dump(tree));
        match rows[0].props.get("item") {
            Some(Value::Ref(r)) => assert_eq!(r.target.doc, m.list.doc, "an item of the list"),
            other => panic!("a row of the todo card shows an item, not {other:?}"),
        }
        assert_eq!(
            cards[1].props.get("count"),
            Some(&Value::Int(0)),
            "the notes card counts the pinned notes, none yet"
        );
    }

    #[test]
    fn the_morning_cleanup_asks_for_every_foreign_change_and_replays_without_repeating() {
        let apps = apps();
        let m = morning(&apps);
        add_todo(&m, &apps, "a", true);
        add_todo(&m, &apps, "b", false);
        add_todo(&m, &apps, "c", true);

        let view = m.w.view(&apps.today, "today", vec![res(&m.today)]);
        let button = button_labelled(view.tree(), "Morning cleanup").key.clone();
        let mut out = FlowOutcome::Suspended(suggested(view.press(&button).unwrap()));
        let mut asked = Vec::new();
        let finished = loop {
            match out {
                FlowOutcome::Suspended(s) => {
                    assert_eq!(
                        s.chain,
                        vec![m.w.id_of(5).clone()],
                        "Today wrote these invocations"
                    );
                    asked.push(s.action.clone());
                    out = m.w.rt.resolve(&s.run, &me(), Resolution::Approve).unwrap();
                }
                FlowOutcome::Done(v) => break v,
            }
        };
        assert_eq!(finished, Value::Int(2));
        assert_eq!(
            asked,
            [
                apps.todo.action("remove"),
                apps.todo.action("remove"),
                apps.notes.action("new_note"),
                apps.notes.action("retitle"),
                apps.notes.action("tag"),
            ],
            "one question per foreign action, in flow order"
        );
        assert_eq!(items(&m.w, &m.list, "items").len(), 1);
        let notes = items(&m.w, &m.notebook, "items");
        assert_eq!(notes.len(), 1, "replays did not file a second note");
        assert_eq!(text_of(&m.w, &notes[0], "title"), "Morning: 2 done");
        assert_eq!(
            m.w.rt.read(&notes[0], "tags").unwrap(),
            Value::List(vec![Value::text("morning")])
        );
        let removes =
            m.w.rt
                .history(&m.list.doc)
                .unwrap()
                .iter()
                .filter(|n| n.action == apps.todo.action("remove"))
                .count();
        assert_eq!(removes, 2);
    }

    #[test]
    fn a_trusted_board_runs_the_cleanup_without_asking() {
        let apps = apps();
        let m = morning(&apps);
        add_todo(&m, &apps, "a", true);
        m.w.auth.trust(m.w.id_of(5));
        let flow = apps.today.flow("morning_cleanup");
        let out =
            m.w.rt
                .run_flow(&me(), &flow, vec![res(&m.list), res(&m.notebook)])
                .unwrap();
        assert_eq!(out, FlowOutcome::Done(Value::Int(1)));
        assert!(items(&m.w, &m.list, "items").is_empty());
    }

    // ----- every app at every width -------------------------------------------------------------

    fn at(v: &mut ViewInstance, width: f64) -> UiNode {
        v.set_space("", Space::from_rem(width, 40.0));
        v.poll_patches().unwrap();
        v.tree().clone()
    }

    const COMPACT: f64 = 24.0;
    const WIDE: f64 = 90.0;

    #[test]
    fn counter_shows_its_value_and_the_buttons_that_change_it_at_every_width() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let c = w.create("Counter");
        let mut v = w.view(&apps.counter, "counter", vec![res(&c)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        for tree in [&compact, &wide] {
            assert_eq!(tree.kind, "ui.NavShell");
            let tree = content(tree);
            let shown = of_kind(tree, "ui.Text")[0];
            // A number that changes: a title in tabular figures.
            assert_eq!(shown.props["role"], Value::text("title"));
            assert_eq!(shown.props["numeric"], Value::Int(1));
            let plus = button_labelled(tree, "Increment");
            assert_eq!(plus.props["role"], Value::text("primary"));
            assert_eq!(plus.props["icon"], Value::text("plus"));
            assert_eq!(
                button_labelled(tree, "Decrement").props["icon"],
                Value::text("minus")
            );
        }
        let plus = button_labelled(content(&wide), "Increment").key.clone();
        v.press(&plus).unwrap();
        v.poll_patches().unwrap();
        assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(1));
        assert_eq!(
            text_prop(of_kind(content(v.tree()), "ui.Text")[0], "value"),
            "1"
        );
    }

    #[test]
    fn todo_is_one_row_per_item_at_every_width() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let list = w.create("Tasks");
        for t in ["a", "b"] {
            w.call(&apps.todo, "add", vec![res(&list), Value::text(t)]);
        }
        let mut v = w.view(&apps.todo, "home", vec![res(&list)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        for tree in [&compact, &wide] {
            assert!(of_kind(tree, "grid").is_empty());
            assert_eq!(
                of_kind(tree, "ui.TextInput").len(),
                1,
                "the quick add alone"
            );
            let rows = of_kind(tree, "ui.ListRow");
            assert_eq!(rows.len(), 2);
            assert!(
                rows.iter().all(|r| r.binding.is_some()),
                "each row's checkbox is bound"
            );
        }
    }

    #[test]
    fn the_new_item_action_adds_an_item_that_opens_in_its_editor() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let list = w.create("Tasks");
        let mut v = w.view(&apps.todo, "home", vec![res(&list)]);
        assert_eq!(
            text_prop(of_kind(v.tree(), "ui.EmptyState")[0], "title"),
            "No tasks here"
        );
        let item = returned_ref(w.call(&apps.todo, "new_task", vec![res(&list)]));
        v.poll_patches().unwrap();
        assert_eq!(item_titled(v.tree(), ""), item);
        let editor = w.rt.open_resource(&me(), &item, None).unwrap();
        assert_eq!(text_prop(editor.tree(), "content_kind"), "document");
        let title = of_kind(editor.tree(), "ui.TextInput")[0].key.clone();
        editor.edit(&title, &Value::text("bread")).unwrap();
        v.poll_patches().unwrap();
        assert_eq!(row_titles(v.tree()), ["bread"]);
    }

    #[test]
    fn an_action_whose_requirement_fails_is_not_offered_and_is_refused_with_its_reason() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let list = w.create("Tasks");
        let item = returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text("milk")]));
        let labels = |v: &ViewInstance| -> Vec<String> {
            of_kind(v.tree(), "ui.Button")
                .iter()
                .map(|button| text_prop(button, "label"))
                .collect()
        };
        let mut v = w.view(&apps.todo, "home", vec![res(&list)]);
        assert!(
            !labels(&v).contains(&"Clear done".to_owned()),
            "nothing is done, so the header offers no Clear done: {:?}",
            labels(&v)
        );
        let refused =
            w.rt.invoke_action(
                &me(),
                (apps.todo.action("clear_done").as_str(), vec![res(&list)]),
            )
            .expect_err("clear_done is refused while nothing is done");
        assert!(
            refused.to_string().contains("Nothing is done yet"),
            "{refused}"
        );
        w.call(&apps.todo, "check", vec![res(&item)]);
        v.poll_patches().unwrap();
        assert!(
            labels(&v).contains(&"Clear done".to_owned()),
            "once an item is done, Clear done is offered: {:?}",
            labels(&v)
        );
    }

    #[test]
    fn clearing_done_removes_only_the_finished_items() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let list = w.create("Tasks");
        for (t, done) in [("milk", true), ("eggs", false), ("rice", true)] {
            let item = returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text(t)]));
            if done {
                w.call(&apps.todo, "check", vec![res(&item)]);
            }
        }
        let out = w.call(&apps.todo, "clear_done", vec![res(&list)]);
        assert!(matches!(out, InvokeOutcome::Done(Value::Int(2))), "{out:?}");
        let left: Vec<String> = items(&w, &list, "items")
            .iter()
            .map(|i| text_of(&w, i, "title"))
            .collect();
        assert_eq!(left, ["eggs"]);
    }

    #[test]
    fn notes_list_rows_carry_the_title_and_a_preview() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let nb = w.create("Notes");
        w.call(&apps.notes, "new_note", vec![res(&nb)]);
        let mut v = w.view(&apps.notes, "notebook", vec![res(&nb)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        for tree in [&compact, &wide] {
            let rows = of_kind(tree, "ui.ListRow");
            assert_eq!(rows.len(), 1);
            assert!(rows[0].props.contains_key("secondary"));
            assert!(
                of_kind(tree, "split").is_empty(),
                "list and detail are two windows, not a split"
            );
        }
    }

    #[test]
    fn letters_list_rows_carry_the_subject_and_the_recipient() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let desk = w.create("Desk");
        let d = returned_ref(w.call(&apps.letters, "new_draft", vec![res(&desk)]));
        let editor = w.view(&apps.letters, "editor", vec![res(&d)]);
        let to = of_kind(editor.tree(), "ui.TextInput")[0].key.clone();
        editor.edit(&to, &Value::text("bo@example.org")).unwrap();
        let mut v = w.view(&apps.letters, "desk", vec![res(&desk)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        for tree in [&compact, &wide] {
            let rows = of_kind(tree, "ui.ListRow");
            assert_eq!(rows.len(), 1);
            assert_eq!(text_prop(rows[0], "title"), "Untitled");
            assert_eq!(text_prop(rows[0], "secondary"), "bo@example.org");
            assert!(of_kind(tree, "split").is_empty());
        }
    }

    #[test]
    fn chat_names_each_messages_author_for_the_transcript_to_draw() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        post(&w, &apps, &conv, "person:ada", "hello", "");
        post(&w, &apps, &conv, "person:bo", "hi", "");
        let mut v = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        for tree in [&compact, &wide] {
            let who: Vec<Value> = of_kind(tree, "ui.ListItem")
                .iter()
                .map(|item| item.props["author"].clone())
                .collect();
            assert_eq!(
                who,
                [
                    Value::Principal(Principal::new("person:ada")),
                    Value::Principal(Principal::new("person:bo"))
                ]
            );
            assert_eq!(authors_of(tree), ["person:ada", "person:bo"]);
        }
    }

    #[test]
    fn board_is_a_grid_of_cards_at_every_width() {
        let apps = apps();
        let m = morning(&apps);
        let mut v = m.w.view(&apps.today, "today", vec![res(&m.today)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        for tree in [&compact, &wide] {
            let grid = of_kind(tree, "grid")[0];
            assert_eq!(grid.props["min.rem"], Value::Float(22.0));
            assert_eq!(grid.children.len(), 3, "one card per app");
            assert!(grid.children.iter().all(|c| c.kind == "ui.Card"));
        }
    }
}
