//! Configured sources are the selection model; discovery only supplies edit scopes.

use super::*;
use crate::widgets::{
    Item, SettingsSection, StatusTone, block_section, note, status_pill, untitled,
};
use settings_store::keyboard_sources::{ALL, KeyboardProfile, OSK, SourceOptions};

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
        // The only keyboard edits the shared sources: a list of its own
        // would override them invisibly, since no selector names it.
        let title = keyboards.pop().map(|(_, title)| title).unwrap_or_default();
        return vec![(ALL.into(), title)];
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

/// Attached physical keyboards that type their own sources instead of the
/// shared ones, with their titles, in key order.
pub(super) fn keyboards_with_own_sources(
    snapshot: Option<&InputHardwareSnapshot>,
    input: &InputPrefs,
) -> Vec<(String, String)> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let mut keyboards: Vec<_> = snapshot
        .of_kind(InputDeviceKind::Keyboard)
        .into_iter()
        .filter(|device| {
            !matches!(device.key.as_str(), ALL | OSK)
                && input.keyboard_assignments.contains_key(&device.key)
        })
        .map(|device| (device.key.clone(), device_title(device)))
        .collect();
    keyboards.sort_by(|a, b| a.0.cmp(&b.0));
    keyboards.dedup_by(|a, b| a.0 == b.0);
    keyboards
}

/// An edit of the shared sources must be what an attached keyboard types
/// when that keyboard is not offered for editing on its own: its retained
/// list would otherwise keep overriding the edit with nothing on screen
/// saying so. Disconnected keyboards keep their lists.
fn drop_hidden_assignments(input: &mut InputPrefs, area: &InputArea) {
    let offered = area.keyboard_scopes();
    for (key, _) in keyboards_with_own_sources(area.devices().as_ref(), input) {
        if !offered.iter().any(|(scope, _)| scope == &key) {
            input.keyboard_assignments.remove(&key);
        }
    }
}

/// Make one keyboard type the shared sources again.
fn use_shared_sources(app: &SettingsApp, scope: &str) -> EffectAnswer {
    let settings = app.host().settings::<InputPrefs>();
    let mut input = settings.get();
    if matches!(scope, ALL | OSK) || input.keyboard_assignments.remove(scope).is_none() {
        return Err(t("input-error-keyboard-shared"));
    }
    settings
        .set(&input)
        .map(|()| Effected::done())
        .map_err(|error| error.to_string())
}

