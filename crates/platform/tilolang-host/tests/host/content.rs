//! A body pasted past a mebibyte is more than a change records whole: its action keeps the
//! body as content in the record's own store, sealed, and offers it on the record's route. The
//! writing device reads the action back as it ran, after the record opens again too, and undo
//! takes the paste back exactly; a peer that holds only the change asks the route for the body
//! and decodes the action once it arrives.

use super::*;
use tilo_actions::{ActionRegistry, Verified, entry_of, verify_change};
use tilo_content::ContentStore;

/// `paragraphs` paragraphs past a mebibyte in all, each saying where it is.
fn pasted_body(paragraphs: usize) -> patch::rich::RichText {
    let words = 190_000_usize.div_ceil(paragraphs);
    patch::rich::RichText {
        blocks: (0..paragraphs)
            .map(|at| patch::rich::Block {
                kind: "paragraph".to_owned(),
                text: format!("Pasted paragraph {at}: {}", "words ".repeat(words)),
                ..patch::rich::Block::default()
            })
            .collect(),
        embeds: Vec::new(),
    }
}

/// The body of the note `target` names, as the instance's runtime reads it.
fn body_of(host: &TilolangHost, target: &patch::ResourceRef) -> String {
    let instance = host.instances().next().unwrap().instance();
    let resource = tilolang_tilo::instance::wire::runtime_resource(target).unwrap();
    match instance
        .host()
        .unwrap()
        .runtime()
        .read(&resource, "body")
        .unwrap()
    {
        Value::Rich(body) => body.written_text(),
        other => panic!("a note's body is {other:?}"),
    }
}

/// The body field of the note the window shows beside the list, and that note.
fn body_field(window: &Window) -> (String, patch::ResourceRef) {
    let field = node_with(
        window.root.as_ref().expect("the notebook is shown"),
        "placeholder",
        "Write something",
    )
    .expect("the open note has a body")
    .key
    .clone();
    let note = window
        .in_place
        .clone()
        .expect("the press showed the note beside the list");
    (field, note)
}

/// The change of `doc` whose recorded action keeps parts as content, and those parts.
fn kept_as_content(doc: &mut automerge::AutoCommit) -> (automerge::ChangeHash, Vec<String>) {
    let document = doc.document();
    document
        .get_changes(&[])
        .iter()
        .rev()
        .find_map(|change| {
            let hash = change.hash();
            let action = entry_of(document, hash)?.action?;
            let parts = action.content_parts();
            (!parts.is_empty()).then_some((hash, parts))
        })
        .expect("the paste's change keeps its body as content")
}

/// What replaying the change `change` of `doc` shows, its action's parts read from `store`
/// alone: on a thread no window lent a store, as a history read or a peer's check is.
fn decoded(
    doc: &automerge::AutoCommit,
    change: automerge::ChangeHash,
    store: ContentStore,
) -> Verified {
    let mut doc = doc.clone();
    std::thread::spawn(move || {
        let mut registry = ActionRegistry::new();
        tilolang_tilo::history::register(&mut registry);
        registry.connect_content(store);
        verify_change(&mut doc, change, &registry).unwrap()
    })
    .join()
    .unwrap()
}

/// What the instance's connection offered the record's route as content, by address.
fn offered(sent: &Sent) -> BTreeMap<String, Vec<u8>> {
    sent.borrow()
        .iter()
        .filter_map(|request| match request {
            CollabRequest::OfferContent { hash, bytes } => Some((hash.clone(), bytes.clone())),
            _ => None,
        })
        .collect()
}

/// The address a sealed reference names its ciphertext by.
fn address(reference: &str) -> &str {
    reference
        .split_once('.')
        .map_or(reference, |(hash, _)| hash)
}

#[test]
fn a_paste_past_a_mebibyte_is_kept_in_the_records_store_and_decodes_here_and_on_a_peer() {
    pastes_past_a_mebibyte(17);
}

#[test]
fn one_paragraph_pasted_past_a_mebibyte_is_kept_and_undoes() {
    pastes_past_a_mebibyte(1);
}

/// A paragraph longer than one text may hold is refused: Ada hears it, and the note keeps what
/// she typed.
#[test]
fn a_paragraph_past_what_a_text_may_hold_is_refused_aloud() {
    let route = RouteSpec::app(NOTES, uuid::Uuid::new_v4());
    let mut host = TilolangHost::new(None, wake());
    let (mut window, _sent, _inbox) = instance_on(
        &mut host,
        spec(1, "ada", &route),
        notes(),
        &route,
        Rc::default(),
        Rc::default(),
        automerge::AutoCommit::new(),
    );
    settle(&mut host, &mut window);
    press_new_note(&mut window);
    settle(&mut host, &mut window);
    let (field, note) = body_field(&window);
    for text in ["draft".to_owned(), "words ".repeat(1_500_000)] {
        window.send(&[ToApp::EditRich {
            lane: Lane::Root,
            target: note.clone(),
            key: field.clone(),
            rich: typed_rich(&text),
        }]);
        settle(&mut host, &mut window);
    }
    assert_eq!(
        body_of(&host, &note),
        "draft",
        "the note keeps what was typed"
    );
    assert_eq!(window.notices.len(), 1, "Ada hears the paste was refused");
}

