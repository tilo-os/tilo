//! Real App surface frames must use the checked publication owner, not guessed controls.
use super::*;
use crate::application::{AppComponent, LaunchTarget, RestoreInput};
use crate::application_test_support::{ProbeHost, definition, runner_inputs};
use crate::widget::{Button, Column, Element, resource_actions};
use tilo_actions::{
    ActionApp, ActionResource, INTERACTION_VERSION, InteractionAction, InteractionActor,
    InteractionActorKind, InteractionAnchor, InteractionPhase, InvocationId, SemanticInteraction,
};

struct Page(Element);
impl Widget for Page {
    fn name(&self) -> &'static str {
        "PublicationPage"
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.0.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![self.0.as_mut()]
    }
    crate::testing::delegate_widget_methods!(
        0: sync, build_layout, apply_layout, draw, handle_event, get_rect
    );
}
crate::testing::test_app_widget!(
    Page,
    "Publication",
    icon = crate::widget::IconDescriptor::Named("notes".into())
);

#[expect(
    clippy::unwrap_used,
    reason = "test fixture requires its declared production action binding; missing setup must fail fast"
)]
fn component(
    mut context: crate::application::HostContext,
    count: usize,
) -> Result<AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_record(|_| Ok(()))
        .map_err(crate::launch::LaunchError::Record)?;
    let mut resource = resource_actions::resource("locus", "Exact locus", "edit", "Edit");
    resource.actions[0].contract.permission = tilo_actions::PermissionRequirement::None;
    let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
    let mut column = Column::new();
    for index in 0..count {
        column = column.push(Box::new(
            Button::new(&format!("locus-{index}"), "Edit").on_action(binding.clone()),
        ));
    }
    let root = resource_actions::immediate(Box::new(column), resource, |_| {
        Ok(tilo_actions::ActionOutput::default())
    });
    Ok(AppComponent::new(
        context,
        Box::new(Page(root)),
        tilo_actions::ActionRegistry::new(),
    ))
}

struct Surface;
impl AppSurface for Surface {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(640, 480)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: CursorIcon) {}
}

#[expect(
    clippy::unwrap_used,
    reason = "test fixture requires a mounted production component and exact published owner; missing setup must fail fast"
)]
fn mounted(count: usize) -> (ProbeHost, App, SemanticInteraction) {
    let host = ProbeHost::new();
    let create: crate::application::ComponentFactory = if count == 1 {
        |context, _, _| component(context, 1)
    } else {
        |context, _, _| component(context, 2)
    };
    let definition = definition(create);
    let context = host.prepare(&definition, 12, &LaunchTarget::New).unwrap();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let mut app = App::from_component(component, runner_inputs()).unwrap();
    let snapshots = crate::bound_dispatch::discover_bound_resources(
        app.root_widget.as_ref(),
        &app.state.doc,
        None,
    )
    .unwrap();
    let snapshot = snapshots
        .iter()
        .find(|snapshot| !snapshot.resources.is_empty())
        .unwrap();
    let resource = app
        .actions
        .bound_publication
        .project(&snapshot.binding, snapshot.resources[0].clone())
        .unwrap();
    app.set_presence_broadcast_scope(Some(crate::presence::PresenceKey::Workspace {
        id: "room-a".into(),
    }));
    app.presence_ctx_mut().apply_collaboration_snapshot(&[
        crate::collaboration::Collaborator {
            id: "person:viewer".into(),
            name: "Viewer".into(),
            colour: None,
            is_agent: false,
            is_self: true,
        },
        crate::collaboration::Collaborator {
            id: "agent:scribe".into(),
            name: "Scribe".into(),
            colour: None,
            is_agent: true,
            is_self: false,
        },
    ]);
    let interaction = SemanticInteraction {
        version: INTERACTION_VERSION,
        invocation: InvocationId(uuid::Uuid::from_u128(41)),
        sequence: 1,
        actor: InteractionActor {
            id: "agent:scribe".into(),
            kind: InteractionActorKind::Agent,
        },
        phase: InteractionPhase::Applying,
        action: InteractionAction {
            id: "edit".into(),
            label: "Edit".into(),
            app: Some(ActionApp {
                id: definition.app_id.into(),
                label: "Publication".into(),
            }),
        },
        resource: ActionResource {
            id: resource.id.as_str().into(),
            kind: "locus".into(),
            label: "Exact locus".into(),
        },
        anchor: InteractionAnchor::Resource,
        group: None,
    };
    (host, app, interaction)
}

