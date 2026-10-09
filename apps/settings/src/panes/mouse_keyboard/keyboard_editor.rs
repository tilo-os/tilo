//! Configured sources are the selection model; discovery only supplies edit scopes.

use super::*;
use settings_store::keyboard_sources::{ALL, KeyboardProfile, OSK, SourceOptions};
use ui::reorder::DropPlacement;
use ui::widget::{ListRow, VirtualList};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SourceView {
    #[default]
    Native,
    Configured,
    Live,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EditorState {
    pub scope: String,
    pub selected: Option<String>,
    pub adding: bool,
    pub pending: Option<String>,
    pub options_open: bool,
    pub inspection_open: bool,
    pub view: SourceView,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            scope: ALL.into(),
            selected: None,
            adding: false,
            pending: None,
            options_open: false,
            inspection_open: false,
            view: SourceView::Native,
        }
    }
}

impl InputArea {
    fn note_legacy_source_view(&self, operation: &InputEffect) {
        let view = match operation {
            InputEffect::ShowLiveKeyboardMap => SourceView::Live,
            InputEffect::PreviewLayout { .. } => SourceView::Configured,
            _ => return,
        };
        let mut editor = self.editor.get();
        editor.view = view;
        editor.selected = None;
        editor.adding = false;
        editor.pending = None;
        self.editor.set(editor);
    }

    pub(super) fn keyboard_scopes(&self) -> Vec<(String, String)> {
        scopes(self.devices().as_ref(), self.osk_always)
    }

    fn profile(&self, input: &InputPrefs, scope: &str) -> KeyboardProfile {
        if let Some(profile) = input
            .keyboard_assignments
            .get(scope)
            .filter(|_| scope != OSK && scope != ALL)
        {
            return profile.clone();
        }
        if scope == OSK {
            return osk_profile(&self.document);
        }
        let mut profile = KeyboardProfile::from_input(&input.rich_input());
        if let Some(defaults) = input.keyboard_assignments.get(ALL) {
            profile.options.clone_from(&defaults.options);
        }
        profile
    }
}

pub(super) fn scopes(
    snapshot: Option<&InputHardwareSnapshot>,
    osk_always: bool,
) -> Vec<(String, String)> {
    let Some(snapshot) = snapshot else {
        return vec![(ALL.into(), t("input-scope-all"))];
    };
    let mut keyboards: Vec<_> = snapshot
        .of_kind(InputDeviceKind::Keyboard)
        .into_iter()
        .map(|device| (device.key.clone(), device_title(device)))
        .collect();
    keyboards.sort_by(|a, b| a.0.cmp(&b.0));
    keyboards.dedup_by(|a, b| a.0 == b.0);
    let mut totals = HashMap::new();
    for (_, label) in &keyboards {
        *totals.entry(label.clone()).or_insert(0usize) += 1;
    }
    let mut ordinals = HashMap::new();
    for (_, label) in &mut keyboards {
        if totals.get(label).is_some_and(|total| *total > 1) {
            let ordinal = ordinals.entry(label.clone()).or_insert(0i64);
            *ordinal += 1;
            *label = t_args(
                "input-keyboard-numbered",
                &[
                    ("name", ArgValue::from(label.clone())),
                    ("number", ArgValue::from(*ordinal)),
                ],
            );
        }
    }
    let osk = osk_always || snapshot.has(InputDeviceKind::Touchscreen);
    if keyboards.is_empty() && osk {
        return vec![(OSK.into(), t("a11y-osk"))];
    }
    if !osk && keyboards.len() == 1 {
        return keyboards;
    }
    if !osk && keyboards.is_empty() {
        return vec![(ALL.into(), t("input-scope-all"))];
    }
    let mut result = vec![(ALL.into(), t("input-scope-all"))];
    result.extend(keyboards);
    if osk {
        result.push((OSK.into(), t("a11y-osk")));
    }
    result
}

pub(super) fn answer_editor(app: &SettingsApp, operation: &InputEffect) -> Option<EffectAnswer> {
    let area = app.input();
    if matches!(operation, InputEffect::InspectKeyboardSources) {
        return Some(Ok(inspect_sources(app)));
    }
    let mut editor = area.editor.get();
    let input = app.host().settings::<InputPrefs>().get();
    let profile = area.profile(&input, &editor.scope);
    if !matches!(
        operation,
        InputEffect::SelectKeyboardScope { .. } | InputEffect::CloseSourcePicker
    ) && !area
        .keyboard_scopes()
        .iter()
        .any(|(scope, _)| scope == &editor.scope)
    {
        return Some(Err(t("input-error-keyboard-retained")));
    }
    if editor.scope == OSK
        && matches!(
            operation,
            InputEffect::AddSource { .. }
                | InputEffect::RemoveSource { .. }
                | InputEffect::MoveSourceUp { .. }
                | InputEffect::MoveSourceDown { .. }
                | InputEffect::ReorderSource { .. }
                | InputEffect::SetActiveSource { .. }
        )
    {
        let mut profile = profile;
        if let Err(error) = mutate_sources(&mut profile, operation.clone(), area, OSK) {
            return Some(Err(error));
        }
        return Some(store_osk(&area.document, &profile));
    }
    let result = match operation {
        InputEffect::SelectKeyboardScope { scope } => {
            if !area.keyboard_scopes().iter().any(|(id, _)| id == scope) {
                return Some(Err(t("input-error-keyboard-unavailable")));
            }
            editor.scope.clone_from(scope);
            editor.view = SourceView::Configured;
            editor.selected = None;
            editor.adding = false;
            editor.pending = None;
            Ok(Effected::done())
        }
        InputEffect::SelectConfiguredSource { source } => {
            if !profile.sources.contains(source) {
                return Some(Err(t("input-error-source-unconfigured")));
            }
            editor.selected = Some(source.clone());
            editor.view = SourceView::Configured;
            Ok(Effected::done())
        }
        InputEffect::SetSourceOptionsVisible { visible } => {
            editor.options_open = *visible;
            Ok(Effected::done())
        }
        InputEffect::SetSourceInspectionVisible { visible } => {
            editor.inspection_open = *visible;
            if editor.view == SourceView::Native {
                editor.view = SourceView::Live;
            }
            Ok(Effected::done())
        }
        InputEffect::OpenSourcePicker => {
            editor.view = SourceView::Configured;
            editor.adding = true;
            editor.pending = None;
            Ok(Effected::done())
        }
        InputEffect::CloseSourcePicker => {
            editor.adding = false;
            editor.pending = None;
            Ok(Effected::done())
        }
        InputEffect::ChooseSourceToAdd { source } => {
            if !editor.adding
                || !available(area, &profile, &editor.scope)
                    .iter()
                    .any(|entry| &entry.id == source)
            {
                return Some(Err(t("input-error-source-add-unavailable")));
            }
            editor.pending = Some(source.clone());
            Ok(Effected::done())
        }
        InputEffect::ConfirmSourceToAdd => confirm_added_source(app, &mut editor, input, profile),
        _ => return None,
    };
    area.editor.set(editor);
    Some(result)
}