/// Ada pastes `paragraphs` paragraphs past a mebibyte in all over what she typed.
fn pastes_past_a_mebibyte(paragraphs: usize) {
    let route = RouteSpec::app(NOTES, uuid::Uuid::new_v4());
    let device = ContentStore::memory();
    let mut host = TilolangHost::new(None, wake());
    let (mut window, sent, _inbox) = instance_keeping(
        &mut host,
        spec(1, "ada", &route),
        notes(),
        &route,
        Rc::default(),
        Rc::default(),
        automerge::AutoCommit::new(),
        device.clone(),
    );
    settle(&mut host, &mut window);
    press_new_note(&mut window);
    settle(&mut host, &mut window);
    let (field, note) = body_field(&window);
    window.send(&[ToApp::EditRich {
        lane: Lane::Root,
        target: note.clone(),
        key: field.clone(),
        rich: typed_rich("draft"),
    }]);
    settle(&mut host, &mut window);
    let before = body_of(&host, &note);
    assert_eq!(before, "draft", "what Ada typed shows");

    // Ada pastes the body: one edit, too large to record inline.
    let body = pasted_body(paragraphs);
    window.send(&[ToApp::EditRich {
        lane: Lane::Root,
        target: note.clone(),
        key: field,
        rich: body,
    }]);
    settle(&mut host, &mut window);
    assert!(
        window.notices.is_empty(),
        "the paste is kept: {:?}",
        window.notices
    );
    let shown = body_of(&host, &note);
    assert!(
        shown.starts_with("Pasted paragraph 0:")
            && shown.contains(&format!(
                "Pasted paragraph {}:",
                paragraphs.saturating_sub(1)
            ))
            && shown.len() > 1 << 20,
        "the paste shows: {shown:.60}"
    );
    let mut doc = host.instances().next().unwrap().record().doc().clone();
    let (paste, parts) = kept_as_content(&mut doc);
    let record = host.instances().next().unwrap().record();
    assert!(
        parts
            .iter()
            .all(|part| tilo_content::is_sealed(part) && record.content().has(part)),
        "the record's store keeps every part, sealed: {parts:?}"
    );
    let offers = offered(&sent);
    assert!(
        parts.iter().all(|part| offers.contains_key(address(part))),
        "every part is offered on the record's route: {parts:?}, offered {:?}",
        offers.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        decoded(&doc, paste, record.content().clone()),
        Verified::Reproduced,
        "the writing device reads the paste back as it ran"
    );

    reopened_reads_it_back(&route, &sent, device, paste);
    a_peer_fetches_it(&route, &sent, paste, &parts, &offers);

    // Ada's undo takes the paste back exactly.
    window.send(&[ToApp::Undo]);
    settle(&mut host, &mut window);
    assert_eq!(
        body_of(&host, &note),
        before,
        "undo takes the paste back exactly"
    );
}

/// The record opens again on the same device, over what its workspace holds: the device's store
/// still holds the parts of the paste `paste`, and the action reads back as it ran.
fn reopened_reads_it_back(
    route: &RouteSpec,
    sent: &Sent,
    device: ContentStore,
    paste: automerge::ChangeHash,
) {
    // The record opens again on the same device, over what its workspace holds: the device's
    // store still holds the parts, and the action reads back as it ran.
    let mut again = TilolangHost::new(None, wake());
    let (mut reopened, _, _) = instance_keeping(
        &mut again,
        spec(2, "ada", route),
        notes(),
        route,
        Rc::default(),
        Rc::default(),
        workspace_copy(sent),
        device,
    );
    settle(&mut again, &mut reopened);
    let record = again.instances().next().unwrap().record();
    assert_eq!(
        decoded(&workspace_copy(sent), paste, record.content().clone()),
        Verified::Reproduced,
        "after the record opens again, the paste reads back as it ran"
    );
}

/// A peer holds the change `paste` and not the `parts` it kept: decoding it asks the record's
/// route, and once what Ada `offers` arrives the action reproduces its change there too.
fn a_peer_fetches_it(
    route: &RouteSpec,
    sent: &Sent,
    paste: automerge::ChangeHash,
    parts: &[String],
    offers: &BTreeMap<String, Vec<u8>>,
) {
    // A peer holds the change and not its body: decoding it asks the record's route, and once
    // the body arrives the action reproduces its change there too.
    let peer_route = route.clone();
    let mut theirs = TilolangHost::new(None, wake());
    let (mut graces, grace_sent, grace_inbox) = instance_on(
        &mut theirs,
        spec(3, "grace", &peer_route),
        notes(),
        &peer_route,
        Rc::default(),
        Rc::default(),
        workspace_copy(sent),
    );
    settle(&mut theirs, &mut graces);
    let store = theirs
        .instances()
        .next()
        .unwrap()
        .record()
        .content()
        .clone();
    assert!(
        matches!(
            decoded(&workspace_copy(sent), paste, store.clone()),
            Verified::ContentMissing(_)
        ),
        "the peer lacks the body until it arrives"
    );
    settle(&mut theirs, &mut graces);
    let wanted: Vec<String> = grace_sent
        .borrow()
        .iter()
        .filter_map(|request| match request {
            CollabRequest::WantContent { hash } => Some(hash.clone()),
            _ => None,
        })
        .collect();
    assert!(
        parts
            .iter()
            .any(|part| wanted.iter().any(|hash| hash == address(part))),
        "the peer asks the record's route for the body: wanted {wanted:?}"
    );
    for part in parts {
        let hash = address(part);
        grace_inbox
            .borrow_mut()
            .push_back(CollabEvent::RecordContent {
                route: peer_route.clone(),
                hash: hash.to_owned(),
                bytes: offers[hash].clone(),
            });
    }
    settle(&mut theirs, &mut graces);
    assert_eq!(
        decoded(&workspace_copy(sent), paste, store),
        Verified::Reproduced,
        "once the body arrived, the peer's action reproduces its change"
    );
}
