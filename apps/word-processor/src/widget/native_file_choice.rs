//! Selected-file reconciliation preserves the admitted document graph.
use super::{
    Affine, AutoCommit, DocumentSaveStatus, Point, Rect, Scene, TextRenderer, Theme, WindowEvent,
    WordProcessorWidget, WpFormat, t,
};

#[cfg(test)]
pub(super) fn same_file_content(
    document: &AutoCommit,
    bytes: &[u8],
    format: WpFormat,
) -> Result<bool, String> {
    same_file_content_with(document, bytes, format, &tilo_content::NoContent)
}

/// Whether `bytes` project to the same document as `document` in `format`,
/// exporting the images `document` references from `content`.
pub(super) fn same_file_content_with(
    document: &AutoCommit,
    bytes: &[u8],
    format: WpFormat,
    content: &dyn tilo_content::ContentSource,
) -> Result<bool, String> {
    fn normalize(
        bytes: &[u8],
        format: WpFormat,
    ) -> Result<tilo_doc::snapshot::DocSnapshot, String> {
        let mut parsed = AutoCommit::new();
        let containers = tilo_doc::model::init(&mut parsed, &automerge::ROOT);
        let mut ids = tilo_doc::IdGen::new("selected-file-comparison".into());
        // Embedded content is compared by the address the document names.
        let mut inputs = tilo_content::ContentInputs::new();
        format
            .import(
                bytes,
                &mut tilo_doc::DocMut {
                    doc: &mut parsed,
                    c: &containers,
                    ids: &mut ids,
                },
                &mut inputs,
            )
            .map_err(|error| error.to_string())?;
        Ok(tilo_doc::model::materialize(&parsed, &containers))
    }
    let containers =
        tilo_doc::model::open(document, &automerge::ROOT).map_err(|error| error.to_string())?;
    let projected = format
        .export(
            tilo_doc::DocRef {
                doc: document,
                c: &containers,
            },
            content,
        )
        .map_err(|error| error.to_string())?;
    // Compare the selected format's complete projection, with deterministic
    // scratch IDs. Neither scratch graph is merged into the admitted document.
    Ok(normalize(&projected, format)? == normalize(bytes, format)?)
}

pub(super) struct FileChoice {
    pub(super) generation: u64,
    pub(super) continuation_domain: ui::widget::ActionDirectoryInvocationDomain,
    path: std::path::PathBuf,
    bytes: Vec<u8>,
    format: WpFormat,
    pub(super) resource: tilo_actions::Resource,
    emitters: [ui::widget::ActionEmitter<()>; 2],
    focus: usize,
    pointer: Option<Point>,
    layout: Option<ui::widget::modal_dialog::ModalLayout>,
}
impl WordProcessorWidget {
    pub fn retain_file_history(&mut self, reader: ui::record::FileHistoryReader) {
        self.file_history = Some(reader);
    }
    pub(crate) fn adopt_existing_file(
        &mut self,
        document: &AutoCommit,
        path: std::path::PathBuf,
        bytes: Vec<u8>,
        format: WpFormat,
    ) -> Result<(), String> {
        let same = same_file_content_with(document, &bytes, format, self.inner.content_store())?;
        self.set_file(path.clone(), format);
        if same {
            self.remember_open_document(document);
            self.check_for_recovery();
        } else {
            self.offer_file_choice(path, bytes, format)?;
        }
        Ok(())
    }
    pub(super) fn read_existing_file(
        &self,
        path: &std::path::Path,
    ) -> Result<Option<ui::record::InitialFileHistory>, String> {
        // Hosted, the framework's record is the document and a file is only
        // imported into it; there is no file record with a history of its own.
        if self.hosted() {
            return Ok(None);
        }
        let want = ui::RecordCheckout::new(
            ui::RecordId::for_file(path),
            ui::RecordKind::new(crate::RECORD_KIND),
        );
        if let Some(reader) = &self.file_history {
            return reader.read(&want);
        }
        // Older locally constructed widgets still resolve the actual mode.
        // An inherited service without its construction bridge fails closed.
        ui::RecordSet::for_local_files("so.tilo.word-processor")?.initial_file_history(&want)
    }

