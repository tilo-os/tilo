//! Calculator's checked application entry point for native and browser hosts.
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
};
use ui::{launch::LaunchError, record::RecordKind, widget::IconDescriptor};

static DEFINITION: std::sync::LazyLock<ApplicationDefinition> =
    std::sync::LazyLock::new(|| ApplicationDefinition {
        app_id: "so.tilo.calculator",
        title: "Calculator",
        icon: IconDescriptor::Named("calculator".into()),
        record_kind: RecordKind::new("calculator.document"),
        required: vec![],
        optional: vec![],
        create,
    });

pub fn definition() -> &'static ApplicationDefinition {
    &DEFINITION
}

fn create(
    mut context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(crate::doc::initialize_empty)
            .map_err(LaunchError::Record)?;
    }
    let root = context
        .read_record(crate::CalculatorApp::from_document)
        .map_err(LaunchError::Record)?
        .map_err(LaunchError::Operation)?;
    let mut registry = tilo_actions::ActionRegistry::new();
    crate::actions::register(&mut registry);
    let mut component = AppComponent::new(context, Box::new(root), registry);
    component
        .register_document_history(&crate::framework_history::HISTORY)
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    Ok(component)
}