#[expect(
    clippy::unwrap_used,
    reason = "test fixture interaction must be admitted before checking painted production markers; refusal must fail fast"
)]
fn paint(
    app: &mut App,
    interaction: &SemanticInteraction,
) -> Vec<crate::presence::AgentMarkerTarget> {
    app.presence_ctx_mut()
        .apply_interaction(interaction.clone())
        .unwrap();
    let mut scene = Scene::new();
    let _ = app.surface_frame(&Surface, &mut scene);
    assert!(!scene.encoding().is_empty());
    let surface = app.presence_ctx().agent_marker_surface_key("primary");
    app.presence_ctx()
        .agent_marker_targets(&surface, crate::time::now())
}

#[test]
fn bound_publication_activity_is_painted_by_the_real_app_surface() {
    let (_host, mut app, interaction) = mounted(1);
    let before = app.state.doc.clone().save();
    let markers = paint(&mut app, &interaction);
    let target = app
        .actions
        .bound_publication
        .target(&tilo_actions::ActionAddress::new(
            &interaction.resource.id,
            &interaction.action.id,
        ))
        .unwrap()
        .expect("live publication owner");
    assert!(
        crate::action_locus::publication::PublicationGeometry {
            root: app.root_widget.as_ref(),
            nodes: &app.layout.node_map,
            names: &app.bindings.semantic_node_names(&app.layout.node_map),
            focused: app.focused_node,
        }
        .bounds(&app.state.doc, &target)
        .is_some(),
        "visible bound control must resolve"
    );
    assert_eq!(
        app.presence_ctx()
            .visible_interactions(crate::time::now())
            .len(),
        1
    );
    assert_eq!(
        target.binding.app_id,
        interaction.action.app.as_ref().unwrap().id
    );
    assert!(
        app.history
            .as_ref()
            .is_none_or(|history| history.live_visible()),
        "live app history"
    );
    assert!(
        app.history_layout.is_none_or(|layout| layout.app_visible),
        "visible app layout"
    );
    assert!(app.close_admission.dialog.is_none(), "no close dialog");
    let projected = crate::action_locus::publication::PublicationGeometry {
        root: app.root_widget.as_ref(),
        nodes: &app.layout.node_map,
        names: &app.bindings.semantic_node_names(&app.layout.node_map),
        focused: app.focused_node,
    }
    .interactions(
        &app.state.doc,
        &app.actions.bound_publication,
        std::slice::from_ref(&interaction),
    );
    assert_eq!(projected.len(), 1);
    assert!(projected[0].1.is_some(), "exact bound alias projected");
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].interaction, interaction);
    assert!(KurboRect::new(0.0, 0.0, 640.0, 480.0).contains(markers[0].bounds.center()));
    assert_eq!(app.state.doc.clone().save(), before);
}

#[test]
fn bound_publication_activity_refuses_foreign_app_and_ambiguous_controls() {
    let (_host, mut app, mut interaction) = mounted(1);
    interaction.action.app.as_mut().unwrap().id = "so.tilo.foreign".into();
    assert!(paint(&mut app, &interaction).is_empty());
    let (_host, mut app, interaction) = mounted(2);
    assert!(paint(&mut app, &interaction).is_empty());
}

