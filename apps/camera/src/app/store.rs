//! Camera as a bound view: the [`Store`] the window shares with the view's
//! effect handler, and [`window`], which takes the window into the view.
//!
//! The effects are the window's own work: the view hands each to the
//! window, which does it when it next looks and answers a job's caller
//! once its work settles. Only an image to scan is chosen here, where the
//! invocation's file chooser answers.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectReply, EffectStatus, Effected, Jobs, Live, Publishes, RunMode,
    ViewCx,
};
use ui::tilo_actions::InvocationId;
use ui::widget::Element;

use super::{CameraApp, ChooserOpen};
use crate::actions::{CameraEffect, CameraResourceCtx};
use crate::model::{CameraDevicePreferences, CaptureMode, CapturePreferences};
use crate::record::Record;
use crate::resource::ShotClock;

/// What the view asked of the window, done when it next looks.
pub(super) enum Ask {
    /// Do an effect; a job's caller waits for `EffectReply`.
    Effect(CameraEffect, Option<InvocationId>, Option<EffectReply>),
    /// Scan the code in the image the person chose.
    ScanImage(Vec<u8>, Option<InvocationId>, EffectReply),
    /// Say something in the window.
    Say(String),
    /// A file chooser answered: show it closed.
    ChooserClosed,
    /// Stage a capture's scenario (`None` for a plain capture).
    Stage(Option<String>),
}

/// What the window and the view's effect handler share.
#[derive(Default)]
pub(crate) struct Store {
    /// What the view asked of the window since it last looked.
    pub(super) asks: Vec<Ask>,
    /// What the rows show of the window, as it last looked; the camera
    /// devices are listed when the rows are read.
    pub(super) shown: CameraResourceCtx,
    /// Whether the window lists this device's real cameras.
    lists_devices: bool,
    /// Set while the file chooser for an image to scan is open.
    scan_import_pending: Rc<Cell<bool>>,
    /// The record as the document holds it, shown as it changes; `None` for
    /// a window outside the framework.
    document: Option<Live<Record>>,
    /// The `(revision, loads)` of the record last shown.
    presented: Option<(u64, u64)>,
    /// Where the window starts work of its own, once the view attached it.
    jobs: Option<Jobs<Record>>,
}

/// The [`Store`] as the window and the view share it.
pub(crate) type Shared = Rc<RefCell<Store>>;

impl Store {
    /// The store of a window that lists this device's real cameras, or not.
    pub(super) fn shared(lists_devices: bool) -> Shared {
        Rc::new(RefCell::new(Self {
            lists_devices,
            ..Self::default()
        }))
    }

    /// The rows of the camera, the scanners and the document scanner.
    fn rows(&self) -> Vec<ui::framework::LiveRow> {
        let mut ctx = self.shown.clone();
        ctx.camera_devices = super::camera_devices(self.lists_devices);
        ctx.scan_import_pending = self.scan_import_pending.get();
        crate::actions::live_rows(&ctx)
    }

    /// Switching between photo and video stops nothing: while a video
    /// records, the switch is unavailable.
    fn statuses(&self) -> Vec<EffectStatus> {
        if !self.shown.recording {
            return Vec::new();
        }
        vec![EffectStatus::unavailable(
            CameraEffect::USE_MODE,
            "recording",
            "Stop recording to switch modes",
        )]
    }
}

/// Hand one [`CameraEffect`], invoked by a control, the palette, an agent or
/// a script, to the window. A job's caller hears back once its work
/// settles.
fn answer(store: &Shared, effect: CameraEffect, cx: &mut EffectCx<Record>) -> EffectAnswer {
    let job = match &effect {
        CameraEffect::UseMode { .. } if store.borrow().shown.recording => {
            return Err("Stop recording to switch modes".into());
        }
        CameraEffect::ImportImage => {
            return import_image(store, cx);
        }
        CameraEffect::Capture
        | CameraEffect::Record
        | CameraEffect::CapturePage
        | CameraEffect::RecognizeText
        | CameraEffect::ExportPdf => true,
        _ => false,
    };
    let invocation = cx.invocation();
    let reply = job.then(|| cx.reply());
    store
        .borrow_mut()
        .asks
        .push(Ask::Effect(effect, invocation, reply));
    Ok(if job {
        Effected::Pending
    } else {
        Effected::done()
    })
}

/// Ask for an image with the file chooser and have the window scan the
/// code in it. The caller hears back once the scan settles.
fn import_image(store: &Shared, cx: &mut EffectCx<Record>) -> EffectAnswer {
    let pending = Rc::clone(&store.borrow().scan_import_pending);
    let Some(open) = ChooserOpen::open(&pending) else {
        return Err("An image picker is already open".into());
    };
    store.borrow_mut().asks.push(Ask::Say(
        "Choose an image containing a QR code or barcode".into(),
    ));
    let request = ui::portal::PickRequest::open("Scan a code from an image", "Scan").filter(
        ui::portal::FileFilter::new("Images")
            .mimetype("image/png")
            .mimetype("image/jpeg")
            .mimetype("image/webp")
            .mimetype("image/gif"),
    );
    let store = Rc::clone(store);
    Ok(cx.choose_file(request, move |(), chosen, cx| {
        drop(open);
        let ask = match chosen {
            Some(file) => Ask::ScanImage(file.bytes, cx.invocation(), cx.reply()),
            None => Ask::Say("Image scan cancelled".into()),
        };
        let chosen = matches!(ask, Ask::ScanImage(..));
        store.borrow_mut().asks.push(ask);
        if chosen {
            Ok(Effected::Pending)
        } else {
            Err("Image scan cancelled".into())
        }
    }))
}

