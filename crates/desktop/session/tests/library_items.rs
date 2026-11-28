//! PDF Viewer keeps one library for the workspace (`pdf.library`), and each of its windows shows
//! one document of it (`document: library.documents`). Two PDFs a person opens from their files
//! are two documents of that one library, each in a window the session holds on it; the same file
//! opened again, or its document found in search, comes back to that window. An edit made through
//! the app's own action shows in the window on its document and reaches the library every reader
//! lists, and closing one window leaves the other on its document, which the next start of the
//! device opens it on again. A search hit on a page's words opens the document's window turned to
//! that page.
//!
//! The device is a real one: its session, its daemon and the tilolang host that runs PDF Viewer's
//! instances, one per window. Every step goes through what a person or an agent reaches: the
//! app's published actions, the shell's search and its places, and the workspace's record API.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live \
              until their sessions end"
)]

use serde_json::{Value, json};
use tilo_actions::remote::RemoteActionTarget;
use tilo_server_protocol::workspaces::Role;
use tilo_session_core::desktop::navigation::LayoutEffect;
use tilo_session_core::desktop::verbs::{SEARCH_RESOURCE, SHELL_RESOURCE};
use tilo_session_core::layout;

use crate::common::people::{Device, Person, Site, join_together, target};

/// PDF Viewer's package id.
const PDF: &str = "so.tilo.pdf-viewer";
/// The kind a document of PDF Viewer's library is published under.
const DOCUMENT: &str = "tl.so.tilo.pdf-viewer.Document";
/// The kind a page of one of its documents is published under.
const PAGE: &str = "tl.so.tilo.pdf-viewer.Page";

/// The step `action` on `device` with `args`, waited until it settled: what it returned.
fn step(device: &Device, action: &RemoteActionTarget, args: &Value) -> Value {
    let answer = device.act_wait(action, args);
    match answer["update"]["state"].as_str() {
        Some("succeeded") => answer["update"]["output"]["value"].clone(),
        other => panic!(
            "{} {:?} ended as {other:?}: {answer}\n{}",
            device.label,
            action.address,
            device.diagnostic()
        ),
    }
}

/// The published resource of the document named `name` in PDF Viewer's library, as `device`
/// lists it once one of the app's windows publishes it.
fn document(device: &Device, name: &str) -> String {
    let mut found = None;
    tilo_test_env::wait_until(
        format!("{} lists the document {name}", device.label),
        || {
            found = device
                .actions()
                .into_iter()
                .find(|row| {
                    row.app_id == PDF && row.resource.kind == DOCUMENT && row.resource.label == name
                })
                .map(|row| row.resource.id);
            found.is_some()
        },
    );
    found.expect("the document is listed")
}

/// PDF Viewer's `action` on the document `resource`, as one of the app's windows publishes it.
fn document_action(device: &Device, resource: &str, action: &str) -> RemoteActionTarget {
    let mut found = None;
    tilo_test_env::wait_until(
        format!("{} offers {action} on {resource}", device.label),
        || {
            found = device
                .actions()
                .iter()
                .find(|row| {
                    row.app_id == PDF
                        && row.resource.id == resource
                        && row
                            .action
                            .as_ref()
                            .is_some_and(|offered| offered.address.action == action)
                })
                .map(target);
            found.is_some()
        },
    );
    found.expect("the action is offered")
}

/// The shell's action `action` on `resource`, as `device` publishes it.
fn shell_action(device: &Device, resource: &str, action: &str) -> RemoteActionTarget {
    let mut found = None;
    tilo_test_env::wait_until(
        format!("{} offers {action} on {resource}", device.label),
        || {
            found = device.offered(resource, action);
            found.is_some()
        },
    );
    found.expect("the shell offers it")
}

