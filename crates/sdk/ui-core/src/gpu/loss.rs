//! A GPU that stops working under a renderer, and what the renderer opens next.
//!
//! A GPU dies in one of two ways, and a renderer must notice both:
//!
//! - **The driver says so.** A reset that failed, or a kernel that fails the
//!   fences of a GPU it cannot bring back: wgpu runs the device-lost callback,
//!   which [`DeviceLossMonitor`] records.
//! - **Nothing is said.** The firmware stops answering and nothing submitted
//!   ever completes; every fence wait just runs out. [`LossWatch`] notices work
//!   that has not finished [`NOT_PROGRESSING_AFTER`] after it was found
//!   unfinished, and declares the device lost on the monitor, so both cases
//!   reach the renderer through the one question, `reason()`.
//!
//! What comes next is [`recover`]'s call, the same for the compositor, the
//! render service and an app: a device that finished frames before it was lost
//! was reset and is opened again on the same adapter; one that was lost before
//! it finished a single frame is on a GPU that cannot be brought back, and the
//! process renders in software from then on ([`software_fallback`], which
//! [`super::RenderContext`] honours when it picks an adapter).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::time::Instant;

use tilo_lock::MutexRecover as _;
use wgpu::{AdapterInfo, Device, DeviceType, Queue};

use super::DeviceLossMonitor;

/// How long work found unfinished may stay unfinished before the GPU counts as
/// no longer making progress. A frame takes a hardware GPU milliseconds; a
/// kernel resets a GPU stuck on one job within seconds and fails its fence; a
/// software rasteriser takes seconds for a large frame at worst. Work still
/// pending this long after a renderer stopped waiting for it will not finish.
pub const NOT_PROGRESSING_AFTER: Duration = Duration::from_secs(8);

impl DeviceLossMonitor {
    /// Record that the device is lost although wgpu never said so: its GPU
    /// stopped finishing work. The first reason, wgpu's or this, stays.
    pub fn declare(&self, reason: impl Into<String>) {
        let mut recorded = self.0.lock_recover();
        if recorded.is_none() {
            *recorded = Some(reason.into());
        }
    }
}

/// Everything submitted to a queue up to some moment, and whether it finished.
#[derive(Clone)]
pub struct SubmittedWork(Arc<AtomicBool>);

impl SubmittedWork {
    /// Watch the work submitted to `queue` so far.
    pub fn watch(queue: &Queue) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let signal = done.clone();
        queue.on_submitted_work_done(move || signal.store(true, Ordering::Release));
        Self(done)
    }

    /// Whether that work finished, as of the device's last poll. Never blocks.
    pub fn finished(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Watches one device for loss, reported or not.
pub struct LossWatch {
    monitor: DeviceLossMonitor,
    /// Work found unfinished, when, and the queue it was submitted to.
    pending: Option<(SubmittedWork, Instant, Queue)>,
}

impl LossWatch {
    pub fn new(monitor: DeviceLossMonitor) -> Self {
        Self {
            monitor,
            pending: None,
        }
    }

    /// The renderer stopped waiting for what it submitted to `queue` before it
    /// finished (a frame ran out of its completion allowance, or the renderer
    /// does not wait at all). Work already watched keeps its earlier start.
    pub fn unfinished(&mut self, queue: &Queue) {
        self.unfinished_at(queue, crate::time::now());
    }

    fn unfinished_at(&mut self, queue: &Queue, now: Instant) {
        if self
            .pending
            .as_ref()
            .is_some_and(|(work, _, _)| !work.finished())
        {
            return;
        }
        self.pending = Some((SubmittedWork::watch(queue), now, queue.clone()));
    }

    /// Why the device is lost, if it is: wgpu reported it, or work found
    /// unfinished is still unfinished [`NOT_PROGRESSING_AFTER`] later, which
    /// this declares. Polls `device` once, without blocking, when work is
    /// pending; costs nothing otherwise.
    pub fn lost(&mut self, device: &Device) -> Option<String> {
        self.lost_at(device, crate::time::now())
    }

    fn lost_at(&mut self, device: &Device, now: Instant) -> Option<String> {
        if let Some((work, since, _)) = self.pending.as_ref() {
            let _ = device.poll(wgpu::PollType::Poll);
            let stalled = now.saturating_duration_since(*since);
            if work.finished() {
                self.pending = None;
            } else if stalled >= NOT_PROGRESSING_AFTER
                && let Some((_, _, queue)) = self.pending.take()
            {
                self.stopped(queue, stalled);
            }
        }
        self.monitor.reason()
    }

    /// The renderer waited `waited` itself, a bound at least
    /// [`NOT_PROGRESSING_AFTER`] long, for work on `queue` that never
    /// finished: the GPU stopped. Declares the device lost.
    pub fn stopped(&mut self, queue: Queue, waited: Duration) {
        self.pending = None;
        self.monitor.declare(format!(
            "the GPU stopped finishing work: nothing completed in {}s",
            waited.as_secs()
        ));
        // Dropping a queue waits for its last submission and panics when that
        // never finishes, which would take the process down with the GPU. The
        // queue of a GPU that stopped is kept.
        std::mem::forget(queue);
    }
}

/// What a renderer opens after its device was lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// A new device on the same adapter: the GPU was reset and works again.
    SameAdapter,
    /// A software rasteriser: the GPU cannot finish a frame any more.
    Software,
}

