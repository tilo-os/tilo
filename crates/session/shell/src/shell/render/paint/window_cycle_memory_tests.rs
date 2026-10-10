//! What one window's open/close cycle costs the compositor (TIL-692).
//!
//! The nightly VM run saw the compositor's anonymous memory climb by about
//! 14 MiB per app opened and closed, with no plateau in sight. Each cycle here
//! is the compositor's side of that: an shm client commits frames, the
//! capture worker converts them, the presenting renderer draws them over the
//! wallpaper, and the window closes with one last commit still queued. After
//! the desktop has painted a while without it, nothing of the window may
//! remain: the image atlas holds the wallpaper alone, at the size it had
//! before the window opened.
//!
//! The assertions are on the renderer's own accounting, which is the same on
//! every runner. The process's anonymous RSS and glibc's arena figures are
//! printed per cycle for the record; run alone to read them
//! (`cargo test -p shell --lib window_cycle_memory -- --nocapture`), since
//! other tests in the same process allocate beside it.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "frame sizes and cycle counters of a few hundred, far from overflow"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

use super::VelloDrmBridge;
use crate::compositor::surface_readback::OwnedShmFrame;
use crate::shell::foreign::{SurfaceId, store};

const DESKTOP: (u32, u32) = (640, 400);
const WINDOW: (u32, u32) = (600, 380);
const CYCLES: u64 = 16;
const COMMITS: u8 = 10;
/// Paints of the bare desktop after a close: long enough for the atlas to
/// come back down (`SHRINK_AFTER_RESOLVES` in Vello's image cache is 60).
const IDLE_PAINTS: u32 = 80;

fn image(width: u32, height: u32, value: u8) -> ImageData {
    ImageData {
        data: Blob::new(Arc::new(vec![value; (width * height * 4) as usize])),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::AlphaPremultiplied,
        width,
        height,
    }
}

fn paint(bridge: &mut VelloDrmBridge, wallpaper: &ImageData, window: Option<&ImageData>) {
    let mut scene = Scene::new();
    scene.draw_image(wallpaper, Affine::IDENTITY);
    if let Some(window) = window {
        scene.draw_image(window, Affine::translate((20.0, 10.0)));
    }
    bridge.paint(&scene).expect("the desktop paints");
}

/// Queue one shm commit of `id` and wait for the capture worker to publish it.
fn commit(id: SurfaceId, value: u8) {
    commit_sized(id, value, WINDOW);
}

/// [`commit`] of a `size` window.
fn commit_sized(id: SurfaceId, value: u8, size: (u32, u32)) {
    let before = store().frame_seq(id);
    let pixels = [value, value, 0x40, 0xFF].repeat((size.0 * size.1) as usize);
    let frame = OwnedShmFrame::argb8888(pixels, size.0 as usize, size.1 as usize);
    let generation = store()
        .prepare_shm_capture(id, frame)
        .expect("the shm worker runs");
    let deadline = Instant::now() + Duration::from_secs(10);
    while store().frame_seq(id) == before {
        assert!(Instant::now() < deadline, "the shm worker never published");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(store().note_shm_committed(id, generation));
}

/// Anonymous resident memory of this process, in KiB.
fn rss_anon_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("RssAnon:")?
                    .trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse()
                    .ok()
            })
        })
        .unwrap_or(0)
}

/// glibc's view: bytes in arenas, in mmapped chunks, in use, and free in arenas.
fn malloc_kib() -> (usize, usize, usize, usize) {
    // SAFETY: `mallinfo2` only reads allocator statistics; it takes no
    // arguments and returns a plain struct by value.
    let info = unsafe { libc::mallinfo2() };
    (
        info.arena / 1024,
        info.hblkhd / 1024,
        info.uordblks / 1024,
        info.fordblks / 1024,
    )
}

