//! Target-user prepared setup consumer; no UI initialization or activation.
#[cfg(target_os = "linux")]
fn bootstrap() -> Option<std::process::ExitCode> {
    // The private consuming child isolates its mount namespace before any
    // controller, runtime or output initialization. Its inherited descriptor
    // is routing only; the driver authenticates actual parent and Shell roles.
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new(
            launcher::ordinary_child::CONSUMING_BOOTSTRAP_MODE,
        ))
    {
        return Some(
            match imager::setup::prepared_setup_helper::retained_consumer_bootstrap(
                std::env::args_os().skip(2),
            ) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("Consuming bootstrap could not continue: {error}");
                    std::process::ExitCode::FAILURE
                }
            },
        );
    }
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("--installed-original-copy-receiver"))
    {
        return Some(
            match imager::setup::installed_copy::run_receiver(std::env::args_os().skip(2)) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("Installed copy could not continue: {error}");
                    std::process::ExitCode::FAILURE
                }
            },
        );
    }
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("--installed-original-copy-controller"))
    {
        return Some(
            match imager::setup::prepared_setup_helper::installed_original_copy_controller(
                std::env::args_os().skip(2),
            ) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("Installed Copy controller could not continue: {error}");
                    std::process::ExitCode::FAILURE
                }
            },
        );
    }
    // Root controller reexec uses the same registered UTF-8 argv as Imager.
    // Existing unrelated consumers retain their OsString path handling.
    let controller_arguments = std::env::args_os()
        .skip(1)
        .map(std::ffi::OsString::into_string)
        .collect::<Result<Vec<_>, _>>();
    if let Ok(arguments) = controller_arguments {
        match imager::setup::prepared_setup_helper::maybe_run_original_controller(&arguments) {
            Ok(Some(0)) => return Some(std::process::ExitCode::SUCCESS),
            Ok(Some(_)) => return Some(std::process::ExitCode::FAILURE),
            Err(error) => {
                eprintln!("{error}");
                return Some(std::process::ExitCode::FAILURE);
            }
            Ok(None) => {}
        }
    }
    None
}

/// Installed consumer capability producers and checks keep their own closed
/// diagnostics, which the image build and release packaging report.
#[cfg(target_os = "linux")]
fn installed_consumers() -> Option<std::process::ExitCode> {
    use std::io::Write;
    let mode = std::env::args_os().nth(1)?.into_string().ok()?;
    if !imager_backend::installed_consumers::MODES.contains(&mode.as_str()) {
        return None;
    }
    Some(
        match imager_backend::installed_consumers::run(&mode, std::env::args_os().skip(2)) {
            Ok(output) if std::io::stdout().lock().write_all(&output).is_ok() => {
                std::process::ExitCode::SUCCESS
            }
            Ok(_) => std::process::ExitCode::FAILURE,
            Err(diagnostic) => {
                eprintln!("{diagnostic}");
                std::process::ExitCode::FAILURE
            }
        },
    )
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    if let Some(status) = installed_consumers() {
        return status;
    }
    if let Some(status) = bootstrap() {
        return status;
    }
    let mut arguments = std::env::args_os().skip(1).peekable();
    let first = arguments.peek().cloned();
    let mode = first.as_deref().and_then(std::ffi::OsStr::to_str);
    if matches!(
        mode,
        Some(
            "--selected-install-generation"
                | "--stream-completed-payload"
                | "--home-consumer-capability"
                | "--original-source-consumer-capability"
                | "--applications-consumer-capability"
                | "--applications-consumer-qualification-child"
                | "--reattest-original-source-consumer-capability"
                | "--access-shared-files"
                | "--install-enrolled-device"
                | "--authenticate-install"
                | "--access-existing"
                | "--portable-home"
                | "--portable-applications"
                | "--browser-transfer"
                | "--migration"
                | "--installed-applications"
                | "--retained-home-consumer"
                | "--unseal-prepared-handoff"
                | "--seal-prepared-handoff"
        )
    ) {
        arguments.next();
    }
    let succeeded = dispatch(mode, arguments);
    if succeeded {
        std::process::ExitCode::SUCCESS
    } else {
        eprintln!("Prepared setup could not continue.");
        std::process::ExitCode::FAILURE
    }
}

