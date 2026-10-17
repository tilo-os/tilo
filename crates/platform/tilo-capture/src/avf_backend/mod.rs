//! Native capture on macOS through `AVFoundation`: one `AVCaptureSession` per
//! capture, whose data output hands each picture or run of samples to a
//! delegate on a dispatch queue of its own. Pictures go through a single
//! latest-frame slot, so a slow UI never queues camera-sized allocations.
//!
//! A camera or microphone session starts only once macOS has the person's
//! answer (`requestAccessForMediaType`, which asks at most once per app). A
//! session's runtime error is [`CaptureState::Failed`], its device going
//! away [`CaptureState::Ended`].

#[cfg(avf_microphone)]
mod audio;
#[cfg(any(avf_camera, avf_screen))]
mod video;

#[cfg(avf_microphone)]
pub(crate) use audio::microphone;
#[cfg(avf_camera)]
pub(crate) use video::camera;
#[cfg(avf_screen)]
pub(crate) use video::screen;

use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVCaptureAudioDataOutputSampleBufferDelegate, AVCaptureConnection, AVCaptureDevice,
    AVCaptureDeviceWasDisconnectedNotification, AVCaptureInput, AVCaptureOutput, AVCaptureSession,
    AVCaptureSessionErrorKey, AVCaptureSessionRuntimeErrorNotification,
    AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{
    NSError, NSNotification, NSNotificationCenter, NSNumber, NSObject, NSObjectProtocol, NSString,
};

use crate::{CaptureError, CaptureState, Device, DeviceKind, Wake};
use tilo_lock::MutexRecover as _;

/// What the delegate does with each sample buffer.
type OnSample = Box<dyn Fn(&CMSampleBuffer) + Send + Sync>;

/// An output's delegate and the queue it is called on.
type Feed = (Retained<SampleDelegate>, DispatchRetained<DispatchQueue>);

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `SampleDelegate`
    // does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = AllocAnyThread]
    #[name = "TiloCaptureSampleDelegate"]
    #[ivars = OnSample]
    struct SampleDelegate;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for SampleDelegate {}

    // SAFETY: the method signature matches the protocol's.
    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for SampleDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(
            &self,
            _output: &AVCaptureOutput,
            sample: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            (self.ivars())(sample);
        }
    }

    // SAFETY: the audio delegate's one method has the video delegate's
    // selector and signature, defined above.
    unsafe impl AVCaptureAudioDataOutputSampleBufferDelegate for SampleDelegate {}
);

impl SampleDelegate {
    fn new(on_sample: OnSample) -> Retained<Self> {
        let this = Self::alloc().set_ivars(on_sample);
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// What the delegate hands over, and where the capture is.
struct Shared<T> {
    latest: Option<T>,
    state: Option<CaptureState>,
    /// Told when `state` changes.
    wake: Option<Wake>,
}

impl<T> Default for Shared<T> {
    fn default() -> Self {
        Self {
            latest: None,
            state: None,
            wake: None,
        }
    }
}

type Slot<T> = Arc<Mutex<Shared<T>>>;

/// A later state replaces an earlier one, except that [`CaptureState::Live`]
/// never replaces anything. A change wakes whoever asked to hear of it;
/// the `Live` every picture settles does not, once it holds.
fn settle<T>(slot: &Slot<T>, state: CaptureState) {
    let mut shared = slot.lock_recover();
    let changed = if state == CaptureState::Live {
        let first = shared.state.is_none();
        shared.state.get_or_insert(state);
        first
    } else if shared.state.as_ref() == Some(&state) {
        false
    } else {
        shared.state = Some(state);
        true
    };
    let wake = changed.then(|| shared.wake.clone()).flatten();
    drop(shared);
    if let Some(wake) = wake {
        wake();
    }
}

fn state<T>(slot: &Slot<T>) -> CaptureState {
    slot.lock_recover()
        .state
        .clone()
        .unwrap_or(CaptureState::Starting)
}

fn media(kind: DeviceKind) -> Option<&'static AVMediaType> {
    // SAFETY: immutable framework constants.
    unsafe {
        match kind {
            DeviceKind::Camera => AVMediaTypeVideo,
            DeviceKind::Microphone => AVMediaTypeAudio,
        }
    }
}

pub(crate) fn devices(kind: DeviceKind) -> Vec<Device> {
    let Some(media) = media(kind) else {
        return Vec::new();
    };
    // SAFETY: a class method that only reads the device list.
    #[expect(
        deprecated,
        reason = "the discovery session's external-camera and microphone device types \
                  need macOS 14, and Tilo supports macOS 13"
    )]
    let found = unsafe { AVCaptureDevice::devicesWithMediaType(media) };
    found
        .iter()
        // SAFETY: plain property reads on a device AVFoundation listed.
        .map(|device| unsafe {
            Device {
                id: device.uniqueID().to_string(),
                label: device.localizedName().to_string(),
                kind,
            }
        })
        .collect()
}

