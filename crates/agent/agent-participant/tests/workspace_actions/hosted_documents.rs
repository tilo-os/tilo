//! Document work on a hosted workspace with nobody's page open: an agent
//! drives PDF Viewer and Writer over its MCP session, and the workspace's
//! host runs every action in the apps' own code, in confined app workers.
//!
//! The agent uploads a PDF form and a Word statement into the workspace,
//! reads both, lists the form's fields and fills them, writes a cover
//! letter and exports it as Word and as PDF. Only then does a reviewer's
//! page open the workspace: it sees the filled values, each change grouped
//! under the agent's run, and undoes the run from the records' own
//! history. The workspace's shell, where commands and VMs run, is never
//! asked.
//!
//! The steps are free functions over an [`McpServer`], so another scenario
//! (a public API's end-to-end test, say) drives the same path.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    reason = "a fixture's offsets and an answer's ASCII prefix; a slip is the failure it reports"
)]

use super::*;
use serde_json::{Value, json};
use tilo_browser_peer::session_actions::SessionActions;
use tilo_runtime::mcp::{McpServer, Surface};

/// The apps the agent works in.
pub const PDF: &str = "so.tilo.pdf-viewer";
pub const WRITER: &str = "so.tilo.word-processor";

/// The agent's run, as History names it.
pub const RUN: &str = "visa-run-1";
pub const RUN_LABEL: &str = "Visa agent";

