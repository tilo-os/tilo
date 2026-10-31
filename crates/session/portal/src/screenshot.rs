//! `org.freedesktop.impl.portal.Screenshot`.
//!
//! An interactive screenshot is asked as a question of kind
//! [`SCREENSHOT_QUESTION`]: the person takes it (or not). A
//! non-interactive one needs the person's access decision for the app
//! ([`crate::consent`]), unless the frontend already checked its
//! permission store. Either way the picture is the session's capture of
//! what the displays show ([`crate::Capture`]), handed back as a file URI.
//!
//! A colour pick asks the same session for the pixel under the pointer
//! ([`Capture::color_at_pointer`]), answered as red, green and blue.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tilo_service::Question;
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue};

use crate::consent::AccessQuestion;
use crate::doc_store;
use crate::file_chooser::file_uri;
use crate::reply::{Ended, OrFail, Outcome, Reply, Results, answer};
use crate::{
    Capture, CaptureError, CaptureTarget, Portal, PortalCaller, lockdown, option, question_handle,
};

/// The kind of the question an interactive screenshot asks.
pub const SCREENSHOT_QUESTION: &str = "portal.screenshot";
/// The capability a non-interactive screenshot needs.
pub const SCREENSHOT_CAPABILITY: &str = "screenshot";

/// What the sheet shows for an interactive screenshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenshotQuestion {
    /// The window the app asks from, when it named one.
    pub parent_window: Option<String>,
    /// The app asked for the sheet to keep its window from input.
    pub modal: bool,
}

/// How a screenshot request is decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    /// The person takes it in a sheet.
    Ask,
    /// The app's access decides; asked once when it has none.
    Access,
    /// The frontend checked its permission store already.
    Granted,
}

fn decision(options: &HashMap<String, OwnedValue>) -> Decision {
    if option::<bool>(options, "interactive").unwrap_or(false) {
        Decision::Ask
    } else if option::<bool>(options, "permission_store_checked").unwrap_or(false) {
        Decision::Granted
    } else {
        Decision::Access
    }
}

/// What the app asks to capture: its `target` option, which is one of the
/// bits this session offers in `available`; everything the displays show
/// when it names none.
fn target(options: &HashMap<String, OwnedValue>, available: u32) -> Result<CaptureTarget, Ended> {
    let Some(raw) = option::<u32>(options, "target") else {
        return Ok(CaptureTarget::Screen);
    };
    match CaptureTarget::of_bit(raw) {
        Some(target) if available & target.bit() != 0 => Ok(target),
        _ => {
            tracing::warn!(target = raw, available, "a screenshot target nobody offers");
            Err(Ended::Failed)
        }
    }
}

/// The `org.freedesktop.impl.portal.Screenshot` interface.
pub struct Screenshot {
    portal: Portal,
}

impl Screenshot {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }
}

/// Decide request `path` of `caller` as `options` ask, then capture.
async fn take(
    portal: &Portal,
    caller: &PortalCaller,
    path: &str,
    parent_window: &str,
    options: &HashMap<String, OwnedValue>,
) -> Outcome {
    let handle = question_handle(path);
    // What is asked for is settled before the person is asked anything: a
    // target this session cannot take is refused at once.
    let target = target(options, portal.capture.targets())?;
    match decision(options) {
        Decision::Ask => {
            let question = ScreenshotQuestion {
                parent_window: Some(parent_window.trim())
                    .filter(|window| !window.is_empty())
                    .map(str::to_owned),
                modal: option::<bool>(options, "modal").unwrap_or(true),
            };
            portal
                .questions
                .ask(Question {
                    handle,
                    kind: SCREENSHOT_QUESTION.into(),
                    app_id: Some(caller.app_id.clone()),
                    label: caller.app_id.clone(),
                    details: serde_json::to_value(question).unwrap_or_default(),
                })
                .await
                .ok_or(Ended::Cancelled)?;
        }
        Decision::Access => {
            portal
                .consents
                .decide(
                    caller,
                    &handle,
                    AccessQuestion {
                        capability: SCREENSHOT_CAPABILITY.into(),
                        reason: None,
                        may_remember: true,
                        text: None,
                    },
                )
                .await?;
        }
        Decision::Granted => {}
    }
    let picture = portal
        .capture
        .screenshot(target)
        .await
        .map_err(|error| match error {
            // The person closed the selector without taking anything: a cancel,
            // as it is for every other selection an app asks for.
            CaptureError::Cancelled => Ended::Cancelled,
            CaptureError::Failed(error) => {
                tracing::warn!(%error, "screenshot not captured");
                Ended::Failed
            }
        })?;
    // The picture is handed out as the person's picks are: what the app
    // opens is its own path in the store its launcher bound, never a file
    // in the person's folders.
    let export = doc_store::exports(
        portal.documents(),
        caller,
        &[picture],
        tilo_documents::Kind::Read,
    )
    .pop()
    .ok_or(Ended::Failed)?;
    let uri = file_uri(&export.app_path).or_fail()?;
    let mut results = Results::new();
    results.insert(
        "uri".into(),
        OwnedValue::from(zbus::zvariant::Str::from(uri)),
    );
    Ok(results)
}

