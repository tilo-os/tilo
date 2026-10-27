//! A window bar's row as assistive technology and automation reach it:
//! its slots and menus as nodes, and the targets they run.

use super::{
    ACTION_TARGET_PREFIX, BACK_TARGET, CHOICE_TARGET_PREFIX, FORWARD_TARGET, LEADING_TARGET_PREFIX,
    LOCATION_TARGET, MenuEntry, NOTICE_TARGET_PREFIX, OVERFLOW_TARGET, Rect, SEGMENT_TARGET_PREFIX,
    SIDEBAR_TARGET, SUGGESTION_TARGET_PREFIX, Slot, ToolbarEvent, ToolbarRow, VIEW_TARGET_PREFIX,
    VIEWS_MENU_TARGET, Widget, sidebar_key, trailing_word,
};

impl ToolbarRow {
    /// The slots as assistive technology and automation reach them.
    pub(crate) fn accessibility(&self) -> Vec<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let Some(toolbar) = self.toolbar() else {
            return Vec::new();
        };
        let button = |name: String, target: String, rect: Rect, enabled: bool| {
            AccessNode::new(Role::Button)
                .with_name(name)
                .with_action_target(target)
                .with_bounds(rect)
                .with_state(AccessState {
                    focusable: enabled,
                    disabled: !enabled,
                    ..Default::default()
                })
        };
        let mut nodes = Vec::new();
        if let Some(open) = toolbar.sidebar {
            let mut toggle = button(
                crate::i18n::I18nManager::lookup_plain(sidebar_key(toolbar)),
                SIDEBAR_TARGET.into(),
                self.slots.sidebar,
                !toolbar.sidebar_unavailable,
            );
            toggle.role = Role::ToggleButton;
            toggle.state.checked = Some(open);
            nodes.push(toggle);
        }
        if let Some(nav) = toolbar.navigation {
            nodes.push(button(
                crate::i18n::I18nManager::lookup_plain("window-toolbar-back"),
                BACK_TARGET.into(),
                self.slots.back,
                nav.back,
            ));
            nodes.push(button(
                crate::i18n::I18nManager::lookup_plain("window-toolbar-forward"),
                FORWARD_TARGET.into(),
                self.slots.forward,
                nav.forward,
            ));
        }
        for (action, rect) in toolbar
            .leading
            .iter()
            .zip(&self.slots.leading)
            .filter(|(_, rect)| rect.area() > 0.0)
        {
            let mut node = button(
                action.name.clone(),
                format!("{LEADING_TARGET_PREFIX}{}", action.id),
                *rect,
                action.enabled,
            );
            if action.toggles || action.on {
                node.role = Role::ToggleButton;
                node.state.checked = Some(action.on);
            }
            nodes.push(node);
        }
        if let Some(location) = toolbar.location.as_ref()
            && self.slots.location.area() > 0.0
            && !self.slots.views_in_location()
        {
            if let Some(field) = self.field.as_ref().and_then(Widget::accessibility) {
                nodes.push(field);
                nodes.extend(self.suggestion_nodes());
            } else {
                let name = if location.name.is_empty() {
                    location.label.clone()
                } else {
                    location.name.clone()
                };
                let picker = !location.editable && !location.choices.is_empty();
                let mut node = AccessNode::new(if location.editable || picker {
                    Role::Button
                } else {
                    Role::StaticText
                })
                .with_name(name)
                .with_optional_value(Some(location.edit_value().to_owned()))
                .with_bounds(self.slots.location);
                if let Some(word) = trailing_word(location)
                    && toolbar.notice.is_none()
                {
                    node = node.with_description(word);
                }
                if toolbar.notice.is_some() {
                    // A notice is read out as it arrives, without moving
                    // the keyboard.
                    node = node.with_live(crate::a11y::Politeness::Polite);
                }
                if location.editable || picker {
                    node = node
                        .with_action_target(LOCATION_TARGET)
                        .with_state(AccessState {
                            focusable: true,
                            expanded: picker.then(|| self.choices_open()),
                            ..Default::default()
                        });
                }
                if picker && self.choices_open() {
                    // The open picker's rows: each one goes there.
                    let count = location.choices.len();
                    node = node.with_children(
                        location
                            .choices
                            .iter()
                            .enumerate()
                            .map(|(index, choice)| {
                                AccessNode::new(Role::MenuItem)
                                    .with_name(choice.label.clone())
                                    .with_action_target(format!("{CHOICE_TARGET_PREFIX}{index}"))
                                    .with_position(index.saturating_add(1), count)
                                    .with_state(AccessState {
                                        focusable: true,
                                        checked: Some(choice.current),
                                        ..Default::default()
                                    })
                            })
                            .collect(),
                    );
                }
                nodes.push(node);
            }
        }
        // The location's levels: each ancestor goes there.
        if let Some(location) = toolbar.location.as_ref()
            && self.field.is_none()
            && self.slots.location.area() > 0.0
        {
            let ancestors = location.segments.len().saturating_sub(1);
            for (i, segment) in location.segments.iter().take(ancestors).enumerate() {
                nodes.push(button(
                    segment.label.clone(),
                    format!("{SEGMENT_TARGET_PREFIX}{i}"),
                    self.slots.location,
                    true,
                ));
            }
        }
        // The notice's buttons, in order right after its text.
        if let Some(notice) = toolbar.notice.as_ref() {
            for (action, rect) in notice.actions.iter().zip(&self.slots.notice) {
                nodes.push(button(
                    action.name.clone(),
                    format!("{NOTICE_TARGET_PREFIX}{}", action.id),
                    *rect,
                    action.enabled,
                ));
            }
        }
        // A compact window's notice, in its strip under the row: read out
        // as it arrives, its buttons after it.
        if let Some((notice, text, _, at)) = self.strip_notice() {
            nodes.push(
                AccessNode::new(Role::StaticText)
                    .with_name(notice.text.clone())
                    .with_bounds(text)
                    .with_live(crate::a11y::Politeness::Polite),
            );
            for (action, rect) in notice.actions.iter().zip(&at) {
                nodes.push(button(
                    action.name.clone(),
                    format!("{NOTICE_TARGET_PREFIX}{}", action.id),
                    *rect,
                    action.enabled,
                ));
            }
        }
        // The views the row shows as tabs, the chosen one selected; without room
        // they are the overflow menu's.
        if let Some(views) = toolbar.views.as_ref() {
            for (view, rect) in views.choices.iter().zip(&self.slots.views) {
                let mut node = button(
                    view.label.clone(),
                    format!("{VIEW_TARGET_PREFIX}{}", view.id),
                    *rect,
                    true,
                );
                node.role = Role::Tab;
                node.state.selected = view.id == views.chosen;
                if !views.name.is_empty() {
                    node = node.with_description(views.name.clone());
                }
                nodes.push(node);
            }
            // Folded, the views are a menu that names the chosen one; its
            // items run while it is closed, as the overflow menu's do.
            if self.slots.views_menu.area() > 0.0 {
                let chosen = views.chosen_index().and_then(|i| views.choices.get(i));
                let views_name = if views.name.is_empty() {
                    crate::i18n::I18nManager::lookup_plain("window-toolbar-views")
                } else {
                    views.name.clone()
                };
                // As the views' picker the location is named by what it
                // shows, its period, and says it picks the view.
                let in_location = toolbar
                    .location
                    .as_ref()
                    .filter(|_| self.slots.views_in_location());
                let name = in_location.map_or_else(|| views_name.clone(), |l| l.label.clone());
                let mut menu = button(
                    name.clone(),
                    VIEWS_MENU_TARGET.into(),
                    self.slots.views_menu,
                    true,
                )
                .with_optional_value(chosen.map(|view| view.label.clone()));
                if in_location.is_some() {
                    menu = menu.with_description(views_name);
                }
                menu.state.expanded = Some(self.menu_open_for(&Slot::ViewsMenu));
                menu = menu.with_children(vec![self.menu_node(
                    name,
                    &Slot::ViewsMenu,
                    self.slots.views_menu,
                    (0..views.choices.len()).map(MenuEntry::View),
                )]);
                nodes.push(menu);
            }
        }
        // The actions the row shows; a menu button's items are its menu's.
        for ((i, action), rect) in toolbar.actions.iter().enumerate().zip(&self.slots.actions) {
            let mut node = button(
                action.name.clone(),
                format!("{ACTION_TARGET_PREFIX}{}", action.id),
                *rect,
                action.enabled,
            );
            if action.toggles || action.on {
                node.role = Role::ToggleButton;
                node.state.checked = Some(action.on);
            }
            if !action.items.is_empty() {
                let slot = Slot::Action(i);
                node.state.expanded = Some(self.menu_open_for(&slot));
                let entries = (0..action.items.len()).map(|j| MenuEntry::Item(i, j));
                node = node.with_children(vec![self.menu_node(
                    action.name.clone(),
                    &slot,
                    *rect,
                    entries,
                )]);
            }
            nodes.push(node);
        }
        // What the row has no room for is the overflow menu's. Its items
        // are there for assistive technology and automation to run even
        // while it is closed, hidden from a reader until it opens, each
        // where its row is drawn while it is open.
        if self.slots.overflow.area() > 0.0 {
            let name = crate::i18n::I18nManager::lookup_plain("window-toolbar-more");
            let mut more = button(
                name.clone(),
                OVERFLOW_TARGET.into(),
                self.slots.overflow,
                true,
            );
            more.state.expanded = Some(self.menu_open_for(&Slot::Overflow));
            let entries = self.overflow_entries();
            if !entries.is_empty() {
                more = more.with_children(vec![self.menu_node(
                    name,
                    &Slot::Overflow,
                    self.slots.overflow,
                    entries.into_iter(),
                )]);
            }
            nodes.push(more);
        }
        nodes.extend(self.reaction_nodes());
        nodes
    }

    /// What the overflow menu lists, in its order: the views the row has
    /// no room for, then the leading actions and the actions it does not
    /// show, a menu button's items in its place.
    pub(super) fn overflow_entries(&self) -> Vec<MenuEntry> {
        let Some(toolbar) = self.toolbar() else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        if self.views_in_overflow()
            && let Some(views) = toolbar.views.as_ref()
        {
            entries.extend((0..views.choices.len()).map(MenuEntry::View));
        }
        entries.extend(self.slots.folded_leading().map(MenuEntry::Leading));
        for (i, action) in toolbar
            .actions
            .iter()
            .enumerate()
            .skip(self.slots.actions.len())
        {
            if action.items.is_empty() {
                entries.push(MenuEntry::Action(i));
            } else {
                entries.extend((0..action.items.len()).map(|j| MenuEntry::Item(i, j)));
            }
        }
        entries
    }

    /// `slot`'s menu, named `name`, as assistive technology reaches it:
    /// one item per entry, each running what its row runs. While the menu
    /// is open each item is where its row is drawn; while it is closed the
    /// items are hidden from a reader and stand at `anchor`, the control
    /// that opens the menu.
    pub(super) fn menu_node(
        &self,
        name: String,
        slot: &Slot,
        anchor: Rect,
        entries: impl Iterator<Item = MenuEntry>,
    ) -> crate::a11y::AccessNode {
        use crate::a11y::{AccessNode, AccessState, Role};
        let open = self.menu_open_for(slot);
        let entries: Vec<MenuEntry> = entries.collect();
        let count = entries.len();
        let items = entries
            .iter()
            .enumerate()
            .filter_map(|(position, entry)| {
                let (name, target, enabled, checked) = self.entry_semantics(*entry)?;
                let rect = open
                    .then(|| {
                        self.menu_entries
                            .iter()
                            .position(|row| *row == Some(*entry))
                            .and_then(|row| self.menu.item_rect(row))
                    })
                    .flatten()
                    .unwrap_or(anchor);
                Some(
                    AccessNode::new(Role::MenuItem)
                        .with_name(name)
                        .with_action_target(target)
                        .with_bounds(rect)
                        .with_position(position.saturating_add(1), count)
                        .with_state(AccessState {
                            focusable: enabled && open,
                            disabled: !enabled,
                            checked,
                            hidden: !open,
                            ..Default::default()
                        }),
                )
            })
            .collect();
        AccessNode::new(Role::Menu)
            .with_name(name)
            .with_children(items)
            .with_state(AccessState {
                hidden: !open,
                ..Default::default()
            })
    }

    /// A menu entry's name, action target, whether it runs, and its check.
    pub(super) fn entry_semantics(
        &self,
        entry: MenuEntry,
    ) -> Option<(String, String, bool, Option<bool>)> {
        let toolbar = self.toolbar()?;
        let action = |action: &crate::window_toolbar::ToolbarAction| {
            (
                action.name.clone(),
                format!("{ACTION_TARGET_PREFIX}{}", action.id),
                action.enabled,
                (action.toggles || action.on).then_some(action.on),
            )
        };
        match entry {
            MenuEntry::Action(i) => toolbar.actions.get(i).map(action),
            MenuEntry::Leading(i) => toolbar.leading.get(i).map(|leading| {
                (
                    leading.name.clone(),
                    format!("{LEADING_TARGET_PREFIX}{}", leading.id),
                    leading.enabled,
                    (leading.toggles || leading.on).then_some(leading.on),
                )
            }),
            MenuEntry::Item(i, j) => toolbar
                .actions
                .get(i)
                .and_then(|menu| menu.items.get(j))
                .map(action),
            MenuEntry::View(i) => {
                let views = toolbar.views.as_ref()?;
                let view = views.choices.get(i)?;
                Some((
                    view.label.clone(),
                    format!("{VIEW_TARGET_PREFIX}{}", view.id),
                    true,
                    Some(view.id == views.chosen),
                ))
            }
            MenuEntry::Choice(_) => None,
        }
    }

    /// The listed suggestions as assistive technology reaches them: each
    /// one an item that commits it, its line under the title its
    /// description.
    pub(super) fn suggestion_nodes(&self) -> Vec<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        if !self.suggestions_open() {
            return Vec::new();
        }
        let rows = self.suggestions.suggestions();
        let list = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let rect = self.suggestions.row_rect(index)?;
                let mut node = AccessNode::new(Role::ListItem)
                    .with_name(&row.label)
                    .with_action_target(format!("{SUGGESTION_TARGET_PREFIX}{index}"))
                    .with_bounds(rect)
                    .with_position(index.saturating_add(1), rows.len())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(self.suggestions.highlight() == Some(index)),
                    );
                if let Some(subtitle) = &row.subtitle {
                    node = node.with_description(subtitle);
                }
                Some(node)
            })
            .collect();
        let mut node = AccessNode::new(Role::List)
            .with_name(crate::i18n::I18nManager::lookup_plain(
                "window-toolbar-suggestions",
            ))
            .with_children(list);
        if let Some(panel) = self.suggestions.rect() {
            node = node.with_bounds(panel);
        }
        vec![node]
    }

    /// Run the slot `target` names, as assistive technology activates it.
    pub(crate) fn perform(&mut self, target: &str) -> bool {
        if let Some(ran) = self.perform_reaction(target) {
            return ran;
        }
        if let Some(index) = target
            .strip_prefix(SUGGESTION_TARGET_PREFIX)
            .and_then(|index| index.parse::<usize>().ok())
        {
            return self.suggestions_open() && self.commit_suggestion(index);
        }
        if let Some(index) = target
            .strip_prefix(CHOICE_TARGET_PREFIX)
            .and_then(|index| index.parse::<usize>().ok())
        {
            return self.commit_choice(index);
        }
        let slot = match target {
            SIDEBAR_TARGET => Slot::Sidebar,
            BACK_TARGET => Slot::Back,
            FORWARD_TARGET => Slot::Forward,
            LOCATION_TARGET => Slot::Location,
            OVERFLOW_TARGET => Slot::Overflow,
            VIEWS_MENU_TARGET => Slot::ViewsMenu,
            _ => {
                if let Some(level) = target
                    .strip_prefix(SEGMENT_TARGET_PREFIX)
                    .and_then(|level| level.parse::<usize>().ok())
                {
                    let Some(value) = self
                        .toolbar()
                        .and_then(|toolbar| toolbar.location.as_ref())
                        .and_then(|location| location.segments.get(level))
                        .map(|segment| segment.value.clone())
                    else {
                        return false;
                    };
                    self.push(ToolbarEvent::Commit { text: value });
                    return true;
                }
                if let Some(id) = target.strip_prefix(NOTICE_TARGET_PREFIX) {
                    let Some(i) = self
                        .notice_up()
                        .and_then(|notice| notice.actions.iter().position(|a| a.id == id))
                    else {
                        return false;
                    };
                    return self.activate(&Slot::Notice(i));
                }
                if let Some(id) = target.strip_prefix(LEADING_TARGET_PREFIX) {
                    let Some(i) = self
                        .toolbar()
                        .and_then(|toolbar| toolbar.leading.iter().position(|a| a.id == id))
                    else {
                        return false;
                    };
                    return self.run_leading(i);
                }
                if let Some(id) = target.strip_prefix(VIEW_TARGET_PREFIX) {
                    let Some(i) = self
                        .toolbar()
                        .and_then(|toolbar| toolbar.views.as_ref())
                        .and_then(|views| views.choices.iter().position(|v| v.id == id))
                    else {
                        return false;
                    };
                    // A view in the overflow menu is chosen as one in the row is.
                    let chosen = self.choose_view(i);
                    return self.ran_from_menu(chosen);
                }
                let Some(id) = target.strip_prefix(ACTION_TARGET_PREFIX) else {
                    return false;
                };
                let Some(toolbar) = self.toolbar() else {
                    return false;
                };
                if let Some(i) = toolbar.actions.iter().position(|a| a.id == id) {
                    // An action in the overflow menu runs as one in the row
                    // does; a menu button opens its menu.
                    let opens = toolbar.actions.get(i).is_some_and(|a| !a.items.is_empty());
                    let ran = self.activate(&Slot::Action(i));
                    return if opens { ran } else { self.ran_from_menu(ran) };
                }
                let Some((i, j)) = toolbar.actions.iter().enumerate().find_map(|(i, action)| {
                    action
                        .items
                        .iter()
                        .position(|item| item.id == id)
                        .map(|j| (i, j))
                }) else {
                    return false;
                };
                // A menu button's item runs without its menu open.
                let ran = self.run_item(i, j);
                return self.ran_from_menu(ran);
            }
        };
        self.activate(&slot)
    }
}