/// Why this process renders in software although it has a GPU, once it does.
static SOFTWARE_FALLBACK: Mutex<Option<String>> = Mutex::new(None);

/// Decide what follows the loss of a device on `adapter`. `finished_frames`
/// says whether that device finished at least one frame before it was lost. A
/// loss before any is a GPU that cannot be brought back, and from then on this
/// process opens only software adapters ([`software_fallback`]).
pub fn recover(adapter: &AdapterInfo, finished_frames: bool, reason: &str) -> Recovery {
    if finished_frames && software_fallback().is_none() {
        return Recovery::SameAdapter;
    }
    let mut fallback = SOFTWARE_FALLBACK.lock_recover();
    if fallback.is_none() {
        tracing::error!(
            adapter = %adapter.name,
            %reason,
            "the GPU cannot be brought back; rendering in software from now on"
        );
        *fallback = Some(format!("{}: {reason}", adapter.name));
    }
    Recovery::Software
}

/// Why this process renders in software although it has a GPU, once
/// [`recover`] decided it must.
pub fn software_fallback() -> Option<String> {
    SOFTWARE_FALLBACK.lock_recover().clone()
}

/// Forget the fallback, so tests that share a process start from a GPU.
#[doc(hidden)]
pub fn reset_software_fallback_for_test() {
    *SOFTWARE_FALLBACK.lock_recover() = None;
}

