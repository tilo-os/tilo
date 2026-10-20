//! Every app under `apps/tl`, compiled with its committed ids and run end to end: chat with
//! embedded agent blocks, letters, the board's foreign views and flow, lists that open their
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
        Principal, Resolution, ResourceRef, Runtime, RuntimeError, SimpleAuthority, Space, UiNode,
        Value, ViewEvent, ViewInstance,
    };

    macro_rules! app_source {
        ($name:literal) => {
            (
                include_str!(concat!("../../../../apps/tl/", $name, "/app.tl")),
                include_str!(concat!("../../../../apps/tl/", $name, "/app.ids")),
            )
        };
    }

    /// Compile a package with its committed lock, which must already be complete: building
    /// assigns ids to anything new, and a lock that would change is a lock not committed.
    fn build(name: &str, (src, ids): (&str, &str), imports: &[&App]) -> App {
        let lock = IdsLock::parse(ids).unwrap_or_else(|e| panic!("{name}/app.ids: {e}"));
        let metas: Vec<tilolang::PackageMeta> = imports.iter().map(|a| a.meta().clone()).collect();
        let compiled = tilolang::compile_package(src, &lock, &metas)
            .unwrap_or_else(|d| panic!("{name}: {d:#?}"));
        assert_eq!(
            compiled.ids.to_text(),
            ids,
            "apps/tl/{name}/app.ids is out of date: run `tilolang build apps/tl/{name}`"
        );
        App { compiled }
    }

    struct Apps {
        notes: App,
        todo: App,
        chat: App,
        letters: App,
        counter: App,
        board: App,
    }

    fn apps() -> Apps {
        let notes = build("notes", app_source!("notes"), &[]);
        let todo = build("todo", app_source!("todo"), &[]);
        let chat = build("chat", app_source!("chat"), &[]);
        let letters = build("letters", app_source!("letters"), &[]);
        let counter = build("counter", app_source!("counter"), &[]);
        let board = build("board", app_source!("board"), &[&notes, &todo, &chat]);
        Apps {
            notes,
            todo,
            chat,
            letters,
            counter,
            board,
        }
    }

    struct World {
        rt: Runtime,
        auth: SimpleAuthority,
        sink: MemorySink,
        ids: Vec<PackageId>,
    }

    fn world(apps: &Apps, store: &AmStore) -> World {
        let auth = SimpleAuthority::new();
        let rt = Runtime::new(store.clone(), auth.clone()).unwrap();
        let sink = MemorySink::new();
        rt.set_sink(sink.clone());
        let ids = [
            &apps.notes,
            &apps.todo,
            &apps.chat,
            &apps.letters,
            &apps.counter,
            &apps.board,
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

        fn view(&self, app: &App, name: &str, args: Vec<Value>) -> ViewInstance {
            self.rt
                .open_view(&me(), &app.view(name), args)
                .unwrap_or_else(|e| panic!("{name}: {e}"))
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
                Value::Ref(r) => r.target,
                other => panic!("not a reference: {other:?}"),
            },
            other => panic!("the host has recorded no invocation: {other:?}"),
        }
    }

    fn start_chat(w: &World, apps: &Apps) -> (ResourceRef, ResourceRef) {
        let chat = w.create("Chat");
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
                Value::Ref(r) => r.target,
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

    fn captions(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "text")
            .into_iter()
            .filter(|t| t.props.get("style") == Some(&Value::text("caption")))
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    fn button_labelled<'a>(tree: &'a UiNode, label: &str) -> &'a UiNode {
        of_kind(tree, "button")
            .into_iter()
            .find(|b| b.props.get("label") == Some(&Value::text(label)))
            .unwrap_or_else(|| panic!("no button `{label}` in\n{}", dump(tree)))
    }

    /// The row of a list with this title.
    fn row_titled<'a>(tree: &'a UiNode, title: &str) -> &'a UiNode {
        of_kind(tree, "list_row")
            .into_iter()
            .find(|row| row.props.get("title") == Some(&Value::text(title)))
            .unwrap_or_else(|| panic!("no row `{title}` in\n{}", dump(tree)))
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
        assert_eq!(w.ids.len(), 6);
        for app in [&a.notes, &a.todo, &a.chat, &a.letters, &a.counter, &a.board] {
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
            captions(view.tree()),
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
        let inputs = of_kind(view.tree(), "input");
        let draft = inputs[1].key.clone();
        assert_eq!(
            view.edit(&draft, &Value::text("hello")).unwrap(),
            EditOutcome::Applied
        );
        let send = button_labelled(view.tree(), "Send").key.clone();
        assert!(matches!(view.press(&send).unwrap(), InvokeOutcome::Done(_)));
        assert_eq!(thread(&w, &conv).len(), 1);
        assert_eq!(text_of(&w, &conv, "draft"), "");
        view.poll_patches().unwrap();
        assert_eq!(
            captions(view.tree()),
            ["me"],
            "the composer posts as whoever is looking, as the runtime knows them"
        );
    }

    #[test]
    fn an_agents_block_that_is_a_notes_view_renders_that_view_inline_and_stays_live() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let notebook = w.create("Notebook");
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
        let title = of_kind(found[0], "input")[0];
        let binding = title.binding.clone().expect("the title input is bound");
        assert_eq!(binding.field().map(|(target, _)| target), Some(&note));
        assert_eq!(
            &binding.author,
            w.id_of(0),
            "the nested view is the notes package's code, not the agent's"
        );
        assert_eq!(text_prop(title, "placeholder"), "Title");

        let pin = button_labelled(found[0], "Pin").key.clone();
        let out = view.press(&pin).unwrap();
        assert!(
            matches!(out, InvokeOutcome::Done(_)),
            "the notes view's own button is allowed even inside an agent's message: {out:?}"
        );
        assert_eq!(w.rt.read(&note, "pinned").unwrap(), Value::Bool(true));
        view.poll_patches().unwrap();
        button_labelled(embeds(view.tree())[0], "Unpin");
    }

    #[test]
    fn a_block_that_is_a_value_renders_through_the_default_view() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let list = w.create("TodoList");
        w.call(
            &apps.todo,
            "rename",
            vec![res(&list), Value::text("Groceries")],
        );
        post(&w, &apps, &conv, "agent:helper", "", "1 + 2");
        post(
            &w,
            &apps,
            &conv,
            "agent:helper",
            "",
            &format!("@todo.TodoList:{}", list.id_string()),
        );
        let view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(view.tree());
        assert_eq!(
            found.iter().map(|e| state(e)).collect::<Vec<_>>(),
            ["data", "data"]
        );
        assert_eq!(text_prop(of_kind(found[0], "text")[0], "value"), "3");
        assert_eq!(
            text_prop(of_kind(found[1], "heading")[0], "value"),
            "Groceries"
        );
    }

    #[test]
    fn a_block_that_does_not_compile_renders_its_diagnostics_and_the_chat_goes_on() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        post(&w, &apps, &conv, "agent:helper", "see", "todo.nothing(1)");
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
        assert_eq!(captions(view.tree()), ["agent:helper", "person:ada"]);
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
        let inputs: Vec<String> = of_kind(editor.tree(), "input")
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
        assert_eq!(text_prop(inv, "app"), "so.tilo.lang.letters");
        let run = text_prop(inv, "run");
        assert_eq!(
            text_prop(of_kind(inv, "heading")[0], "value"),
            "Send to bo@example.org",
            "the proposal view shows the letter: {}",
            dump(inv)
        );
        assert!(w.rt.pending_outbox().is_empty(), "asking sends nothing");

        // A re-render shows the same suggestion rather than making another.
        assert!(view.set_space("", Space::from_rem(20.0, 40.0)));
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
            of_kind(view.tree(), "heading")
                .iter()
                .all(|h| text_prop(h, "value") != "Send to bo@example.org"),
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
        let list = w.create("TodoList");
        let first = returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text("milk")]));
        let second = returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text("eggs")]));
        let remove = |item: &ResourceRef| {
            format!(
                "button(\"Remove\", on: todo.remove(@todo.TodoList:{}, @todo.Todo:{}))",
                list.id_string(),
                item.id_string()
            )
        };
        post(&w, &apps, &conv, "person:stranger", "", &remove(&first));
        let chat_view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(chat_view.tree());
        assert_eq!(state(found[0]), "view");
        let button = of_kind(found[0], "button")[0].key.clone();
        let s = suggested(chat_view.press(&button).unwrap());
        assert_eq!(s.action, apps.todo.action("remove"));
        assert_eq!(
            s.chain,
            vec![ModuleHash::principal("person:stranger")],
            "the chain names the principal who wrote the block"
        );
        assert_eq!(items(&w, &list, "items").len(), 2, "asking changed nothing");

        // The same action from the todo app's own view runs without asking.
        let own = w.view(&apps.todo, "todo_list", vec![res(&list)]);
        let own_remove = of_kind(own.tree(), "button")
            .into_iter()
            .find(|b| b.props.get("label") == Some(&Value::text("Remove")))
            .unwrap()
            .key
            .clone();
        let out = own.press(&own_remove).unwrap();
        assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
        assert_eq!(items(&w, &list, "items"), vec![second.clone()]);

        // Trusting the principal covers every block it writes, from the next one on.
        w.auth.trust_principal("person:stranger");
        let third = returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text("bread")]));
        post(&w, &apps, &conv, "person:stranger", "", &remove(&third));
        let chat_view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(chat_view.tree());
        let button = of_kind(found[1], "button")[0].key.clone();
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
        let list = wa.create("TodoList");
        wa.auth.trust_principal("agent:helper");
        let src = format!("todo.add(@todo.TodoList:{}, \"bread\")", list.id_string());
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
        let list = w.create("TodoList");
        w.auth.trust_principal("agent:helper");
        let src = format!("todo.add(@todo.TodoList:{}, \"spam\")", list.id_string());

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
        let inputs: Vec<String> = of_kind(editor.tree(), "input")
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
            let view = wb
                .rt
                .open_view(who, &apps.chat.view("thread"), vec![res(&conv)])
                .unwrap();
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
        let view =
            w.rt.open_view(&ada, &apps.chat.view("thread"), vec![res(&conv)])
                .unwrap();
        let draft = of_kind(view.tree(), "input")[1].key.clone();
        view.edit(&draft, &Value::text("hello")).unwrap();
        let send = button_labelled(view.tree(), "Send").key.clone();
        assert!(matches!(view.press(&send).unwrap(), InvokeOutcome::Done(_)));
        assert_eq!(thread(&w, &conv)[0].0, "person:ada");
    }

    // ----- windows: open(r), window state and lenses ------------------------------------------

    fn opens(v: &mut ViewInstance) -> Vec<(ResourceRef, Option<String>)> {
        v.poll_events()
            .into_iter()
            .map(|e| match e {
                ViewEvent::Open { target, view } => (target, view),
            })
            .collect()
    }

    // USER REQUIREMENT (2026-10-20): "notes = a notebook list view (`root Notebook`, list rows
    // that `open(n)`) plus a `default view note_editor for Note`; no `open` field in Notebook
    // anymore".
    #[test]
    fn a_notebook_row_opens_its_note_in_a_window_of_its_own() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let nb = w.create("Notebook");
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
        let mut list = w.view(&apps.notes, "notebook", vec![res(&nb)]);
        assert_eq!(list.window(), nb.id_string(), "a window is its resource's");
        let before = w.rt.history(&nb.doc).unwrap().len();
        let row = row_titled(list.tree(), "Plans").key.clone();
        assert!(matches!(list.press(&row).unwrap(), InvokeOutcome::Done(_)));
        assert_eq!(opens(&mut list), vec![(note.clone(), None)]);
        assert_eq!(
            w.rt.history(&nb.doc).unwrap().len(),
            before,
            "opening changed nothing"
        );

        let editor = w.rt.open_resource(&me(), &note, None).unwrap();
        assert_eq!(editor.window(), note.id_string());
        let title = of_kind(editor.tree(), "input")[0];
        assert_eq!(text_prop(title, "placeholder"), "Title");
        assert_eq!(
            w.rt.default_view(&note.ty).as_deref(),
            Some(apps.notes.view("note_editor").as_str())
        );
    }

    // USER REQUIREMENT (2026-10-20): "window state stored per view instance/window id (shared by
    // all principals viewing it), lens per (principal, window) ... two principals on one view
    // share window state but not lens (test)."
    #[test]
    fn two_people_in_one_window_share_its_window_state_but_not_their_lenses() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let nb = w.create("Notebook");
        for title in ["b", "a"] {
            let n = returned_ref(w.call(&apps.notes, "new_note", vec![res(&nb)]));
            w.call(&apps.notes, "retitle", vec![res(&n), Value::text(title)]);
        }
        let (ada, bo) = (Principal::new("person:ada"), Principal::new("person:bo"));
        let open = |who: &Principal| {
            w.rt.open_window(
                who,
                "window-1",
                &apps.notes.view("notebook"),
                vec![res(&nb)],
            )
            .unwrap()
        };
        let (mut va, mut vb) = (open(&ada), open(&bo));
        let titles = |v: &ViewInstance| -> Vec<String> {
            of_kind(v.tree(), "list_row")
                .into_iter()
                .map(|row| text_prop(row, "title"))
                .collect()
        };
        assert_eq!(titles(&va), ["b", "a"]);

        // Window state: Ada sorts the window, and Bo's view of it sorts too.
        let sort = button_labelled(va.tree(), "Sort by title").key.clone();
        va.press(&sort).unwrap();
        assert!(
            va.is_stale() && vb.is_stale(),
            "both read the window's order"
        );
        va.poll_patches().unwrap();
        vb.poll_patches().unwrap();
        assert_eq!(titles(&va), ["a", "b"]);
        assert_eq!(titles(&vb), ["a", "b"]);

        // Lens: Ada expands a row; Bo's rows stay as they were.
        let more = |v: &ViewInstance| {
            of_kind(v.tree(), "button")
                .into_iter()
                .filter(|b| text_prop(b, "label") == "Show actions")
                .map(|b| b.key.clone())
                .collect::<Vec<_>>()
        };
        let first = more(&va)[0].clone();
        va.press(&first).unwrap();
        assert!(!vb.is_stale(), "Bo did not read Ada's lens");
        va.poll_patches().unwrap();
        assert_eq!(more(&va).len(), 1, "one of Ada's rows is expanded");
        button_labelled(va.tree(), "Delete");
        assert_eq!(more(&vb).len(), 2, "none of Bo's");

        // Another window over the same notebook has state of its own.
        let elsewhere =
            w.rt.open_window(
                &ada,
                "window-2",
                &apps.notes.view("notebook"),
                vec![res(&nb)],
            )
            .unwrap();
        assert_eq!(titles(&elsewhere), ["b", "a"]);
        assert_eq!(more(&elsewhere).len(), 2);
    }

    #[test]
    fn a_lists_filter_is_window_state_and_changes_nothing_in_the_list() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let list = w.create("TodoList");
        for (t, done) in [("milk", true), ("eggs", false)] {
            let item = returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text(t)]));
            if done {
                w.call(&apps.todo, "toggle", vec![res(&item)]);
            }
        }
        let mut v = w.view(&apps.todo, "todo_list", vec![res(&list)]);
        assert_eq!(of_kind(v.tree(), "checkbox").len(), 2);
        let before = w.rt.history(&list.doc).unwrap().len();
        let open = button_labelled(v.tree(), "Open").key.clone();
        v.press(&open).unwrap();
        v.poll_patches().unwrap();
        let shown: Vec<String> = of_kind(v.tree(), "checkbox")
            .into_iter()
            .map(|c| text_prop(c, "label"))
            .collect();
        assert_eq!(shown, ["eggs"]);
        assert_eq!(
            button_labelled(v.tree(), "Open").props.get("style"),
            Some(&Value::text("primary")),
            "the chosen filter is a run-time choice of the button's style"
        );
        assert_eq!(
            w.rt.history(&list.doc).unwrap().len(),
            before,
            "view state is no change to the list"
        );
    }

    // ----- board ------------------------------------------------------------------------------

    struct Morning {
        w: World,
        board: ResourceRef,
        list: ResourceRef,
        notebook: ResourceRef,
    }

    fn morning(apps: &Apps) -> Morning {
        let w = world(apps, &AmStore::new());
        let board = w.create("Board");
        let notebook = w.create("Notebook");
        let list = w.create("TodoList");
        let chat = w.create("Chat");
        w.call(
            &apps.board,
            "attach",
            vec![res(&board), res(&notebook), res(&list), res(&chat)],
        );
        Morning {
            w,
            board,
            list,
            notebook,
        }
    }

    fn add_todo(m: &Morning, apps: &Apps, title: &str, done: bool) {
        let item =
            returned_ref(m.w.call(&apps.todo, "add", vec![res(&m.list), Value::text(title)]));
        if done {
            m.w.call(&apps.todo, "toggle", vec![res(&item)]);
        }
    }

    #[test]
    fn the_boards_views_apply_other_packages_views() {
        let apps = apps();
        let m = morning(&apps);
        add_todo(&m, &apps, "milk", false);
        m.w.call(&apps.notes, "new_note", vec![res(&m.notebook)]);
        let view = m.w.view(&apps.board, "board", vec![res(&m.board)]);
        let tree = view.tree();
        assert_eq!(
            of_kind(tree, "heading")
                .iter()
                .map(|h| text_prop(h, "value"))
                .collect::<Vec<_>>(),
            ["Today", "Todo (1 open)", "Notes", "Chats"],
            "{}",
            dump(tree)
        );
        assert_eq!(of_kind(tree, "checkbox").len(), 1, "the todo app's rows");
    }

    #[test]
    fn the_morning_cleanup_asks_for_every_foreign_change_and_replays_without_repeating() {
        let apps = apps();
        let m = morning(&apps);
        add_todo(&m, &apps, "a", true);
        add_todo(&m, &apps, "b", false);
        add_todo(&m, &apps, "c", true);

        let view = m.w.view(&apps.board, "board", vec![res(&m.board)]);
        let button = button_labelled(view.tree(), "Morning cleanup").key.clone();
        let mut out = FlowOutcome::Suspended(suggested(view.press(&button).unwrap()));
        let mut asked = Vec::new();
        let finished = loop {
            match out {
                FlowOutcome::Suspended(s) => {
                    assert_eq!(
                        s.chain,
                        vec![m.w.id_of(5).clone()],
                        "the board wrote these invocations"
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
        let notes = items(&m.w, &m.notebook, "notes");
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
        let flow = apps.board.flow("morning_cleanup");
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

    fn differ(compact: &UiNode, wide: &UiNode) {
        assert_ne!(
            dump(compact),
            dump(wide),
            "the layout reads `space`, so the trees differ"
        );
    }

    #[test]
    fn counter_is_centred_with_a_display_value_and_large_buttons_at_every_width() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let c = w.create("Counter");
        let mut v = w.view(&apps.counter, "counter", vec![res(&c)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        for tree in [&compact, &wide] {
            let shown = of_kind(tree, "text")[0];
            assert_eq!(shown.props["style"], Value::text("display"));
            let plus = button_labelled(tree, "+1");
            assert_eq!(plus.props["style"], Value::text("primary"));
            assert_eq!(plus.props["size"], Value::text("large"));
            assert_eq!(
                button_labelled(tree, "-1").props["size"],
                Value::text("large")
            );
        }
        let plus = button_labelled(&wide, "+1").key.clone();
        v.press(&plus).unwrap();
        v.poll_patches().unwrap();
        assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(1));
        assert_eq!(text_prop(of_kind(v.tree(), "text")[0], "value"), "1");
    }

    #[test]
    fn todo_is_one_row_per_item_at_every_width() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let list = w.create("TodoList");
        for t in ["a", "b"] {
            w.call(&apps.todo, "add", vec![res(&list), Value::text(t)]);
        }
        let mut v = w.view(&apps.todo, "todo_list", vec![res(&list)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        // One row per item at every width, also inside the card of another view.
        assert!(of_kind(&compact, "grid").is_empty());
        assert!(of_kind(&wide, "grid").is_empty());
        assert_eq!(of_kind(&compact, "input").len(), 1);
        assert_eq!(of_kind(&wide, "input").len(), 2);
        assert_eq!(of_kind(&compact, "checkbox").len(), 2);
        assert_eq!(of_kind(&wide, "checkbox").len(), 2);
    }

    #[test]
    fn typing_an_item_and_adding_it_appends_and_clears_the_entry() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let list = w.create("TodoList");
        let mut v = w.view(&apps.todo, "todo_list", vec![res(&list)]);
        let entry = of_kind(v.tree(), "input")[0].key.clone();
        v.edit(&entry, &Value::text("bread")).unwrap();
        let add = button_labelled(v.tree(), "Add").key.clone();
        assert!(matches!(v.press(&add).unwrap(), InvokeOutcome::Done(_)));
        v.poll_patches().unwrap();
        assert_eq!(of_kind(v.tree(), "checkbox").len(), 1);
        assert_eq!(text_of(&w, &list, "draft"), "");
    }

    #[test]
    fn notes_list_rows_carry_the_title_and_a_preview() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let nb = w.create("Notebook");
        w.call(&apps.notes, "new_note", vec![res(&nb)]);
        let mut v = w.view(&apps.notes, "notebook", vec![res(&nb)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        for tree in [&compact, &wide] {
            let rows = of_kind(tree, "list_row");
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
        w.call(&apps.letters, "new_draft", vec![res(&desk)]);
        let mut v = w.view(&apps.letters, "desk", vec![res(&desk)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        for tree in [&compact, &wide] {
            assert_eq!(of_kind(tree, "list_row").len(), 1);
            assert!(of_kind(tree, "split").is_empty());
        }
    }

    #[test]
    fn chat_puts_the_author_beside_the_message_only_when_there_is_room() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        post(&w, &apps, &conv, "person:ada", "hello", "");
        let mut v = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        differ(&compact, &wide);
        let author_row = |tree: &UiNode| {
            of_kind(tree, "text")
                .into_iter()
                .find(|t| t.props.get("style") == Some(&Value::text("caption")))
                .map(|t| t.props.get("min.rem").cloned())
        };
        assert_eq!(author_row(&compact), Some(None));
        assert_eq!(author_row(&wide), Some(Some(Value::Float(8.0))));
    }

    #[test]
    fn board_is_a_stack_when_compact_and_a_grid_when_wide() {
        let apps = apps();
        let m = morning(&apps);
        let mut v = m.w.view(&apps.board, "board", vec![res(&m.board)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        differ(&compact, &wide);
        assert!(of_kind(&compact, "grid").is_empty());
        let grid = of_kind(&wide, "grid")[0];
        assert_eq!(grid.props["min.rem"], Value::Float(22.0));
        assert_eq!(grid.children.len(), 3, "one cell per app");
        assert_eq!(grid.children[0].props["span"], Value::Int(2));
        assert_eq!(
            of_kind(&compact, "heading").len(),
            4,
            "the same four panels in both"
        );
    }
}
