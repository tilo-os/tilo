//! Disk Imager — thin binary entry point. All logic lives in the library.
//!
//! The binary is the framework app [`imager::ImagerShell`]: the write wizard,
//! the guided setup with `--setup`, the folder imports with
//! `--import-folders`, the guided `--installer` (`--installer-preview` alias),
//! the interactive fixture-only `--demo` (those four with `installation`; the
//! Tilo app is the write wizard alone), and the standard `--shots`, `--shots-dir` and
//! `--headless-drive` semantic actions on the production app. Shots render its
//! scenarios. Started with a helper's own command
//! line it is that helper instead (the privileged writer, the staging steps,
//! the packaging probes), which the app declares as its helpers.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// Account custody with the WebDAV client, and the projection worker's FUSE
// presentation, come from their engine libraries, not the tapp.
#[cfg(feature = "runtime-engine")]
extern crate tilo_accounts_engine;
#[cfg(all(feature = "runtime-engine", target_os = "linux"))]
extern crate tilo_fuse_engine;

fn main() {
    #[cfg(all(not(target_arch = "wasm32"), feature = "installation"))]
    {
        if guided_installer(std::env::args().skip(1)) {
            ui::framework::run::<imager::resource::GuidedInstallerShell>();
        } else {
            ui::framework::run::<imager::ImagerShell>();
        }
    }
    // Without the installation (the Tilo app) the imager is the write wizard.
    #[cfg(all(not(target_arch = "wasm32"), not(feature = "installation")))]
    ui::framework::run::<imager::ImagerShell>();
}

#[cfg(all(not(target_arch = "wasm32"), feature = "installation"))]
fn guided_installer(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--installer" | "--installer-preview" | "--demo"
        )
    })
}

#[cfg(all(test, not(target_arch = "wasm32"), feature = "installation"))]
mod tests {
    #[test]
    fn only_guided_installation_switches_choose_the_installer_presentation() {
        for args in [
            vec!["--installer"],
            vec!["--demo"],
            vec!["--installer-preview", "--headless"],
        ] {
            assert!(super::guided_installer(args.into_iter().map(str::to_owned)));
        }
        for args in [
            vec![],
            vec!["--setup"],
            vec!["--import-folders"],
            vec!["--write"],
        ] {
            assert!(!super::guided_installer(
                args.into_iter().map(str::to_owned)
            ));
        }
    }
}