/// PDF Viewer's windows on `device`, each by its tab with the item it was opened on, where it is
/// drawn, and whether it is the one the person works in.
fn pdf_windows(device: &Device) -> Vec<Value> {
    device.ask(&json!({"op": "windows"}))["answer"]["windows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["app_id"].as_str() == Some(PDF))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// PDF Viewer's windows on `device`, each by its tab with the item it was opened on.
fn windows(device: &Device) -> Vec<(u64, Option<String>)> {
    pdf_windows(device)
        .iter()
        .map(|row| {
            (
                row["tab"].as_u64().expect("a window's tab"),
                row["item"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

/// The tab of the window on `device` opened on `item`, once one is.
fn window_on(device: &Device, item: &str) -> u64 {
    let mut tab = None;
    tilo_test_env::wait_until(
        format!(
            "{} shows a window on {item}: {:?}",
            device.label,
            windows(device)
        ),
        || {
            tab = windows(device)
                .into_iter()
                .find(|(_, shown)| shown.as_deref() == Some(item))
                .map(|(tab, _)| tab);
            tab.is_some()
        },
    );
    tab.expect("a window shows the item")
}

/// How many of PDF Viewer's windows on `device` are on `item`.
fn windows_on(device: &Device, item: &str) -> usize {
    windows(device)
        .iter()
        .filter(|(_, shown)| shown.as_deref() == Some(item))
        .count()
}

/// Whether the window `tab` on `device` is the one the person works in.
fn in_front(device: &Device, tab: u64) -> bool {
    pdf_windows(device)
        .iter()
        .any(|row| row["tab"].as_u64() == Some(tab) && row["active"].as_bool() == Some(true))
}

/// Whether the window `tab` on `device` draws a control that says `words`: what a person reads
/// in that window, and in no other.
fn window_says(device: &Device, tab: u64, words: &str) -> bool {
    let Some(rect) = pdf_windows(device)
        .into_iter()
        .find(|row| row["tab"].as_u64() == Some(tab))
        .and_then(|row| {
            let rect = row["rect"].as_array()?.clone();
            let at = |i: usize| rect.get(i).and_then(Value::as_i64);
            Some((at(0)?, at(1)?, at(2)?, at(3)?))
        })
    else {
        return false;
    };
    let (x, y, width, height) = rect;
    device.ask(&json!({"op": "list_targets"}))["targets"]
        .as_array()
        .is_some_and(|targets| {
            targets.iter().any(|target| {
                let centre = target["rect"].as_array().and_then(|r| {
                    let at = |i: usize| r.get(i).and_then(Value::as_i64);
                    Some((at(0)? + at(2)? / 2, at(1)? + at(3)? / 2))
                });
                target["name"]
                    .as_str()
                    .is_some_and(|name| name.contains(words))
                    && centre.is_some_and(|(cx, cy)| {
                        cx >= x && cx < x + width && cy >= y && cy < y + height
                    })
            })
        })
}

/// A PDF whose pages each say one line of `pages`, in Helvetica: a file as a person keeps one,
/// whose words the reader reads and the workspace's search finds.
fn pdf(pages: &[&str]) -> Vec<u8> {
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        String::new(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let mut kids = Vec::new();
    for words in pages {
        let stream = format!("BT /F1 24 Tf 72 700 Td ({words}) Tj ET");
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));
        let content = objects.len();
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 3 0 R >> >> /Contents {content} 0 R >>"
        ));
        kids.push(format!("{} 0 R", objects.len()));
    }
    objects[1] = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        pages.len()
    );
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (at, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", at + 1).as_bytes());
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

/// The folder a device's person keeps what they download in: what the files search finds.
const DOWNLOADS: &str = "Downloads";

/// Keep `bytes` as the file `name` in the Downloads of `device`'s person.
fn download(device: &Device, name: &str, bytes: &[u8]) {
    let folder = device.directory().join("home").join(DOWNLOADS);
    std::fs::create_dir_all(&folder).expect("the Downloads folder");
    std::fs::write(folder.join(name), bytes).expect("the downloaded file");
}

/// Choose the row of the system's search on `device` that `pick` picks, once one shows for
/// `query`: what the person does typing it and pressing the row.
fn choose_from_search(
    device: &Device,
    query: &str,
    what: &str,
    pick: impl Fn(&tilo_actions::remote::CatalogueRow) -> bool,
) {
    let search = shell_action(device, SHELL_RESOURCE, LayoutEffect::SEARCH.id);
    let answer = device.act_wait(&search, &json!({"query": query}));
    assert!(
        answer.get("err").is_none(),
        "{} searches for {query}: {answer}",
        device.label
    );
    let mut row = None;
    tilo_test_env::wait_until(
        format!("{}'s search for {query} to show {what}", device.label),
        || {
            row = device
                .actions()
                .into_iter()
                .find(|row| {
                    row.parent_resource == SEARCH_RESOURCE
                        && row.action.as_ref().is_some_and(|action| {
                            action.address.action == LayoutEffect::CHOOSE_SEARCH_ROW.id
                        })
                        && pick(row)
                })
                .map(|row| target(&row));
            row.is_some()
        },
    );
    let answer = device.act_wait(&row.expect("a row shows it"), &json!({}));
    assert!(
        answer.get("err").is_none(),
        "{} chooses {what}: {answer}",
        device.label
    );
}

/// What a search row says under its name.
fn detail(row: &tilo_actions::remote::CatalogueRow) -> Option<&str> {
    row.resource
        .presentation
        .as_ref()
        .and_then(|presentation| presentation.detail.as_deref())
}

/// Open the file `name` from the person's Downloads, found by the system's search, in the app
/// that opens it: as a person opens a PDF they downloaded.
fn open_file(device: &Device, name: &str) {
    choose_from_search(device, name, &format!("the file {name}"), |row| {
        row.resource.label.starts_with(name) && detail(row) == Some(DOWNLOADS)
    });
}

/// Open the document of PDF Viewer's library called `name` from the system's search.
fn open_from_search(device: &Device, name: &str) {
    choose_from_search(device, name, &format!("the document {name}"), |row| {
        row.resource.label.starts_with(name) && detail(row) != Some(DOWNLOADS)
    });
}

/// The published resource of the page of PDF Viewer's library that says `words`, once the
/// reader has read its words.
fn page_saying(device: &Device, words: &str) -> String {
    let mut found = None;
    tilo_test_env::wait_until(
        format!("{} lists the page saying {words}", device.label),
        || {
            found = device
                .actions()
                .into_iter()
                .find(|row| {
                    row.app_id == PDF
                        && row.resource.kind == PAGE
                        && row.resource.label.contains(words)
                })
                .map(|row| row.resource.id);
            found.is_some()
        },
    );
    found.expect("the page is listed")
}

/// The field `field` of the document called `name` in PDF Viewer's library, as the record API
/// reads it: what every reader of the library reads of it.
fn document_field(device: &Device, workspace: &str, name: &str, field: &str) -> Option<Value> {
    fn find(value: &Value, name: &str, field: &str) -> Option<Value> {
        match value {
            Value::Object(fields) => {
                if fields.get("source_name").and_then(Value::as_str) == Some(name) {
                    return fields.get(field).cloned();
                }
                fields.values().find_map(|inner| find(inner, name, field))
            }
            Value::Array(items) => items.iter().find_map(|inner| find(inner, name, field)),
            _ => None,
        }
    }
    let records = device
        .read(workspace, (PDF, ""), None, None)
        .unwrap_or_else(|error| panic!("{} reads PDF Viewer's library: {error}", device.label));
    find(&records, name, field)
}

/// The id a published resource (`tl:<app>:<document>/<item>`) names its item by.
fn item_id(resource: &str) -> &str {
    resource.rsplit_once('/').map_or(resource, |(_, item)| item)
}

/// PDF Viewer's library in `workspace`, as the workspace's record API reads it for any reader:
/// another app, an agent, a script.
fn library(device: &Device, workspace: &str) -> String {
    device
        .read(workspace, (PDF, ""), None, None)
        .unwrap_or_else(|error| {
            panic!(
                "{} reads PDF Viewer's library: {error}\n{}",
                device.label,
                device.diagnostic()
            )
        })["records"]
        .to_string()
}

/// Ada's laptop in the Studio workspace with PDF Viewer installed, and two PDFs in her
/// Downloads: `First.pdf`, one page, and `Second.pdf`, whose second page speaks of the harbour.
fn studio() -> (Site, Device) {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    site.publish(Role::Editor);
    let mut laptop = ada.device("Ada's laptop");
    download(&laptop, "First.pdf", &pdf(&["A first letter"]));
    download(
        &laptop,
        "Second.pdf",
        &pdf(&["An opening page", "The harbour route"]),
    );
    laptop.install_dev_apps();
    crate::common::dev_apps::install_tree_app("pdf", &laptop.store());
    let hers = site.admit(ada.public);
    join_together(&mut [&mut laptop], &[hers.as_str()]);
    laptop.wait_holding(&site.workspace);
    laptop.wait_showing(&site.workspace);
    (site, laptop)
}

#[test]
fn each_window_shows_one_document_of_the_one_library() {
    let (site, mut laptop) = studio();

    // Each PDF opened from the person's files is a document of the one library, in a window of
    // its own that the session holds on that document.
    open_file(&laptop, "First.pdf");
    let first = document(&laptop, "First.pdf");
    let first_window = window_on(&laptop, &first);
    open_file(&laptop, "Second.pdf");
    let second = document(&laptop, "Second.pdf");
    assert_ne!(first, second, "two documents of the library");
    let second_window = window_on(&laptop, &second);
    assert_ne!(first_window, second_window, "a window for each document");

    // Opening the first file again comes back to its window: the bytes are the document it
    // already is, and the document has one window.
    open_file(&laptop, "First.pdf");
    tilo_test_env::wait_until("the first document's window to come forward", || {
        in_front(&laptop, first_window)
    });
    assert_eq!(windows_on(&laptop, &first), 1, "{:?}", windows(&laptop));
    assert_eq!(
        windows(&laptop).len(),
        2,
        "no third window: {:?}",
        windows(&laptop)
    );
    // So does finding the document itself in search.
    open_from_search(&laptop, "First.pdf");
    assert_eq!(windows_on(&laptop, &first), 1, "{:?}", windows(&laptop));

    // An edit of the second document through the app's own action is seen in the second
    // window's view, the one that shows it, and reaches the library every reader lists.
    open_from_search(&laptop, "Second.pdf");
    tilo_test_env::wait_until("the second document's window to come forward", || {
        in_front(&laptop, second_window)
    });
    let opening = page_saying(&laptop, "An opening page");
    step(
        &laptop,
        &document_action(&laptop, &second, "sign_with_name"),
        &json!({"p": opening, "signer": "Ada Lovelace"}),
    );
    tilo_test_env::wait_until("the second window to show the signature", || {
        window_says(&laptop, second_window, "Ada Lovelace")
    });
    assert!(
        !window_says(&laptop, first_window, "Ada Lovelace"),
        "the first window shows its own document"
    );
    tilo_test_env::wait_until("the library to hold the signature", || {
        library(&laptop, &site.workspace).contains("Ada Lovelace")
    });
    // Both windows go on showing their own documents.
    assert_eq!(window_on(&laptop, &first), first_window);
    assert_eq!(window_on(&laptop, &second), second_window);

    // Closing the first window leaves the second on its document.
    step(
        &laptop,
        &shell_action(
            &laptop,
            &layout::Item::Place.resource(&first_window.to_string()),
            "close",
        ),
        &json!({}),
    );
    tilo_test_env::wait_until("the first window to close", || {
        !windows(&laptop).iter().any(|(tab, _)| *tab == first_window)
    });
    assert_eq!(window_on(&laptop, &second), second_window);
    assert!(
        library(&laptop, &site.workspace).contains("First.pdf"),
        "closing a window removes no document of the library"
    );

    // The next start of the device opens the window on the document it showed.
    laptop.stop();
    laptop.start();
    laptop.wait_showing(&site.workspace);
    window_on(&laptop, &second);
    assert_eq!(
        windows_on(&laptop, &first),
        0,
        "the closed window stays closed: {:?}",
        windows(&laptop)
    );
}

#[test]
fn a_hit_inside_a_document_opens_its_window_at_the_hit() {
    let (site, laptop) = studio();
    open_file(&laptop, "Second.pdf");
    let second = document(&laptop, "Second.pdf");
    let window = window_on(&laptop, &second);
    let harbour = page_saying(&laptop, "harbour");

    // A hit on the words of the document's second page opens the document's window, turned to
    // that page: no window of the page's own, and the reader is where the hit is.
    choose_from_search(&laptop, "harbour", "the page that says it", |row| {
        row.resource.label.contains("harbour") && detail(row) != Some(DOWNLOADS)
    });
    tilo_test_env::wait_until("the reader to turn to the page the hit names", || {
        document_field(&laptop, &site.workspace, "Second.pdf", "reading_page")
            .and_then(|page| page.as_str().map(str::to_owned))
            .is_some_and(|page| item_id(&page) == item_id(&harbour))
    });
    assert_eq!(windows(&laptop).len(), 1, "{:?}", windows(&laptop));
    assert_eq!(window_on(&laptop, &second), window);

    // With the window closed, the same hit opens a window on the document, at the page.
    step(
        &laptop,
        &shell_action(
            &laptop,
            &layout::Item::Place.resource(&window.to_string()),
            "close",
        ),
        &json!({}),
    );
    tilo_test_env::wait_until("the window to close", || windows(&laptop).is_empty());
    step(
        &laptop,
        &document_action(&laptop, &second, "remember_reading"),
        &json!({"p": page_saying(&laptop, "An opening page"), "fraction": 0.0, "zoom": 100.0}),
    );
    choose_from_search(&laptop, "harbour", "the page that says it", |row| {
        row.resource.label.contains("harbour") && detail(row) != Some(DOWNLOADS)
    });
    let reopened = window_on(&laptop, &second);
    assert_ne!(reopened, window, "a window of its own, on the document");
    tilo_test_env::wait_until("the new window to turn to the page the hit names", || {
        document_field(&laptop, &site.workspace, "Second.pdf", "reading_page")
            .and_then(|page| page.as_str().map(str::to_owned))
            .is_some_and(|page| item_id(&page) == item_id(&harbour))
    });
    assert_eq!(windows(&laptop).len(), 1, "{:?}", windows(&laptop));
}