#[cfg(target_os = "linux")]
fn dispatch(mode: Option<&str>, arguments: impl Iterator<Item = std::ffi::OsString>) -> bool {
    let mut output = std::io::stdout().lock();
    match mode {
        Some("--retained-home-consumer") => {
            imager::setup::prepared_setup_helper::retained_home_consumer(arguments).is_ok()
        }
        Some("--applications-consumer-capability") => {
            imager::setup::prepared_setup_helper::original_reattest::qualify_applications(
                arguments,
                &mut output,
            )
            .is_ok()
        }
        Some("--applications-consumer-qualification-child") => {
            imager::setup::prepared_setup_helper::applications_consumer_qualification_child(
                arguments,
                &mut output,
            )
            .is_ok()
        }
        Some("--original-source-consumer-capability") => {
            imager::setup::prepared_setup_helper::original_reattest::qualify(arguments, &mut output)
                .is_ok()
        }
        Some("--reattest-original-source-consumer-capability") => {
            imager::setup::prepared_setup_helper::original_reattest::run(arguments, &mut output)
                .is_ok()
        }
        Some("--selected-install-generation") => {
            imager::setup::prepared_setup_helper::completed::selected(arguments, &mut output)
                .is_ok()
        }
        Some("--stream-completed-payload") => {
            imager::setup::prepared_setup_helper::completed::payload(arguments, &mut output).is_ok()
        }
        Some("--home-consumer-capability") => {
            imager::setup::prepared_setup_helper::home_consumer_capability(arguments, &mut output)
                .is_ok()
        }
        Some("--install-enrolled-device") => imager::setup::prepared_setup_helper::enrolled::run(
            arguments,
            &mut std::io::stdin().lock(),
            &mut output,
        )
        .is_ok(),
        Some("--authenticate-install") => imager::setup::prepared_setup_helper::authenticate::run(
            arguments,
            &mut std::io::stdin().lock(),
            &mut output,
        )
        .is_ok(),
        Some("--unseal-prepared-handoff") => {
            imager::setup::prepared_setup_helper::unseal_handoff(arguments, &mut output).is_ok()
        }
        Some("--seal-prepared-handoff") => imager_backend::setup::prepared_tpm::seal_from_reader(
            std::io::stdin().lock(),
            &mut output,
        )
        .is_ok(),
        Some("--access-shared-files") => {
            imager::setup::installed_migration_helper::run_shared_access(
                arguments,
                std::io::stdin(),
                &mut output,
            )
            .is_ok()
        }
        Some("--access-existing") => imager::setup::installed_migration_helper::run_access(
            arguments,
            std::io::stdin(),
            &mut output,
        )
        .is_ok(),
        Some("--portable-home") => imager::setup::portable_home_helper::run(
            arguments,
            &mut std::io::stdin().lock(),
            &mut output,
        )
        .is_ok(),
        Some("--portable-applications") => imager::setup::portable_applications_helper::run(
            arguments,
            &mut std::io::stdin().lock(),
            &mut output,
        )
        .is_ok(),
        Some("--browser-transfer") => imager::setup::browser_transfer_helper::run(
            arguments,
            &mut std::io::stdin().lock(),
            &mut output,
        )
        .is_ok(),
        Some("--installed-applications") => {
            imager::setup::installed_migration_helper::run_applications(
                arguments,
                std::io::stdin(),
                &mut output,
            )
            .is_ok()
        }
        Some("--migration") => {
            imager::setup::installed_migration_helper::run(arguments, std::io::stdin(), &mut output)
                .is_ok()
        }
        _ => default_setup(arguments, &mut output),
    }
}
#[cfg(target_os = "linux")]
fn default_setup(
    arguments: impl Iterator<Item = std::ffi::OsString>,
    output: &mut impl std::io::Write,
) -> bool {
    imager::setup::prepared_setup_helper::run(arguments, &mut std::io::stdin().lock(), output)
        .is_ok()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("Prepared setup import is unavailable on this platform.");
    std::process::ExitCode::FAILURE
}