fn inspect_sources(app: &SettingsApp) -> Effected {
    let area = app.input();
    let input = app.host().settings::<InputPrefs>().get();
    let scopes: Vec<_> = area
        .keyboard_scopes()
        .into_iter()
        .map(|(id, label)| {
            let profile = area.profile(&input, &id);
            serde_json::json!({"id": id, "label": label, "sources": profile.sources, "active": profile.active})
        })
        .collect();
    let editor = area.editor.get();
    Effected::value(
        serde_json::json!({"scopes": scopes, "editing": editor.scope, "selected": editor.selected, "adding": editor.adding}),
    )
}

fn confirm_added_source(
    app: &SettingsApp,
    editor: &mut EditorState,
    input: InputPrefs,
    mut profile: KeyboardProfile,
) -> EffectAnswer {
    let area = app.input();
    let source = editor
        .pending
        .clone()
        .filter(|_| editor.adding)
        .ok_or_else(|| t("input-error-source-choose"))?;
    let active = profile.active.clone();
    if editor.scope == OSK {
        mutate_sources(
            &mut profile,
            InputEffect::AddSource {
                source: source.clone(),
            },
            area,
            OSK,
        )?;
        profile.active = active;
        store_osk(&area.document, &profile)?;
    } else {
        let mut updated = input;
        apply_scoped(
            &mut updated,
            InputEffect::AddSource {
                source: source.clone(),
            },
            area,
        )?;
        if editor.scope == ALL {
            updated.keyboard_layout.clone_from(&active);
        }
        if let Some(profile) = updated.keyboard_assignments.get_mut(&editor.scope) {
            profile.active = active;
        }
        app.host()
            .settings::<InputPrefs>()
            .set(&updated)
            .map_err(|error| error.to_string())?;
    }
    editor.selected = Some(source);
    editor.adding = false;
    editor.pending = None;
    Ok(Effected::done())
}

fn available(area: &InputArea, profile: &KeyboardProfile, scope: &str) -> Vec<CatalogEntry> {
    let catalog = if scope == OSK {
        vec![
            CatalogEntry {
                id: "qwerty".into(),
                label: t("input-osk-qwerty"),
            },
            CatalogEntry {
                id: "azerty".into(),
                label: t("input-osk-azerty"),
            },
        ]
    } else if area.catalog_error.is_none() {
        area.catalog.clone()
    } else {
        Vec::new()
    };
    catalog
        .into_iter()
        .filter(|entry| {
            !profile.sources.contains(&entry.id)
                && (scope == OSK || crate::input_sources::has_room_for(&profile.sources, &entry.id))
        })
        .collect()
}

pub(super) fn apply_scoped(
    input: &mut InputPrefs,
    operation: InputEffect,
    area: &InputArea,
) -> Result<(), String> {
    let editor = area.editor.get();
    if !area
        .keyboard_scopes()
        .iter()
        .any(|(scope, _)| scope == &editor.scope)
    {
        return Err(t("input-error-keyboard-retained"));
    }
    if editor.scope == OSK && matches!(operation, InputEffect::ShowLiveKeyboardMap) {
        return Err(t("input-error-live-map-physical"));
    }
    area.note_legacy_source_view(&operation);
    // Preserve explicit legacy automation requests; the new screen always edits
    // a configured selection and never creates this independent preview state.
    if editor.selected.is_none()
        && !editor.adding
        && input.keyboard_preview.is_some()
        && matches!(operation, InputEffect::SetKeyboardModel { .. })
    {
        return apply(input, operation, area.catalog());
    }
    let source_action = matches!(
        operation,
        InputEffect::SetActiveSource { .. }
            | InputEffect::AddSource { .. }
            | InputEffect::MoveSourceUp { .. }
            | InputEffect::MoveSourceDown { .. }
            | InputEffect::RemoveSource { .. }
            | InputEffect::ReorderSource { .. }
            | InputEffect::SetComposeKey { .. }
            | InputEffect::SetCapsLock { .. }
            | InputEffect::SetKeyboardModel { .. }
    );
    if !source_action {
        if matches!(
            operation,
            InputEffect::SetPreviewLevel { .. }
                | InputEffect::SetPreviewExtraLevels { .. }
                | InputEffect::InspectKeyboardKey { .. }
        ) {
            return apply_selected_inspection(input, operation, area, &editor);
        }
        return apply(input, operation, area.catalog());
    }
    let mut profile = area.profile(input, &editor.scope);
    let selected = editor
        .selected
        .as_ref()
        .filter(|id| profile.sources.contains(id))
        .cloned()
        .unwrap_or_else(|| profile.active.clone());
    if matches!(
        operation,
        InputEffect::SetComposeKey { .. }
            | InputEffect::SetCapsLock { .. }
            | InputEffect::SetKeyboardModel { .. }
    ) {
        if editor.scope == OSK {
            return Err(t("input-error-source-physical-options"));
        }
        apply_source_options(input, &mut profile, selected, operation)?;
    } else {
        mutate_sources(&mut profile, operation, area, &editor.scope)?;
    }
    if editor.scope == ALL {
        input.keyboard_layouts.clone_from(&profile.sources);
        input.keyboard_layout.clone_from(&profile.active);
    }
    input.keyboard_assignments.insert(editor.scope, profile);
    Ok(())
}