/// The software adapter `instance` offers, preferring Vulkan (lavapipe) to
/// OpenGL (llvmpipe), or `None` when it offers none.
pub(super) async fn software_adapter(instance: &wgpu::Instance) -> Option<wgpu::Adapter> {
    let mut software: Vec<wgpu::Adapter> = instance
        .enumerate_adapters(wgpu::Backends::all())
        .await
        .into_iter()
        .filter(|adapter| adapter.get_info().device_type == DeviceType::Cpu)
        .collect();
    software.sort_by_key(|adapter| adapter.get_info().backend != wgpu::Backend::Vulkan);
    software.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(device_type: DeviceType) -> AdapterInfo {
        AdapterInfo {
            name: "Adreno X1-85".to_owned(),
            vendor: 0,
            device: 0,
            device_type,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend: wgpu::Backend::Vulkan,
            subgroup_min_size: 4,
            subgroup_max_size: 128,
            transient_saves_memory: false,
        }
    }

    #[test]
    fn a_reset_gpu_is_opened_again_and_a_dead_one_gives_way_to_software() {
        reset_software_fallback_for_test();
        let gpu = adapter(DeviceType::IntegratedGpu);
        assert_eq!(
            recover(&gpu, true, "reset"),
            Recovery::SameAdapter,
            "a device that drew frames was reset: its GPU works again"
        );
        assert!(software_fallback().is_none());
        assert_eq!(
            recover(&gpu, false, "Device is lost"),
            Recovery::Software,
            "a device lost before its first frame is on a GPU that cannot come back"
        );
        assert!(
            software_fallback().is_some_and(|why| why.contains("Device is lost")),
            "the process now opens software adapters only"
        );
        assert_eq!(
            recover(&gpu, true, "again"),
            Recovery::Software,
            "once given up, the GPU stays given up for this process"
        );
        assert_eq!(
            recover(&adapter(DeviceType::Cpu), true, "lost"),
            Recovery::Software,
            "a lost software device is replaced by a software device"
        );
        reset_software_fallback_for_test();
    }

    /// A device of this machine's adapter (lavapipe in CI and on development
    /// machines: tests never run on a real GPU).
    fn device() -> (super::super::RenderContext, usize) {
        let mut context = super::super::RenderContext::new();
        let id = pollster::block_on(context.device(None)).expect("a wgpu adapter");
        (context, id)
    }

    fn submit_empty(device: &Device, queue: &Queue) {
        let encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        queue.submit([encoder.finish()]);
    }

    #[test]
    fn a_destroyed_device_is_lost_to_its_watch() {
        let (context, id) = device();
        let handle = &context.devices[id];
        let mut watch = LossWatch::new(handle.loss_monitor());
        assert!(watch.lost(&handle.device).is_none());
        handle.device.destroy();
        let _ = handle.device.poll(wgpu::PollType::Poll);
        assert!(
            watch
                .lost(&handle.device)
                .is_some_and(|reason| reason.contains("Destroyed")),
            "wgpu's own report reaches the renderer through the watch"
        );
    }

    #[test]
    fn work_that_finishes_late_is_not_a_lost_device() {
        let (context, id) = device();
        let handle = &context.devices[id];
        let mut watch = LossWatch::new(handle.loss_monitor());
        let start = crate::time::now();
        submit_empty(&handle.device, &handle.queue);
        watch.unfinished_at(&handle.queue, start);
        let _ = handle.device.poll(wgpu::PollType::wait_indefinitely());
        assert!(
            watch
                .lost_at(&handle.device, start + NOT_PROGRESSING_AFTER)
                .is_none(),
            "a slow GPU that finished is still working"
        );
    }

    /// The GPU of the Yoga that froze its session: its firmware stopped
    /// answering and nothing submitted ever completed, with no error at all.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_gpu_that_stops_finishing_work_is_declared_lost() {
        let (context, id) = device();
        let handle = &context.devices[id];
        submit_empty(&handle.device, &handle.queue);
        let _ = handle.device.poll(wgpu::PollType::wait_indefinitely());
        // SAFETY: the hal device is only flagged; no raw handle leaves this block.
        unsafe { handle.device.as_hal::<wgpu::hal::api::Vulkan>() }
            .expect("tests render through Vulkan")
            .simulate_hang();
        submit_empty(&handle.device, &handle.queue);

        let mut watch = LossWatch::new(handle.loss_monitor());
        let start = crate::time::now();
        watch.unfinished_at(&handle.queue, start);
        assert!(
            watch
                .lost_at(&handle.device, start + Duration::from_secs(1))
                .is_none(),
            "work in flight for a second is not yet a dead GPU"
        );
        let reason = watch.lost_at(&handle.device, start + NOT_PROGRESSING_AFTER);
        assert!(
            reason
                .as_deref()
                .is_some_and(|reason| reason.contains("stopped finishing work")),
            "work unfinished past the deadline declares the device lost: {reason:?}"
        );
        assert_eq!(
            handle.loss_monitor().reason(),
            reason,
            "every holder of the device's monitor sees the declared loss"
        );
    }
}