#[test]
fn window_cycle_memory_returns_to_the_bare_desktop() {
    let mut bridge = VelloDrmBridge::new(DESKTOP.0, DESKTOP.1)
        .expect("the memory gate requires a real wgpu adapter");
    let wallpaper = image(DESKTOP.0, DESKTOP.1, 0x30);
    let wallpaper_bytes = wallpaper.data.len() as u64;
    for _ in 0..IDLE_PAINTS {
        paint(&mut bridge, &wallpaper, None);
    }
    let bare = bridge.image_cache_stats();
    let bare_atlas_bytes = bridge.renderer_memory_stats().image_bytes;
    eprintln!(
        "cycle  rss_anon_kib  arena_kib  mmapped_kib  in_use_kib  free_kib  atlas  resident  resident_kib  atlas_texture_kib  evicted  shrinks"
    );
    let mut closed = Vec::new();
    let mut after_close = Vec::new();
    for cycle in 0..CYCLES {
        let id: SurfaceId = 0x0692_0000 + cycle;
        store().forget(id);
        let mut peak_side = 0;
        for frame in 0..COMMITS {
            commit(id, frame.wrapping_mul(23));
            let (window, _) = store().frame_image(id).expect("a published frame draws");
            paint(&mut bridge, &wallpaper, Some(&window));
            peak_side = peak_side.max(bridge.image_cache_stats().atlas_side);
        }
        // The client commits once more and closes before the worker is done.
        let last = OwnedShmFrame::argb8888(
            vec![0xFF; (WINDOW.0 * WINDOW.1 * 4) as usize],
            WINDOW.0 as usize,
            WINDOW.1 as usize,
        );
        let _ = store().prepare_shm_capture(id, last);
        store().forget(id);
        closed.push(id);
        for _ in 0..IDLE_PAINTS {
            paint(&mut bridge, &wallpaper, None);
        }
        let stats = bridge.image_cache_stats();
        let atlas_bytes = bridge.renderer_memory_stats().image_bytes;
        let (arena, mmapped, in_use, free) = malloc_kib();
        eprintln!(
            "{cycle:>5}  {:>12}  {arena:>9}  {mmapped:>11}  {in_use:>10}  {free:>8}  {:>5}  {:>8}  {:>12}  {:>17}  {:>7}  {:>7}  (open: atlas {peak_side})",
            rss_anon_kib(),
            stats.atlas_side,
            stats.resident,
            stats.resident_cpu_bytes / 1024,
            atlas_bytes / 1024,
            stats.evicted_total,
            stats.shrinks_total,
        );
        after_close.push((stats, atlas_bytes));
    }
    // Asserted after the loop, so the table above always covers every cycle.
    for (cycle, (stats, atlas_bytes)) in after_close.into_iter().enumerate() {
        assert_eq!(
            (stats.resident, stats.resident_cpu_bytes),
            (1, wallpaper_bytes),
            "cycle {cycle}: the closed window's frames are still resident: {stats:?}"
        );
        assert_eq!(
            stats.atlas_side, bare.atlas_side,
            "cycle {cycle}: the atlas kept the size the window needed: {stats:?}"
        );
        assert_eq!(
            atlas_bytes, bare_atlas_bytes,
            "cycle {cycle}: the atlas texture did not come back down"
        );
    }
    // By now the worker has long finished every queued commit; one that
    // landed after its window closed must not have brought the window back.
    for id in closed {
        assert!(
            store().snapshot(id).is_none(),
            "surface {id:#x} came back with a frame after it closed"
        );
        assert_eq!(store().frame_seq(id), 0, "surface {id:#x} is tracked again");
    }
}

/// Set in the child process [`closed_windows_heap_goes_back_to_the_kernel`]
/// runs its cycles in.
const HEAP_TRIM_CHILD: &str = "TILO_HEAP_TRIM_TEST_CHILD";