fn apply_source_options(
    input: &mut InputPrefs,
    profile: &mut KeyboardProfile,
    selected: String,
    operation: InputEffect,
) -> Result<(), String> {
    let current = input.rich_input();
    let options = profile
        .options
        .entry(selected)
        .or_insert_with(|| SourceOptions {
            model: Some(current.keyboard_model),
            compose_key: current.compose_key,
            caps_lock: current.caps_lock,
            customised: true,
        });
    options.customised = true;
    match operation {
        InputEffect::SetComposeKey { key } => {
            options.compose_key = stored_for(COMPOSE_KEY_CHOICES, COMPOSE_DISABLED, &key)
                .ok_or_else(|| t("input-error-source-compose"))?;
        }
        InputEffect::SetCapsLock { behaviour } => {
            options.caps_lock = stored_for(&CAPS_LOCK_CHOICES, CAPS_LOCK_UNCHANGED, &behaviour)
                .ok_or_else(|| t("input-error-source-caps"))?;
        }
        InputEffect::SetKeyboardModel { model } if crate::keyboard_preview::valid_model(&model) => {
            options.model = Some(model);
            input.keyboard_inspected_key = "AD01".into();
            input.keyboard_preview_level = 0;
        }
        _ => return Err(t("input-error-source-option")),
    }
    Ok(())
}

fn apply_selected_inspection(
    input: &mut InputPrefs,
    operation: InputEffect,
    area: &InputArea,
    editor: &EditorState,
) -> Result<(), String> {
    let profile = area.profile(input, &editor.scope);
    let selected = editor
        .pending
        .as_ref()
        .filter(|_| editor.adding)
        .or(editor.selected.as_ref())
        .or(input.keyboard_preview.as_ref())
        .filter(|source| {
            profile.sources.contains(source)
                || editor.adding
                || (editor.selected.is_none() && input.keyboard_preview.as_ref() == Some(source))
        })
        .unwrap_or(&profile.active);
    let mut selected_profile = profile.clone();
    selected_profile.active.clone_from(selected);
    let effective = selected_profile.effective(&input.rich_input());
    let mut preview = input.clone();
    preview.keyboard_preview = Some(selected.clone());
    preview.keyboard_model = effective.keyboard_model;
    apply(&mut preview, operation, area.catalog())?;
    input.keyboard_preview_level = preview.keyboard_preview_level;
    input.keyboard_preview_extra_levels = preview.keyboard_preview_extra_levels;
    input.keyboard_inspected_key = preview.keyboard_inspected_key;
    Ok(())
}

fn mutate_sources(
    profile: &mut KeyboardProfile,
    operation: InputEffect,
    area: &InputArea,
    scope: &str,
) -> Result<(), String> {
    let position = |id: &str, profile: &KeyboardProfile| {
        profile
            .sources
            .iter()
            .position(|source| source == id)
            .ok_or_else(|| t("input-error-source-missing"))
    };
    match operation {
        InputEffect::AddSource { source } => {
            if !available(area, profile, scope)
                .iter()
                .any(|entry| entry.id == source)
            {
                return Err(t("input-error-source-add"));
            }
            profile.active.clone_from(&source);
            profile.sources.push(source);
        }
        InputEffect::SetActiveSource { source } => {
            position(&source, profile)?;
            profile.active = source;
        }
        InputEffect::RemoveSource { source } => {
            if profile.sources.len() <= 1 {
                return Err(t("input-error-source-last"));
            }
            let index = position(&source, profile)?;
            profile.sources.remove(index);
            profile.options.remove(&source);
            if profile.active == source {
                profile.active = profile
                    .sources
                    .first()
                    .cloned()
                    .ok_or_else(|| t("input-error-source-empty"))?;
            }
        }
        InputEffect::MoveSourceUp { source } => {
            let index = position(&source, profile)?;
            let before = index
                .checked_sub(1)
                .ok_or_else(|| t("input-error-source-first"))?;
            profile.sources.swap(index, before);
        }
        InputEffect::MoveSourceDown { source } => {
            let index = position(&source, profile)?;
            let after = index
                .checked_add(1)
                .filter(|index| *index < profile.sources.len())
                .ok_or_else(|| t("input-error-source-end"))?;
            profile.sources.swap(index, after);
        }
        InputEffect::ReorderSource {
            source,
            target,
            after,
        } => {
            if source == target {
                return Ok(());
            }
            let from = position(&source, profile)?;
            position(&target, profile)?;
            profile.sources.remove(from);
            let target_index = position(&target, profile)?;
            let destination = if after {
                target_index
                    .checked_add(1)
                    .ok_or_else(|| t("input-error-source-overflow"))?
            } else {
                target_index
            };
            profile.sources.insert(destination, source);
        }
        _ => return Err(t("input-error-source-operation")),
    }
    Ok(())
}

impl MouseKeyboardPane {
    pub(super) fn preview_input(&self, source: &str) -> InputPrefs {
        let mut input = self.last.clone();
        let mut profile = self.editor_profile();
        profile.active = source.into();
        let effective = profile.effective(&input.rich_input());
        input.keyboard_model = effective.keyboard_model;
        input.compose_key = effective.compose_key;
        input.caps_lock = effective.caps_lock;
        input
    }

