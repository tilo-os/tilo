//! Host-mounted entry point: credential custody comes from the host context.
use crate::TubeApp;
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
};
use ui::launch::LaunchError;
use ui::widget::{AppKind, AppWidget, IconDescriptor};

pub(crate) fn definition() -> &'static ApplicationDefinition {
    static DEFINITION: std::sync::OnceLock<ApplicationDefinition> = std::sync::OnceLock::new();
    DEFINITION.get_or_init(|| ApplicationDefinition {
        app_id: "so.tilo.tube",
        title: "Tube",
        icon: IconDescriptor::Named("application".into()),
        record_kind: ui::record::RecordKind::new("so.tilo.tube.app"),
        required: vec![],
        optional: vec![],
        opens: &[],
        create,
    })
}

fn create(
    mut context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    let joined = !matches!(target, LaunchTarget::New | LaunchTarget::PrimaryNew);
    drop(target);
    if !joined {
        context
            .initialize_new_record(|_| Ok(()))
            .map_err(LaunchError::Record)?;
    }
    let root = TubeApp::empty().online_with_secrets(&context.secrets());
    let mut registry = tilo_actions::ActionRegistry::new();
    crate::actions::register(&mut registry);
    Ok(AppComponent::new(context, Box::new(root), registry))
}

impl AppWidget for TubeApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Tube"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Named("application".into())
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}