    pub(crate) fn file_choice_resource(&self) -> Option<tilo_actions::Resource> {
        self.file_choice
            .as_ref()
            .map(|offer| offer.resource.clone())
    }

    pub(crate) fn offer_file_choice(
        &mut self,
        path: std::path::PathBuf,
        bytes: Vec<u8>,
        format: WpFormat,
    ) -> Result<u64, String> {
        if self.file_choice.is_some() {
            return Err("a selected file choice is already pending".into());
        }
        let generation = self
            .file_choice_generation
            .checked_add(1)
            .ok_or("file choice generation exhausted")?;
        let mut resource = crate::actions::file_choice_resource(generation)?;
        let continuation_domain = crate::actions::continuation::bind_file_choice(
            &mut resource,
            generation,
            &path,
            &bytes,
            format,
        );
        let mut emitters = [
            ui::widget::ActionEmitter::new(
                tilo_actions::ActionBinding::unit_for(&resource, "keep-workspace")
                    .map_err(|e| e.to_string())?,
            ),
            ui::widget::ActionEmitter::new(
                tilo_actions::ActionBinding::unit_for(&resource, "use-selected")
                    .map_err(|e| e.to_string())?,
            ),
        ];
        if let Some(domain) = &self.file_choice_domain {
            for emitter in &mut emitters {
                emitter
                    .install_custody(domain, ui::control_custody::CustodyAttachment::Dynamic)
                    .map_err(|e| format!("file choice custody unavailable: {e:?}"))?;
            }
        }
        self.file_choice_generation = generation;
        self.file_choice = Some(FileChoice {
            generation,
            continuation_domain,
            path,
            bytes,
            format,
            resource,
            emitters,
            focus: 0,
            pointer: None,
            layout: None,
        });
        self.saved_document = None;
        self.save_status = DocumentSaveStatus::Modified;
        self.lifecycle.dirty = true;
        Ok(generation)
    }
    pub(super) fn resolve_file_choice(
        &mut self,
        state: &mut AutoCommit,
        generation: u64,
        use_selected: bool,
    ) -> Result<(), String> {
        self.resolve_file_choice_for_job(state, generation, use_selected, None)
    }
    pub(super) fn resolve_file_choice_for_job(
        &mut self,
        state: &mut AutoCommit,
        generation: u64,
        use_selected: bool,
        scope: Option<&ui::invocation_scope::ResourceInvocationScope>,
    ) -> Result<(), String> {
        let offer = self
            .file_choice
            .as_ref()
            .filter(|offer| offer.generation == generation)
            .ok_or("selected file choice is no longer current")?;
        let selected =
            use_selected.then(|| (offer.bytes.clone(), offer.format, offer.path.clone()));
        if let Some((bytes, format, path)) = selected {
            let Some(action) = self.inner.stage_replacement(state, &bytes, format) else {
                return Err(format!("could not parse {}", path.display()));
            };
            if !self.try_perform_action(state, &action) {
                return Err(format!("could not parse {}", path.display()));
            }
            self.inner.seat_after_replacement(state);
            self.remember_saved_document(state);
            self.save_status = DocumentSaveStatus::Clean;
            self.lifecycle.needs_commit = true;
        }
        self.file_choice = None;
        if let Some(scope) = scope {
            scope.check()?;
        }
        self.check_for_recovery();
        self.refresh_blocks(state);
        self.layout_cache.clear();
        self.recent_layout_cache.clear();
        self.spell_cache.clear();
        self.lifecycle.dirty = true;
        Ok(())
    }
}