    fn editor_profile(&self) -> KeyboardProfile {
        if let Some(profile) = self
            .last
            .keyboard_assignments
            .get(&self.editor.get().scope)
            .filter(|_| !matches!(self.editor.get().scope.as_str(), ALL | OSK))
        {
            return profile.clone();
        }
        if self.editor.get().scope == OSK {
            return osk_profile(self.cx.storage().settings_document());
        }
        let mut profile = KeyboardProfile::from_input(&self.last.rich_input());
        if let Some(defaults) = self.last.keyboard_assignments.get(ALL) {
            profile.options.clone_from(&defaults.options);
        }
        profile
    }

    fn selected_source(&self) -> String {
        let profile = self.editor_profile();
        self.editor
            .get()
            .selected
            .filter(|source| profile.sources.contains(source))
            .or_else(|| {
                self.last
                    .keyboard_preview
                    .clone()
                    .filter(|_| !self.editor.get().adding)
            })
            .unwrap_or(profile.active)
    }

    fn source_name(&self, id: &str) -> String {
        match id {
            "qwerty" => t("input-osk-qwerty"),
            "azerty" => t("input-osk-azerty"),
            _ => self.source_label(id),
        }
    }

    fn source_list(&self, profile: &KeyboardProfile) -> El {
        let cx = self.cx.clone();
        let selected = self.selected_source();
        let active = profile.active.clone();
        let mut selection = ui::selection::KeySelection::new();
        selection.select_only(selected.clone());
        let labels: HashMap<_, _> = profile
            .sources
            .iter()
            .map(|id| (id.clone(), self.source_name(id)))
            .collect();
        let list = VirtualList::from_items(
            profile.sources.clone(),
            self.source_row_height,
            String::clone,
            move |id, _, _| {
                let mut row = ListRow::new(
                    format!("input.source.{id}"),
                    labels.get(id).cloned().unwrap_or_else(|| id.clone()),
                )
                .selected(id == &selected);
                if id == &active {
                    row = row.value(t("input-sources-active"));
                }
                Box::new(row)
            },
        );
        let Ok(list) = list else {
            return error_caption(&t("input-sources-unavailable"));
        };
        let on_select = cx.clone();
        let on_activate = cx.clone();
        Box::new(
            list.with_id("input.sources.list")
                .with_accessible_name(t("input-sources-title"))
                .with_semantic_key(String::clone)
                .with_selection(selection)
                .with_viewport_height(
                    f32::from(u16::try_from(profile.sources.len()).unwrap_or(4))
                        * self.source_row_height,
                )
                .on_select(move |source, selected| {
                    if selected {
                        on_select.invoke(InputEffect::SelectConfiguredSource { source });
                    }
                })
                .on_activate(move |source| {
                    on_activate.invoke(InputEffect::SelectConfiguredSource { source });
                })
                .with_reorder(
                    |_, source, target| {
                        source != &target.key && target.placement != DropPlacement::Inside
                    },
                    move |_, drop| {
                        cx.invoke(InputEffect::ReorderSource {
                            source: drop.source,
                            target: drop.target.key,
                            after: drop.target.placement == DropPlacement::After,
                        });
                        false
                    },
                ),
        )
    }

    pub(super) fn keyboard_editor_card(&self) -> El {
        let profile = self.editor_profile();
        let selected = self.selected_source();
        let mut content = Vec::new();
        if self.scopes.len() > 1 {
            content.push(choice_row(
                &t("input-keyboard-scope"),
                Box::new(
                    id_dropdown(
                        self.scopes.clone(),
                        &self.editor.get().scope,
                        "input.keyboard.scope",
                        &t("input-keyboard-scope"),
                    )
                    .bound(&self.cx, effect(InputEffect::SELECT_KEYBOARD_SCOPE)),
                ),
            ));
            if self.editor.get().scope == ALL {
                content.push(caption(&t("input-scope-inheritance")));
            }
        }
        content.push(caption(&t("input-sources-selection-description")));
        content.push(self.source_list(&profile));
        let add = Box::new(self.cx.bind_invocation(
            Button::new("input.sources.add", &t("input-source-add-short")),
            InputEffect::OpenSourcePicker,
        ));
        let index = profile
            .sources
            .iter()
            .position(|source| source == &selected)
            .unwrap_or(0);
        let mut controls = Row::new();
        if let Some(target) = index
            .checked_sub(1)
            .and_then(|index| profile.sources.get(index))
        {
            controls = controls.push(Box::new(self.cx.bind_invocation(
                Button::new("input.source.move-up", &t("input-sources-move-up")),
                InputEffect::ReorderSource {
                    source: selected.clone(),
                    target: target.clone(),
                    after: false,
                },
            )));
        }
        if let Some(target) = index
            .checked_add(1)
            .and_then(|index| profile.sources.get(index))
        {
            controls = controls.push(Box::new(self.cx.bind_invocation(
                Button::new("input.source.move-down", &t("input-sources-move-down")),
                InputEffect::ReorderSource {
                    source: selected.clone(),
                    target: target.clone(),
                    after: true,
                },
            )));
        }
        controls = controls.push(Box::new(
            self.cx.bind_invocation(
                Button::new("input.source.activate", &t("input-source-activate"))
                    .with_enabled(selected != profile.active),
                InputEffect::SetActiveSource {
                    source: selected.clone(),
                },
            ),
        ));
        controls = controls.push(Box::new(
            self.cx.bind_invocation(
                Button::new("input.source.remove", &t("input-sources-remove"))
                    .with_enabled(profile.sources.len() > 1),
                InputEffect::RemoveSource {
                    source: selected.clone(),
                },
            ),
        ));
        content.push(Box::new(controls));
        content.extend(self.selected_source_preview(&selected));
        if self.editor.get().scope != OSK {
            content.push(self.source_options_card(&selected));
        }
        crate::widgets::card_with_action(&t("input-sources-title"), add, rows(content))
    }

    fn source_options_card(&self, selected: &str) -> El {
        let open = self.editor.get().options_open;
        let button = self.cx.bind_invocation(
            Button::new(
                "input.source.options",
                &t(if open {
                    "input-source-hide"
                } else {
                    "input-source-show"
                }),
            ),
            InputEffect::SetSourceOptionsVisible { visible: !open },
        );
        crate::widgets::card_with_action(
            &t("input-source-options"),
            Box::new(button),
            rows(if open {
                self.selected_source_options(selected)
            } else {
                Vec::new()
            }),
        )
    }