/// What closed windows freed goes back to the kernel (TIL-705). glibc kept it
/// before: 61–75 MiB free in its arenas stayed in the compositor's anonymous
/// RSS. Closing a window asks the heap trimmer to return it; once the
/// trimmer has served the closes, a further `malloc_trim` has nothing left
/// to give back.
///
/// The cycles run in a child process of their own, so no test beside this
/// one allocates or frees while its RSS is read.
#[test]
fn closed_windows_heap_goes_back_to_the_kernel() {
    if std::env::var_os(HEAP_TRIM_CHILD).is_none() {
        let name = format!(
            "{}::closed_windows_heap_goes_back_to_the_kernel",
            module_path!()
                .split_once("::")
                .map_or(module_path!(), |(_, path)| path)
        );
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
            .env(HEAP_TRIM_CHILD, "1")
            .output()
            .expect("the child test runs");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "the isolated run failed ({:?}):\n{stdout}\n{stderr}",
            output.status
        );
        eprint!("{stderr}");
        return;
    }
    let mut bridge = VelloDrmBridge::new(DESKTOP.0, DESKTOP.1)
        .expect("the memory gate requires a real wgpu adapter");
    let wallpaper = image(DESKTOP.0, DESKTOP.1, 0x30);
    for _ in 0..IDLE_PAINTS {
        paint(&mut bridge, &wallpaper, None);
    }
    let trims_before = crate::shell::heap_trim::trims_completed();
    for cycle in 0..3 {
        let id: SurfaceId = 0x0705_0000 + cycle;
        store().forget(id);
        for frame in 0..COMMITS {
            commit(id, frame.wrapping_mul(23));
            let (window, _) = store().frame_image(id).expect("a published frame draws");
            paint(&mut bridge, &wallpaper, Some(&window));
        }
        store().forget(id);
        for _ in 0..IDLE_PAINTS {
            paint(&mut bridge, &wallpaper, None);
        }
    }
    assert!(
        crate::shell::heap_trim::settle(Duration::from_secs(30)),
        "the heap trimmer never served the closes"
    );
    let trims = crate::shell::heap_trim::trims_completed() - trims_before;
    assert!(trims >= 1, "closing three windows asked for no heap trim");
    let before = rss_anon_kib();
    // SAFETY: malloc_trim only releases memory the allocator holds free.
    unsafe { libc::malloc_trim(0) };
    let left_kib = before.saturating_sub(rss_anon_kib());
    let (arena, mmapped, in_use, free) = malloc_kib();
    eprintln!(
        "after {trims} trim(s): rss_anon_kib {before} arena_kib {arena} mmapped_kib {mmapped} \
         in_use_kib {in_use} free_kib {free}; a further trim returned {left_kib} KiB"
    );
    assert!(
        before > 0 && left_kib <= 2048,
        "{left_kib} KiB the closed windows freed were still resident after the heap trimmer ran"
    );
}

/// A desktop and window large enough that a window's frames grow the image
/// atlas well past the bare desktop's.
const LARGE_DESKTOP: (u32, u32) = (1280, 800);
const LARGE_WINDOW: (u32, u32) = (1200, 760);
/// Device memory a closed window may leave allocated, in MiB: an empty
/// 4 MiB suballocation block the allocator keeps per memory type.
const DEVICE_SLACK_MIB: u64 = 8;
/// Frames a desktop draws after a close before it goes idle.
const CLOSE_PAINTS: u32 = 8;