#[test]
fn bound_publication_activity_withdraws_the_marker_when_its_mount_disappears() {
    let (_host, mut app, interaction) = mounted(1);
    assert_eq!(paint(&mut app, &interaction).len(), 1);
    app.root_widget = Box::new(crate::widget::Text::new("Replacement"));
    app.layout.structure_dirty = true;
    let mut scene = Scene::new();
    let _ = app.surface_frame(&Surface, &mut scene);
    let surface = app.presence_ctx().agent_marker_surface_key("primary");
    assert!(
        app.presence_ctx()
            .agent_marker_targets(&surface, crate::time::now())
            .is_empty()
    );
}

#[test]
fn cached_native_presence_layer_publishes_and_retires_exact_bound_markers() {
    let clock = crate::background::Pumped::new();
    let _entered = clock.background().enter();
    let (_host, mut app, interaction) = mounted(1);
    let mut scene = Scene::new();
    let _ = app.surface_frame(&Surface, &mut scene);
    // Both production drawing paths receive the same stopped UI tick.
    // Comparing draws at different ticks would compare animation positions,
    // rather than whether the cached layer uses the exact published locus.
    let now = clock.now();
    app.presence_ctx_mut()
        .apply_interaction_at(interaction.clone(), now)
        .unwrap();
    let viewport = KurboRect::new(0.0, 0.0, 640.0, 480.0);
    draw_agent_action_presence(&mut app, &mut scene, Affine::IDENTITY, viewport, now);
    let surface = app.presence_ctx().agent_marker_surface_key("primary");
    let expected = app.presence_ctx().agent_marker_targets(&surface, now);
    assert_eq!(expected.len(), 1);
    let before = app.state.doc.clone().save();
    app.presence_ctx_mut().clear_agent_marker_surface(&surface);
    let mut layer = crate::presence::PresenceLayer::new();
    let _ = draw_presence_layer(&mut app, &mut layer, Affine::IDENTITY, viewport, now);
    let markers = app.presence_ctx().agent_marker_targets(&surface, now);
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].interaction, interaction);
    assert_eq!(markers[0].bounds, expected[0].bounds);
    assert_eq!(
        app.window.agent_marker_surface.as_deref(),
        Some(surface.as_str())
    );
    assert!(!layer.scene().encoding().is_empty());
    layer.presented();

    // A frame re-records the layer against the current retained owner; it
    // must not reuse last frame's marker when that owner disappears.
    app.root_widget = Box::new(crate::widget::Text::new("Replacement"));
    app.init_layout();
    let _ = draw_presence_layer(&mut app, &mut layer, Affine::IDENTITY, viewport, now);
    assert!(
        app.presence_ctx()
            .agent_marker_targets(&surface, now)
            .is_empty()
    );
    let damage = layer
        .damage()
        .expect("both recorded layers have bounded parts");
    assert!(
        damage
            .iter()
            .any(|rect| rect.contains(expected[0].bounds.center()))
    );
    assert_eq!(app.state.doc.clone().save(), before);
}

#[test]
fn cached_native_presence_layer_refuses_foreign_and_ambiguous_bound_loci() {
    for ambiguous in [false, true] {
        let (_host, mut app, mut interaction) = mounted(if ambiguous { 2 } else { 1 });
        if !ambiguous {
            interaction.action.app.as_mut().unwrap().id = "so.tilo.foreign".into();
        }
        assert!(paint(&mut app, &interaction).is_empty());
        let surface = app.presence_ctx().agent_marker_surface_key("primary");
        let mut layer = crate::presence::PresenceLayer::new();
        let now = crate::time::now();
        let _ = draw_presence_layer(
            &mut app,
            &mut layer,
            Affine::IDENTITY,
            KurboRect::new(0.0, 0.0, 640.0, 480.0),
            now,
        );
        assert!(
            app.presence_ctx()
                .agent_marker_targets(&surface, now)
                .is_empty()
        );
    }
}