impl FileChoice {
    pub(super) fn install_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        for emitter in &mut self.emitters {
            let _ = emitter.install_custody(domain, attachment);
        }
    }
    pub(super) fn take(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.emitters
            .iter_mut()
            .find_map(ui::widget::ActionEmitter::take)
    }
    pub(super) fn refresh(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        for emitter in &mut self.emitters {
            emitter.refresh_availability(allowed);
        }
    }
    pub(super) fn bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        self.emitters
            .iter()
            .enumerate()
            .map(|(i, e)| e.metadata(format!("file-choice:{i}")))
            .collect()
    }
    fn emit(&mut self, index: usize) {
        if let Some(emitter) = self.emitters.get_mut(index) {
            let _ = emitter.emit(());
        }
    }
    pub(super) fn event(&mut self, event: &WindowEvent, scale: f64) {
        use ui::widget::modal_dialog::ModalHit;
        use winit::event::{ElementState, MouseButton};
        use winit::keyboard::{Key, NamedKey};
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some(Point::new(position.x / scale, position.y / scale));
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(ModalHit::Action(i)) =
                    self.pointer.and_then(|p| self.layout.as_ref()?.hit(p))
                {
                    self.emit(i);
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match &event.logical_key {
                    Key::Named(NamedKey::Tab | NamedKey::ArrowLeft | NamedKey::ArrowRight) => {
                        self.focus = 1 - self.focus;
                    }
                    Key::Named(NamedKey::Enter | NamedKey::Space) => self.emit(self.focus),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
    ) {
        use ui::widget::modal_dialog::{self, ModalAction, ModalSpec};
        let title = t("file-choice-title");
        let body = t("file-choice-body");
        let labels = [t("file-choice-keep"), t("file-choice-use")];
        let mut actions = [
            ModalAction::primary(&labels[0]),
            ModalAction::secondary(&labels[1]),
        ];
        for (i, action) in actions.iter_mut().enumerate() {
            action.focused = self.focus == i;
            action.enabled = self
                .emitters
                .get(i)
                .is_some_and(ui::widget::ActionEmitter::is_available);
        }
        let spec = ModalSpec::new(&title).closable(false).actions(&actions);
        let preliminary = modal_dialog::layout(viewport, &spec, tr, theme);
        let width = crate::num::f32_of_f64(preliminary.body.width().max(1.0));
        let style = theme.text(ui::tokens::TextRole::Body);
        let (_, height) = tr.measure_style(&body, Some(width), &style);
        let spec = spec.body_height(f64::from(height));
        let layout = modal_dialog::layout(viewport, &spec, tr, theme);
        modal_dialog::paint_backdrop(scene, transform, tr, theme, &layout, &spec);
        tr.draw_style(
            scene,
            &body,
            modal_dialog::ink(theme).0,
            transform * Affine::translate((layout.body.x0, layout.body.y0)),
            Some(width),
            &style,
        );
        modal_dialog::paint_actions(scene, transform, tr, theme, &layout, &spec);
        self.layout = Some(layout);
    }
    pub(super) fn accessibility(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut children = Vec::new();
        for (i, label) in ["file-choice-keep", "file-choice-use"].iter().enumerate() {
            let mut node = AccessNode::new(Role::Button)
                .with_name(t(label))
                .with_action_target(format!("file-choice:{i}"))
                .with_state(AccessState {
                    focusable: true,
                    focused: self.focus == i,
                    ..Default::default()
                });
            if let Some(rect) = self.layout.as_ref().and_then(|l| l.actions.get(i)) {
                node = node.with_bounds(*rect);
            }
            children.push(node);
        }
        let mut node = AccessNode::new(Role::Dialog)
            .with_name(t("file-choice-title"))
            .with_description(t("file-choice-body"))
            .with_state(AccessState {
                modal: true,
                ..Default::default()
            })
            .with_children(children);
        if let Some(layout) = &self.layout {
            node = node.with_bounds(layout.card);
        }
        node
    }
    pub(super) fn accessible_action(&mut self, action: &ui::a11y::Action, keys: &[u64]) {
        if let Some(index) =
            (0..2).find(|i| keys.last() == Some(&ui::a11y::hash_key(&format!("file-choice:{i}"))))
        {
            if matches!(action, ui::a11y::Action::Activate) {
                self.emit(index);
            } else if matches!(action, ui::a11y::Action::Focus) {
                self.focus = index;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{ReadDoc, Widget};
    fn markdown(bytes: &[u8]) -> AutoCommit {
        let mut document = AutoCommit::new();
        let containers = tilo_doc::model::init(&mut document, &automerge::ROOT);
        let mut ids = tilo_doc::IdGen::new("original-file-history".into());
        WpFormat::Markdown
            .import(
                bytes,
                &mut tilo_doc::DocMut {
                    doc: &mut document,
                    c: &containers,
                    ids: &mut ids,
                },
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        document.commit();
        document
    }
    #[test]
    fn native_file_choice_keep_preserves_history_and_stale_choice_retains_file() {
        let mut document = markdown(b"Workspace version.\n");
        let mut widget = WordProcessorWidget::new();
        widget.sync(&mut document, &automerge::ROOT);
        document.commit();
        let heads = document.get_heads();
        let generation = widget
            .offer_file_choice(
                std::path::PathBuf::from("chosen.md"),
                b"Selected version.\n".to_vec(),
                WpFormat::Markdown,
            )
            .unwrap();
        assert!(
            widget
                .resolve_file_choice(&mut document, generation + 1, true)
                .is_err()
        );
        assert!(widget.file_choice.is_some());
        widget
            .resolve_file_choice(&mut document, generation, false)
            .unwrap();
        assert!(widget.file_choice.is_none());
        assert_eq!(document.get_heads(), heads);
        assert!(same_file_content(&document, b"Workspace version.\n", WpFormat::Markdown).unwrap());
    }
    #[test]
    fn native_file_choice_import_adds_operations_to_original_history() {
        let mut document = markdown(b"Workspace version.\n");
        let mut widget = WordProcessorWidget::new();
        widget.sync(&mut document, &automerge::ROOT);
        document.commit();
        let heads = document.get_heads();
        let generation = widget
            .offer_file_choice(
                std::path::PathBuf::from("chosen.md"),
                b"Selected version.\n".to_vec(),
                WpFormat::Markdown,
            )
            .unwrap();
        widget
            .resolve_file_choice(&mut document, generation, true)
            .unwrap();
        document.commit();
        assert!(same_file_content(&document, b"Selected version.\n", WpFormat::Markdown).unwrap());
        for head in heads {
            assert!(document.get_change_by_hash(&head).is_some());
        }
        assert!(widget.file_choice.is_none());
    }
    #[test]
    fn native_file_choice_semantic_controls_emit_the_catalogued_operation() {
        let mut document = markdown(b"Workspace version.\n");
        let mut widget = WordProcessorWidget::new();
        widget.sync(&mut document, &automerge::ROOT);
        let generation = widget
            .offer_file_choice(
                std::path::PathBuf::from("selected.md"),
                b"Selected.\n".to_vec(),
                WpFormat::Markdown,
            )
            .unwrap();
        let key = ui::a11y::hash_key(&"file-choice:1");
        assert!(widget.perform_action_at(
            &ui::a11y::Action::Activate,
            &[key],
            &mut document,
            &automerge::ROOT
        ));
        let invocation = ui::widget::controls::take_control_invocation(&mut widget)
            .expect("ordinary action invocation");
        assert_eq!(
            invocation.address.resource,
            format!("word-processor.file-choice:{generation}")
        );
        assert_eq!(invocation.address.action, "use-selected");
        assert!(
            widget.file_choice.is_some(),
            "the control itself does not mutate or discard the offer"
        );
        assert!(same_file_content(&document, b"Workspace version.\n", WpFormat::Markdown).unwrap());
    }
    #[test]
    fn native_file_choice_action_completes_through_mutable_sync_and_rejects_replay() {
        use automerge::transaction::Transactable;
        let mut document = markdown(b"Workspace version.\n");
        let mut widget = WordProcessorWidget::new();
        widget.sync(&mut document, &automerge::ROOT);
        let generation = widget
            .offer_file_choice(
                std::path::PathBuf::from("selected.md"),
                b"Selected.\n".to_vec(),
                WpFormat::Markdown,
            )
            .unwrap();
        // A newer admitted edit remains in the graph while the choice is open.
        document
            .put(automerge::ROOT, "retained_later_edit", "preserved")
            .unwrap();
        document.commit();
        let heads = document.get_heads();
        let invocation = tilo_actions::InvocationId::new();
        let payload =
            serde_json::to_vec(&crate::actions::WordProcessorOperation::ResolveFileChoice {
                generation,
                use_selected: true,
            })
            .unwrap();
        assert!(matches!(
            widget
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::WORD_PROCESSOR_OPERATION_TYPE,
                        &payload
                    ),
                    &mut document,
                    &automerge::ROOT
                )
                .unwrap(),
            Some(ui::widget::ResourceOperationStart::Accepted)
        ));
        assert!(widget.file_choice.is_some());
        widget.sync(&mut document, &automerge::ROOT);
        assert!(matches!(
            widget.poll_resource_operation(invocation, false, &document, &automerge::ROOT),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        assert!(same_file_content(&document, b"Selected.\n", WpFormat::Markdown).unwrap());
        for head in heads {
            assert!(document.get_change_by_hash(&head).is_some());
        }
        assert_eq!(
            document
                .get(automerge::ROOT, "retained_later_edit")
                .unwrap()
                .unwrap()
                .0
                .as_str(),
            Some("preserved")
        );
        assert!(
            widget
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        crate::actions::WORD_PROCESSOR_OPERATION_TYPE,
                        &payload
                    ),
                    &mut document,
                    &automerge::ROOT
                )
                .is_err()
        );
    }
    #[test]
    fn native_file_choice_failed_import_retains_exact_selection_and_history() {
        let mut document = markdown(b"Workspace version.\n");
        let mut widget = WordProcessorWidget::new();
        widget.sync(&mut document, &automerge::ROOT);
        document.commit();
        let heads = document.get_heads();
        let bytes = b"not an ODT archive".to_vec();
        let generation = widget
            .offer_file_choice(
                std::path::PathBuf::from("selected.odt"),
                bytes.clone(),
                WpFormat::Odt,
            )
            .unwrap();
        assert!(
            widget
                .resolve_file_choice(&mut document, generation, true)
                .is_err()
        );
        assert_eq!(widget.file_choice.as_ref().unwrap().bytes, bytes);
        assert_eq!(document.get_heads(), heads);
    }
    #[test]
    fn native_file_choice_uses_typed_ordinary_operation() {
        let payload = br#"{"ResolveFileChoice":{"generation":7,"use_selected":true}}"#;
        let operation = crate::actions::decode_operation(
            crate::actions::WORD_PROCESSOR_OPERATION_TYPE,
            payload,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            serde_json::to_value(operation).unwrap(),
            serde_json::from_slice::<serde_json::Value>(payload).unwrap()
        );
    }
    #[test]
    fn native_file_choice_equal_projection_does_not_replace_history() {
        let mut document = markdown(b"# Original\n\nRetained **format**.\n");
        let heads = document.get_heads();
        assert!(
            same_file_content(
                &document,
                b"# Original\n\nRetained **format**.\n",
                WpFormat::Markdown
            )
            .unwrap()
        );
        assert_eq!(document.get_heads(), heads);
    }
    #[test]
    fn native_file_choice_different_selected_content_is_explicit() {
        let document = markdown(b"Workspace **version**.\n");
        assert!(
            !same_file_content(
                &document,
                b"Selected different version.\n",
                WpFormat::Markdown
            )
            .unwrap()
        );
    }
}