/// The form an applicant fills: text fields, a choice and a checkbox, each
/// with a widget on its one page.
#[must_use]
pub fn application_pdf() -> Vec<u8> {
    let objects: Vec<String> = vec![
        // 1: catalog, with the form
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R 6 0 R 7 0 R 8 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 11 0 R >> >> >> >>".into(),
        // 2: pages
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        // 3: page
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Annots [4 0 R 5 0 R 6 0 R 7 0 R 8 0 R] /Contents 12 0 R /Resources << /Font << /Helv 11 0 R >> >> >>".into(),
        // 4-6: text fields
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (surname) /V () /DA (/Helv 12 Tf 0 g) /Rect [200 760 520 780] /F 4 /P 3 0 R >>".into(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (given_names) /V () /DA (/Helv 12 Tf 0 g) /Rect [200 720 520 740] /F 4 /P 3 0 R >>".into(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (passport_number) /V () /DA (/Helv 12 Tf 0 g) /Rect [200 680 520 700] /F 4 /P 3 0 R >>".into(),
        // 7: purpose of the journey, a choice
        "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (purpose) /Opt [(Tourism) (Business) (Visiting family)] /V (Tourism) /DA (/Helv 12 Tf 0 g) /Rect [200 640 400 660] /F 4 /P 3 0 R >>".into(),
        // 8: the declaration, a checkbox
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (declaration) /V /Off /AS /Off /Rect [200 600 216 616] /F 4 /P 3 0 R /AP << /N << /Yes 9 0 R /Off 10 0 R >> >> >>".into(),
        // 9, 10: the checkbox's appearances
        stream("<< /Type /XObject /Subtype /Form /BBox [0 0 16 16] >>", "0 g 3 3 10 10 re f"),
        stream("<< /Type /XObject /Subtype /Form /BBox [0 0 16 16] >>", "0 G 0.5 0.5 15 15 re S"),
        // 11: font
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
        // 12: the page's labels
        stream(
            "<< >>",
            "BT /Helv 12 Tf 40 800 Td (Application for Schengen Visa) Tj 0 -36 Td (Surname) Tj 0 -40 Td (Given names) Tj 0 -40 Td (Passport number) Tj 0 -40 Td (Purpose of journey) Tj 0 -40 Td (I declare the above is true) Tj ET",
        ),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn stream(dict: &str, content: &str) -> String {
    let dict = dict.trim_end_matches(">>").trim_end();
    format!(
        "{dict} /Length {} >>\nstream\n{content}\nendstream",
        content.len()
    )
}

/// An applicant's bank statement, as a Word file.
#[must_use]
pub fn statement_docx() -> Vec<u8> {
    let markdown =
        "# Bank statement\n\nAccount holder: Amara Okafor\n\nClosing balance: EUR 12,480.00\n";
    let (document, inputs) =
        tilo_doc_codecs::convert::import("md", markdown.as_bytes()).expect("markdown reads");
    tilo_doc_codecs::convert::export(&document, &inputs, "docx").expect("a Word file")
}

/// What the agent's MCP session answers `request`.
pub async fn ask(server: &McpServer<SessionActions>, request: Value) -> Value {
    server.handle(request).await.expect("a request is answered")
}

/// The text a tool call answers; a failed call fails the test.
pub async fn tool(server: &McpServer<SessionActions>, name: &str, arguments: Value) -> String {
    let answer = ask(
        server,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": name, "arguments": arguments}}),
    )
    .await;
    let result = &answer["result"];
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        result["isError"] != true && answer.get("error").is_none(),
        "{name} {arguments} failed: {}",
        if text.is_empty() {
            answer.to_string()
        } else {
            text
        }
    );
    text
}

/// The first JSON object a tool's text answer carries.
#[must_use]
pub fn json_in(text: &str) -> Value {
    let start = text
        .find('{')
        .unwrap_or_else(|| panic!("no object in {text}"));
    serde_json::Deserializer::from_str(&text[start..])
        .into_iter::<Value>()
        .next()
        .and_then(Result::ok)
        .unwrap_or_else(|| panic!("no object in {text}"))
}

/// The `tilo.content:<hash>` a tool's answer names.
#[must_use]
pub fn content_in(text: &str) -> String {
    let start = text
        .find("tilo.content:")
        .unwrap_or_else(|| panic!("no content named in {text}"));
    text[start..]
        .chars()
        .take("tilo.content:".len() + 64)
        .collect()
}

/// Put `bytes` into the workspace through the session's uploads, in
/// chunks as an agent sends them; answers the content's reference.
pub async fn put(server: &McpServer<SessionActions>, bytes: &[u8]) -> String {
    use base64::Engine as _;
    let begun = json_in(&tool(server, "tilo_put_begin", json!({"size": bytes.len()})).await);
    let upload = begun["upload"].as_u64().expect("an upload id");
    let mut offset = 0_usize;
    // Small chunks, so an upload of several takes more than one frame.
    for chunk in bytes.chunks(2048) {
        let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
        tool(
            server,
            "tilo_put_append",
            json!({"upload": upload, "offset": offset, "chunk": encoded}),
        )
        .await;
        offset += chunk.len();
    }
    content_in(&tool(server, "tilo_put_finish", json!({"upload": upload})).await)
}

/// Run one action by its path, and the record it acted on.
pub async fn act(
    server: &McpServer<SessionActions>,
    path: &str,
    args: Value,
) -> (String, Option<DocumentRoute>) {
    let text = tool(server, "tilo_do", json!({"path": path, "args": args})).await;
    let acted = server.acted().map(|acted| DocumentRoute {
        app_id: acted.app,
        cluster_id: WORKSPACE.into(),
        record: acted.record,
    });
    (text, acted)
}

/// The form's fields as `read-form-fields` answers them, by name.
pub async fn form_fields(server: &McpServer<SessionActions>) -> BTreeMap<String, Value> {
    let (read, _) = act(server, "pdf-viewer#read-form-fields", json!({})).await;
    json_in(&read)["fields"]
        .as_array()
        .expect("a list of fields")
        .iter()
        .map(|field| (field["name"].as_str().unwrap().to_owned(), field.clone()))
        .collect()
}

/// The runs the changes of `route` the page received are grouped under.
fn runs_of(page: &mut Browser, route: &DocumentRoute) -> Vec<tilo_actions::WorkGroup> {
    page.pump();
    let mut doc = automerge::AutoCommit::new();
    for (_, bytes) in page.documents.get(route).into_iter().flatten() {
        let _ = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes);
    }
    doc.get_changes(&[])
        .iter()
        .filter_map(|change| change.message())
        .filter_map(tilo_actions::ActionContext::from_change_message)
        .filter_map(|context| context.group)
        .collect()
}

/// The form values the PDF record holds, as the page has received it.
fn form_values(
    page: &mut Browser,
    route: &DocumentRoute,
) -> BTreeMap<String, pdf_viewer::model::FormEntry> {
    page.pump();
    if page.heard(route) == 0 {
        return BTreeMap::new();
    }
    let record = page.mount(route);
    record
        .read(|doc| {
            <pdf_viewer::markup::PdfMarkup as ui::tilo_resource::Resource>::read(doc)
                .map(|markup| markup.form_values)
                .unwrap_or_default()
        })
        .unwrap()
}

/// The text of the Writer record, as the page has received it.
fn writer_text(page: &mut Browser, route: &DocumentRoute) -> String {
    page.pump();
    if page.heard(route) == 0 {
        return String::new();
    }
    let record = page.mount(route);
    record
        .read(|doc| {
            let containers = word_processor::model::open(doc).expect("a document");
            tilo_doc::model::materialize_region(doc, &containers, tilo_doc::model::Region::Body)
                .iter()
                .map(|block| block.text.clone())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap()
}

/// Undo `run` in the record `route` names, through the history `undo` the
/// record publishes, as the activity's undo asks it.
async fn undo_run(page: &mut Browser, route: &DocumentRoute, run: &str) {
    let provider = tilo_agent_participant::record_provider(route);
    let rows = page
        .catalogue(CatalogueQuery {
            provider: Some(provider.clone()),
            needle: "undo".into(),
            max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        })
        .await
        .expect("the record's actions")
        .rows;
    let address = rows
        .into_iter()
        .filter_map(|row| row.action)
        .find(|action| {
            action.address.action == "undo" && action.params.iter().any(|param| param.name == "run")
        })
        .unwrap_or_else(|| panic!("{} publishes no history undo", route.app_id))
        .address;
    let mut argument = serde_json::Map::new();
    argument.insert("run".into(), run.into());
    page.invoke_on(&provider, address, InvokeArgument::Json(argument))
        .await
        .unwrap_or_else(|error| panic!("the run is undone in {}: {error}", route.app_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_fills_a_form_and_writes_a_letter_on_a_hosted_workspace_with_no_page_open() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo.sh", DeviceCapabilities::host());
    let reviewer = enrol(&root, "reviewer's phone", DeviceCapabilities::member());
    let computer = enrol(&root, "visa agent", DeviceCapabilities::member());
    let state = support::scratch_dir();
    // The workspace's shell: where its commands run, here or on the VM host
    // a placer names. Document work never asks it.
    let shell_state = tempfile::tempdir().unwrap();
    let shells = shell_state.path().join("shells");
    let hosted = harness::hosted::start(
        &root,
        &host,
        state.path(),
        Options {
            shell: Some(tilo_relay::agents::host_shells(shells.clone()).unwrap()),
            ..Options::offering(common::catalogue, ModelChoice::Scripted)
        },
    )
    .await;
    let _laptop = participant::Participant::start(&hosted.relay, &root).await;

    // The agent's session: one of the person's devices, as `tilo login`
    // makes one, serving the MCP projection of the workspace's host. No
    // person's page is open.
    let agent = Browser::open(&hosted, &computer, &host).await;
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        person,
    )
    .with_surface(Surface::Agent)
    .with_agent_run(RUN, RUN_LABEL);

    // 1. The applicant's files go into the workspace.
    let application = put(&server, &application_pdf()).await;
    let statement = put(&server, &statement_docx()).await;

    // 2. The agent reads them: the form's fields, and the statement.
    let (_, opened) = act(&server, "pdf-viewer#open", json!({})).await;
    eprintln!("pdf-viewer#open acted on {opened:?}");
    let (imported, pdf) = act(
        &server,
        "pdf-viewer#import-pdf-content",
        json!({"content": application, "name": "application.pdf"}),
    )
    .await;
    assert!(imported.contains("application.pdf"), "{imported}");
    let pdf = pdf.expect("the import names the PDF's record");
    let (text, _) = act(&server, "pdf-viewer#read-text", json!({})).await;
    assert!(text.contains("Schengen Visa"), "{text}");
    let fields = form_fields(&server).await;
    assert_eq!(
        fields.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "declaration",
            "given_names",
            "passport_number",
            "purpose",
            "surname"
        ],
        "{fields:#?}"
    );
    assert_eq!(fields["purpose"]["kind"], "choice");
    assert_eq!(fields["declaration"]["kind"], "checkbox");

    act(&server, "word-processor#open", json!({})).await;
    let (read, _) = act(
        &server,
        "word-processor#read-content",
        json!({"content": statement, "name": "statement.docx"}),
    )
    .await;
    assert!(read.contains("Closing balance: EUR 12,480.00"), "{read}");

    // ... and fills the form, each field by the edit its row names.
    for (name, value) in [
        ("surname", json!({"kind": "text", "value": "Okafor"})),
        ("given_names", json!({"kind": "text", "value": "Amara"})),
        (
            "passport_number",
            json!({"kind": "text", "value": "A12345678"}),
        ),
        (
            "purpose",
            json!({"kind": "choice", "value": "Visiting family"}),
        ),
        (
            "declaration",
            json!({"kind": "checked", "value": {"on": true, "export": null}}),
        ),
    ] {
        let edit = json!({"path": fields[name]["path"], "op": "set", "value": value});
        act(
            &server,
            "pdf-viewer/PDF Viewer#field-edit",
            json!({"edit": edit}),
        )
        .await;
    }
    let filled = form_fields(&server).await;
    assert_eq!(
        filled["surname"]["value"],
        json!({"kind": "text", "value": "Okafor"})
    );
    assert_eq!(
        filled["purpose"]["value"],
        json!({"kind": "choice", "value": "Visiting family"})
    );

    // 3. It writes the cover letter and exports it, as Word and as PDF.
    let mut writer = None;
    for line in [
        "Dear Visa Officer,",
        "I, Amara Okafor, apply for a short-stay visa to visit my family. My bank statement shows a closing balance of EUR 12,480.00.",
        "Yours faithfully, Amara Okafor",
    ] {
        let (_, acted) = act(
            &server,
            "word-processor#append-paragraph",
            json!({"text": line}),
        )
        .await;
        writer = acted;
    }
    let writer = writer.expect("the letter's record");
    assert_eq!(writer.app_id, WRITER);
    let (docx, _) = act(
        &server,
        "word-processor#export-content",
        json!({"format": "docx"}),
    )
    .await;
    let docx = content_in(&docx);
    let (letter_pdf, _) = act(
        &server,
        "word-processor#export-content",
        json!({"format": "pdf"}),
    )
    .await;
    let letter_pdf = content_in(&letter_pdf);
    // What it exported is kept in the workspace: the letter reads back.
    let (letter, _) = act(
        &server,
        "word-processor#read-content",
        json!({"content": docx, "name": "cover-letter.docx"}),
    )
    .await;
    assert!(letter.contains("Dear Visa Officer"), "{letter}");
    let kept = hosted
        .agents
        .content(WORKSPACE, &letter_pdf)
        .expect("the letter's PDF is kept in the workspace");
    assert!(kept.starts_with(b"%PDF"), "a PDF");

    // 4. The reviewer opens the workspace on their phone.
    let mut page = Browser::open(&hosted, &reviewer, &host).await;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let values = form_values(&mut page, &pdf);
        if values.get("surname") == Some(&pdf_viewer::model::FormEntry::Text("Okafor".into()))
            && writer_text(&mut page, &writer).contains("Dear Visa Officer")
        {
            assert_eq!(
                values.get("purpose"),
                Some(&pdf_viewer::model::FormEntry::Choice(
                    "Visiting family".into()
                ))
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the filled form never reached the page: {values:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // History names the agent's run for every edit.
    for route in [&pdf, &writer] {
        let runs = runs_of(&mut page, route);
        assert!(
            !runs.is_empty()
                && runs
                    .iter()
                    .all(|run| run.id == RUN && run.label == RUN_LABEL),
            "{} keeps its changes as the agent's run: {runs:?}",
            route.app_id
        );
    }

    // Undoing the run takes the agent's edits back.
    undo_run(&mut page, &pdf, RUN).await;
    undo_run(&mut page, &writer, RUN).await;
    loop {
        let values = form_values(&mut page, &pdf);
        let text = writer_text(&mut page, &writer);
        if !values.contains_key("surname") && !text.contains("Dear Visa Officer") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the run was never undone: {values:?} / {text}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // No shell ran: no VM or command was asked for any of it.
    assert!(
        !shells.join(WORKSPACE).exists(),
        "the workspace's shell was asked"
    );
}
