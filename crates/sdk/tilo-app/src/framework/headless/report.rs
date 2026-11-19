//! What an app publishes, as `tilo-app surface` commits it and
//! `tilo-app check` compares it: the resource's surface, the person's own resources
//! and those the views open beside it, the controls its view binds, and the
//! ids of its effects and named views.

use std::rc::Rc;

use tilo_resource::Resource;
use ui_core::headless::FrameHarness;
use ui_core::permissions::Role;

use super::super::{PersonKind, controls, reserved_action_ids};
use super::{
    AUTHOR, Launch, Opening, Profile, ResourceApp, ResourceRoot, TEST_DEVICE, TEST_WORKSPACE,
    open_app, operation, profile_directory,
};

/// What `A` publishes and how its view binds to it, as `tilo-app surface`
/// commits it and `tilo-app check` compares it: the resource's
/// [`Surface`](tilo_resource::Surface), those of the person's own resources it
/// declares ([`ResourceApp::PERSON_RESOURCES`]) and those of the resources its
/// views open beside its own (the parts of its chains, the settings
/// sections), every control its view, built headless on an empty document,
/// binds to something the resource does not publish, the ids of its effects
/// and named views, and the ids the framework keeps for its own actions
/// ([`reserved_action_ids`](super::reserved_action_ids)).
#[must_use]
pub fn surface_report<A: ResourceApp>() -> serde_json::Value {
    let mut report = report_surface(
        &Opening::of::<A>(),
        <A::Resource as Resource>::surface(),
        A::PERSON_RESOURCES,
        &published_ids::<A>,
    );
    for other in A::OTHER_WINDOWS {
        other.merge_into(&mut report);
    }
    report
}

/// What a built view publishes beside its resource's verbs: its effects and
/// the resource's named views, and what the effects declare, which its surface
/// publishes as actions of the resource.
#[derive(Default)]
struct PublishedActions {
    /// The ids of every effect, views included.
    effects: Vec<String>,
    /// The ids of the resource's named views.
    views: Vec<String>,
    /// Every read effect's declaration: what its surface publishes it as.
    rows: Vec<tilo_resource::effect::EffectRow>,
}

/// [`PublishedActions`] of `A`'s root `harness` hosts.
fn published_ids<A: ResourceApp>(harness: &mut FrameHarness) -> PublishedActions {
    harness
        .with_widget_mut(|root: &mut ResourceRoot| {
            let typed = root.typed::<A::Resource>()?;
            let views = <A::Resource as Resource>::views(&typed.live.get());
            Some(PublishedActions {
                effects: typed
                    .effects
                    .specs()
                    .iter()
                    .map(|spec| spec.id.clone())
                    .collect(),
                views: views.into_iter().map(|view| view.id).collect(),
                rows: typed.effects.rows(),
            })
        })
        .flatten()
        .unwrap_or_default()
}

/// [`surface_report`] of the app `opening` describes, whose resource's
/// surface is `surface` and whose person resources are `person`; `published`
/// reads the ids of the effects and the named views a built view publishes.
fn report_surface(
    opening: &Opening,
    mut surface: tilo_resource::Surface,
    person: &[PersonKind],
    published: &dyn Fn(&mut FrameHarness) -> PublishedActions,
) -> serde_json::Value {
    let collecting = collect_unbound();
    let profile = profile_directory().map(Rc::new).map_err(operation);
    let built = profile.and_then(|profile| {
        open_app(
            opening,
            Launch {
                profile: Profile::Temporary(
                    profile,
                    TEST_WORKSPACE.to_owned(),
                    TEST_DEVICE.to_owned(),
                ),
                role: Role::Owner,
                author: AUTHOR,
                arguments: &[],
                file: None,
                handoff: None,
                capture: None,
            },
            None,
            &mut |_| {},
        )
    });
    // What the view publishes besides the resource's verbs: its effects and
    // the resource's named views, whose ids must not be the framework's. The
    // app's read effects are actions of its own resource and are published
    // with it, as a verb of the surface.
    let mut declared = PublishedActions::default();
    let mut view = None;
    match built {
        Ok(mut opened) => declared = published(&mut opened.harness),
        Err(error) => view = Some(error.to_string()),
    }
    add_effect_verbs(&mut surface, &declared.rows);
    let PublishedActions { effects, views, .. } = declared;
    let (unbound, opened) = collecting.finish();
    let mut resources = vec![surface];
    resources.extend(opened);
    // The person's own resources the app declares are part of its surface whether
    // or not a view of it was showing: the host admits the app's changes to
    // them, and its reads of them, against this.
    for declared in person {
        match resources
            .iter_mut()
            .find(|resource| resource.kind == declared.kind())
        {
            Some(resource) => resource.person = true,
            None => resources.push(declared.surface()),
        }
    }
    serde_json::json!({
        "resources": resources,
        "unbound": unbound,
        "effects": effects,
        "views": views,
        "reserved": reserved_action_ids(),
        "view": view,
    })
}

