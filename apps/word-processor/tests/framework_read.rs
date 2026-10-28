//! An agent reads the document through the bound-action door the broker
//! and the MCP bridge reach: the framework's `read` answers the committed
//! body text, the same door every other resource's content is read through.

use ui::bound_dispatch::BoundActionAddress;
use ui::framework::testing::TestApp;
use word_processor::framework::WordProcessorApp;

const INSTANCE: u64 = 7;

fn read_address(resources: &[tilo_actions::Resource]) -> Option<tilo_actions::ActionAddress> {
    resources.iter().find_map(|resource| {
        resource
            .actions
            .iter()
            .find(|action| action.id == "read" && action.contract.reads_document())
            .and_then(|action| resource.action_address(&action.id))
            .or_else(|| read_address(&resource.children))
    })
}

/// Invoke the document's `read` through the bound-action door, answering
/// what it answered.
fn read(
    app: &mut TestApp<WordProcessorApp>,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, tilo_actions::DoorError> {
    let harness = app.harness();
    let address = BoundActionAddress {
        instance_id: INSTANCE,
        address: read_address(&harness.resources()).expect("the document publishes its read"),
    };
    harness
        .invoke_bound_action(&address, arguments.as_object().unwrap())
        .map(|started| {
            started
                .completed()
                .and_then(|output| output.value)
                .unwrap_or_default()
        })
}

/// The body text a read answers, whether it answered now's document or a
/// past version's page.
fn page(answer: &serde_json::Value) -> &str {
    answer["markdown"]
        .as_str()
        .or_else(|| answer["content"]["markdown"].as_str())
        .expect("the read answers its page")
}

/// The id the line of `markdown` holding `text` ends in, as its actions
/// take it.
fn id_of(markdown: &str, text: &str) -> String {
    let (_, tagged) = markdown
        .split_once(&format!("{text} ^"))
        .expect("the text is read");
    tagged
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn the_read_answers_the_committed_document_for_an_agent() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::new().unwrap();
    app.harness().type_text("Q3 Planning");
    app.harness().key("Enter");
    app.harness().type_text("Book the venue");

    let whole = read(&mut app, &serde_json::json!({})).unwrap();
    let markdown = whole["markdown"].as_str().unwrap();
    assert!(
        markdown.starts_with("Q3 Planning ^") && markdown.contains("\nBook the venue ^"),
        "{markdown}"
    );
    assert_eq!(whole["blocks"], 2, "{whole}");

    // One block, by the id the whole read named.
    let paragraph = id_of(markdown, "Book the venue");
    let one = read(&mut app, &serde_json::json!({ "item": paragraph })).unwrap();
    assert_eq!(one["blocks"], 1, "{one}");
    assert!(
        one["markdown"].as_str().unwrap().contains("Book the venue"),
        "{one}"
    );

    // An item the document does not hold is refused, naming why.
    let missing = read(&mut app, &serde_json::json!({ "item": "no-such-block" }));
    assert!(missing.is_err(), "{missing:?}");
}

#[test]
fn a_read_of_a_past_version_answers_the_text_as_it_was() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::new().unwrap();
    app.harness().type_text("First draft");
    app.settle();
    let past_heads = app.read_document(|doc| {
        doc.clone()
            .get_heads()
            .into_iter()
            .map(|hash| hash.to_string())
            .collect::<Vec<_>>()
    });
    assert!(!past_heads.is_empty());
    app.harness().key("Enter");
    app.harness().type_text("Second thought");
    app.settle();
    let heads = past_heads.join("+");
    let past = read(&mut app, &serde_json::json!({ "version": heads })).unwrap();
    let markdown = page(&past);
    assert!(markdown.contains("First draft"), "{past}");
    assert!(!markdown.contains("Second thought"), "{past}");
    assert_eq!(past["heads"], serde_json::json!(past_heads), "{past}");
}

fn invoke(
    app: &mut TestApp<WordProcessorApp>,
    action: &str,
    arguments: &serde_json::Value,
) -> Option<serde_json::Value> {
    fn address(
        resources: &[tilo_actions::Resource],
        id: &str,
    ) -> Option<tilo_actions::ActionAddress> {
        resources.iter().find_map(|resource| {
            resource
                .actions
                .iter()
                .find(|action| action.id == id)
                .and_then(|action| resource.action_address(&action.id))
                .or_else(|| address(&resource.children, id))
        })
    }
    let harness = app.harness();
    let address = BoundActionAddress {
        instance_id: INSTANCE,
        address: address(&harness.resources(), action).expect("the action is published"),
    };
    let (invocation, started) = harness
        .start_bound_job(&address, arguments.as_object().unwrap(), false)
        .unwrap();
    // An edit at the caret is applied as the frame settles; one that answers
    // with what it changed does so then.
    harness.relayout();
    if let Some(output) = started.completed() {
        return output.value;
    }
    for _ in 0..4 {
        if let Some(update) = harness
            .job_updates(invocation)
            .iter()
            .find(|update| update.is_terminal())
        {
            return match update {
                tilo_actions::InvocationUpdate::Succeeded { output, .. } => output.value.clone(),
                other => panic!("{action} failed: {other:?}"),
            };
        }
        harness.relayout();
    }
    panic!("{action} never answered")
}

