//! The existing explicit Capture interface over a focused renderer source.
use tilo_app::feedback::{
    Error, FocusedSurface, MaskedScreenshot, ReviewText, ReviewedAttachments, capture_focused,
};

use super::transport::Capture;

/// Holds no raw pixels and enables no background capture. The host supplies
/// its existing focused-frame renderer and profile content custody.
pub struct FocusedCapture<S> {
    source: S,
    attachments: ReviewedAttachments,
}

impl<S: FocusedSurface> FocusedCapture<S> {
    pub fn new(source: S, attachments: ReviewedAttachments) -> Self {
        Self {
            source,
            attachments,
        }
    }
}

impl<S: FocusedSurface> Capture for FocusedCapture<S> {
    fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
        let frame = self.source.inspect()?;
        if frame.protected {
            return Err(Error::CaptureProtected);
        }
        let context = self.source.context()?;
        if self.source.inspect()? != frame {
            return Err(Error::CaptureChanged);
        }
        Ok(context)
    }
    fn capture(&mut self) -> Result<MaskedScreenshot, Error> {
        capture_focused(&mut self.source)
    }
    fn persist(&mut self, image: &MaskedScreenshot) -> Result<(), Error> {
        self.attachments.persist(image)
    }
    fn persist_async(
        &mut self,
        image: MaskedScreenshot,
    ) -> tilo_app::feedback::AttachmentFuture<'static> {
        let attachments = self.attachments.clone();
        Box::pin(async move { attachments.persist_async(&image).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    use tilo_app::feedback::{CaptureInspection, PaintClass, PaintRegion, PixelRect};

    struct Source(Rc<Cell<usize>>);
    impl FocusedSurface for Source {
        fn inspect(&mut self) -> Result<CaptureInspection, Error> {
            Ok(CaptureInspection {
                surface: "synthetic:focused".into(),
                revision: 1,
                width: 1,
                height: 1,
                protected: false,
                regions: Some(vec![PaintRegion {
                    bounds: PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    class: PaintClass::Unclassified,
                }]),
            })
        }
        fn read_rgba(&mut self, _: &CaptureInspection) -> Result<Vec<u8>, Error> {
            self.0.set(self.0.get().saturating_add(1));
            Ok(vec![80, 90, 100, 255])
        }
    }

    struct ContextSource {
        calls: Rc<Cell<u64>>,
        protected: bool,
        change_after: bool,
    }

    impl FocusedSurface for ContextSource {
        fn inspect(&mut self) -> Result<CaptureInspection, Error> {
            Ok(CaptureInspection {
                surface: "synthetic:context".into(),
                revision: if self.change_after {
                    self.calls.get()
                } else {
                    1
                },
                width: 1,
                height: 1,
                protected: self.protected,
                regions: None,
            })
        }
        fn read_rgba(&mut self, _: &CaptureInspection) -> Result<Vec<u8>, Error> {
            Err(Error::CaptureProtected)
        }
        fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
            self.calls.set(self.calls.get().saturating_add(1));
            Ok(Vec::new())
        }
    }

    #[test]
    fn context_refuses_protected_and_changed_focus_before_disclosure() {
        for (protected, change_after, expected_calls) in [(true, false, 0), (false, true, 1)] {
            let calls = Rc::new(Cell::new(0));
            let mut adapter = FocusedCapture::new(
                ContextSource {
                    calls: Rc::clone(&calls),
                    protected,
                    change_after,
                },
                ReviewedAttachments::new(tilo_content::ContentStore::memory()),
            );
            let result = adapter.context();
            if protected {
                assert!(matches!(result, Err(Error::CaptureProtected)));
            } else {
                assert!(matches!(result, Err(Error::CaptureChanged)));
            }
            assert_eq!(calls.get(), expected_calls);
        }
    }

    #[test]
    fn stable_unprotected_context_is_returned_without_pixel_readback() {
        let calls = Rc::new(Cell::new(0));
        let mut adapter = FocusedCapture::new(
            ContextSource {
                calls: Rc::clone(&calls),
                protected: false,
                change_after: false,
            },
            ReviewedAttachments::new(tilo_content::ContentStore::memory()),
        );
        assert!(adapter.context().unwrap().is_empty());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn constructing_and_inspecting_context_never_acquires_pixels() {
        let reads = Rc::new(Cell::new(0));
        let mut adapter = FocusedCapture::new(
            Source(Rc::clone(&reads)),
            ReviewedAttachments::new(tilo_content::ContentStore::memory()),
        );
        assert_eq!(reads.get(), 0);
        assert!(adapter.context().is_err());
        assert_eq!(reads.get(), 0);
        let image = adapter.capture().unwrap();
        assert_eq!(reads.get(), 1);
        assert!(matches!(adapter.persist(&image), Err(Error::NotDurable)));
        assert_eq!(
            reads.get(),
            1,
            "persistence cannot recapture or replace reviewed pixels"
        );
    }
}
