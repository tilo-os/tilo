//! Writer's checked application entry point, shared by native and browser hosts.
use ui::application::{AppComponent, ApplicationDefinition, HostContext, LaunchTarget};
use ui::{launch::LaunchError, record::RecordKind, widget::IconDescriptor};

static DEFINITION: std::sync::LazyLock<ApplicationDefinition> =
    std::sync::LazyLock::new(|| ApplicationDefinition {
        app_id: "so.tilo.word-processor",
        title: "Writer",
        icon: IconDescriptor::Glyph("󰈬"),
        record_kind: RecordKind::new(crate::RECORD_KIND),
        required: vec![],
        optional: vec![],
        opens: &[],
        create: |context, target, _| create(context, &target),
    });
pub fn definition() -> &'static ApplicationDefinition {
    &DEFINITION
}
fn create(mut context: HostContext, target: &LaunchTarget) -> Result<AppComponent, LaunchError> {
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(crate::initialize_empty_document)
            .map_err(LaunchError::Record)?;
    }
    let widget = context
        .read_record(crate::WordProcessorWidget::from_document)
        .map_err(LaunchError::Record)?
        .map_err(LaunchError::Operation)?;
    let mut registry = tilo_actions::ActionRegistry::new();
    crate::actions::register(&mut registry);
    Ok(AppComponent::new(context, Box::new(widget), registry))
}