#[interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl Screenshot {
    /// The bits an app may name as its `target`, one per kind of capture
    /// this session can take.
    #[zbus(property, name = "AvailableTargets")]
    fn available_targets(&self) -> u32 {
        self.portal.capture.targets()
    }

    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        3
    }

    async fn screenshot(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, Some(&parent_window))
                .await?;
            if lockdown::current_policy().disable_save_to_disk {
                return Err(Ended::Cancelled);
            }
            let path = handle.as_str();
            self.portal
                .requests
                .run(
                    connection,
                    path,
                    auth.owner.as_str(),
                    take(&self.portal, &auth.caller, path, &parent_window, &options),
                )
                .await
        })
        .await
    }

    /// The pixel under the pointer, answered as red, green and blue, each
    /// from 0 to 1: the session takes it when the app asks, so the person
    /// moves the pointer over the colour and the app has it.
    async fn pick_color(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let path = handle.as_str();
            let auth = self
                .portal
                .authenticate(&header, path, &app_id, Some(&parent_window))
                .await?;
            self.portal
                .requests
                .run(
                    connection,
                    path,
                    auth.owner.as_str(),
                    pick(&*self.portal.capture),
                )
                .await
        })
        .await
    }
}

/// The colour the session picks, as the interface's answer.
async fn pick(capture: &dyn Capture) -> Outcome {
    let (red, green, blue) = capture
        .color_at_pointer()
        .await
        .map_err(|error| match error {
            CaptureError::Cancelled => Ended::Cancelled,
            CaptureError::Failed(error) => {
                tracing::warn!(%error, "no colour picked");
                Ended::Failed
            }
        })?;
    let mut results = Results::new();
    results.insert(
        "color".into(),
        OwnedValue::try_from(zbus::zvariant::Value::new((red, green, blue))).or_fail()?,
    );
    Ok(results)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use tilo_documents::Documents;
    use tilo_service::Questions;

    use super::*;
    use crate::reply::reply;

    #[test]
    fn interactive_asks_checked_skips_and_otherwise_access_decides() {
        let with = |pairs: &[(&str, bool)]| {
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), OwnedValue::from(*value)))
                .collect::<HashMap<_, _>>()
        };
        assert_eq!(decision(&with(&[("interactive", true)])), Decision::Ask);
        assert_eq!(
            decision(&with(&[
                ("interactive", true),
                ("permission_store_checked", true)
            ])),
            Decision::Ask,
            "an interactive screenshot is always the person's"
        );
        assert_eq!(
            decision(&with(&[("permission_store_checked", true)])),
            Decision::Granted
        );
        assert_eq!(decision(&HashMap::new()), Decision::Access);
    }

    #[test]
    fn the_target_option_names_what_to_capture_and_only_what_is_offered() {
        let with = |target: u32| HashMap::from([("target".to_owned(), OwnedValue::from(target))]);
        let offered = CaptureTarget::Screen.bit() | CaptureTarget::ActiveWindow.bit();
        assert_eq!(target(&HashMap::new(), offered), Ok(CaptureTarget::Screen));
        assert_eq!(target(&with(1), offered), Ok(CaptureTarget::Screen));
        assert_eq!(target(&with(8), offered), Ok(CaptureTarget::ActiveWindow));
        // A bit this session does not offer is refused, as is one that
        // stands for no target at all.
        assert_eq!(target(&with(2), offered), Err(Ended::Failed));
        assert_eq!(target(&with(4), offered), Err(Ended::Failed));
        assert_eq!(target(&with(16), offered), Err(Ended::Failed));
        assert_eq!(target(&with(0), offered), Err(Ended::Failed));
        // A session that takes what the person picks or draws answers all
        // four.
        let every = offered | CaptureTarget::Window.bit() | CaptureTarget::Area.bit();
        assert_eq!(target(&with(2), every), Ok(CaptureTarget::Window));
        assert_eq!(target(&with(4), every), Ok(CaptureTarget::Area));
        assert_eq!(target(&with(16), every), Err(Ended::Failed));
    }

    /// A session that answers a screenshot with `answer` rather than
    /// capturing one, and picks no colour.
    struct Picture(Result<PathBuf, CaptureError>);

    impl crate::Capture for Picture {
        fn screenshot(
            &self,
            _target: CaptureTarget,
        ) -> crate::Pending<Result<PathBuf, CaptureError>> {
            let answer = self.0.clone();
            Box::pin(async move { answer })
        }

        fn color_at_pointer(&self) -> crate::Pending<Result<(f64, f64, f64), CaptureError>> {
            Box::pin(async {
                Err(CaptureError::Failed(
                    crate::NO_COLOUR_UNDER_POINTER.to_owned(),
                ))
            })
        }

        fn targets(&self) -> u32 {
            CaptureTarget::Screen.bit()
                | CaptureTarget::Window.bit()
                | CaptureTarget::Area.bit()
                | CaptureTarget::ActiveWindow.bit()
        }
    }

    /// A session that answers a pick with `answer` rather than picking one.
    struct Palette(Result<(f64, f64, f64), CaptureError>);

    impl crate::Capture for Palette {
        fn screenshot(
            &self,
            _target: CaptureTarget,
        ) -> crate::Pending<Result<PathBuf, CaptureError>> {
            Box::pin(async { Err(CaptureError::Failed("nothing to screenshot".to_owned())) })
        }

        fn color_at_pointer(&self) -> crate::Pending<Result<(f64, f64, f64), CaptureError>> {
            let answer = self.0.clone();
            Box::pin(async move { answer })
        }

        fn targets(&self) -> u32 {
            0
        }
    }

    /// What a pick for `answer` is answered with.
    async fn picked(answer: Result<(f64, f64, f64), CaptureError>) -> Reply {
        let questions = Questions::new();
        let portal = crate::tests::portal_with(&questions, Documents::new())
            .with_capture(Arc::new(Palette(answer)));
        reply(pick(&*portal.capture).await)
    }

    #[tokio::test]
    async fn a_pick_is_the_colour_the_session_reads_off_the_screen() {
        let (code, results) = picked(Ok((1.0, 0.5, 0.0))).await;
        assert_eq!(code, 0);
        let (red, green, blue): (f64, f64, f64) = results["color"]
            .try_clone()
            .unwrap()
            .try_into()
            .expect("three channels");
        assert_eq!((red, green, blue), (1.0, 0.5, 0.0));
    }

    #[tokio::test]
    async fn a_pick_with_nothing_under_the_pointer_fails_rather_than_cancels() {
        let questions = Questions::new();
        let portal = crate::tests::portal_with(&questions, Documents::new()).with_capture(
            Arc::new(Palette(Err(CaptureError::Failed(
                crate::NO_COLOUR_UNDER_POINTER.to_owned(),
            )))),
        );
        let (code, results) = reply(pick(&*portal.capture).await);
        assert_eq!(code, 2, "the request fails");
        assert!(results.is_empty());
        assert!(
            questions.pending().is_empty(),
            "the person was asked nothing"
        );
    }

    /// A caller in `room`, sandboxed or not.
    fn caller(room: Option<&str>, sandboxed: bool) -> PortalCaller {
        PortalCaller {
            sandboxed,
            workspace: room.map(str::to_owned),
            ..PortalCaller::new("so.tilo.shots")
        }
    }

    /// The picture `capture` writes, in `folder`.
    fn picture(folder: &std::path::Path) -> PathBuf {
        let path = folder.join("screenshot-1.png");
        std::fs::write(&path, b"what the app asked for").unwrap();
        path
    }

    /// `take` for `caller`, with `capture` answering for the session and
    /// `documents` the store it hands files out of. The frontend checked
    /// its permission store, so the person is asked nothing before the
    /// selection.
    async fn taken(
        caller: &PortalCaller,
        capture: Result<PathBuf, CaptureError>,
        documents: &Documents,
    ) -> Reply {
        let questions = Questions::new();
        let portal = crate::tests::portal_with(&questions, documents.clone())
            .with_capture(Arc::new(Picture(capture)));
        let options = HashMap::from([
            (
                "permission_store_checked".to_owned(),
                OwnedValue::from(true),
            ),
            (
                "target".to_owned(),
                OwnedValue::from(CaptureTarget::Area.bit()),
            ),
        ]);
        reply(
            take(
                &portal,
                caller,
                "/org/freedesktop/portal/desktop/request/1_2/t",
                "",
                &options,
            )
            .await,
        )
    }

    /// The URI a granted screenshot answers with, as a path.
    fn picture_of(results: &Results) -> PathBuf {
        let uri = String::try_from(results["uri"].try_clone().unwrap()).unwrap();
        let url = url::Url::parse(&uri).expect("a URI");
        url.to_file_path().expect("a file URI")
    }

    #[tokio::test]
    async fn a_sandboxed_app_asks_for_the_drawn_area_and_is_handed_its_own_path_to_it() {
        let folder = tempfile::tempdir().unwrap();
        let kept = picture(folder.path());
        let documents = Documents::new();
        let caller = caller(Some("studio"), true);

        let (code, results) = taken(&caller, Ok(kept.clone()), &documents).await;
        assert_eq!(code, 0);
        let answered = picture_of(&results);
        assert!(
            answered.starts_with(tilo_documents::SANDBOX_DIR),
            "the app is handed a path in the store it has bound, not {}",
            answered.display()
        );
        assert_eq!(
            answered.file_name(),
            Some(kept.file_name().unwrap()),
            "the picture keeps the name the session wrote"
        );

        // What the app opens is the picture: the store keeps it for that
        // app, to read.
        let key = crate::doc_store::key_of(&caller).expect("a sandboxed caller in a room");
        let target = tilo_documents::host::resolve(&documents, &key, &answered)
            .expect("the answer names a document of the app");
        assert_eq!(
            std::fs::read(target.host_path()).unwrap(),
            b"what the app asked for"
        );
        let (token, _) = tilo_documents::split_sandbox_path(&answered).unwrap();
        assert_eq!(
            documents
                .document(&key, &token)
                .map(|document| document.kind),
            Some(tilo_documents::Kind::Read),
            "the app may read it, not write it"
        );
    }

    #[tokio::test]
    async fn a_host_app_is_answered_with_the_picture_itself() {
        let folder = tempfile::tempdir().unwrap();
        let kept = picture(folder.path());
        let (code, results) = taken(
            &caller(Some("studio"), false),
            Ok(kept.clone()),
            &Documents::new(),
        )
        .await;
        assert_eq!(code, 0);
        assert_eq!(picture_of(&results), kept);
    }

    #[tokio::test]
    async fn a_selection_the_person_closed_is_a_cancel_and_a_capture_that_failed_is_a_failure() {
        let documents = Documents::new();
        let caller = caller(Some("studio"), true);
        assert_eq!(
            taken(&caller, Err(CaptureError::Cancelled), &documents)
                .await
                .0,
            1
        );
        assert_eq!(
            taken(
                &caller,
                Err(CaptureError::Failed("no display is on".to_owned())),
                &documents
            )
            .await
            .0,
            2
        );
    }
}
