//! The guided journey as its tests drive it: the setup window opened on one page, on a state of its
//! own, drawn headless, and the readings of what it drew.
#![cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "the walks that open the setup window run on Unix hosts"
    )
)]

use super::*;
use crate::hosting::testing::TestHost;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;

pub(super) use tilo_ui_base::widget::find_test_id as control;

/// The setup window on an XDG state of its own, for as long as the walk runs.
pub(super) struct Walk {
    app: TestHost,
    _environment: tilo_test_env::ScopedEnv,
    _state: tempfile::TempDir,
}

impl Deref for Walk {
    type Target = TestHost;

    fn deref(&self) -> &TestHost {
        &self.app
    }
}

impl DerefMut for Walk {
    fn deref_mut(&mut self) -> &mut TestHost {
        &mut self.app
    }
}

/// An XDG state the setup writes into, on this test's own temporary directory.
pub(super) fn private_state() -> (tempfile::TempDir, tilo_test_env::ScopedEnv) {
    let root = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    for key in [
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
    ] {
        environment.set(key, root.path().join(key));
    }
    (root, environment)
}

impl Walk {
    /// The setup window as it opens, before the person chose anything.
    pub(super) fn launched() -> Self {
        Self::launched_with(&[])
    }

    /// [`Self::launched`] with `variables` also set for the walk.
    pub(super) fn launched_with(variables: &[(&str, &str)]) -> Self {
        let (state, mut environment) = private_state();
        for (key, value) in variables {
            environment.set(key, value);
        }
        Self {
            app: TestHost::launched(&["--setup"]),
            _environment: environment,
            _state: state,
        }
    }

    /// The setup window, offline, on `page` of an install onto this computer, at `viewport` in
    /// `theme` with motion reduced.
    pub(super) fn at(page: Page, viewport: (u32, u32), theme: Theme) -> Self {
        let mut walk = Self::launched();
        walk.edit(|preview| {
            preview.live = false;
            preview.capture = false;
            preview.journey = Journey::new();
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.device_name = "My computer".into();
            preview.journey.page = page;
        });
        walk.harness().resize(viewport).unwrap();
        walk.harness().set_theme(theme.with_reduce_motion(true));
        walk.settle();
        walk
    }

    /// The page as `edit` leaves it, marked for a rebuild.
    pub(super) fn edit(&mut self, edit: impl FnOnce(&mut InstallerPreview)) {
        self.with_widget::<InstallerPreview, _>(|preview| {
            edit(preview);
            preview.dirty = true;
        })
        .unwrap();
    }

    /// What `look` reads from the page.
    pub(super) fn view<O>(&mut self, look: impl FnOnce(&mut InstallerPreview) -> O) -> O {
        self.with_widget::<InstallerPreview, _>(look).unwrap()
    }

    /// Each of `effects`, invoked as a caller invokes it; the first refusal fails the test.
    pub(super) fn act(&mut self, effects: impl IntoIterator<Item = InstallerPreviewEffect>) {
        for effect in effects {
            self.effect(&effect)
                .unwrap_or_else(|error| panic!("{} failed: {error:?}", effect.id()));
        }
    }

    /// What the page publishes as its state.
    pub(super) fn read(&mut self) -> serde_json::Value {
        self.effect(&InstallerPreviewEffect::ReadPreview)
            .unwrap()
            .value
            .unwrap()
    }

    /// Where `id` is drawn.
    pub(super) fn rect(&mut self, id: &str) -> vello::kurbo::Rect {
        self.view(|preview| {
            control(preview, id)
                .unwrap_or_else(|| panic!("missing {id}"))
                .get_rect()
        })
    }

    /// Keyboard focus on `id`, as assistive technology moves it.
    pub(super) fn focus(&mut self, id: &str) {
        self.harness()
            .perform_named(id, &tilo_ui_base::a11y::Action::Focus)
            .unwrap();
        self.settle();
    }

    /// A pointer click in the middle of `id`.
    pub(super) fn click(&mut self, id: &str) {
        let point = self.rect(id).center();
        self.harness().click_at(point.x, point.y);
        self.settle();
    }
}

/// Each of `ids` is drawn large enough to use and inside `viewport`; the password fields share one
/// column without overlapping.
pub(super) fn inspect_controls(preview: &InstallerPreview, ids: &[&str], viewport: (u32, u32)) {
    for id in ids {
        let rect = control(preview, id)
            .unwrap_or_else(|| panic!("missing {id}"))
            .get_rect();
        assert!(
            rect.width() >= 24.0 && rect.height() >= 20.0,
            "{id}: {rect:?}"
        );
        assert!(
            rect.x0 >= 0.0 && rect.x1 <= f64::from(viewport.0),
            "horizontal clipping {id}: {rect:?}"
        );
        assert!(
            rect.y0 >= 0.0 && rect.y1 <= f64::from(viewport.1),
            "vertical clipping {id}: {rect:?}"
        );
    }
    if preview.journey.page == Page::Password {
        let first = control(preview, "installer.password").unwrap().get_rect();
        let second = control(preview, "installer.confirmation")
            .unwrap()
            .get_rect();
        assert!((first.x0 - second.x0).abs() < 1.0);
        assert!((first.x1 - second.x1).abs() < 1.0);
        assert!(second.y0 >= first.y1, "password fields must not overlap");
    }
}

/// The page, settled, rasterized and painted, with `ids` usable; a capture of it when
/// `TILO_UI_SCREENSHOT_DIR` asks for one.
pub(super) fn paint_app(
    app: &mut TestHost,
    scenario: &str,
    ids: &[&str],
    viewport: (u32, u32),
    tag: &str,
) {
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| inspect_controls(preview, ids, viewport))
        .unwrap();
    let pixels = app.harness().render_rgba();
    let expected = usize::try_from(viewport.0)
        .unwrap()
        .checked_mul(usize::try_from(viewport.1).unwrap())
        .unwrap()
        .checked_mul(4)
        .unwrap();
    assert_eq!(
        pixels.len(),
        expected,
        "the actual scene must be rasterized"
    );
    let ground = pixels.get(..4).unwrap();
    let painted = pixels
        .chunks_exact(4)
        .filter(|pixel| *pixel != ground)
        .count();
    assert!(painted > 200, "{scenario}/{tag}: blank or unpainted frame");
    if let Some(directory) = std::env::var_os("TILO_UI_SCREENSHOT_DIR") {
        let directory = PathBuf::from(directory).join("installer-journey");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!(
            "{scenario}-{tag}-{}x{}.png",
            viewport.0, viewport.1
        ));
        app.harness().render_png(&path);
        eprintln!("installer visual evidence: {}", path.display());
    }
}

/// The scroll view holding the page's body: its visible rectangle and whether its content overflows.
pub(super) fn body_scroll_geometry(root: &mut dyn Widget) -> Option<(vello::kurbo::Rect, bool)> {
    if let Some(scroll) = root
        .as_any_mut()
        .and_then(|widget| widget.downcast_mut::<ui::widget::Scrollable>())
    {
        return Some((scroll.content_viewport_rect(), scroll.vertical_overflows()));
    }
    root.children_mut()
        .into_iter()
        .find_map(body_scroll_geometry)
}
