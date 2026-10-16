//! The block actions of [`block_actions`](crate::block_actions), one by one:
//! what each writes, what it answers, how it is refused, and that each is one
//! attributed edit that replays and merges.

use serde_json::json;
use tilo_resource::Replica;

use super::*;

fn call(
    replica: &mut Replica<RichText>,
    id: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, String> {
    call_as(replica, "ada", id, arguments)
}

/// `call`, invoked by `writer`, whose name the text it writes carries.
fn call_as(
    replica: &mut Replica<RichText>,
    writer: &str,
    id: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, String> {
    call_enveloped(replica, writer, id, arguments).map(|(answer, _)| answer)
}

fn call_enveloped(
    replica: &mut Replica<RichText>,
    writer: &str,
    id: &str,
    arguments: serde_json::Value,
) -> Result<(serde_json::Value, tilo_actions::ActionEnvelope), String> {
    let serde_json::Value::Object(arguments) = arguments else {
        return Err("arguments are an object".to_owned());
    };
    let action = tilo_resource::build_action::<RichText>(id, &arguments, 0)?;
    let before = replica.heads();
    let encoded = tilo_actions::EncodedAction::encode(RICH_TEXT_ACTION_TYPE, &action)
        .map_err(|error| error.to_string())?;
    let context = tilo_actions::ActionContext::for_resource(id, RichText::KIND, RichText::LABEL)
        .with_invoker(tilo_actions::ActionInvoker::actor(writer));
    let envelope = tilo_actions::apply_local_with_context(
        replica.doc_mut(),
        &action,
        encoded,
        Some(writer.as_bytes()),
        context,
    )
    .map_err(|error| error.to_string())?;
    let after = replica.heads();
    let changes = replica.doc_mut().diff(&before, &after);
    let answer = block_actions::action_result_under(replica.doc(), &automerge::ROOT, id, &changes)
        .ok_or_else(|| format!("{id} answers"))?;
    Ok((answer, envelope))
}

fn read(replica: &Replica<RichText>) -> String {
    block_actions::read_markdown_under(replica.doc(), &automerge::ROOT).unwrap()
}

/// The page as its lines, ids left off.
fn page(replica: &Replica<RichText>) -> Vec<String> {
    read(replica)
        .lines()
        .map(|line| match line.rsplit_once(" ^") {
            Some((text, _)) => text.to_owned(),
            None => line.to_owned(),
        })
        .collect()
}

/// The id of the block whose line starts with `prefix`, as the read shows it.
fn id_of(replica: &Replica<RichText>, prefix: &str) -> String {
    read(replica)
        .lines()
        .find(|line| line.trim_start().starts_with(prefix))
        .and_then(|line| line.rsplit_once(" ^"))
        .map_or_else(
            || panic!("no line starts with {prefix:?} in\n{}", read(replica)),
            |(_, id)| id.to_owned(),
        )
}

fn blocks(replica: &Replica<RichText>) -> Vec<tilo_doc::snapshot::BlockView> {
    let c = tilo_doc::model::open(replica.doc(), &automerge::ROOT).unwrap();
    tilo_doc::model::materialize_region(replica.doc(), &c, tilo_doc::model::Region::Body)
}

fn changes(replica: &mut Replica<RichText>) -> usize {
    replica.doc_mut().get_changes(&[]).len()
}

fn seeded(markdown: &str) -> Replica<RichText> {
    let mut replica = Replica::<RichText>::new().as_author("ada");
    call(
        &mut replica,
        "insert-markdown",
        json!({ "markdown": markdown }),
    )
    .unwrap();
    replica
}

#[test]
fn a_page_reads_as_markdown_with_an_id_on_every_line() {
    let replica = seeded(
        "# Todo\nbuy **oat** milk\n- [ ] eggs\n  - [x] free range\n> remember bags\n```rust\nlet a = 1;\n```\n---",
    );
    assert_eq!(
        page(&replica),
        [
            "# Todo",
            "buy **oat** milk",
            "- [ ] eggs",
            "  - [x] free range",
            "> remember bags",
            "```rust",
            "let a = 1;",
            "```",
            "---",
        ]
    );
    let ids: Vec<String> = read(&replica)
        .lines()
        .filter_map(|line| line.rsplit_once(" ^").map(|(_, id)| id.to_owned()))
        .collect();
    assert_eq!(ids.len(), 7, "{}", read(&replica));
    assert!(ids.iter().all(|id| id.len() <= 8), "short ids: {ids:?}");
    let one = block_actions::read_under(replica.doc(), &automerge::ROOT, Some(&ids[2])).unwrap();
    assert_eq!(one["markdown"], format!("- [ ] eggs ^{}\n", ids[2]));
}

#[test]
fn a_block_of_any_kind_is_inserted_after_an_id_or_at_the_ends() {
    let mut replica = Replica::<RichText>::new().as_author("ada");
    // The new body's empty paragraph is replaced, not left above the text.
    let first = call(
        &mut replica,
        "insert-block",
        json!({ "text": "Groceries", "kind": "heading" }),
    )
    .unwrap();
    assert_eq!(first["created"].as_array().unwrap().len(), 1, "{first}");
    assert_eq!(page(&replica), ["# Groceries"]);
    let heading = id_of(&replica, "# Groceries");

    call(
        &mut replica,
        "insert-block",
        json!({ "kind": "checklist", "text": "milk", "done": true }),
    )
    .unwrap();
    call(
        &mut replica,
        "insert-block",
        json!({ "kind": "checklist", "text": "eggs", "indent": 1 }),
    )
    .unwrap();
    call(
        &mut replica,
        "insert-block",
        json!({ "kind": "numbered", "text": "one", "after": heading }),
    )
    .unwrap();
    call(
        &mut replica,
        "insert-block",
        json!({ "kind": "quote", "text": "first", "position": "start" }),
    )
    .unwrap();
    call(
        &mut replica,
        "insert-block",
        json!({ "kind": "code", "text": "x = 1", "lang": "py" }),
    )
    .unwrap();
    call(
        &mut replica,
        "insert-block",
        json!({ "kind": "bullet", "text": "last" }),
    )
    .unwrap();
    assert_eq!(
        page(&replica),
        [
            "> first",
            "# Groceries",
            "1. one",
            "- [x] milk",
            "  - [ ] eggs",
            "```py",
            "x = 1",
            "```",
            "- last",
        ]
    );

    for refused in [
        json!({ "kind": "table", "text": "x" }),
        json!({ "kind": "heading", "level": 9, "text": "x" }),
        json!({ "text": "x", "after": "nope" }),
        json!({ "text": "x", "position": "middle" }),
    ] {
        let error = call(&mut replica, "insert-block", refused.clone()).unwrap_err();
        assert!(!error.is_empty(), "{refused} is refused");
    }
}

#[test]
fn markdown_is_inserted_as_the_blocks_it_says_with_their_marks() {
    let mut replica = seeded("intro");
    let intro = id_of(&replica, "intro");
    let answer = call(
        &mut replica,
        "insert-markdown",
        json!({ "markdown": "## Plan\n- a **b** c\n- [ ] d", "after": intro }),
    )
    .unwrap();
    assert_eq!(answer["created"].as_array().unwrap().len(), 3);
    assert_eq!(
        page(&replica),
        ["intro", "## Plan", "- a **b** c", "- [ ] d"]
    );
    let bold = blocks(&replica)
        .into_iter()
        .find(|block| block.text == "a b c")
        .unwrap();
    assert_eq!(
        bold.marks
            .iter()
            .filter(|mark| mark.name == "strong")
            .count(),
        1
    );

    assert!(
        call(
            &mut replica,
            "insert-markdown",
            json!({ "markdown": "| a | b |\n| - | - |" })
        )
        .unwrap_err()
        .contains("table")
    );
    assert!(
        call(
            &mut replica,
            "insert-markdown",
            json!({ "markdown": "  \n\n" })
        )
        .unwrap_err()
        .contains("no blocks")
    );
}

#[test]
fn block_text_written_as_the_read_shows_it_means_what_its_markdown_says() {
    let mut replica = seeded("[tilo OS](search for the tilo homepage)");
    let id = id_of(&replica, "\\[tilo OS");
    // The read escapes the brackets that are no link; text taken from it
    // writes them back as they were, not with the escapes in them.
    let shown = page(&replica).remove(0);
    assert_eq!(shown, "\\[tilo OS\\](search for the tilo homepage)");
    call(
        &mut replica,
        "set-block-text",
        json!({ "block": id, "text": format!("{shown} today") }),
    )
    .unwrap();
    assert_eq!(
        blocks(&replica)[0].text,
        "[tilo OS](search for the tilo homepage) today"
    );

    // A link written as Markdown, with nothing said about it, is a link.
    call(
        &mut replica,
        "set-block-text",
        json!({ "block": id, "text": "[tilo OS](https://tilo.so/)" }),
    )
    .unwrap();
    let block = &blocks(&replica)[0];
    assert_eq!(block.text, "tilo OS");
    assert!(
        block
            .marks
            .iter()
            .any(|mark| mark.name == "link" && (mark.start, mark.end) == (0, 7)),
        "{:?}",
        block.marks
    );
    assert_eq!(page(&replica), ["[tilo OS](https://tilo.so/)"]);

    // `markdown: false` writes the characters as they are.
    call(
        &mut replica,
        "set-block-text",
        json!({ "block": id, "text": "[a](https://tilo.so/)", "markdown": false }),
    )
    .unwrap();
    assert_eq!(blocks(&replica)[0].text, "[a](https://tilo.so/)");
}

#[test]
fn block_text_changes_only_the_characters_that_differ_and_is_attributed() {
    let mut replica = seeded("buy **oat** milk");
    let id = id_of(&replica, "buy");
    let signed = |replica: &Replica<RichText>| -> Vec<(usize, usize)> {
        tilo_doc::author_runs(&blocks(replica)[0].marks)
            .into_iter()
            .map(|(start, end, _)| (start, end))
            .collect()
    };
    assert_eq!(signed(&replica), [(0, 12)]);

    // Plain text leaves the marks where they are.
    let mut bob = replica.fork();
    let answer = call_as(
        &mut bob,
        "bob",
        "set-block-text",
        json!({ "block": id, "text": "buy oat milk today" }),
    )
    .unwrap();
    assert_eq!(answer["changed"].as_array().unwrap().len(), 1, "{answer}");
    assert_eq!(page(&bob), ["buy **oat** milk today"]);
    let runs = tilo_doc::author_runs(&blocks(&bob)[0].marks);
    assert!(
        runs.iter().any(|(_, _, who)| who == "bob"),
        "the words bob typed are his: {runs:?}"
    );
    assert!(
        runs.iter().any(|(_, _, who)| who == "ada"),
        "the words he left are still ada's: {runs:?}"
    );

    // Markdown text sets the marks too.
    call_as(
        &mut bob,
        "bob",
        "set-block-text",
        json!({ "block": id, "text": "buy _oat_ milk today", "markdown": true }),
    )
    .unwrap();
    assert_eq!(page(&bob), ["buy _oat_ milk today"]);
    assert!(
        call(
            &mut bob,
            "set-block-text",
            json!({ "block": "zzzz", "text": "x" })
        )
        .unwrap_err()
        .contains("no block")
    );
}

#[test]
fn a_block_changes_kind_keeping_its_text() {
    let mut replica = seeded("milk\neggs");
    let milk = id_of(&replica, "milk");
    call(
        &mut replica,
        "set-block-kind",
        json!({ "block": milk, "kind": "checklist", "done": true }),
    )
    .unwrap();
    assert_eq!(page(&replica), ["- [x] milk", "eggs"]);
    call(
        &mut replica,
        "set-block-kind",
        json!({ "block": milk, "kind": "heading", "level": 2 }),
    )
    .unwrap();
    let eggs = id_of(&replica, "eggs");
    call(
        &mut replica,
        "set-block-kind",
        json!({ "block": eggs, "kind": "numbered", "indent": 2 }),
    )
    .unwrap();
    assert_eq!(page(&replica), ["## milk", "    1. eggs"]);
    call(
        &mut replica,
        "set-block-kind",
        json!({ "block": milk, "kind": "paragraph" }),
    )
    .unwrap();
    let stored = &blocks(&replica)[0];
    assert_eq!(stored.kind, "paragraph");
    assert!(
        !stored.attrs.contains_key("level") && !stored.attrs.contains_key("checked"),
        "the kind it left leaves nothing behind: {:?}",
        stored.attrs
    );
}

#[test]
fn an_item_is_ticked_unticked_toggled_and_a_plain_line_is_refused() {
    let mut replica = seeded("- [ ] milk\n- eggs\nplain");
    let milk = id_of(&replica, "- [ ] milk");
    let eggs = id_of(&replica, "- eggs");
    call(&mut replica, "set-block-kind", json!({ "block": milk })).unwrap();
    assert_eq!(page(&replica)[0], "- [x] milk");
    call(&mut replica, "set-block-kind", json!({ "block": milk })).unwrap();
    assert_eq!(page(&replica)[0], "- [ ] milk");
    call(
        &mut replica,
        "set-block-kind",
        json!({ "block": milk, "done": true }),
    )
    .unwrap();
    call(
        &mut replica,
        "set-block-kind",
        json!({ "block": milk, "done": true }),
    )
    .unwrap();
    assert_eq!(page(&replica)[0], "- [x] milk");
    call(
        &mut replica,
        "set-block-kind",
        json!({ "block": eggs, "done": true }),
    )
    .unwrap();
    assert_eq!(page(&replica)[1], "- [x] eggs");
    let plain = id_of(&replica, "plain");
    let error = call(&mut replica, "set-block-kind", json!({ "block": plain })).unwrap_err();
    assert!(error.contains("not a list item"), "{error}");
}

#[test]
fn an_item_is_indented_and_outdented_within_the_depth_drawn() {
    let mut replica = seeded("- a\n- b");
    let b = id_of(&replica, "- b");
    call(&mut replica, "indent-block", json!({ "block": b })).unwrap();
    assert_eq!(page(&replica), ["- a", "  - b"]);
    call(
        &mut replica,
        "indent-block",
        json!({ "block": b, "levels": 3 }),
    )
    .unwrap();
    call(
        &mut replica,
        "indent-block",
        json!({ "block": b, "levels": 5 }),
    )
    .unwrap();
    assert_eq!(page(&replica)[1], format!("{}- b", "  ".repeat(5)));
    call(
        &mut replica,
        "indent-block",
        json!({ "block": b, "levels": -5 }),
    )
    .unwrap();
    call(
        &mut replica,
        "indent-block",
        json!({ "block": b, "levels": -1 }),
    )
    .unwrap();
    assert_eq!(page(&replica), ["- a", "- b"]);
    assert!(
        call(
            &mut replica,
            "indent-block",
            json!({ "block": b, "levels": 6 })
        )
        .unwrap_err()
        .contains("levels")
    );
}

#[test]
fn a_block_moves_after_another_or_to_either_end() {
    let mut replica = seeded("a\nb\nc\nd");
    let (a, b, d) = (
        id_of(&replica, "a"),
        id_of(&replica, "b"),
        id_of(&replica, "d"),
    );
    call(
        &mut replica,
        "move-block",
        json!({ "block": a, "position": "end" }),
    )
    .unwrap();
    assert_eq!(page(&replica), ["b", "c", "d", "a"]);
    call(
        &mut replica,
        "move-block",
        json!({ "block": d, "position": "start" }),
    )
    .unwrap();
    assert_eq!(page(&replica), ["d", "b", "c", "a"]);
    let answer = call(
        &mut replica,
        "move-block",
        json!({ "block": b, "after": a }),
    )
    .unwrap();
    assert_eq!(page(&replica), ["d", "c", "a", "b"]);
    assert_eq!(answer["changed"].as_array().unwrap().len(), 1);
    assert!(
        call(
            &mut replica,
            "move-block",
            json!({ "block": b, "after": b })
        )
        .unwrap_err()
        .contains("itself")
    );
}

#[test]
fn blocks_are_deleted_and_a_page_is_never_left_without_one() {
    let mut replica = seeded("a\nb\nc");
    let (a, c) = (id_of(&replica, "a"), id_of(&replica, "c"));
    let answer = call(&mut replica, "delete-blocks", json!({ "blocks": [a, c] })).unwrap();
    assert_eq!(answer["removed"].as_array().unwrap().len(), 2, "{answer}");
    assert_eq!(page(&replica), ["b"]);
    let b = id_of(&replica, "b");
    call(&mut replica, "delete-blocks", json!({ "blocks": [b] })).unwrap();
    assert_eq!(blocks(&replica).len(), 1);
    assert_eq!(blocks(&replica)[0].text, "");
    assert!(
        call(&mut replica, "delete-blocks", json!({ "blocks": ["nope"] }))
            .unwrap_err()
            .contains("no block")
    );
}

#[test]
fn replacing_the_body_keeps_unchanged_blocks_and_edits_the_rest_as_one_change() {
    let mut replica = seeded("buy milk\nbuy eggs\nbuy bread\ncall mum\nwater plants");
    let ids: Vec<String> = [
        "buy milk",
        "buy eggs",
        "buy bread",
        "call mum",
        "water plants",
    ]
    .iter()
    .map(|text| id_of(&replica, text))
    .collect();
    let before: Vec<String> = blocks(&replica).into_iter().map(|block| block.id).collect();
    let mut peer = replica.fork();
    let changes_before = changes(&mut replica);

    // The todo page becomes a checklist: every line turns into an item (no
    // ids written), one is reworded, one is new, one is gone, one moved.
    let (answer, envelope) = call_enveloped(
        &mut replica,
        "ada",
        "replace-body",
        json!({
            "markdown": "# Todo\n- [ ] buy milk\n- [ ] buy oat milk\n- [ ] buy bread\n- [ ] call mum\n- [ ] water the plants\n",
        }),
    )
    .unwrap();
    assert_eq!(
        changes(&mut replica),
        changes_before + 1,
        "the whole replacement is one change, one undo step"
    );
    assert_eq!(
        page(&replica),
        [
            "# Todo",
            "- [ ] buy milk",
            "- [ ] buy oat milk",
            "- [ ] buy bread",
            "- [ ] call mum",
            "- [ ] water the plants",
        ]
    );
    let after = blocks(&replica);
    let kept: Vec<&str> = after.iter().map(|block| block.id.as_str()).collect();
    for old in [&before[0], &before[2], &before[3], &before[4]] {
        assert!(kept.contains(&old.as_str()), "{old} survives: {kept:?}");
    }
    // The reworded line is the same block as before.
    assert!(
        kept.contains(&before[1].as_str()),
        "eggs was edited into oat milk in place"
    );
    assert_eq!(answer["created"].as_array().unwrap().len(), 1, "{answer}");
    assert!(answer.get("removed").is_none(), "{answer}");
    let _ = ids;

    // A peer replays the envelope into the very same document.
    peer.replay(&envelope).unwrap();
    assert_eq!(peer.heads(), replica.heads());
}

#[test]
fn naming_a_line_by_its_id_keeps_its_block_whatever_the_text() {
    let mut replica = seeded("one\ntwo\nthree");
    let (one, two, three) = (
        id_of(&replica, "one"),
        id_of(&replica, "two"),
        id_of(&replica, "three"),
    );
    let old: Vec<String> = blocks(&replica).into_iter().map(|block| block.id).collect();
    call(
        &mut replica,
        "replace-body",
        json!({ "markdown": format!("three, reversed ^{three}\ntwo ^{two}\nentirely different ^{one}\nnew line\n") }),
    )
    .unwrap();
    assert_eq!(
        page(&replica),
        ["three, reversed", "two", "entirely different", "new line"]
    );
    let after = blocks(&replica);
    assert_eq!(after[0].id, old[2]);
    assert_eq!(after[1].id, old[1]);
    assert_eq!(after[2].id, old[0]);
    assert!(!old.contains(&after[3].id));

    let error = call(
        &mut replica,
        "replace-body",
        json!({ "markdown": "[table] ^gone" }),
    )
    .unwrap_err();
    assert!(error.contains("not on this page"), "{error}");
}

#[test]
fn a_replacement_that_changes_nothing_writes_nothing() {
    let mut replica = seeded("# Todo\n- [ ] a\n- [x] b\n> q");
    let markdown = read(&replica);
    let before = changes(&mut replica);
    call(
        &mut replica,
        "replace-body",
        json!({ "markdown": markdown }),
    )
    .unwrap();
    // The action is applied, but no block was written to.
    let again = read(&replica);
    let answer = call(&mut replica, "replace-body", json!({ "markdown": again })).unwrap();
    assert_eq!(answer, json!({}), "nothing to report");
    assert!(changes(&mut replica) <= before + 2);
}

#[test]
fn a_body_replaced_by_nothing_keeps_a_paragraph_to_write_in() {
    let mut replica = seeded("a\nb");
    call(&mut replica, "replace-body", json!({ "markdown": "" })).unwrap();
    assert_eq!(blocks(&replica).len(), 1);
    assert_eq!(blocks(&replica)[0].kind, "paragraph");
}

#[test]
fn text_is_found_with_the_ids_to_edit_it_by() {
    let replica = seeded("Buy Milk\nsome eggs\nmilk again\n- [ ] MILK");
    let found = block_actions::find_under(replica.doc(), &automerge::ROOT, "milk", false).unwrap();
    let hits = found["matches"].as_array().unwrap();
    assert_eq!(hits.len(), 3, "{found}");
    assert_eq!(hits[0]["text"], "Buy Milk");
    assert_eq!(hits[0]["block"], id_of(&replica, "Buy Milk"));
    let exact = block_actions::find_under(replica.doc(), &automerge::ROOT, "milk", true).unwrap();
    assert_eq!(exact["matches"].as_array().unwrap().len(), 1);
    let none = block_actions::find_under(replica.doc(), &automerge::ROOT, "", false).unwrap();
    assert!(none["matches"].as_array().unwrap().is_empty());
}

#[test]
fn the_declared_actions_are_published_with_typed_described_parameters() {
    let declared = RichText::declarations();
    for id in &block_actions::ANSWERING {
        let decl = declared
            .iter()
            .find(|decl| decl.id == *id)
            .unwrap_or_else(|| panic!("{id} is declared"));
        assert!(decl.description.is_some(), "{id} says what it does");
        assert!(decl.result.is_some(), "{id} answers");
        assert!(
            decl.params
                .iter()
                .all(|param| !param.description.is_empty()),
            "{id}'s parameters are described"
        );
    }
    assert!(declared.iter().any(|decl| decl.id == "append-paragraph"));
}

#[test]
fn two_collaborators_editing_the_same_page_merge_by_block() {
    let mut ada = seeded("- [ ] milk\n- [ ] eggs");
    let mut bob = ada.fork().as_author("bob");
    let (milk, eggs) = (id_of(&ada, "- [ ] milk"), id_of(&ada, "- [ ] eggs"));
    call(
        &mut ada,
        "set-block-kind",
        json!({ "block": milk, "done": true }),
    )
    .unwrap();
    call_as(
        &mut bob,
        "bob",
        "set-block-text",
        json!({ "block": eggs, "text": "free-range eggs" }),
    )
    .unwrap();
    call_as(
        &mut bob,
        "bob",
        "set-block-kind",
        json!({ "block": eggs, "done": true }),
    )
    .unwrap();
    ada.sync_with(&mut bob).unwrap();
    assert_eq!(page(&ada), ["- [x] milk", "- [x] free-range eggs"]);
    assert_eq!(page(&bob), page(&ada));
}

#[test]
fn a_viewer_cannot_reach_the_actions_the_editor_takes() {
    // Every declaration of the block actions changes the document, so each
    // needs the right to write.
    for decl in block_actions::declarations() {
        assert_eq!(
            decl.kind,
            tilo_actions::contract::ActionKind::Undoable,
            "{} writes",
            decl.id
        );
    }
}
