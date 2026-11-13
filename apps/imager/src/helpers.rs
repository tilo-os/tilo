//! The helpers this binary also runs before the app starts: the privileged writer and
//! the staging helper.

use ui::framework::Helper;

#[cfg(feature = "installation")]
pub(crate) const HELPERS: &[Helper] = &[
    Helper { run: native_copy },
    Helper {
        run: |args| crate::setup::staging::capacity_probe::maybe_run(args),
    },
    Helper {
        run: read_selected_media,
    },
    Helper { run: home_capture },
    Helper {
        run: home_capture_host,
    },
    Helper {
        run: |args| crate::setup::desktop_worker::maybe_run_desktop_helper(args),
    },
    Helper { run: write_helper },
    Helper {
        run: customize_image,
    },
    Helper {
        run: |args| crate::setup::staging::helper::maybe_run_staging_helper(args),
    },
    Helper {
        run: |args| imager_backend::staging::final_bootnext::maybe_run(args),
    },
    Helper {
        run: |args| crate::setup::staging::cli::maybe_run_staging_request(args),
    },
    Helper { run: package_smoke },
    Helper {
        run: flatpak_probe_smoke,
    },
];

#[cfg(not(feature = "installation"))]
pub(crate) const HELPERS: &[Helper] = &[
    Helper { run: write_helper },
    Helper {
        run: customize_image,
    },
    Helper { run: package_smoke },
    Helper {
        run: flatpak_probe_smoke,
    },
];

fn write_helper(args: &[String]) -> Result<Option<i32>, String> {
    crate::write::maybe_run_write_helper(args).map_err(|error| error.to_string())
}

fn customize_image(args: &[String]) -> Result<Option<i32>, String> {
    crate::customize_image::maybe_run(args).map_err(|error| error.to_string())
}

#[cfg(all(target_os = "linux", feature = "installation"))]
fn home_capture(args: &[String]) -> Result<Option<i32>, String> {
    imager_backend::setup::home_capture_helper::maybe_run(args)
}

#[cfg(all(not(target_os = "linux"), feature = "installation"))]
fn home_capture(_args: &[String]) -> Result<Option<i32>, String> {
    Ok(None)
}

#[cfg(all(target_os = "linux", feature = "installation"))]
fn home_capture_host(args: &[String]) -> Result<Option<i32>, String> {
    imager_backend::setup::home_capture::maybe_run_host(args)
}

#[cfg(all(not(target_os = "linux"), feature = "installation"))]
fn home_capture_host(_args: &[String]) -> Result<Option<i32>, String> {
    Ok(None)
}

#[cfg(all(target_os = "linux", feature = "installation"))]
fn native_copy(args: &[String]) -> Result<Option<i32>, String> {
    imager_backend::migration_native::maybe_run_command(args).map_err(|error| error.to_string())
}

#[cfg(all(not(target_os = "linux"), feature = "installation"))]
fn native_copy(_args: &[String]) -> Result<Option<i32>, String> {
    Ok(None)
}

fn package_smoke(args: &[String]) -> Result<Option<i32>, String> {
    Ok(args
        .iter()
        .any(|arg| arg == "--tilo-package-smoke")
        .then_some(0))
}

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

#[cfg(all(target_os = "linux", feature = "installation"))]
fn read_selected_media(args: &[String]) -> Result<Option<i32>, String> {
    crate::setup::read_media::maybe_run(args)
}
#[cfg(all(not(target_os = "linux"), feature = "installation"))]
fn read_selected_media(_args: &[String]) -> Result<Option<i32>, String> {
    Ok(None)
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
        #[cfg(feature = "installation")]
        assert_eq!(
            ui::framework::run_helper::<ImagerShell>(&args(&["--tilo-final-bootnext"])),
            Some(1),
            "a helper's refused command line ends the process with 1"
        );
        #[cfg(not(feature = "installation"))]
        assert_eq!(
            ui::framework::run_helper::<ImagerShell>(&args(&["--tilo-final-bootnext"])),
            None,
            "the Tilo app is no staging helper"
        );
        assert_eq!(
            ui::framework::run_helper::<ImagerShell>(&args(&["--customize-image"])),
            Some(1),
            "an incomplete customization is refused before GUI startup"
        );
        #[cfg(all(target_os = "linux", feature = "installation"))]
        assert_eq!(
            ui::framework::run_helper::<ImagerShell>(&args(&[
                "--tilo-home-capture-host",
                "unexpected",
            ])),
            Some(1),
            "a malformed capture host command must exit before starting the app"
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
