#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local rendering geometry"
)]
#[cfg(feature = "thin")]
extern crate runtime_dylib;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;
#[allow(
    clippy::unnecessary_wraps,
    reason = "ShotFactory requires this exact result type"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(windows::app_root_scenario(
        args.scenario.as_deref().unwrap_or("wide"),
    ))
}
fn shot_viewport(args: &LaunchArgs) -> (u32, u32) {
    match args.scenario.as_deref().unwrap_or("wide") {
        "detail" | "error-compact" => (480, 800),
        "running" | "error" => (1000, 760),
        _ => (1280, 800),
    }
}
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch's argument-aware root factory"
)]
fn live_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(args
        .positionals()
        .first()
        .map_or_else(windows::app_root, |path| {
            windows::app_root_open(std::path::Path::new(path))
        }))
}
pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args("dev.qos.windows", live_root)
            .max_positionals(1)
            .shot_root(shot_root)
            .scenarios(&["wide", "detail", "running", "error", "error-compact"])
            .shot_viewport(shot_viewport)
            .configure_app(configure_app),
    );
}

/// Install the document-action decoders used by the Windows resource surface.
/// The standard launcher applies this to the live host before entering the
/// event loop, keeping palette, agent, history, and collaboration replay on the
/// same action contract as the widget that publishes the resources.
fn configure_app(app: &mut ui::App) {
    windows::actions::register(app.action_registry_mut());
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_actions::ActionInput;

    /// Regression for the production launch boundary. Pull an action from the
    /// real Windows widget surface, then pass a host through the exact callback
    /// installed on `LaunchSpec`; a detached test registry would not catch the
    /// live catalogue being withdrawn because its decoder was never installed.
    #[test]
    fn live_launch_accepts_the_published_resource_surface() {
        let surface = windows::app_root_offline();
        let resources = surface.resources(&automerge::AutoCommit::new());
        assert!(
            !resources.is_empty(),
            "Windows publishes a resource surface"
        );
        for action in qos_actions::flatten(&resources)
            .into_iter()
            .map(|action| action.action)
        {
            assert!(
                action.contract.validate_declaration().is_empty(),
                "{} has an invalid action contract",
                action.id,
            );
        }

        let published = qos_actions::flatten(&resources)
            .into_iter()
            .map(|action| action.action)
            .find(|action| {
                action.kind == qos_actions::OperationKind::Document
                    && matches!(action.input, ActionInput::Immediate(_))
            })
            .expect("Windows publishes a replayable route action");
        let ActionInput::Immediate(encoded) = &published.input else {
            unreachable!("the selected action is immediate");
        };
        assert_eq!(encoded.type_id, windows::actions::WINDOWS_ACTION_TYPE);

        let mut app = ui::App::new(ui::State::new(), windows::app_root_offline());
        configure_app(&mut app);
        assert!(
            app.action_registry_mut()
                .contains(windows::actions::WINDOWS_ACTION_TYPE),
            "the LaunchSpec callback must install Windows' document decoder"
        );
        app.action_registry_mut()
            .decode(encoded)
            .expect("the live host decodes an action from its published surface");
    }
}
