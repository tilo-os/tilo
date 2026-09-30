//! Canonical Notes application definition.
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
};
use ui::launch::LaunchError;
use ui::record::RecordKind;
use ui::widget::IconDescriptor;

static DEFINITION: std::sync::LazyLock<ApplicationDefinition> =
    std::sync::LazyLock::new(|| ApplicationDefinition {
        app_id: "so.tilo.notes",
        title: "Notes",
        icon: IconDescriptor::Named("notes".into()),
        record_kind: RecordKind::new("notes.notebook"),
        required: vec![],
        optional: vec![],
        opens: &[],
        create: |context, target, restore| create_component(context, &target, &restore),
    });
pub fn definition() -> &'static ApplicationDefinition {
    &DEFINITION
}
fn create_component(
    mut context: HostContext,
    target: &LaunchTarget,
    restore: &RestoreInput,
) -> Result<AppComponent, LaunchError> {
    if let RestoreInput::Local(saved) = restore
        && let Some(local) = &saved.application
    {
        // The framework has validated the envelope against its exact durable
        // route. Preflight the payload using the same finite definition lookup
        // as the later widget restore, without rewriting the saved binding.
        if local.app_id != definition().app_id {
            return Err(LaunchError::Restore(ui::view::ThawError::BindingMismatch));
        }
        let mut definition_local = local.clone();
        definition_local.app_id = definition().app_id.into();
        crate::local_view::NotesLocalViewV1::decode(&definition_local)
            .map_err(LaunchError::Restore)?;
    }
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(crate::notebook::initialize_empty)
            .map_err(LaunchError::Record)?;
    }
    // The sidebar resolves its strings while it is built, before the main
    // panel would register the catalog.
    crate::metadata::register_messages();
    let author = context.author().clone();
    let mut root = context
        .read_record(|doc| crate::NotesApp::from_document(doc, &author))
        .map_err(LaunchError::Record)?
        .map_err(LaunchError::Operation)?;
    if let Some(target) = crate::hosting::take_native_launch_target() {
        root.queue_launch_markdown(&target)
            .map_err(LaunchError::Operation)?;
    }
    let mut registry = tilo_actions::ActionRegistry::new();
    crate::actions::register(&mut registry);
    let mut component = AppComponent::new(context, Box::new(root), registry);
    component
        .register_document_history(&crate::framework_history::HISTORY)
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    Ok(component)
}