/// The device `id` names ([`Device::id`]), or the default one, as an input.
#[cfg(any(avf_camera, avf_microphone))]
fn device_input(
    kind: DeviceKind,
    id: Option<&str>,
) -> Result<(Retained<AVCaptureDevice>, Retained<AVCaptureInput>), CaptureError> {
    let media = media(kind).ok_or(CaptureError::NotFound)?;
    // SAFETY: class methods that look a device up.
    let device = unsafe {
        match id {
            Some(id) => AVCaptureDevice::deviceWithUniqueID(&NSString::from_str(id)),
            None => AVCaptureDevice::defaultDeviceWithMediaType(media),
        }
    }
    .ok_or(CaptureError::NotFound)?;
    // SAFETY: an input for a device AVFoundation returned.
    let input =
        unsafe { objc2_av_foundation::AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
            .map_err(|error| CaptureError::Failed(error.localizedDescription().to_string()))?;
    Ok((device, Retained::into_super(input)))
}

/// A configured session and what feeds it; dropping it stops the capture.
struct Capture {
    session: Retained<AVCaptureSession>,
    /// Set once the capture is dropped, so a late permission answer does not
    /// start it. Apart from the delegate's slot, so that stopping never waits
    /// on a delegate that waits on the slot.
    stopped: Arc<Mutex<bool>>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    _delegate: Retained<SampleDelegate>,
    _queue: DispatchRetained<DispatchQueue>,
}

impl Capture {
    /// `input` feeding `output`, whose delegate is `delegate` on `queue`;
    /// its failures and `device` going away land in `slot`.
    fn new<T: Send + 'static>(
        input: &AVCaptureInput,
        output: &AVCaptureOutput,
        (delegate, queue): Feed,
        slot: &Slot<T>,
        device: Option<&AVCaptureDevice>,
    ) -> Result<Self, CaptureError> {
        // SAFETY: a new session, configured before it runs.
        let session = unsafe {
            let session = AVCaptureSession::new();
            session.beginConfiguration();
            let fits = session.canAddInput(input) && session.canAddOutput(output);
            if fits {
                session.addInput(input);
                session.addOutput(output);
            }
            session.commitConfiguration();
            fits.then_some(session)
        }
        .ok_or_else(|| CaptureError::Failed("the device cannot be captured".into()))?;
        let failed = Arc::clone(slot);
        let mut observers = vec![observe(
            // SAFETY: an immutable framework constant.
            unsafe { AVCaptureSessionRuntimeErrorNotification },
            &session,
            move |note| settle(&failed, CaptureState::Failed(runtime_error(note))),
        )];
        if let Some(device) = device {
            let ended = Arc::clone(slot);
            observers.push(observe(
                // SAFETY: an immutable framework constant.
                unsafe { AVCaptureDeviceWasDisconnectedNotification },
                device,
                move |_| settle(&ended, CaptureState::Ended),
            ));
        }
        Ok(Self {
            session,
            stopped: Arc::default(),
            observers,
            _delegate: delegate,
            _queue: queue,
        })
    }

    /// Start once the person allows this app `kind`, or fail with
    /// [`CaptureError::PermissionDenied`].
    #[cfg(any(avf_camera, avf_microphone))]
    fn start_when_allowed<T: Send + 'static>(&self, kind: DeviceKind, slot: &Slot<T>) {
        let Some(media) = media(kind) else {
            settle(slot, CaptureState::Failed(CaptureError::NotFound));
            return;
        };
        let (session, stopped, slot) = (
            self.session.clone(),
            Arc::clone(&self.stopped),
            Arc::clone(slot),
        );
        let answered = RcBlock::new(move |allowed: objc2::runtime::Bool| {
            if allowed.as_bool() {
                start(&session, &stopped, &slot);
            } else {
                settle(&slot, CaptureState::Failed(CaptureError::PermissionDenied));
            }
        });
        // SAFETY: the handler may run on any queue: a session starts from
        // any thread, and the slot and the flag are behind mutexes.
        unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &answered) };
    }

    /// Start now, for an input macOS asks about by itself (the screen).
    #[cfg(avf_screen)]
    fn start_now<T>(&self, slot: &Slot<T>) {
        start(&self.session, &self.stopped, slot);
    }
}

/// Start `session` unless its capture was dropped meanwhile.
fn start<T>(session: &AVCaptureSession, stopped: &Mutex<bool>, slot: &Slot<T>) {
    let stopped = stopped.lock_recover();
    if *stopped {
        return;
    }
    // SAFETY: a configured session; `stopped` orders this against the stop.
    let running = unsafe {
        session.startRunning();
        session.isRunning()
    };
    drop(stopped);
    if !running {
        settle(
            slot,
            CaptureState::Failed(CaptureError::Failed(
                "the capture session did not start".into(),
            )),
        );
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let mut stopped = self.stopped.lock_recover();
        *stopped = true;
        // SAFETY: stopping a session is allowed in any state.
        unsafe { self.session.stopRunning() };
        drop(stopped);
        let center = NSNotificationCenter::defaultCenter();
        for observer in &self.observers {
            // SAFETY: an observer this capture added.
            unsafe { center.removeObserver(observer.as_ref()) };
        }
    }
}

/// Call `then` with every `name` notification `object` posts.
fn observe(
    name: &NSString,
    object: &AnyObject,
    then: impl Fn(&NSNotification) + 'static,
) -> Retained<ProtocolObject<dyn NSObjectProtocol>> {
    let block = RcBlock::new(move |note: NonNull<NSNotification>| {
        // SAFETY: the centre passes a valid notification.
        then(unsafe { note.as_ref() });
    });
    // SAFETY: no queue, so the block runs where the notification is posted;
    // `then` only takes a mutex.
    unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(name),
            Some(object),
            None,
            &block,
        )
    }
}

fn runtime_error(note: &NSNotification) -> CaptureError {
    let error = note.userInfo().and_then(|info| {
        // SAFETY: an immutable framework constant.
        let key = unsafe { AVCaptureSessionErrorKey };
        info.objectForKey(key)?.downcast::<NSError>().ok()
    });
    CaptureError::Failed(error.map_or_else(
        || "the capture session failed".into(),
        |error| error.localizedDescription().to_string(),
    ))
}

fn number(value: u32) -> Retained<AnyObject> {
    Retained::into_super(Retained::into_super(NSNumber::new_u32(value))).into()
}