    fn selected_source_options(&self, source: &str) -> Vec<El> {
        let input = self.preview_input(source);
        vec![
            choice_row(
                &t("input-map-model"),
                Box::new(
                    id_dropdown(
                        vec![
                            ("pc104".into(), t("input-map-ansi")),
                            ("pc105".into(), t("input-map-iso")),
                            ("jp106".into(), t("input-map-jis")),
                            ("kr106".into(), t("input-map-korean")),
                        ],
                        &input.keyboard_model,
                        "input.map.model",
                        &t("input-map-model"),
                    )
                    .bound(&self.cx, effect(InputEffect::SET_KEYBOARD_MODEL)),
                ),
            ),
            choice_row(
                &t("input-compose-key"),
                Box::new(
                    Dropdown::from_state(
                        compose_options(),
                        &ui::LocalState::new(compose_shown(compose_english(
                            input.compose_key.as_deref(),
                        ))),
                    )
                    .bound(
                        &self.cx,
                        effect(InputEffect::SET_COMPOSE_KEY).map(|shown: String| {
                            compose_from_label(&shown)
                                .and_then(StoredKeyChoice::as_stored)
                                .unwrap_or(COMPOSE_DISABLED)
                                .to_owned()
                        }),
                    )
                    .with_id("input.compose_key"),
                ),
            ),
            choice_row(
                &t("input-caps-lock-key"),
                Box::new(
                    Dropdown::from_state(
                        caps_lock_options(),
                        &ui::LocalState::new(caps_lock_shown(caps_lock_label(
                            input.caps_lock.as_deref(),
                        ))),
                    )
                    .bound(
                        &self.cx,
                        effect(InputEffect::SET_CAPS_LOCK).map(|shown: String| {
                            caps_lock_from_label(&shown)
                                .and_then(StoredKeyChoice::as_stored)
                                .unwrap_or(CAPS_LOCK_UNCHANGED)
                                .to_owned()
                        }),
                    )
                    .with_id("input.caps_lock"),
                ),
            ),
        ]
    }

    fn selected_source_preview(&self, source: &str) -> Vec<El> {
        if self.editor.get().scope == OSK {
            return vec![
                Box::new(touch_preview(source, self.source_name(source))),
                caption(&t("input-osk-preview-description")),
            ];
        }
        match self.keyboard_preview(source) {
            Ok(map) => self.keyboard_inspection_controls(map),
            Err(_) => vec![error_caption(&t("input-map-unavailable"))],
        }
    }

