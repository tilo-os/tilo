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
    reason = "signature is fixed by ui::launch's argument-aware root factory"
)]
fn installer_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(args
        .positionals()
        .first()
        .map_or_else(windows::app_root, |path| {
            windows::app_root_open(std::path::Path::new(path))
        }))
}

/// Opening an installer handed over on the command line needs the root that
/// takes the path; every other launch, captures included, is the framework
/// app.
fn opens_installer(args: impl Iterator<Item = String>) -> bool {
    let args: Vec<String> = args.collect();
    let capture = args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--shots"
                | "--responsive-matrix"
                | "--native-audit"
                | "--headless"
                | "--headless-drive"
        )
    });
    if capture {
        return false;
    }
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            flag if flag.starts_with('-') => {}
            _ => return true,
        }
    }
    false
}

pub fn main() {
    if !opens_installer(std::env::args().skip(1)) {
        ui::framework::run::<windows::WindowsShell>();
        return;
    }

    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args("so.tilo.windows", installer_root)
            .max_positionals(1)
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
    use tilo_actions::ActionInput;

    /// Regression for the production launch boundary. Pull an action from the
    /// real Windows widget surface, then pass a host through the exact callback
    /// installed on `LaunchSpec`; a detached test registry would not catch the
    /// live catalogue being withdrawn because its decoder was never installed.
    #[test]
    fn live_launch_accepts_the_published_resource_surface() {
        let surface = windows::app_root_offline();
        let resources = ui::resources::resources(surface.as_ref(), &automerge::AutoCommit::new());
        assert!(
            !resources.is_empty(),
            "Windows publishes a resource surface"
        );
        for action in tilo_actions::flatten(&resources)
            .into_iter()
            .map(|action| action.action)
        {
            assert!(
                action.contract.validate_declaration().is_empty(),
                "{} has an invalid action contract",
                action.id,
            );
        }

        let published = tilo_actions::flatten(&resources)
            .into_iter()
            .map(|action| action.action)
            .find(|action| {
                action.kind == tilo_actions::OperationKind::Document
                    && matches!(action.input, ActionInput::Immediate(_))
            })
            .expect("Windows publishes a replayable route action");
        assert!(
            matches!(published.input, ActionInput::Immediate(_)),
            "the selected route action is immediate"
        );
        let ActionInput::Immediate(encoded) = &published.input else {
            return;
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