#[test]
fn an_edit_answers_with_the_blocks_it_changed() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::new().unwrap();
    app.harness().type_text("Draft");
    invoke(&mut app, "select-all", &serde_json::json!({}));
    let pasted = invoke(
        &mut app,
        "paste",
        &serde_json::json!({"markdown": "# Standup\n\nMaya is out\n"}),
    );
    let summary = pasted
        .as_ref()
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    assert!(
        summary.starts_with("2 changed:\n# Standup {#") && summary.contains("\nMaya is out {#"),
        "{pasted:?}"
    );
    let styled = invoke(
        &mut app,
        "set-paragraph-style",
        &serde_json::json!({"kind": "heading", "level": 2}),
    );
    assert!(
        styled
            .as_ref()
            .and_then(|value| value.as_str())
            .is_some_and(|summary| summary.starts_with("1 changed:\n## ")
                && summary.matches(" {#").count() == 1),
        "{styled:?}"
    );
}

/// `text` with every block id taken out, so a test can state the layout it
/// expects without the generated ids: the `^id` an edit answers with, and
/// the `{#id}` a document answers with.
fn without_ids(text: &str) -> String {
    // An edit's answer ends each changed line with the block's id; a page's
    // line is the block's text as it is, so its trailing `^` is no anchor.
    let marked: String = text
        .lines()
        .map(|line| {
            if line.ends_with(" ^") {
                line.trim_end_matches(" ^")
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = String::new();
    let mut rest = marked.as_str();
    while let Some((before, tag)) = rest.split_once("{#") {
        out.push_str(before);
        let (_, after) = tag.split_once('}').expect("a closed tag");
        rest = after;
    }
    out.push_str(rest);
    out.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `text` with the `^id` of each block and cell taken out.
fn without_marked_ids(text: &str) -> String {
    text.lines()
        .map(|line| {
            let mut out = String::new();
            let mut rest = line;
            while let Some((before, tag)) = rest.split_once(" ^") {
                out.push_str(before);
                let (id, after) = tag
                    .split_once(|c: char| c.is_whitespace() || c == '|')
                    .expect("an id is followed by a space, a cell edge or nothing");
                assert!(
                    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()),
                    "only an id is taken out: ^{tag}"
                );
                rest = after;
            }
            out.push_str(rest.trim_end_matches('^').replace("  |", " |").as_str());
            out.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The table `table` of `markdown`, as the cells its rows hold: the first
/// cell a row gives up its own is its header.
fn rows_of(markdown: &str, table: &str) -> Vec<Vec<(String, String)>> {
    // The table's rows are the line that tags it, the header, the rule and
    // then the rows, until the line that begins the next block.
    let mut inside = false;
    let mut rows: Vec<&str> = Vec::new();
    for line in markdown.lines() {
        if line.contains(&format!(" ^table:{table}")) {
            inside = true;
            rows.push(line);
            continue;
        }
        if inside && is_row(line) {
            rows.push(line);
            continue;
        }
        if inside {
            break;
        }
    }
    assert!(rows.len() > 1, "the table {table} is read: {markdown}");
    rows.into_iter()
        .filter(|line| !line.starts_with("| ---"))
        .map(|line| {
            line.trim_end_matches(&format!(" ^table:{table}"))
                .split('|')
                .filter(|cell| !cell.trim().is_empty())
                .map(|cell| match cell.trim().rsplit_once(" ^") {
                    Some((text, id)) => (text.trim().to_owned(), id.to_owned()),
                    None => panic!("a cell reads with its id: {line}"),
                })
                .collect()
        })
        .collect()
}

/// Whether `line` is a table's row: it starts at a cell edge.
fn is_row(line: &str) -> bool {
    line.starts_with('|')
}

/// The id the table whose first cell says `first` is tagged with.
fn table_of(markdown: &str, first: &str) -> String {
    let row = markdown
        .lines()
        .find(|line| line.starts_with('|') && line.contains(first))
        .expect("the table's first row is read");
    let tagged = row
        .split_once(" ^table:")
        .expect("the table is tagged with its id");
    tagged
        .1
        .split(|c: char| c.is_whitespace() || c == '|')
        .next()
        .expect("a table id")
        .to_owned()
}

/// The body's blocks as they are stored, in page order: what a read of the
/// document is a projection of, read here without the projection's help.
fn stored(app: &TestApp<WordProcessorApp>) -> Vec<(String, String)> {
    blocks(app)
        .into_iter()
        .map(|(kind, text, _)| (kind, text))
        .collect()
}

/// The page `blocks` are, as a read writes it: a Markdown table for each
/// table's run of cells.
fn page_of(blocks: &[(String, String)]) -> String {
    let mut out = String::new();
    let mut index = 0;
    while let Some((kind, text)) = blocks.get(index) {
        index += 1;
        if kind != "cell" {
            out.push_str(text);
            out.push('\n');
            continue;
        }
        // A row's cells are the ones whose text is not the block after them:
        // that block is the empty one the page shows under the last row.
        let mut run: Vec<&str> = vec![text];
        while let Some((next_kind, next)) = blocks.get(index) {
            let starts_a_row = blocks
                .get(index + 1)
                .is_some_and(|(_, after)| after != next);
            if next_kind != "cell" || !starts_a_row {
                break;
            }
            run.push(next);
            index += 1;
        }
        let row: Vec<&str> = run;
        out.push('|');
        for cell in &row {
            out.push_str(&format!(" {cell} |"));
        }
        out.push('\n');
    }
    without_marked_ids(&out)
}

/// The page the document's read answers, as its Markdown.
fn read_page(app: &mut TestApp<WordProcessorApp>) -> String {
    app.settle();
    let answer = read(app, &serde_json::json!({})).unwrap();
    page(&answer).to_owned()
}

#[test]
fn a_heading_list_and_table_read_back_with_the_handles_that_edit_them() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::new().unwrap();
    app.harness().type_text("x");
    invoke(&mut app, "select-all", &serde_json::json!({}));
    invoke(
        &mut app,
        "paste",
        &serde_json::json!({"markdown": "## Standup\n\n- review\n  1. screen reader\n- ship\n\n\
            | Person | Blocked | Next |\n| --- | --- | --- |\n| Maya | no | docs |\n| Ravi | yes | CI |\n"}),
    );
    let layout = "## Standup\n\
        - review\n  1. screen reader\n- ship\n\
        | Person | Blocked | Next |\n| --- | --- | --- |\n\
        | Maya | no | docs |\n| Ravi | yes | CI |";

    let markdown = read_page(&mut app);
    assert_eq!(without_ids(&markdown), layout, "{markdown}");
    // The page says what the document holds, block for block.
    assert_eq!(
        without_marked_ids(&markdown),
        page_of(&stored(&app)),
        "{markdown}"
    );

    // A cell is selected and filled by the id the read tags it with.
    let ravi = rows_of(&markdown, &table_of(&markdown, "Person"))[2].clone();
    let (_, cell) = ravi[1].clone();
    invoke(
        &mut app,
        "select-block",
        &serde_json::json!({"block": cell}),
    );
    invoke(&mut app, "type-text", &serde_json::json!({"text": "no"}));
    let markdown = read_page(&mut app);
    assert!(
        markdown.contains(&format!("| no ^{cell}")),
        "the cell's id selects it: {markdown}"
    );

    // The table read names is the one a row is added to, and a read after
    // it shows the row.
    let table = table_of(&markdown, "Person");
    invoke(
        &mut app,
        "add-row",
        &serde_json::json!({ "table": table.clone() }),
    );
    let markdown = read_page(&mut app);
    let rows = rows_of(&markdown, &table);
    assert_eq!(rows.len(), 4, "the header and three rows: {markdown}");
    assert_eq!(
        rows[3]
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>(),
        ["", "", ""],
        "the row added is empty: {markdown}"
    );
}

/// A paste and a row added answer with the blocks they made, so the caller
/// fills them without reading the document again.
#[test]
fn pasting_a_table_and_adding_a_row_answer_with_the_blocks_they_made() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::new().unwrap();
    app.harness().type_text("x");
    invoke(&mut app, "select-all", &serde_json::json!({}));
    let pasted = invoke(
        &mut app,
        "paste",
        &serde_json::json!({"markdown": "## Standup\n\n- review\n  1. screen reader\n- ship\n\n\
            | Person | Blocked | Next |\n| --- | --- | --- |\n| Maya | no | docs |\n| Ravi | yes | CI |\n"}),
    );
    let layout = "## Standup\n\
        - review\n  1. screen reader\n- ship\n\
        | Person | Blocked | Next |\n| --- | --- | --- |\n\
        | Maya | no | docs |\n| Ravi | yes | CI |";
    let pasted = pasted
        .as_ref()
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    assert_eq!(
        without_ids(pasted),
        format!("14 changed:\n{layout}"),
        "{pasted}"
    );
    app.harness().relayout();
    let table = pasted
        .split_once(" ^table:")
        .and_then(|(_, tagged)| tagged.split(|c: char| c.is_whitespace() || c == '|').next())
        .map(str::to_owned)
        .expect("the paste answers with the table's id");

    // The row it adds is answered with the ids that fill it, and the read
    // shows them in the table.
    let appended = invoke(
        &mut app,
        "add-row",
        &serde_json::json!({ "table": table.clone() }),
    );
    let appended = appended
        .as_ref()
        .and_then(|value| value["changed"].as_str())
        .unwrap_or_default();
    assert_eq!(
        without_ids(appended),
        "3 changed:\n| | | |\n| --- | --- | --- |",
        "the new row answers with the ids that fill it"
    );
    let markdown = read_page(&mut app);
    let rows = rows_of(&markdown, &table);
    assert_eq!(rows.len(), 4, "{markdown}");
    for (text, id) in &rows[3] {
        assert!(text.is_empty(), "the row added is empty: {markdown}");
        assert!(appended.contains(&format!(" ^{id}")), "{markdown}");
    }
}

/// The document's blocks as (kind, text, align), in order.
fn blocks(app: &TestApp<WordProcessorApp>) -> Vec<(String, String, Option<String>)> {
    app.read_document(|doc| {
        let containers = word_processor::model::open(doc).unwrap();
        let order = tilo_doc::model::region_blocks_in_order(
            doc,
            &containers,
            tilo_doc::model::Region::Body,
        );
        tilo_doc::model::materialize_blocks(doc, &containers, &order)
            .into_iter()
            .map(|block| {
                let align = block.str_attr("align").map(str::to_owned);
                (block.kind, block.text, align)
            })
            .collect()
    })
}

#[test]
fn an_edit_told_which_paragraph_acts_there_and_leaves_the_agent_there() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::new().unwrap();
    app.harness().type_text("Launch plan");
    app.harness().key("Enter");
    app.harness().type_text("Book the venue for the offsite");
    app.harness().relayout();

    // The paragraphs are published by their first words, each naming its
    // paragraph as an item of the document.
    let resources = app.harness().resources();
    let root = resources[0].id.as_str().to_owned();
    let title = resources[0]
        .children
        .iter()
        .find(|row| row.label == "Launch plan")
        .expect("the title is published as a place");
    assert_eq!(title.kind, "word-processor.document.paragraph");
    let block = title
        .id
        .as_str()
        .rsplit_once("/blocks/")
        .map(|(_, block)| block.to_owned())
        .unwrap();
    let align = resources[0]
        .actions
        .iter()
        .find(|action| action.id == "align")
        .unwrap();
    assert!(
        align
            .params()
            .iter()
            .any(tilo_actions::ActionParam::is_place),
        "align is told where by a place"
    );

    let agent = tilo_actions::ActionInvoker::actor("scribe")
        .with_participant(tilo_actions::ParticipantId::new("scribe-1").unwrap());
    app.call_by(
        agent.clone(),
        &root,
        "align",
        serde_json::json!({"align": "center", "at": {"item": block}}),
    )
    .unwrap();
    app.settle();
    let after = blocks(&app);
    assert_eq!(after[0].1, "Launch plan");
    assert_eq!(after[0].2.as_deref(), Some("center"), "{after:?}");
    assert_eq!(after[1].2, None, "only the named paragraph is aligned");

    // It is left there: its next edit at its caret is beside the title,
    // not at the end of the document where an agent starts.
    app.call_by(agent, &root, "insert-table-here", serde_json::json!({}))
        .unwrap();
    app.settle();
    let after = blocks(&app);
    assert_eq!(after[0].1, "Launch plan");
    assert_ne!(
        after[1].0, "paragraph",
        "the table follows the title: {after:?}"
    );
    assert_eq!(
        after.last().map(|block| block.1.as_str()),
        Some("Book the venue for the offsite"),
        "{after:?}"
    );

    // A place that is not in the document is refused, and nothing changes.
    let before = blocks(&app);
    assert!(
        app.call(
            "align",
            serde_json::json!({"align": "right", "at": {"item": "no-such-paragraph"}}),
        )
        .is_err()
    );
    assert_eq!(blocks(&app), before);
}
