use shell::compositor::frame::FrameBuilder;

pub(crate) struct PopulatedShellFixture {
    frame: FrameBuilder,
    /// Never read: the owned parent must outlive and drop after the fixture
    /// frame, so it stays declared after `frame`.
    _parent: tempfile::TempDir,
}

impl PopulatedShellFixture {
    pub(crate) fn new(viewport: (u32, u32), theme: ui::theme::Theme) -> Result<Self, String> {
        let parent = tempfile::tempdir()
            .map_err(|error| format!("private populated shell fixture parent: {error}"))?;
        let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
            ui::font_catalog::checkout_root(),
            "en-US",
        );
        let renderer = ui::text_render::TextRenderer::with_font_catalog(&catalog)
            .map_err(|error| format!("controlled populated shell fonts: {error}"))?;
        let inputs =
            shell::shell::fixture_inputs::ShellFixtureInputs::new(parent.path(), renderer, theme)
                .map_err(|error| format!("owned populated shell fixture files: {error}"))?;
        let frame = FrameBuilder::with_fixture_inputs(
            shell::shell::state::WorkspaceTree::bootstrap_demo(),
            viewport,
            inputs,
        )
        .map_err(|error| format!("explicit populated shell fixture: {error}"))?;
        Ok(Self {
            frame,
            _parent: parent,
        })
    }
}

impl std::ops::Deref for PopulatedShellFixture {
    type Target = FrameBuilder;

    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}

impl std::ops::DerefMut for PopulatedShellFixture {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.frame
    }
}