impl CameraApp {
    /// What the window shares with the view's effect handler.
    pub(crate) fn state(&self) -> std::cell::RefMut<'_, Store> {
        self.store.borrow_mut()
    }

    /// Whether the file chooser for an image to scan is open.
    pub(super) fn scan_import_pending(&self) -> bool {
        self.store.borrow().scan_import_pending.get()
    }

    /// Show the record the document holds now, when it changed since.
    pub(super) fn follow_document(&mut self) {
        let changed = {
            let mut store = self.state();
            let store = &mut *store;
            store
                .document
                .as_ref()
                .and_then(|document| document.changed_since(&mut store.presented))
        };
        if let Some(record) = changed {
            self.present_record(&record);
        }
    }

    /// Keep what the rows show in step with the window.
    pub(super) fn publish_rows(&self) {
        let shown = self.camera_resource_ctx_with_recent(
            self.captures.first().map(Self::capture_item),
            Vec::new(),
        );
        let mut store = self.state();
        if store.shown != shown {
            store.shown = shown;
        }
    }

    /// Do what the view asked of the window since it last looked.
    pub(super) fn do_asks(&mut self) {
        let asks = std::mem::take(&mut self.state().asks);
        if asks.is_empty() {
            return;
        }
        self.dirty = true;
        for ask in asks {
            match ask {
                Ask::Effect(CameraEffect::ExportPdf, _, reply) => self.export_document(reply),
                Ask::Effect(effect, invocation, reply) => {
                    let answer = self.answer_effect(effect, invocation);
                    match (answer, reply) {
                        (Ok(true), Some(reply)) => {
                            if let Some(invocation) = invocation {
                                self.replies.insert(invocation, reply);
                            }
                        }
                        (answer, Some(reply)) => {
                            reply.finish(answer.map(|_| ui::tilo_actions::ActionOutput::default()));
                        }
                        (Err(error), None) => self.toast.fail(error),
                        (Ok(_), None) => {}
                    }
                }
                Ask::ScanImage(bytes, invocation, reply) => match self.scan_image_bytes(bytes) {
                    Ok(()) => {
                        self.pending_scan_import = invocation;
                        if let Some(invocation) = invocation {
                            self.replies.insert(invocation, reply);
                        }
                    }
                    Err(error) => reply.finish(Err(error)),
                },
                Ask::Say(text) => self.toast.show(text),
                Ask::ChooserClosed => {}
                Ask::Stage(scenario) => self.stage_scene(scenario.as_deref()),
            }
        }
    }

    /// Ask where to save the scanned pages as one searchable PDF, as the
    /// export effect does; `reply` hears whether they were saved.
    fn export_document(&self, reply: Option<EffectReply>) {
        let refuse = |reply: Option<EffectReply>, why: &str| {
            if let Some(reply) = reply {
                reply.finish(Err(why.into()));
            }
        };
        if self.document_scan.pages.is_empty() {
            return refuse(reply, "Capture a page before exporting");
        }
        let Some(jobs) = self.state().jobs.clone() else {
            return refuse(reply, "Camera is not open on a record");
        };
        let Some(open) = ChooserOpen::open(&self.document_export_pending) else {
            return refuse(reply, "The save location chooser is already open");
        };
        let pdf = self.document_scan.searchable_pdf();
        let store = Rc::clone(&self.store);
        jobs.start::<()>("export-pdf", move |cx| {
            Ok(cx.save_file(
                ui::portal::PickRequest::save(
                    "Save scanned document",
                    "Save",
                    "Scanned document.pdf",
                ),
                pdf,
                move |(), saved, _| {
                    drop(open);
                    store.borrow_mut().asks.push(Ask::ChooserClosed);
                    if let Some(reply) = reply {
                        reply.finish(if saved {
                            Ok(ui::tilo_actions::ActionOutput::default())
                        } else {
                            Err("Document export cancelled".into())
                        });
                    }
                    Ok(Effected::done())
                },
            ))
        });
    }
}

/// The view of the camera: `app` follows the record, and the view hands it
/// the effects and publishes its rows. A live window follows the person's
/// and this device's capture preferences; a capture shows its scenario's;
/// a past version the defaults.
pub(crate) fn window(cx: &ViewCx<Record>, mut app: CameraApp) -> Element {
    match cx.mode() {
        RunMode::Capture => {
            if let Some(clock) = cx.fixture::<ShotClock>() {
                app.set_clock(clock.0);
            }
            if let Some(mode) = cx.fixture::<CaptureMode>() {
                app.stage_mode(*mode);
            }
            if let Some(person) = cx.fixture::<CapturePreferences>() {
                app.stage_preferences(*person, CameraDevicePreferences::default());
            }
        }
        RunMode::Live => app.follow_preferences(
            cx.settings::<CapturePreferences>(),
            cx.settings::<CameraDevicePreferences>(),
        ),
        RunMode::Past => {}
    }
    app.attach_dispatcher(cx.dispatcher());
    app.attach_actions(&cx.actions());
    {
        let mut store = app.state();
        store.document = Some(cx.live().clone());
        store.jobs = Some(cx.jobs());
    }
    app.follow_document();
    app.publish_rows();
    let store = Rc::clone(&app.store);
    {
        let (answer_store, status) = (Rc::clone(&store), Rc::clone(&store));
        cx.effects(move |effect: CameraEffect, cx| answer(&answer_store, effect, cx))
            .status(move || status.borrow().statuses());
    }
    {
        let store = Rc::clone(&store);
        cx.on_stage(move |scenario| {
            store
                .borrow_mut()
                .asks
                .push(Ask::Stage(scenario.map(str::to_owned)));
        });
    }
    cx.publishing(
        Publishes::new().live_rows(move || store.borrow().rows()),
        Box::new(app),
    )
}
