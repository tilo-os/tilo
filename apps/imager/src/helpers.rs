//! The helpers the imager's binary also is, asked before the app starts
//! ([`ui::framework::ResourceApp::helpers`]): the privileged writer and
//! migration copier, the desktop installer, the staging helper, request
//! builder and final boot arming, and the packaging probes. Each recognises
//! its own command line and answers the exit code the process ends with.

use ui::framework::Helper;

/// Every helper, in the order they are asked.
pub(crate) const HELPERS: [Helper; 9] = [
    Helper { run: native_copy },
    Helper {
        run: |args| crate::setup::desktop_worker::maybe_run_desktop_helper(args),
    },
    Helper {
        run: |args| crate::write::maybe_run_write_helper(args).map_err(|error| error.to_string()),
    },
    Helper {
        run: |args| crate::customize_image::maybe_run(args).map_err(|error| error.to_string()),
    },
    Helper {
        run: |args| crate::setup::staging::helper::maybe_run_staging_helper(args),
    },
    Helper {
        run: |args| crate::setup::staging::final_bootnext::maybe_run(args),
    },
    Helper {
        run: |args| crate::setup::staging::cli::maybe_run_staging_request(args),
    },
    Helper { run: package_smoke },
    Helper {
        run: flatpak_probe_smoke,
    },
];

/// The native migration copy and its resumption.
#[cfg(target_os = "linux")]
fn native_copy(args: &[String]) -> Result<Option<i32>, String> {
    crate::setup::migration_native::maybe_run_command(args).map_err(|error| error.to_string())
}

#[cfg(not(target_os = "linux"))]
fn native_copy(_args: &[String]) -> Result<Option<i32>, String> {
    Ok(None)
}

/// Packaging smoke probe: reaching it proves the executable and all
/// runtime-linked libraries load without starting graphics, portals, device
/// discovery, or a privileged path.
fn package_smoke(args: &[String]) -> Result<Option<i32>, String> {
    Ok(args
        .iter()
        .any(|arg| arg == "--tilo-package-smoke")
        .then_some(0))
}

/// The Flatpak's drive inventory, reconciled with the host's, inside its
/// sandbox.
fn flatpak_probe_smoke(args: &[String]) -> Result<Option<i32>, String> {
    if !args.iter().any(|arg| arg == "--tilo-flatpak-probe-smoke") {
        return Ok(None);
    }
    if !cfg!(feature = "flatpak-media-writer")
        || std::env::var("TILO_IMAGER_FLATPAK").as_deref() != Ok("1")
    {
        return Err("the Flatpak probe smoke test is outside its sandbox".into());
    }
    let drives = crate::probe::probe()
        .ok_or("the Flatpak could not reconcile lsblk with host UDisks2 inventory")?;
    if drives.is_empty() {
        return Err("the Flatpak returned an empty drive inventory".into());
    }
    Ok(Some(0))
}

#[cfg(test)]
mod tests {
    fn args(rest: &[&str]) -> Vec<String> {
        std::iter::once("imager")
            .chain(rest.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn helpers_answer_only_their_own_command_lines() {
        use crate::resource::ImagerShell;
        assert_eq!(
            ui::framework::run_helper::<ImagerShell>(&args(&["--tilo-package-smoke"])),
            Some(0),
            "the packaging probe ends before the app starts"
        );
        assert_eq!(
            ui::framework::run_helper::<ImagerShell>(&args(&["--tilo-final-bootnext"])),
            Some(1),
            "a helper's refused command line ends the process with 1"
        );
        assert_eq!(
            ui::framework::run_helper::<ImagerShell>(&args(&["--customize-image"])),
            Some(1),
            "an incomplete customization is refused before GUI startup"
        );
        for app in [&[][..], &["--setup"], &["--import-folders"]] {
            assert_eq!(
                ui::framework::run_helper::<ImagerShell>(&args(app)),
                None,
                "the app's own command line starts the app"
            );
        }
    }
}
