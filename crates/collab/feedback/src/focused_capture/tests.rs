use super::*;

struct Source {
    frame: CaptureInspection,
    after: Option<CaptureInspection>,
    reads: usize,
    fail_read: bool,
}

impl FocusedSurface for Source {
    fn inspect(&mut self) -> Result<CaptureInspection, Error> {
        Ok(if self.reads == 0 {
            self.frame.clone()
        } else {
            self.after.as_ref().unwrap_or(&self.frame).clone()
        })
    }
    fn read_rgba(&mut self, frame: &CaptureInspection) -> Result<Vec<u8>, Error> {
        if frame != &self.frame {
            return Err(Error::CaptureChanged);
        }
        self.reads = self.reads.checked_add(1).ok_or(Error::Full)?;
        if self.fail_read {
            return Err(Error::CaptureProtected);
        }
        Ok(vec![70, 80, 90, 255, 10, 20, 30, 255])
    }
}

fn source() -> Source {
    Source {
        frame: CaptureInspection {
            surface: "synthetic:focused-app".into(),
            revision: 7,
            width: 2,
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
        },
        after: None,
        reads: 0,
        fail_read: false,
    }
}

#[test]
fn unclassified_and_sensitive_geometry_are_masked_before_encoding() {
    for class in [PaintClass::Unclassified, PaintClass::Sensitive] {
        let mut source = source();
        source
            .frame
            .regions
            .as_mut()
            .unwrap()
            .first_mut()
            .unwrap()
            .class = class;
        let image = capture_focused(&mut source).unwrap();
        let pixels = image::load_from_memory(image.png()).unwrap().to_rgba8();
        assert_eq!(pixels.get_pixel(0, 0).0, [0, 0, 0, 255]);
        assert_eq!(pixels.get_pixel(1, 0).0, [10, 20, 30, 255]);
        assert_eq!(source.reads, 1);
    }
}

#[test]
fn protection_and_incomplete_or_invalid_geometry_refuse_pixel_acquisition() {
    let mut protected = source();
    protected.frame.protected = true;
    let mut incomplete = source();
    incomplete.frame.regions = None;
    let mut outside = source();
    outside
        .frame
        .regions
        .as_mut()
        .unwrap()
        .first_mut()
        .unwrap()
        .bounds
        .x = u32::MAX;
    let mut public_outside = source();
    *public_outside
        .frame
        .regions
        .as_mut()
        .unwrap()
        .first_mut()
        .unwrap() = PaintRegion {
        bounds: PixelRect {
            x: 2,
            y: 0,
            width: 1,
            height: 1,
        },
        class: PaintClass::Public,
    };
    for mut source in [protected, incomplete, outside, public_outside] {
        assert!(matches!(
            capture_focused(&mut source),
            Err(Error::CaptureProtected)
        ));
        assert_eq!(source.reads, 0, "privacy inspection must precede readback");
    }
}

#[test]
fn focus_frame_or_classification_changes_and_readback_failures_return_no_image() {
    for field in 0..4 {
        let mut source = source();
        let mut after = source.frame.clone();
        match field {
            0 => after.surface = "synthetic:another-app".into(),
            1 => after.revision = after.revision.saturating_add(1),
            2 => after.protected = true,
            _ => after.regions.as_mut().unwrap().first_mut().unwrap().class = PaintClass::Public,
        }
        source.after = Some(after);
        assert!(matches!(
            capture_focused(&mut source),
            Err(Error::CaptureChanged)
        ));
        assert_eq!(source.reads, 1);
    }
    let mut failed = source();
    failed.fail_read = true;
    assert!(capture_focused(&mut failed).is_err());
    assert_eq!(failed.reads, 1);
}

#[test]
fn overlapping_geometry_cannot_trigger_unbounded_masking_or_readback() {
    let mut source = source();
    source.frame.width = 2000;
    source.frame.height = 2000;
    source.frame.regions = Some(vec![
        PaintRegion {
            bounds: PixelRect {
                x: 0,
                y: 0,
                width: 2000,
                height: 2000,
            },
            class: PaintClass::Unclassified,
        };
        3
    ]);
    assert!(matches!(
        capture_focused(&mut source),
        Err(Error::CaptureProtected)
    ));
    assert_eq!(source.reads, 0);
}
