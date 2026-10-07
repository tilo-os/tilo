//! Synthetic native SDK application for the sandboxed startup timeline.
//! This is a test payload, not an installed system app or an enrollment path.

use tilo_ui::{App, State, wake::WakeScope, widget::Text};

pub mod startup_cache_fixture;

struct MesaFixture {
    _env: tilo_test_env::ScopedEnv,
    _cache: tempfile::TempDir,
}

fn fixture_seed() -> Result<Option<(std::path::PathBuf, &'static str)>, Box<dyn std::error::Error>>
{
    let mut roots = Vec::new();
    if let Some(dir) = tilo_ui::host::environment().paths.pipeline_seed.clone() {
        roots.push((dir, "probe-mesa-source-session"));
    }
    let executable = std::env::current_exe()?;
    let package = executable
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or("probe must live in its package bin directory")?;
    roots.push((
        package.join("startup-mesa-seed"),
        "probe-mesa-source-package",
    ));
    let mut observed = None;
    for (dir, source) in roots {
        match std::fs::metadata(dir.join("startup-probe-fresh-mesa")) {
            Ok(_) if observed.is_some() => return Err("ambiguous Mesa startup fixture".into()),
            Ok(_) => observed = Some((dir, source)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(observed)
}

fn mesa_fixture() -> Result<Option<MesaFixture>, Box<dyn std::error::Error>> {
    let Some((seed, source)) = fixture_seed()? else {
        return Ok(None);
    };
    // Scratch space is inside the app's existing writable cache grant.
    // Package/session archives use the launcher's existing read-only binds.
    let cache_home = std::path::Path::new("/run/tilo/app/.cache");
    let cache = tempfile::Builder::new()
        .prefix("startup-mesa-")
        .tempdir_in(cache_home)?;
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("MESA_SHADER_CACHE_DIR", cache.path())
        .set("MESA_SHADER_CACHE_SHOW_STATS", "true")
        .remove("MESA_SHADER_CACHE_DISABLE")
        .remove("MESA_DISK_CACHE_SINGLE_FILE")
        .remove("MESA_DISK_CACHE_MULTI_FILE")
        .remove("MESA_DISK_CACHE_DATABASE")
        .remove("MESA_DISK_CACHE_READ_ONLY_FOZ_DBS")
        .remove("MESA_DISK_CACHE_READ_ONLY_FOZ_DBS_DYNAMIC_LIST")
        .remove("MESA_DISK_CACHE_COMBINE_RW_WITH_RO_FOZ");
    let mesa = seed.join("mesa");
    match std::fs::metadata(&mesa) {
        Ok(_) => {
            let bytes = startup_cache_fixture::populate_seed(&mesa, cache.path())?;
            if bytes == 0 {
                return Err("empty fixture shader archive".into());
            }
            // The original archive must remain unwritable inside the app.
            match std::fs::OpenOptions::new()
                .write(true)
                .open(seed.join("startup-probe-fresh-mesa"))
            {
                Err(error)
                    if error.kind() == std::io::ErrorKind::PermissionDenied
                        || error.kind() == std::io::ErrorKind::ReadOnlyFilesystem => {}
                Err(error) => return Err(error.into()),
                Ok(_) => return Err("fixture seed bind is writable inside the app".into()),
            }
            env.set("MESA_DISK_CACHE_COMBINE_RW_WITH_RO_FOZ", "1")
                .set("MESA_DISK_CACHE_READ_ONLY_FOZ_DBS", "tilo-seed");
            tilo_startup_metrics::mark("probe-mesa-seed-ready");
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    tilo_startup_metrics::mark("probe-fresh-mesa-cache-ready");
    tilo_startup_metrics::mark(source);
    Ok(Some(MesaFixture {
        _env: env,
        _cache: cache,
    }))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    verify_native_confinement()?;
    tilo_startup_metrics::init();
    tilo_startup_metrics::mark("probe-main-start");
    let _mesa = mesa_fixture()?;
    let wake_scope = WakeScope::new();
    let _entry = wake_scope.enter();
    let app = App::from_scoped_parts(
        wake_scope,
        State::new(),
        Box::new(Text::new("Startup frame probe")),
    )
    .with_window_app_id("io.tilo.startup-frame-probe")
    .without_native_document_chrome();
    tilo_startup_metrics::mark("probe-app-ready");
    app.run();
    tilo_startup_metrics::dump();
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn verify_native_confinement() -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "native sandbox startup probe requires Linux",
    ))
}

#[cfg(target_os = "linux")]
fn verify_native_confinement() -> std::io::Result<()> {
    // SAFETY: this read-only prctl query takes no pointers and changes no
    // process state. It observes confinement without requesting proc access.
    let status = unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) };
    if status < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let no_new_privs = status == 1;
    if !no_new_privs {
        return Err(std::io::Error::other(
            "native startup probe requires no_new_privs",
        ));
    }
    eprintln!("startup-frame-probe: no_new_privs={no_new_privs}");
    if let Some(root) = std::env::var_os("TILO_PIPELINE_SEED_DIR") {
        let manifest = std::path::PathBuf::from(root).join("mesa/seed.json");
        if manifest.is_file() {
            let error = std::fs::OpenOptions::new()
                .write(true)
                .open(manifest)
                .err()
                .ok_or_else(|| {
                    std::io::Error::other("ordinary public seed allowed app write access")
                })?;
            if !matches!(error.raw_os_error(), Some(libc::EACCES | libc::EROFS)) {
                return Err(error);
            }
            eprintln!("startup-frame-probe: public seed write-open denied");
        }
    }
    Ok(())
}