/// Publish the app's read effects on `surface`: its resource's verbs take
/// them, each with its parameters and the value it answers with, and the
/// types and enums that value names join the surface's types.
/// Its view effects are what the surface says it shows.
fn add_effect_verbs(
    surface: &mut tilo_resource::Surface,
    rows: &[tilo_resource::effect::EffectRow],
) {
    use tilo_resource::surface::{FieldType, NamedType};

    let mut types = tilo_resource::surface::TypeSet::new();
    let mut verbs = tilo_resource::surface::effect_verbs(rows, &mut types);
    let declared: Vec<NamedType> = types.into_types();
    // A type the resource's fields already declare, named again by a value
    // an action answers with, keeps the one name: the surface declares it
    // once.
    let mut renames: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for named in &declared {
        let same = surface.types.iter().find(|known| {
            known.name != named.name && known.fields == named.fields && known.cases == named.cases
        });
        if let Some(known) = same {
            renames.insert(named.name.clone(), known.name.clone());
        }
    }
    let renamed_type = |ty: &FieldType| ty.renamed(&renames);
    for verb in &mut verbs {
        for param in &mut verb.params {
            param.ty = renamed_type(&param.ty);
        }
        if let Some(returns) = &mut verb.returns {
            returns.ty = renamed_type(&returns.ty);
        }
    }
    let kept: Vec<NamedType> = declared
        .into_iter()
        .filter(|named| !renames.contains_key(&named.name))
        .map(|mut named| {
            for field in &mut named.fields {
                field.ty = renamed_type(&field.ty);
            }
            for case in &mut named.cases {
                for field in &mut case.fields {
                    field.ty = renamed_type(&field.ty);
                }
            }
            named
        })
        .filter(|named| !surface.types.iter().any(|known| known.name == named.name))
        .collect();
    surface.types.extend(kept);
    surface.verbs.extend(verbs);
    surface.shows = tilo_resource::surface::view_effects(rows);
}

/// Collects the controls bound to something the resource does not publish
/// until [`Self::finish`], instead of failing on the first.
struct Collecting(());

/// Start collecting unbound controls on this thread.
fn collect_unbound() -> Collecting {
    controls::UNBOUND.with(|unbound| *unbound.borrow_mut() = Some(Vec::new()));
    controls::OPENED.with(|opened| *opened.borrow_mut() = Some(Vec::new()));
    Collecting(())
}

impl Collecting {
    /// What was collected: one sentence per unbound control, and the
    /// surfaces of the other documents the views opened.
    fn finish(self) -> (Vec<String>, Vec<tilo_resource::Surface>) {
        let unbound =
            controls::UNBOUND.with(|unbound| unbound.borrow_mut().take().unwrap_or_default());
        let opened = controls::OPENED.with(|opened| opened.borrow_mut().take().unwrap_or_default());
        drop(self);
        (unbound, opened)
    }
}

impl Drop for Collecting {
    fn drop(&mut self) {
        controls::UNBOUND.with(|unbound| *unbound.borrow_mut() = None);
        controls::OPENED.with(|opened| *opened.borrow_mut() = None);
    }
}
