//! What a many-note notebook publishes through the collaboration bridge: the
//! whole surface is large (every note offers the same editing actions, and
//! every body, a rich-text resource of its own, carries the rich-text
//! catalogue), so it crosses as a delta of shared catalogue entries, and an
//! edit republishes only what it touched.

use ui::collaboration::{SurfaceDelta, SurfacePublication, SurfaceTable};

/// What a delta puts on the wire: each shape's encoding, each template's ids
/// and each resource's own fields.
fn wire_bytes(delta: &SurfaceDelta) -> usize {
    let shapes: usize = delta
        .shapes
        .iter()
        .map(|(id, shape)| id.len() + serde_json::to_vec(shape).unwrap().len())
        .sum();
    let templates: usize = delta
        .templates
        .iter()
        .map(|(id, members)| id.len() * (1 + members.len()))
        .sum();
    let resources: usize = delta
        .resources
        .iter()
        .map(|r| {
            r.resource.len()
                + r.fields.resource_kind.len()
                + r.fields.resource_label.len()
                + r.template.len()
                + r.fields
                    .semantic_identity
                    .as_ref()
                    .map_or(0, |i| i.kind.len() + i.key.len())
        })
        .sum();
    let removed: usize = delta.removed.iter().map(String::len).sum();
    shapes + templates + resources + removed
}

/// The wire bytes of the catalogue the note bodies' template alone uses:
/// its id and the shapes no other template holds.
fn rich_text_catalogue(delta: &SurfaceDelta) -> usize {
    let mut bodies: Vec<_> = delta
        .resources
        .iter()
        .filter(|r| r.fields.resource_kind == "tilo.rich-text")
        .map(|r| r.template)
        .collect();
    assert!(bodies.len() >= 45, "every note's body is published");
    bodies.dedup();
    assert_eq!(bodies.len(), 1, "the bodies share one template");
    let body = bodies.first().copied().unwrap();
    let members = |id| {
        delta
            .templates
            .iter()
            .find(|(template, _)| *template == id)
            .map(|(_, members)| members.clone())
            .unwrap_or_default()
    };
    let others: Vec<_> = delta
        .templates
        .iter()
        .filter(|(template, _)| *template != body)
        .flat_map(|(_, members)| members.iter().copied())
        .collect();
    let own = members(body);
    let shapes: usize = delta
        .shapes
        .iter()
        .filter(|(id, _)| own.contains(id) && !others.contains(id))
        .map(|(id, shape)| id.len() + serde_json::to_vec(shape).unwrap().len())
        .sum();
    body.len() * (1 + own.len()) + shapes
}

fn labelled(actions: Vec<ui::collaboration::ExportedAction>) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = actions
        .into_iter()
        .map(|a| (a.resource, a.action, a.resource_label))
        .collect();
    rows.sort();
    rows
}

#[test]
fn a_many_note_notebook_publishes_a_small_surface_and_an_edit_only_its_note() {
    let (mut app, target) = super::cpu_profile::fixture(45);
    let before = tilo_actions::door::survey(&app.harness().resources());
    let full: usize = before
        .iter()
        .map(|d| {
            serde_json::to_vec(&ui::collaboration::ExportedAction::from(d))
                .unwrap()
                .len()
        })
        .sum();
    assert!(
        before.len() > 800 && full > 400_000,
        "the whole surface is large: {} actions, {full} bytes",
        before.len()
    );

    let (first, state) = SurfacePublication::next(None, &before);
    assert!(first.reset);
    let first_bytes = wire_bytes(&first);
    eprintln!(
        "surface-volume first: {} actions, {first_bytes} bytes (whole surface {full}); {} shapes, {} templates, {} resources",
        before.len(),
        first.shapes.len(),
        first.templates.len(),
        first.resources.len()
    );
    // Every note's body is a rich-text resource of its own: the bodies share
    // one template, and its catalogue entries cross once whatever the
    // number of notes. Apart from that one catalogue, Notes publishes what it
    // did before its bodies were resources.
    let rich = rich_text_catalogue(&first);
    eprintln!("surface-volume rich-text catalogue: {rich} bytes");
    assert!(
        rich < 56 * 1024,
        "the rich-text catalogue takes {rich} bytes"
    );
    assert!(
        first_bytes - rich < 100 * 1024,
        "a 45-note notebook publishes {first_bytes} bytes, {rich} of them the rich-text catalogue"
    );

    app.merge_peer_edit(|doc| {
        let action = crate::actions::rename_page_action(target, "Renamed");
        tilo_actions::apply_local(doc, &action, crate::actions::encode(&action)).unwrap();
    });
    let after = tilo_actions::door::survey(&app.harness().resources());
    let (second, _) = SurfacePublication::next(Some((&state, &before)), &after);
    let second_bytes = wire_bytes(&second);
    eprintln!(
        "surface-volume edit: {} resources, {second_bytes} bytes",
        second.resources.len()
    );
    assert!(!second.reset);
    assert!(second.shapes.is_empty(), "no catalogue entry is resent");
    assert!(
        second_bytes < 4 * 1024 && second.resources.len() <= 4,
        "one note edit republishes {} resources, {second_bytes} bytes",
        second.resources.len()
    );

    // The broker's expansion is the surface that was published.
    let mut table = SurfaceTable::default();
    table.apply(&first).unwrap();
    table.apply(&second).unwrap();
    assert_eq!(
        labelled(table.actions()),
        labelled(
            after
                .iter()
                .map(ui::collaboration::ExportedAction::from)
                .collect()
        )
    );
}
