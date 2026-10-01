//! The existing room Chat document, checked and projected without demo seeding.
use ui::application::{AppComponent, ApplicationDefinition, HostContext, LaunchTarget};
use ui::{launch::LaunchError, record::RecordKind, widget::IconDescriptor};
static DEFINITION: std::sync::LazyLock<ApplicationDefinition> =
    std::sync::LazyLock::new(|| ApplicationDefinition {
        app_id: "so.tilo.chat",
        title: "Chat",
        icon: IconDescriptor::Named("chat".into()),
        record_kind: RecordKind::new("chat.workspace"),
        required: vec![],
        optional: vec![],
        opens: &[],
        create: |context, target, _| create(context, &target),
    });
pub fn definition() -> &'static ApplicationDefinition {
    &DEFINITION
}
fn create(mut context: HostContext, target: &LaunchTarget) -> Result<AppComponent, LaunchError> {
    if !context.record().id.is_package_primary() {
        return Err(LaunchError::BindingMismatch);
    }
    let author = context.author().0.clone();
    let identity = crate::identity::LocalIdentity {
        id: author.clone(),
        name: author.clone(),
        initials: author.chars().take(2).collect(),
        color_idx: crate::identity::color_index_for(author.as_bytes()),
    };
    if matches!(target, LaunchTarget::PrimaryNew) {
        context
            .initialize_new_record(|doc| {
                tilo_actions::Action::apply(
                    &crate::actions::ChatAction::SeedStarter {
                        id: identity.id.clone(),
                        name: identity.name.clone(),
                        initials: identity.initials.clone(),
                        color_idx: identity.color_idx,
                    },
                    doc,
                )
                .map_err(|error| error.to_string())
            })
            .map_err(LaunchError::Record)?;
    }
    let runtime = if crate::app::native_services() {
        crate::app::ChatRuntime::Native
    } else {
        crate::app::ChatRuntime::Unavailable
    };
    let widget = context
        .read_record(|doc| crate::ChatApp::from_document(doc, identity, runtime))
        .map_err(LaunchError::Record)?
        .map_err(LaunchError::Operation)?;
    let mut registry = tilo_actions::ActionRegistry::new();
    crate::actions::register(&mut registry);
    Ok(AppComponent::new(context, Box::new(widget), registry))
}
impl ui::widget::AppWidget for crate::ChatApp {
    fn capture_application_view(
        &self,
    ) -> Result<Option<ui::application_view::ApplicationLocalView>, ui::view::ThawError> {
        self.capture_local()
    }
    fn restore_application_view(
        &mut self,
        local: &ui::application_view::ApplicationLocalView,
        doc: &automerge::AutoCommit,
    ) -> Result<ui::application_view::ApplicationFocusRestore, ui::view::ThawError> {
        self.restore_local(local, doc)
    }

    fn app_kind(&self) -> ui::widget::AppKind {
        ui::widget::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Chat"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Named("chat".into())
    }
    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        None
    }
}