/// What closed windows' textures touched goes back to the kernel (TIL-731).
/// On lavapipe, the software rasteriser the dev VM and CI render with,
/// device memory lives in one memfd that never shrinks: four apps opened
/// and closed left about 305 MiB allocated to it and mapped by nobody, and
/// an image atlas grown to 8192 pixels stayed that size because an idle
/// desktop draws too few frames to shrink it. Now the atlas comes down
/// within the few frames a close draws, and the trim the close asks for
/// punches the freed ranges out, so the device holds what the bare desktop
/// held.
///
/// The figure is this device's own memfd, so tests beside this one do not
/// move it. A GPU keeps no freed memory in a memfd; there the test checks
/// only that the adapter is not lavapipe.
#[test]
fn closed_windows_device_memory_goes_back_to_the_kernel() {
    let mut bridge = VelloDrmBridge::new(LARGE_DESKTOP.0, LARGE_DESKTOP.1)
        .expect("the memory gate requires a real wgpu adapter");
    let device = bridge.target.device.clone();
    let wallpaper = image(LARGE_DESKTOP.0, LARGE_DESKTOP.1, 0x30);
    for _ in 0..IDLE_PAINTS {
        paint(&mut bridge, &wallpaper, None);
    }
    if crate::shell::render::software_gpu_memory::device_allocated_bytes(&device).is_none() {
        let info = bridge.adapter_info().expect("the bridge's adapter");
        assert_ne!(
            info.driver, "llvmpipe",
            "lavapipe without its memfd: Mesa changed how it backs device memory"
        );
        eprintln!("{} keeps no freed device memory in a memfd", info.name);
        return;
    }
    // Only the trimmer releases anything below: the bridge registered its
    // device with it, or the windows' memory stays.
    let allocated_mib = || {
        crate::shell::render::software_gpu_memory::device_allocated_bytes(&device)
            .expect("still lavapipe")
            >> 20
    };
    let trimmed = |what: &str| {
        crate::shell::heap_trim::request();
        assert!(
            crate::shell::heap_trim::settle(Duration::from_secs(30)),
            "the trimmer never served {what}"
        );
    };
    trimmed("the bare desktop");
    let bare = allocated_mib();
    let bare_atlas = bridge.image_cache_stats().atlas_side;
    let mut peak = bare;
    let mut after_close = Vec::new();
    // Trims are at least ten seconds apart, so few cycles.
    for cycle in 0..3 {
        let id: SurfaceId = 0x0731_0000 + cycle;
        store().forget(id);
        let mut open_atlas = 0;
        for frame in 0..COMMITS {
            commit_sized(id, frame.wrapping_mul(23), LARGE_WINDOW);
            let (window, _) = store().frame_image(id).expect("a published frame draws");
            paint(&mut bridge, &wallpaper, Some(&window));
            bridge.after_paint();
            open_atlas = open_atlas.max(bridge.image_cache_stats().atlas_side);
        }
        peak = peak.max(allocated_mib());
        // The close asks for the trim; the desktop then draws only the few
        // frames of a close before it goes idle.
        store().forget(id);
        for _ in 0..CLOSE_PAINTS {
            paint(&mut bridge, &wallpaper, None);
            bridge.after_paint();
        }
        let closed = allocated_mib();
        let closed_atlas = bridge.image_cache_stats().atlas_side;
        assert!(
            crate::shell::heap_trim::settle(Duration::from_secs(30)),
            "the trimmer never served the close"
        );
        let after = allocated_mib();
        eprintln!(
            "cycle {cycle}: device memfd {bare} MiB bare, {peak} MiB peak, {closed} MiB \
             once closed, {after} MiB after the trim; atlas {bare_atlas} bare, {open_atlas} \
             open, {closed_atlas} closed"
        );
        assert!(
            open_atlas > bare_atlas,
            "the window never grew the atlas, so its shrink proves nothing"
        );
        assert_eq!(
            closed_atlas, bare_atlas,
            "cycle {cycle}: {CLOSE_PAINTS} frames after the close the atlas had not come down"
        );
        after_close.push(after);
    }
    assert!(
        peak >= bare + 16,
        "the windows touched only {} MiB of device memory, too little to tell a release from none",
        peak - bare
    );
    for (cycle, after) in after_close.into_iter().enumerate() {
        assert!(
            after <= bare + DEVICE_SLACK_MIB,
            "cycle {cycle}: {after} MiB of device memory stayed allocated after the window \
             closed and the trimmer ran; the bare desktop holds {bare} MiB"
        );
    }
}