pub(super) fn answer_editor(app: &SettingsApp, operation: &InputEffect) -> Option<EffectAnswer> {
    let area = app.input();
    if matches!(operation, InputEffect::InspectKeyboardSources) {
        return Some(Ok(inspect_sources(app)));
    }
    if let InputEffect::UseSharedSources { scope } = operation {
        return Some(use_shared_sources(app, scope));
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
            let own = !matches!(id.as_str(), ALL | OSK)
                && input.keyboard_assignments.contains_key(&id);
            serde_json::json!({"id": id, "label": label, "sources": profile.sources, "active": profile.active, "own": own})
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
    } else if area.catalog_error().is_none() {
        area.catalog().to_vec()
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
    if editor.selected.is_some()
        && matches!(
            operation,
            InputEffect::SetActiveSource { .. }
                | InputEffect::AddSource { .. }
                | InputEffect::ReplaceSource { .. }
                | InputEffect::RemoveSource { .. }
        )
    {
        let mut cleared = editor.clone();
        cleared.selected = None;
        area.editor.set(cleared);
    }
    let source_action = matches!(
        operation,
        InputEffect::SetActiveSource { .. }
            | InputEffect::AddSource { .. }
            | InputEffect::ReplaceSource { .. }
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
        drop_hidden_assignments(input, area);
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
        InputEffect::ReplaceSource { from, source } => {
            let index = position(&from, profile)?;
            if from == source {
                return Ok(());
            }
            if !available(area, profile, scope)
                .iter()
                .any(|entry| entry.id == source)
            {
                return Err(t("input-error-source-add"));
            }
            if let Some(slot) = profile.sources.get_mut(index) {
                slot.clone_from(&source);
            }
            profile.options.remove(&from);
            if profile.active == from {
                profile.active = source;
            }
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

    /// One row per layout for a keyboard that types several: its name (and
    /// whether it is the one in use) with, on the trailing edge, buttons to
    /// use it, move it in the switching order and remove it.
    fn source_items(&self, profile: &KeyboardProfile) -> Vec<Item> {
        let small = ui::widget::ControlSize::Small;
        let last = profile.sources.len().saturating_sub(1);
        profile
            .sources
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let mut controls = Row::new()
                    .no_wrap()
                    .with_align_items(taffy::AlignItems::Center)
                    .with_rhythm(ui::tokens::Rhythm::Related);
                if id != &profile.active {
                    controls = controls.push(Box::new(
                        self.cx.bind_invocation(
                            Button::new(&format!("input.source.use.{id}"), &t("input-source-use"))
                                .control_size(small),
                            InputEffect::SetActiveSource { source: id.clone() },
                        ),
                    ));
                }
                let neighbour = |offset: isize| {
                    index
                        .checked_add_signed(offset)
                        .and_then(|at| profile.sources.get(at))
                        .cloned()
                };
                for (offset, icon, key, name) in [
                    (-1, ui::icons::Icon::ArrowUp, "up", "input-sources-move-up"),
                    (
                        1,
                        ui::icons::Icon::ArrowDown,
                        "down",
                        "input-sources-move-down",
                    ),
                ] {
                    let target = neighbour(offset);
                    controls = controls.push(Box::new(
                        self.cx.bind_invocation(
                            Button::icon_only(
                                &format!("input.source.move-{key}.{id}"),
                                icon,
                                &t(name),
                            )
                            .control_size(small)
                            .with_enabled(target.is_some()),
                            InputEffect::ReorderSource {
                                source: id.clone(),
                                target: target.unwrap_or_default(),
                                after: offset > 0,
                            },
                        ),
                    ));
                }
                controls = controls.push(Box::new(
                    self.cx.bind_invocation(
                        Button::icon_only(
                            &format!("input.source.remove.{id}"),
                            ui::icons::Icon::Trash2,
                            &t("input-sources-remove"),
                        )
                        .control_size(small)
                        .with_enabled(last > 0),
                        InputEffect::RemoveSource { source: id.clone() },
                    ),
                ));
                let name = self.source_name(id);
                if id == &profile.active {
                    setting_row_described(&name, &t("input-preview-in-use"), Box::new(controls))
                } else {
                    setting_row(&name, Box::new(controls))
                }
            })
            .collect()
    }

    /// The Keyboard page, top to bottom: the layouts (what people come here
    /// to change), with a warning above them when the keyboard types
    /// something else; the typing options; and the selected layout drawn as
    /// its keycaps, for looking up where a symbol is.
    pub(super) fn keyboard_page(&self) -> Vec<SettingsSection> {
        let profile = self.editor_profile();
        let shown = self.selected_source();
        let osk = self.editor.get().scope == OSK;
        let mut page = Vec::new();
        let map = if osk {
            None
        } else {
            self.keyboard_preview(&shown).ok()
        };
        if let Some(map) = &map
            && shown == profile.active
            && map.live_agrees() == Some(false)
        {
            page.push(self.layout_mismatch_section(&profile));
        }
        // The layout drawn as its keys first, lighting up as they are
        // pressed; then the layouts to choose from and how keys behave,
        // with a field to try them in.
        page.push(self.layout_preview_section(&shown, &profile, map));
        page.push(self.layouts_section(&profile));
        if !osk {
            page.push(self.keyboard_options_section(&shown));
        }
        page
    }

    /// The keyboard types a layout other than the one configured: say so,
    /// and offer to apply the configured one.
    fn layout_mismatch_section(&self, profile: &KeyboardProfile) -> SettingsSection {
        let name = self.source_name(&profile.active);
        untitled(rows(vec![setting_row_described(
            &t("input-mismatch-title"),
            &t_args("input-mismatch-detail", &[("name", ArgValue::from(name))]),
            Box::new(self.cx.bind_invocation(
                Button::new("input.keyboard.apply", &t("input-mismatch-apply")),
                InputEffect::SetActiveSource {
                    source: profile.active.clone(),
                },
            )),
        )]))
    }

    /// The layout in use, drawn as its real keycaps, one layer at a time.
    fn layout_preview_section(
        &self,
        shown: &str,
        profile: &KeyboardProfile,
        map: Option<ui::KeyboardMap>,
    ) -> SettingsSection {
        let name = self.source_name(shown);
        let in_use = shown == profile.active;
        let mut head = Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center)
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Text::new(&name).with_role(ui::tokens::TextRole::Title),
            ))
            .push(Box::new(Spacer::new()));
        head = head.push(status_pill(
            &t(if in_use {
                "input-preview-in-use"
            } else {
                "input-preview-previewing"
            }),
            if in_use {
                StatusTone::Success
            } else {
                StatusTone::Neutral
            },
        ));
        let mut body = ui::widget::Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(head));
        if self.editor.get().scope == OSK {
            body = body
                .push(Box::new(touch_preview(shown, name)))
                .push(caption(&t("input-osk-preview-description")));
            return block_section(Box::new(body));
        }
        match map {
            Some(map) => {
                let levels = map.level_count().clamp(1, 4);
                let level = self.last.keyboard_preview_level.min(levels - 1);
                let layers = ui::widget::ToggleGroup::new(
                    (0..levels)
                        .map(|index| {
                            ui::widget::Segment::new(t(layer_key(index)))
                                .short(t(&format!("{}-short", layer_key(index))))
                        })
                        .collect(),
                    level,
                )
                .with_id("input.map.level")
                .with_accessible_name(t("input-map-level"));
                let layers = self.cx.bind(
                    layers,
                    effect(InputEffect::SET_PREVIEW_LEVEL).map(|index: usize| index.to_string()),
                );
                let live = self.editor.get().view == SourceView::Live;
                let map = if live || self.last.keyboard_preview.is_some() {
                    map.with_inspected_key(&self.last.keyboard_inspected_key)
                } else {
                    map
                }
                .with_live_layout(live)
                .with_single_level(level);
                body = body.push(Box::new(map));
                if live {
                    // Asked for by automation or a person: what the seat
                    // really types, not the configured layout.
                    body = body
                        .push(caption(&t("input-map-live-description")))
                        .push(Box::new(self.cx.bind_invocation(
                            Button::new("input.map.configured", &t("input-map-configured")),
                            InputEffect::PreviewLayout {
                                source: shown.to_owned(),
                            },
                        )));
                } else {
                    body = body.push(Box::new(layers));
                }
            }
            None => body = body.push(error_caption(&t("input-map-unavailable"))),
        }
        block_section(Box::new(body))
    }

    /// The configured layouts: one row for a single layout, a reorderable
    /// list for several, and an inline searchable chooser to add another.
    fn layouts_section(&self, profile: &KeyboardProfile) -> SettingsSection {
        let scope = self.editor.get().scope;
        let mut content = Vec::new();
        if self.scopes.len() > 1 {
            content.push(setting_row(
                &t("input-keyboard-scope"),
                Box::new(self.cx.bind(
                    id_dropdown(
                        self.scopes.clone(),
                        &scope,
                        "input.keyboard.scope",
                        &t("input-keyboard-scope"),
                    ),
                    effect(InputEffect::SELECT_KEYBOARD_SCOPE),
                )),
            ));
            if scope == ALL {
                content.push(note(&t("input-scope-inheritance")));
            }
        }
        if scope == ALL {
            content.extend(self.own_source_rows());
        }
        let area = self.catalog_area();
        let choices: Vec<(String, String)> = available(&area, profile, &scope)
            .into_iter()
            .map(|entry| (entry.id, entry.label))
            .collect();
        if self.catalog_error.is_some() && scope != OSK {
            content.push(note(&t("input-sources-unavailable")));
        }
        if let [only] = profile.sources.as_slice() {
            // One layout is one row: its name, to change in place.
            let mut options = choices.clone();
            options.insert(0, (only.clone(), self.source_name(only)));
            let from = only.clone();
            content.push(setting_row(
                &t("input-source-layout"),
                Box::new(
                    self.cx.bind(
                        id_dropdown(
                            options,
                            only,
                            "input.source.replace",
                            &t("input-source-layout"),
                        ),
                        effect(InputEffect::REPLACE_SOURCE)
                            .map(move |source: String| (from.clone(), source)),
                    ),
                ),
            ));
        } else {
            content.extend(self.source_items(profile));
        }
        if self.catalog_pending {
            // The layouts to add are still being read; everything else on
            // the page is already right.
            content.push(setting_row(
                &t("input-source-add-title"),
                Box::new(
                    id_dropdown(
                        Vec::new(),
                        "",
                        "input.source.add_picker",
                        &t("input-source-add-title"),
                    )
                    .with_placeholder(&t("input-sources-loading"))
                    .with_enabled(false),
                ),
            ));
        } else if choices.is_empty() {
            content.push(note(&t("input-sources-full")));
        } else {
            content.push(setting_row(
                &t("input-source-add-title"),
                Box::new(
                    self.cx.bind(
                        id_dropdown(
                            choices,
                            "",
                            "input.source.add_picker",
                            &t("input-source-add-title"),
                        )
                        .with_placeholder(&t("input-source-add-placeholder")),
                        effect(InputEffect::ADD_SOURCE),
                    ),
                ),
            ));
        }
        card(&t("input-layouts-title"), rows(content))
    }

    /// What the page needs of the catalogue to offer layouts to add.
    fn catalog_area(&self) -> InputArea {
        InputArea {
            catalog: SourceCatalog::ready(self.catalog.clone(), self.catalog_error.clone()),
            live: std::cell::Cell::new(false),
            devices: std::rc::Rc::new(RefCell::new((None, None))),
            following: RefCell::new(std::rc::Weak::new()),
            editor: self.editor.clone(),
            osk_always: false,
            document: self.cx.storage().settings_document().clone(),
        }
    }

    /// Typing options, flat: the physical model, the compose and Caps Lock
    /// keys, how held keys repeat, and a field to try them in.
    fn keyboard_options_section(&self, shown: &str) -> SettingsSection {
        let mut items = self.selected_source_options(shown);
        items.push(self.key_repeat_row());
        items.push(field_row(
            &t("input-test-typing"),
            Box::new(
                TextInput::local()
                    .with_id("input.test-typing")
                    .with_placeholder(t("input-test-typing-placeholder")),
            ),
        ));
        card(&t("input-keyboard-title"), rows(items))
    }

    /// One row per attached keyboard whose own list overrides the shared
    /// sources, so what that keyboard types is never a surprise.
    fn own_source_rows(&self) -> Vec<Item> {
        let snapshot = self.hardware.borrow().1.clone();
        keyboards_with_own_sources(snapshot.as_ref(), &self.last)
            .into_iter()
            .map(|(scope, title)| {
                setting_row(
                    &t_args("input-scope-own", &[("name", ArgValue::from(title))]),
                    Box::new(self.cx.bind_invocation(
                        Button::new(
                            &format!("input.keyboard.shared.{scope}"),
                            &t("input-scope-use-shared"),
                        ),
                        InputEffect::UseSharedSources { scope },
                    )),
                )
            })
            .collect()
    }

    fn selected_source_options(&self, source: &str) -> Vec<Item> {
        let input = self.preview_input(source);
        vec![
            setting_row(
                &t("input-map-model"),
                Box::new(self.cx.bind(
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
                    ),
                    effect(InputEffect::SET_KEYBOARD_MODEL),
                )),
            ),
            self.cx
                .row(t("input-compose-key"))
                .id("input.compose_key")
                .choice_effect(
                    compose_options(),
                    &ui::LocalState::new(compose_shown(compose_english(
                        input.compose_key.as_deref(),
                    ))),
                    effect(InputEffect::SET_COMPOSE_KEY).map(|shown: String| {
                        compose_from_label(&shown)
                            .and_then(StoredKeyChoice::as_stored)
                            .unwrap_or(COMPOSE_DISABLED)
                            .to_owned()
                    }),
                ),
            self.cx
                .row(t("input-caps-lock-key"))
                .id("input.caps_lock")
                .choice_effect(
                    caps_lock_options(),
                    &ui::LocalState::new(caps_lock_shown(caps_lock_label(
                        input.caps_lock.as_deref(),
                    ))),
                    effect(InputEffect::SET_CAPS_LOCK).map(|shown: String| {
                        caps_lock_from_label(&shown)
                            .and_then(StoredKeyChoice::as_stored)
                            .unwrap_or(CAPS_LOCK_UNCHANGED)
                            .to_owned()
                    }),
                ),
        ]
    }
}