    pub(super) fn keyboard_add_screen(&self) -> El {
        let profile = self.editor_profile();
        let scope = self.editor.get().scope;
        let area = InputArea {
            catalog: self.catalog.clone(),
            catalog_error: self.catalog_error.clone(),
            live: std::cell::Cell::new(false),
            devices: std::rc::Rc::new(RefCell::new((None, None))),
            editor: self.editor.clone(),
            osk_always: false,
            document: self.cx.storage().settings_document().clone(),
        };
        let mut content = vec![Box::new(self.cx.bind_invocation(
            Button::new("input.sources.back", &t("input-sources-back")),
            InputEffect::CloseSourcePicker,
        )) as El];
        if scope != OSK && self.catalog_error.is_some() {
            content.push(error_caption(&t("input-sources-unavailable")));
        }
        let choices = available(&area, &profile, &scope)
            .into_iter()
            .map(|entry| (entry.id, entry.label))
            .collect();
        content.push(choice_row(
            &t("input-sources-choose"),
            Box::new(
                id_dropdown(
                    choices,
                    self.editor.get().pending.as_deref().unwrap_or(""),
                    "input.source.add_picker",
                    &t("input-sources-choose"),
                )
                .bound(&self.cx, effect(InputEffect::CHOOSE_SOURCE_TO_ADD)),
            ),
        ));
        if let Some(source) = self.editor.get().pending {
            content.extend(self.selected_source_preview(&source));
        }
        content.push(Box::new(
            self.cx.bind_invocation(
                Button::new("input.sources.confirm", &t("input-source-add-short"))
                    .with_enabled(self.editor.get().pending.is_some()),
                InputEffect::ConfirmSourceToAdd,
            ),
        ));
        content.push(Box::new(self.cx.bind_invocation(
            Button::new("input.sources.cancel", &t("input-sources-cancel")),
            InputEffect::CloseSourcePicker,
        )));
        card(&t("input-source-add-title"), rows(content))
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct OskSourceSettings {
    layout: String,
    layouts: Vec<String>,
}

impl Default for OskSourceSettings {
    fn default() -> Self {
        Self {
            layout: "qwerty".into(),
            layouts: Vec::new(),
        }
    }
}

impl ui::storage::SettingsSchema for OskSourceSettings {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Personal;
}

fn osk_profile(document: &ui::storage::SettingsDocument) -> KeyboardProfile {
    let stored = ui::storage::Settings::<OskSourceSettings>::for_app("so.tilo.osk")
        .in_document(document.clone())
        .get();
    let configured = if stored.layouts.is_empty() {
        tilo_keyboard_map::touch::LAYOUT_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect()
    } else {
        stored.layouts
    };
    let mut sources: Vec<_> = configured
        .into_iter()
        .filter(|source| matches!(source.as_str(), "qwerty" | "azerty"))
        .collect();
    let mut seen = std::collections::HashSet::new();
    sources.retain(|source| seen.insert(source.clone()));
    let active = if matches!(stored.layout.as_str(), "qwerty" | "azerty") {
        stored.layout
    } else {
        "qwerty".into()
    };
    if !sources.contains(&active) {
        sources.insert(0, active.clone());
    }
    KeyboardProfile {
        sources,
        active,
        options: std::collections::BTreeMap::default(),
    }
}

fn store_osk(document: &ui::storage::SettingsDocument, profile: &KeyboardProfile) -> EffectAnswer {
    ui::storage::Settings::<OskSourceSettings>::for_app("so.tilo.osk")
        .in_document(document.clone())
        .set(&OskSourceSettings {
            layout: profile.active.clone(),
            layouts: profile.sources.clone(),
        })
        .map(|()| Effected::done())
        .map_err(|error| error.to_string())
}

fn touch_preview(source: &str, label: String) -> ui::KeyboardMap {
    use tilo_keyboard_map::touch::{KeyboardLayout, ShiftState, key_rects};
    use winit::keyboard::{NativeKeyCode, PhysicalKey};
    let layout = if source == "azerty" {
        KeyboardLayout::azerty()
    } else {
        KeyboardLayout::qwerty()
    };
    let keys = key_rects(&layout, Rect::new(0.0, 0.0, 10.0, 4.0), ShiftState::Off)
        .into_iter()
        .filter_map(|(id, rect)| {
            layout.key(id).map(|key| ui::KeyboardMapKey {
                name: format!("touch:{}:{}", id.row, id.col),
                physical: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                outline: vec![
                    Point::new(rect.x0, rect.y0),
                    Point::new(rect.x1, rect.y0),
                    Point::new(rect.x1, rect.y1),
                    Point::new(rect.x0, rect.y1),
                ],
                levels: vec![key.label.clone()],
            })
        })
        .collect();
    ui::KeyboardMap::new(
        keys,
        taffy::Size {
            width: 10.0,
            height: 4.0,
        },
        label,
    )
    .with_configuration(source.into(), "touch".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{effects::SettingsEffect, resource::SettingsShell};
    use ui::prelude::TestApp;

    fn fixture_app(scenario: &str, viewport: (u32, u32)) -> TestApp<SettingsShell> {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.surface::<SettingsApp, _>(|surface| {
            surface.input().live.set(false);
            // These fixtures edit configured sources without a native seat observation.
            let mut editor = surface.input().editor.get();
            editor.view = SourceView::Configured;
            surface.input().editor.set(editor);
            surface.input().devices.borrow_mut().1 =
                crate::scenarios::keyboard_fixture(Some(scenario));
        })
        .unwrap();
        app.harness().resize(viewport).unwrap();
        let address = crate::categories::Category::MouseKeyboard;
        app.effect_on(
            &crate::effects::pane_row_id(address),
            &crate::effects::open_pane(address),
        )
        .unwrap();
        app.settle();
        app
    }

    fn perform(app: &mut TestApp<SettingsShell>, operation: InputEffect) {
        app.effect(&SettingsEffect::from(operation)).unwrap();
        app.settle();
    }

    #[test]
    fn keyboard_sources_inspection_reports_discovered_scopes_without_mutation() {
        let mut app = fixture_app("keyboard-many", (1200, 900));
        let before = app.settings::<InputPrefs>().get();
        let result = app
            .effect(&SettingsEffect::from(InputEffect::InspectKeyboardSources))
            .unwrap()
            .value
            .unwrap();
        let scopes = result["scopes"].as_array().unwrap();
        assert!(scopes.iter().any(|scope| scope["id"] == OSK));
        assert!(scopes.iter().any(|scope| scope["id"] == "fixture-input-0"));
        assert!(scopes.iter().any(|scope| scope["id"] == "fixture-input-1"));
        assert_eq!(result["editing"], ALL);
        assert_eq!(app.settings::<InputPrefs>().get(), before);
    }

    #[test]
    fn keyboard_sources_touch_selection_cancel_and_reorder_use_live_actions() {
        use winit::event::TouchPhase;
        let mut app = fixture_app("keyboard-touch", (1200, 900));
        let mut input = app.settings::<InputPrefs>().get();
        input.keyboard_layouts = vec!["us".into(), "de".into()];
        app.settings::<InputPrefs>().set(&input).unwrap();
        app.settle();
        let target = app.harness().targets()["input.source.de"].center();
        app.harness()
            .touch(1, TouchPhase::Started, target.x, target.y);
        app.harness()
            .touch(1, TouchPhase::Cancelled, target.x, target.y);
        app.settle();
        assert_eq!(app.settings::<InputPrefs>().get(), input);
        app.harness().tap_named("input.source.de").unwrap();
        app.settle();
        let result = app
            .effect(&SettingsEffect::from(InputEffect::InspectKeyboardSources))
            .unwrap()
            .value
            .unwrap();
        assert_eq!(result["selected"], "de");
        assert_eq!(app.settings::<InputPrefs>().get().keyboard_layout, "us");
        app.harness().tap_named("input.source.move-up").unwrap();
        app.settle();
        assert_eq!(
            app.settings::<InputPrefs>().get().keyboard_layouts,
            ["de", "us"]
        );
        assert_eq!(app.settings::<InputPrefs>().get().keyboard_layout, "us");
        let before = app.settings::<InputPrefs>().get();
        app.harness().tap_named("input.sources.add").unwrap();
        app.settle();
        perform(&mut app, InputEffect::CloseSourcePicker);
        assert_eq!(app.settings::<InputPrefs>().get(), before);
    }

    #[test]
    fn keyboard_editor_selection_add_cancel_reorder_and_options_are_distinct() {
        let mut app = fixture_app("mouse_keyboard", (1200, 900));
        let mut input = app.settings::<InputPrefs>().get();
        input.keyboard_layouts = vec!["us".into(), "de".into()];
        input.keyboard_layout = "us".into();
        app.settings::<InputPrefs>().set(&input).unwrap();
        app.settle();
        perform(
            &mut app,
            InputEffect::SelectConfiguredSource {
                source: "de".into(),
            },
        );
        perform(&mut app, InputEffect::SetComposeKey { key: "ralt".into() });
        let stored = app.settings::<InputPrefs>().get();
        assert_eq!(stored.keyboard_layout, "us");
        assert_eq!(
            settings_store::keyboard_sources::effective(&stored.rich_input(), ALL).compose_key,
            input.compose_key
        );
        assert_eq!(
            stored.keyboard_assignments[ALL].options["de"]
                .compose_key
                .as_deref(),
            Some("ralt")
        );
        perform(&mut app, InputEffect::OpenSourcePicker);
        perform(
            &mut app,
            InputEffect::ChooseSourceToAdd {
                source: "fr".into(),
            },
        );
        perform(&mut app, InputEffect::CloseSourcePicker);
        assert_eq!(
            app.settings::<InputPrefs>().get().keyboard_layouts,
            ["us", "de"]
        );
        perform(&mut app, InputEffect::OpenSourcePicker);
        perform(
            &mut app,
            InputEffect::ChooseSourceToAdd {
                source: "fr".into(),
            },
        );
        perform(&mut app, InputEffect::ConfirmSourceToAdd);
        let stored = app.settings::<InputPrefs>().get();
        assert_eq!(stored.keyboard_layouts, ["us", "de", "fr"]);
        assert_eq!(stored.keyboard_layout, "us");
        perform(
            &mut app,
            InputEffect::ReorderSource {
                source: "fr".into(),
                target: "us".into(),
                after: false,
            },
        );
        assert_eq!(
            app.settings::<InputPrefs>().get().keyboard_layouts,
            ["fr", "us", "de"]
        );
        perform(
            &mut app,
            InputEffect::SetActiveSource {
                source: "de".into(),
            },
        );
        let stored = app.settings::<InputPrefs>().get();
        assert_eq!(stored.keyboard_layout, "de");
        assert_eq!(
            settings_store::keyboard_sources::effective(&stored.rich_input(), ALL)
                .compose_key
                .as_deref(),
            Some("ralt")
        );
        perform(
            &mut app,
            InputEffect::RemoveSource {
                source: "de".into(),
            },
        );
        assert_eq!(app.settings::<InputPrefs>().get().keyboard_layout, "fr");
    }

    #[test]
    fn keyboard_editor_discovery_scopes_are_real_and_hide_redundant_selector() {
        let snapshot = |keyboards: usize, touch: bool| InputHardwareSnapshot {
            devices: (0..keyboards)
                .map(|index| InputDeviceSnapshot {
                    key: format!("keyboard-{index}"),
                    name: "Keyboard".into(),
                    kind: InputDeviceKind::Keyboard,
                    ..Default::default()
                })
                .chain(touch.then_some(InputDeviceSnapshot {
                    kind: InputDeviceKind::Touchscreen,
                    ..Default::default()
                }))
                .collect(),
            ..Default::default()
        };
        assert_eq!(
            scopes(Some(&snapshot(1, false)), false)
                .iter()
                .map(|entry| entry.0.as_str())
                .collect::<Vec<_>>(),
            ["keyboard-0"]
        );
        assert_eq!(
            scopes(Some(&snapshot(2, false)), false)
                .iter()
                .map(|entry| entry.0.as_str())
                .collect::<Vec<_>>(),
            [ALL, "keyboard-0", "keyboard-1"]
        );
        assert_eq!(
            scopes(Some(&snapshot(0, true)), false)
                .iter()
                .map(|entry| entry.0.as_str())
                .collect::<Vec<_>>(),
            [OSK]
        );
        assert_eq!(
            scopes(Some(&snapshot(1, true)), false)
                .iter()
                .map(|entry| entry.0.as_str())
                .collect::<Vec<_>>(),
            [ALL, "keyboard-0", OSK]
        );
        assert_eq!(scopes(None, false).len(), 1);
    }

    #[test]
    fn keyboard_editor_osk_choices_are_independent_and_cancel_preserves_layout() {
        let mut app = fixture_app("keyboard-osk", (440, 820));
        let physical = app.settings::<InputPrefs>().get();
        assert!(
            app.effect(&SettingsEffect::from(InputEffect::ShowLiveKeyboardMap))
                .is_err()
        );
        assert_eq!(app.settings::<InputPrefs>().get(), physical);
        perform(
            &mut app,
            InputEffect::RemoveSource {
                source: "azerty".into(),
            },
        );
        perform(&mut app, InputEffect::OpenSourcePicker);
        perform(
            &mut app,
            InputEffect::ChooseSourceToAdd {
                source: "azerty".into(),
            },
        );
        perform(&mut app, InputEffect::CloseSourcePicker);
        app.surface::<SettingsApp, _>(|surface| {
            assert_eq!(osk_profile(&surface.input().document).sources, ["qwerty"]);
        })
        .unwrap();
        perform(&mut app, InputEffect::OpenSourcePicker);
        perform(
            &mut app,
            InputEffect::ChooseSourceToAdd {
                source: "azerty".into(),
            },
        );
        perform(&mut app, InputEffect::ConfirmSourceToAdd);
        perform(
            &mut app,
            InputEffect::SetActiveSource {
                source: "azerty".into(),
            },
        );
        app.surface::<SettingsApp, _>(|surface| {
            let stored = osk_profile(&surface.input().document);
            assert_eq!(stored.sources, ["qwerty", "azerty"]);
            assert_eq!(stored.active, "azerty");
        })
        .unwrap();
        assert_eq!(app.settings::<InputPrefs>().get(), physical);
    }
    #[test]
    fn keyboard_editor_real_drag_and_device_captures() {
        for (scenario, viewport, theme) in [
            ("keyboard-single", (1200, 900), Theme::light()),
            ("keyboard-many", (1200, 900), Theme::dark()),
            (
                "keyboard-touch",
                (440, 820),
                Theme::high_contrast().with_ui_scale(1.5),
            ),
            ("keyboard-osk", (440, 820), Theme::light()),
        ] {
            let mut app = fixture_app(scenario, viewport);
            app.harness().set_theme(theme);
            let mut input = app.settings::<InputPrefs>().get();
            input.keyboard_layouts = vec!["us".into(), "de".into(), "fr".into()];
            input.keyboard_layout = "us".into();
            app.settings::<InputPrefs>().set(&input).unwrap();
            app.settle();
            let directory = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("settings-keyboard-review")
                .join(scenario);
            app.harness().render_png(&directory.join("sources.png"));
            let targets = app.harness().targets();
            assert_eq!(
                targets.contains_key("input.keyboard.scope"),
                matches!(scenario, "keyboard-many" | "keyboard-touch")
            );
            if scenario == "keyboard-single" {
                let from = targets["input.source.fr"];
                let to = targets["input.source.us"];
                app.harness().drag_at(
                    (from.center().x, from.center().y),
                    (to.center().x, to.y0 + 2.0),
                );
                app.settle();
                let profile =
                    app.settings::<InputPrefs>().get().keyboard_assignments["fixture-input-0"]
                        .clone();
                assert_eq!(profile.sources, ["fr", "us", "de"]);
                assert_eq!(profile.active, "us");
                app.harness().render_png(&directory.join("reordered.png"));
            }
            if matches!(scenario, "keyboard-many" | "keyboard-touch") {
                app.harness().click_named("input.keyboard.scope").unwrap();
                app.settle();
                app.harness().render_png(&directory.join("devices.png"));
                app.harness().key("Escape");
                app.settle();
                perform(
                    &mut app,
                    InputEffect::SelectKeyboardScope { scope: OSK.into() },
                );
                app.harness()
                    .render_png(&directory.join("osk-selected.png"));
            }
            perform(&mut app, InputEffect::OpenSourcePicker);
            app.harness().render_png(&directory.join("add.png"));
            assert!(!app.harness().targets().contains_key("input.sources.list"));
            perform(&mut app, InputEffect::CloseSourcePicker);
            assert!(app.harness().targets().contains_key("input.sources.list"));
            assert!(app.harness().component_contract_violations().is_empty());
        }
    }
    #[test]
    fn keyboard_editor_hotplug_preserves_assignments_and_reconnects_by_identity() {
        let mut app = fixture_app("keyboard-many", (1200, 900));
        perform(
            &mut app,
            InputEffect::SelectKeyboardScope {
                scope: "fixture-input-1".into(),
            },
        );
        perform(
            &mut app,
            InputEffect::AddSource {
                source: "de".into(),
            },
        );
        let assigned =
            app.settings::<InputPrefs>().get().keyboard_assignments["fixture-input-1"].clone();
        app.surface::<SettingsApp, _>(|surface| {
            surface.input().devices.borrow_mut().1 =
                crate::scenarios::keyboard_fixture(Some("keyboard-single"));
        })
        .unwrap();
        app.settle();
        assert_eq!(
            app.settings::<InputPrefs>().get().keyboard_assignments["fixture-input-1"],
            assigned
        );
        assert!(
            app.effect(&SettingsEffect::from(InputEffect::SelectKeyboardScope {
                scope: "fixture-input-1".into()
            }))
            .is_err()
        );
        app.surface::<SettingsApp, _>(|surface| {
            surface.input().devices.borrow_mut().1 =
                crate::scenarios::keyboard_fixture(Some("keyboard-many"));
        })
        .unwrap();
        app.settle();
        perform(
            &mut app,
            InputEffect::SelectKeyboardScope {
                scope: "fixture-input-1".into(),
            },
        );
        perform(
            &mut app,
            InputEffect::SelectConfiguredSource {
                source: "de".into(),
            },
        );
        assert_eq!(
            app.settings::<InputPrefs>().get().keyboard_assignments["fixture-input-1"],
            assigned
        );
        let app = app.reopen().unwrap();
        assert_eq!(
            app.settings::<InputPrefs>().get().keyboard_assignments["fixture-input-1"],
            assigned
        );
        assert_eq!(app.settings::<InputPrefs>().get().keyboard_layout, "us");
    }

    #[test]
    fn keyboard_editor_osk_updates_preserve_existing_preferences_and_survive_restart() {
        #[derive(Default, serde::Serialize, serde::Deserialize)]
        #[serde(default)]
        struct FixtureOskPreferences {
            layout: String,
            layouts: Vec<String>,
            suggestions: bool,
            personalization: bool,
        }
        impl ui::storage::SettingsSchema for FixtureOskPreferences {
            const SCOPE: ui::storage::Scope = ui::storage::Scope::Personal;
        }
        let mut app = fixture_app("keyboard-osk", (440, 820));
        let document = app
            .surface::<SettingsApp, _>(|surface| surface.input().document.clone())
            .unwrap();
        let settings = ui::storage::Settings::<FixtureOskPreferences>::for_app("so.tilo.osk")
            .in_document(document);
        let preferences = FixtureOskPreferences {
            layout: "azerty".into(),
            suggestions: false,
            personalization: false,
            ..Default::default()
        };
        settings.set(&preferences).unwrap();
        app.settle();
        app.surface::<SettingsApp, _>(|surface| {
            let legacy = osk_profile(&surface.input().document);
            assert_eq!(legacy.active, "azerty");
            assert_eq!(legacy.sources, ["qwerty", "azerty"]);
        })
        .unwrap();
        perform(
            &mut app,
            InputEffect::RemoveSource {
                source: "qwerty".into(),
            },
        );
        perform(&mut app, InputEffect::OpenSourcePicker);
        perform(
            &mut app,
            InputEffect::ChooseSourceToAdd {
                source: "qwerty".into(),
            },
        );
        perform(&mut app, InputEffect::ConfirmSourceToAdd);
        assert_eq!(settings.get().layout, "azerty");
        assert!(!settings.get().suggestions);
        assert!(!settings.get().personalization);
        assert_eq!(settings.get().layouts, ["azerty", "qwerty"]);
        let mut app = app.reopen().unwrap();
        app.surface::<SettingsApp, _>(|surface| {
            let stored = osk_profile(&surface.input().document);
            assert_eq!(stored.active, "azerty");
            assert_eq!(stored.sources, ["azerty", "qwerty"]);
        })
        .unwrap();
    }
}
