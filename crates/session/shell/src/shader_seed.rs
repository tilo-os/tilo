//! Trusted public Mesa shader publication after a completed session frame.
//! Persistent seeds contain no app scenes. Apps receive only the existing RO
//! session bind and retain their own bounded writable caches.

use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ui::vello::{self, kurbo, peniko, wgpu};

const RENDER_MODE: &str = "--shader-seed-render";
const PREPARED: &str = "TILO_SHADER_SEED_PREPARED";
static ADAPTER: OnceLock<wgpu::AdapterInfo> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);

fn persistent_dir() -> Option<PathBuf> {
    Some(tilo_paths::xdg::cache_home()?.join("tilo/public-mesa-seed"))
}

fn session_dir() -> PathBuf {
    crate::shell::launcher::runtime_dir()
        .join("tilo")
        .join(launcher::gpu::PIPELINE_SEED_DIR_NAME)
}

/// Restore a validated persisted seed before any GPU or application threads
/// start. Re-exec preserves the PID and registered launch ancestry while setting
/// the native driver's environment without mutating a threaded process.
///
/// # Errors
/// Returns an error for damaged seeds, unavailable storage or failed re-exec.
pub fn prepare_session() -> io::Result<()> {
    use std::os::unix::process::CommandExt as _;
    if std::env::var_os(PREPARED).is_some()
        || std::env::var_os("MESA_SHADER_CACHE_DISABLE").is_some()
    {
        return Ok(());
    }
    let Some(root) = persistent_dir() else {
        return Ok(());
    };
    let source = root.join("mesa");
    match launcher::mesa_seed::fingerprint(&source) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    launcher::mesa_seed::restore(&source, &session_dir())?;
    let private = root.join("session-cache");
    launcher::mesa_seed::prepare(&source, &private)?;
    let error = Command::new(std::env::current_exe()?)
        .args(std::env::args_os().skip(1))
        .env(PREPARED, "1")
        .env("MESA_SHADER_CACHE_DIR", private)
        .env("MESA_SHADER_CACHE_MAX_SIZE", "16M")
        .env("MESA_DISK_CACHE_COMBINE_RW_WITH_RO_FOZ", "1")
        .env("MESA_DISK_CACHE_READ_ONLY_FOZ_DBS", "tilo-seed")
        .env_remove("MESA_DISK_CACHE_SINGLE_FILE")
        .env_remove("MESA_DISK_CACHE_MULTI_FILE")
        .env_remove("MESA_DISK_CACHE_DATABASE")
        .env_remove("MESA_DISK_CACHE_READ_ONLY_FOZ_DBS_DYNAMIC_LIST")
        .exec();
    Err(error)
}

/// Handle the trusted maintenance child before identity/session startup.
#[must_use]
pub fn dispatch(args: &[String]) -> Option<ExitCode> {
    if args.get(1).is_none_or(|arg| arg != RENDER_MODE) {
        return None;
    }
    let result = match args {
        [_, _, cache, output, fingerprint, adapter] => {
            render_public(cache, output, fingerprint, adapter)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shader seed renderer requires cache, output, fingerprint and adapter",
        )),
    };
    Some(match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("public shader seed unavailable: {error}");
            ExitCode::FAILURE
        }
    })
}

/// Remember the actual selected device, never substituting a helper's default.
pub fn observe_device(adapter: wgpu::AdapterInfo) {
    if adapter.backend == wgpu::Backend::Vulkan && adapter.driver_info.contains("Mesa") {
        let _ = ADAPTER.set(adapter);
    }
}

/// Start one bounded maintenance job only after a successful completed render.
/// Unit tests and non-session binaries never publish into a real session.
pub fn after_frame() {
    if cfg!(test)
        || std::env::var_os("MESA_SHADER_CACHE_DISABLE").is_some()
        || STARTED.load(Ordering::Acquire)
    {
        return;
    }
    let Some(adapter) = ADAPTER.get().cloned() else {
        return;
    };
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    if executable.file_name().is_none_or(|name| name != "shell") {
        return;
    }
    let Some(root) = persistent_dir() else {
        return;
    };
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Err(error) = std::thread::Builder::new().name("public-shader-seed".into()).spawn(move || {
        if let Err(error) = refresh(executable, &root, &adapter) {
            tracing::debug!(%error, "public shader seed not refreshed; normal driver compilation remains available");
        }
    }) { tracing::debug!(%error, "public shader seed worker unavailable"); }
}