/// The name of a modifier layer: XKB levels run base, Shift, `AltGr`,
/// Shift + `AltGr`.
fn layer_key(level: usize) -> &'static str {
    match level {
        0 => "input-layer-base",
        1 => "input-layer-shift",
        2 => "input-layer-altgr",
        _ => "input-layer-shift-altgr",
    }
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize, ui::tilo_resource::Model)]
#[serde(default)]
pub struct OskSourceSettings {
    #[model(readonly)]
    layout: String,
    #[model(readonly)]
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

#[ui::tilo_resource::resource(kind = "settings.osk-sources", label = "On-screen keyboard layouts")]
impl OskSourceSettings {}

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
        crate::effects::show_pane(&mut app, crate::categories::Category::Keyboard);
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
        let target = app.harness().targets()["input.source.use.de"].center();
        app.harness()
            .touch(1, TouchPhase::Started, target.x, target.y);
        app.harness()
            .touch(1, TouchPhase::Cancelled, target.x, target.y);
        app.settle();
        assert_eq!(app.settings::<InputPrefs>().get(), input);
        app.harness().tap_named("input.source.use.de").unwrap();
        app.settle();
        assert_eq!(app.settings::<InputPrefs>().get().keyboard_layout, "de");
        assert_eq!(
            app.settings::<InputPrefs>().get().keyboard_layouts,
            ["us", "de"]
        );
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
            [ALL]
        );
        assert_eq!(scopes(Some(&snapshot(1, false)), false)[0].1, "Keyboard");
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
                // The page is rebuilt after the captures above; press once it
                // has settled, and again only if that press found it still
                // settling.
                for _ in 0..3 {
                    app.harness().relayout();
                    app.settle();
                    if app.settings::<InputPrefs>().get().keyboard_layouts == ["us", "fr", "de"] {
                        break;
                    }
                    app.harness()
                        .click_named("input.source.move-up.fr")
                        .unwrap();
                    app.settle();
                }
                let stored = app.settings::<InputPrefs>().get();
                assert_eq!(stored.keyboard_layouts, ["us", "fr", "de"]);
                assert_eq!(stored.keyboard_layout, "us");
                assert!(!stored.keyboard_assignments.contains_key("fixture-input-0"));
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
            if scenario == "keyboard-single" {
                assert!(
                    app.harness()
                        .targets()
                        .contains_key("input.source.add_picker")
                );
            }
            assert!(app.harness().component_contract_violations().is_empty());
        }
    }
    fn fork(sources: &[&str]) -> KeyboardProfile {
        KeyboardProfile {
            sources: sources.iter().map(|source| (*source).into()).collect(),
            active: sources[0].into(),
            options: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn the_only_keyboard_edits_the_shared_sources_and_loses_its_hidden_list() {
        let mut app = fixture_app("keyboard-single", (1200, 900));
        let mut input = app.settings::<InputPrefs>().get();
        input
            .keyboard_assignments
            .insert("fixture-input-0".into(), fork(&["us"]));
        app.settings::<InputPrefs>().set(&input).unwrap();
        app.settle();
        // What the built-in keyboard types differs from the shared list: say so.
        assert!(
            app.harness()
                .targets()
                .contains_key("input.keyboard.shared.fixture-input-0")
        );
        perform(
            &mut app,
            InputEffect::AddSource {
                source: "de".into(),
            },
        );
        perform(
            &mut app,
            InputEffect::SetActiveSource {
                source: "de".into(),
            },
        );
        let stored = app.settings::<InputPrefs>().get();
        assert_eq!(stored.keyboard_layout, "de");
        assert!(stored.keyboard_layouts.contains(&"de".to_string()));
        assert!(!stored.keyboard_assignments.contains_key("fixture-input-0"));
        assert_eq!(
            settings_store::keyboard_sources::effective(&stored.rich_input(), "fixture-input-0")
                .keyboard_layout,
            "de",
            "the keyboard types what the page edited"
        );
        assert!(
            !app.harness()
                .targets()
                .contains_key("input.keyboard.shared.fixture-input-0")
        );
    }

    #[test]
    fn a_keyboard_with_its_own_list_is_shown_and_can_use_the_shared_one() {
        let mut app = fixture_app("keyboard-many", (1200, 900));
        let mut input = app.settings::<InputPrefs>().get();
        input
            .keyboard_assignments
            .insert("fixture-input-1".into(), fork(&["fr"]));
        app.settings::<InputPrefs>().set(&input).unwrap();
        app.settle();
        let result = app
            .effect(&SettingsEffect::from(InputEffect::InspectKeyboardSources))
            .unwrap()
            .value
            .unwrap();
        let own = |id: &str| {
            result["scopes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|scope| scope["id"] == id)
                .unwrap()["own"]
                .clone()
        };
        assert_eq!(own("fixture-input-1"), true);
        assert_eq!(own("fixture-input-0"), false);
        // A shared edit leaves a keyboard offered on its own alone.
        perform(
            &mut app,
            InputEffect::AddSource {
                source: "de".into(),
            },
        );
        assert!(
            app.settings::<InputPrefs>()
                .get()
                .keyboard_assignments
                .contains_key("fixture-input-1")
        );
        app.harness()
            .click_named("input.keyboard.shared.fixture-input-1")
            .unwrap();
        app.settle();
        assert!(
            !app.settings::<InputPrefs>()
                .get()
                .keyboard_assignments
                .contains_key("fixture-input-1")
        );
        assert!(
            app.effect(&SettingsEffect::from(InputEffect::UseSharedSources {
                scope: "fixture-input-1".into()
            }))
            .is_err()
        );
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