fn refresh(
    executable: PathBuf,
    root: &std::path::Path,
    adapter: &wgpu::AdapterInfo,
) -> io::Result<()> {
    std::fs::create_dir_all(root)?;
    // One kernel-held maintenance owner across simultaneous sessions.
    let _lock = advisory_lock::FileLock::try_open(
        &root.join("refresh.lock"),
        advisory_lock::LockMode::Exclusive,
    )
    .map_err(io::Error::other)?;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if (name.starts_with("compile-") || name.starts_with("seed-stage-"))
            && entry.file_type()?.is_dir()
        {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    // Hash outside startup/render threads: any shader or renderer binary change
    // refreshes the public archive. Mesa also validates its own compiler/CPU/device
    // namespace; an incompatible previous seed is an ordinary cache miss.
    let mut hash = blake3::Hasher::new();
    let mut file = std::fs::File::open(&executable)?;
    let mut buffer = vec![0_u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(
            buffer
                .get(..count)
                .ok_or_else(|| io::Error::other("invalid binary read length"))?,
        );
    }
    let descriptor = format!("{adapter:?}");
    hash.update(descriptor.as_bytes());
    if let Ok(cpu) = std::fs::read_to_string("/proc/cpuinfo")
        && let Some(flags) = cpu
            .lines()
            .find(|line| line.starts_with("flags") || line.starts_with("Features"))
    {
        hash.update(flags.as_bytes());
    }
    let fingerprint = hash.finalize().to_hex().to_string();
    if launcher::mesa_seed::fingerprint(&root.join("mesa")).is_ok_and(|value| value == fingerprint)
    {
        return Ok(());
    }
    std::fs::create_dir_all(root)?;
    let cache = tempfile::Builder::new()
        .prefix("compile-")
        .tempdir_in(root)?;
    // Regeneration is deferred once so the completed frame can reach scanout;
    // cached sessions return above and pay no timer or maintenance process.
    std::thread::sleep(Duration::from_secs(1));
    let mut command = Command::new("/usr/bin/nice");
    command
        .args(["-n", "10"])
        .arg(executable)
        .args([RENDER_MODE])
        .arg(cache.path())
        .arg(root)
        .arg(&fingerprint)
        .arg(descriptor)
        .env("MESA_SHADER_CACHE_DIR", cache.path())
        .env("MESA_DISK_CACHE_SINGLE_FILE", "1")
        .env_remove("MESA_SHADER_CACHE_DISABLE")
        .env_remove("MESA_DISK_CACHE_MULTI_FILE")
        .env_remove("MESA_DISK_CACHE_DATABASE")
        .env_remove("MESA_DISK_CACHE_READ_ONLY_FOZ_DBS")
        .env_remove("MESA_DISK_CACHE_READ_ONLY_FOZ_DBS_DYNAMIC_LIST")
        .env_remove("MESA_DISK_CACHE_COMBINE_RW_WITH_RO_FOZ");
    bind_renderer_to_owner(&mut command)?;
    let output = bounded_command::run_prepared(command, None, 65536, Duration::from_secs(30))
        .map_err(io::Error::other)?;
    if !output.status.success() {
        return Err(io::Error::other("public shader renderer failed"));
    }
    launcher::mesa_seed::restore(&root.join("mesa"), &session_dir())
}

fn bind_renderer_to_owner(command: &mut Command) -> io::Result<()> {
    use std::os::unix::process::CommandExt as _;
    let owner = i32::try_from(std::process::id()).map_err(io::Error::other)?;
    // SAFETY: the post-fork hook only makes async-signal-safe kernel calls;
    // it allocates nothing and takes no lock. Both exec targets are ordinary
    // unprivileged image binaries, preserving the parent-death signal.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != owner {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
    Ok(())
}

fn render_public(cache: &str, output: &str, fingerprint: &str, expected: &str) -> io::Result<()> {
    let mut context = ui::gpu::RenderContext::new();
    let index = pollster::block_on(context.device(None))
        .ok_or_else(|| io::Error::other("no shader rendering device"))?;
    let handle = context
        .devices
        .get(index)
        .ok_or_else(|| io::Error::other("shader device missing"))?;
    if format!("{:?}", handle.adapter.get_info()) != expected {
        return Err(io::Error::other(
            "shader helper selected a different adapter",
        ));
    }
    let texture = |format, usage| {
        handle.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tilo-public-shader-seed"),
            size: wgpu::Extent3d {
                width: 664,
                height: 592,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let source = texture(
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let target = texture(
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let blitter = wgpu::util::TextureBlitter::new(&handle.device, wgpu::TextureFormat::Bgra8Unorm);
    let mut renderer = vello::Renderer::new(
        &handle.device,
        vello::RendererOptions {
            use_cpu: false,
            antialiasing_support: vello::AaSupport::area_only(),
            num_init_threads: Some(std::num::NonZeroUsize::MIN.saturating_add(1)),
            pipeline_cache: None,
        },
    )
    .map_err(io::Error::other)?;
    let mut scene = vello::Scene::new();
    // Use the same framework region primitive as native windows, with a
    // shipped public preset. Never read the owner's configured theme or fonts.
    ui::surface::region(
        &mut scene,
        kurbo::Affine::IDENTITY,
        kurbo::Rect::new(0.0, 0.0, 664.0, 592.0),
        ui::surface::Region::Content,
        &ui::Theme::dark(),
    );
    renderer
        .render_to_texture(
            &handle.device,
            &handle.queue,
            &scene,
            &source_view,
            &vello::RenderParams {
                base_color: peniko::Color::TRANSPARENT,
                width: 664,
                height: 592,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
        .map_err(io::Error::other)?;
    let mut encoder = handle
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    blitter.copy(&handle.device, &mut encoder, &source_view, &target_view);
    handle.queue.submit([encoder.finish()]);
    handle
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(io::Error::other)?;
    // Mesa's completed disk cache is published only after all GPU objects close.
    drop(renderer);
    drop(blitter);
    drop(scene);
    drop(source_view);
    drop(target_view);
    drop(source);
    drop(target);
    drop(context);
    launcher::mesa_seed::publish(
        std::path::Path::new(cache),
        std::path::Path::new(output),
        fingerprint,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_renderer_arms_real_kernel_parent_death_protection() {
        use std::os::unix::process::CommandExt as _;
        let mut command = Command::new("true");
        bind_renderer_to_owner(&mut command).unwrap();
        // SAFETY: observe the actual kernel child state after the production
        // callback; this post-fork query allocates nothing and takes no lock.
        unsafe {
            command.pre_exec(|| {
                let mut signal: libc::c_int = 0;
                if libc::prctl(libc::PR_GET_PDEATHSIG, &raw mut signal, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if signal != libc::SIGKILL {
                    return Err(io::Error::from_raw_os_error(libc::EINVAL));
                }
                Ok(())
            });
        }
        let output =
            bounded_command::run_prepared(command, None, 1024, Duration::from_secs(5)).unwrap();
        assert!(output.status.success());
    }
}
