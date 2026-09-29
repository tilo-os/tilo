//! macOS host chrome: the menu bar and a title-bar search field, drawn by
//! AppKit from the host command model.
//!
//! Every item a person can choose is a [`HostCommand`]; choosing it hands the
//! command's invocation back to the app, which enters the same door as the
//! palette. AppKit asks `validateMenuItem:` for enabled state, so items follow
//! each action's availability, and the Help menu's search finds them all.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSControlTextEditingDelegate, NSEventModifierFlags, NSMenu, NSMenuItem,
    NSMenuItemValidation, NSSearchField, NSSearchFieldDelegate, NSSearchToolbarItem,
    NSTextFieldDelegate, NSToolbar, NSToolbarDelegate, NSToolbarItem, NSView, NSWindowToolbarStyle,
};
use objc2_foundation::{NSArray, NSProcessInfo, NSString, ns_string};
use tilo_actions::ControlInvocation;

use super::menus::{self, Entry, MenuKind};
use super::model::{CommandGroup, HostCommand};
use crate::i18n::I18nManager;
use crate::platform::Shortcut;

/// What a person asked of the app through the host chrome.
pub(crate) enum ChromeRequest {
    Command(ControlInvocation),
    /// Text typed into the title-bar search field, to continue in the palette.
    Search(String),
}

const SEARCH_ITEM: &str = "so.tilo.host.search";

pub(crate) struct Ivars {
    commands: RefCell<Vec<HostCommand>>,
    dispatch: Box<dyn Fn(ChromeRequest)>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and `Target` has no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TiloHostChromeTarget"]
    #[ivars = Ivars]
    pub(crate) struct Target;

    impl Target {
        #[unsafe(method(performHostCommand:))]
        fn perform_host_command(&self, item: &NSMenuItem) {
            let invocation = self
                .ivars()
                .commands
                .borrow()
                .get(usize::try_from(item.tag()).unwrap_or(usize::MAX))
                .filter(|command| command.enabled)
                .and_then(HostCommand::invocation);
            if let Some(invocation) = invocation {
                (self.ivars().dispatch)(ChromeRequest::Command(invocation));
            }
        }
    }

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSMenuItemValidation for Target {
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            self.ivars()
                .commands
                .borrow()
                .get(usize::try_from(item.tag()).unwrap_or(usize::MAX))
                .is_some_and(|command| command.enabled)
        }
    }

    unsafe impl NSToolbarDelegate for Target {
        #[unsafe(method_id(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:))]
        fn toolbar_item(
            &self,
            _toolbar: &NSToolbar,
            identifier: &NSString,
            _inserted: bool,
        ) -> Option<Retained<NSToolbarItem>> {
            (identifier.to_string() == SEARCH_ITEM).then(|| {
                let item = NSSearchToolbarItem::initWithItemIdentifier(
                    NSSearchToolbarItem::alloc(self.mtm()),
                    identifier,
                );
                let field = item.searchField();
                field.setPlaceholderString(Some(&NSString::from_str(&I18nManager::lookup_plain(
                    "host-search-placeholder",
                ))));
                // SAFETY: the field holds its delegate weakly; `MacChrome`
                // keeps `Target` alive for as long as the toolbar exists.
                unsafe { field.setDelegate(Some(ProtocolObject::from_ref(self))) };
                Retained::into_super(item)
            })
        }

        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn default_items(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            toolbar_items()
        }

        #[unsafe(method_id(toolbarAllowedItemIdentifiers:))]
        fn allowed_items(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            toolbar_items()
        }
    }

    unsafe impl NSControlTextEditingDelegate for Target {}
    unsafe impl NSTextFieldDelegate for Target {}

    unsafe impl NSSearchFieldDelegate for Target {
        /// Search continues in the palette, where results can act; the field
        /// hands over what was typed and gives the keyboard back.
        #[unsafe(method(searchFieldDidStartSearching:))]
        fn search_started(&self, field: &NSSearchField) {
            let text = field.stringValue().to_string();
            field.setStringValue(ns_string!(""));
            if let Some(window) = field.window() {
                let content = window.contentView();
                window.makeFirstResponder(content.as_deref().map(|view| &**view));
            }
            (self.ivars().dispatch)(ChromeRequest::Search(text));
        }
    }
);

/// The app's menu bar and title-bar toolbar, rebuilt when commands change.
pub(crate) struct MacChrome {
    mtm: MainThreadMarker,
    target: Retained<Target>,
    menu_bar: Retained<NSMenu>,
    edit: Retained<NSMenu>,
    view: Retained<NSMenu>,
    history: Retained<NSMenu>,
    history_item: Option<Retained<NSMenuItem>>,
    window: Retained<NSMenu>,
    help: Retained<NSMenu>,
    shape: RefCell<Vec<(String, CommandGroup, Option<Shortcut>)>>,
    toolbar: RefCell<Option<Retained<NSToolbar>>>,
}

impl MacChrome {
    pub(crate) fn new(mtm: MainThreadMarker, dispatch: impl Fn(ChromeRequest) + 'static) -> Self {
        let target: Retained<Target> = {
            let this = Target::alloc(mtm).set_ivars(Ivars {
                commands: RefCell::new(Vec::new()),
                dispatch: Box::new(dispatch),
            });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        };
        let menu_bar = NSMenu::new(mtm);
        let app_name = NSProcessInfo::processInfo().processName().to_string();
        let named = |key: &str| {
            let mut args = fluent::FluentArgs::new();
            args.set("app", app_name.clone());
            I18nManager::lookup(key, Some(&args))
        };

        let app = submenu(mtm, &menu_bar, &app_name);
        let services = NSMenu::new(mtm);
        app.addItem(&item(
            mtm,
            &named("host-menu-about"),
            Some(sel!(orderFrontStandardAboutPanel:)),
            None,
        ));
        app.addItem(&NSMenuItem::separatorItem(mtm));
        let services_item = item(
            mtm,
            &I18nManager::lookup_plain("host-menu-services"),
            None,
            None,
        );
        services_item.setSubmenu(Some(&services));
        app.addItem(&services_item);
        app.addItem(&NSMenuItem::separatorItem(mtm));
        app.addItem(&item(
            mtm,
            &named("host-menu-hide"),
            Some(sel!(hide:)),
            Some(("h", NSEventModifierFlags::Command)),
        ));
        app.addItem(&item(
            mtm,
            &I18nManager::lookup_plain("host-menu-hide-others"),
            Some(sel!(hideOtherApplications:)),
            Some((
                "h",
                NSEventModifierFlags::Command | NSEventModifierFlags::Option,
            )),
        ));
        app.addItem(&item(
            mtm,
            &I18nManager::lookup_plain("host-menu-show-all"),
            Some(sel!(unhideAllApplications:)),
            None,
        ));
        app.addItem(&NSMenuItem::separatorItem(mtm));
        app.addItem(&item(
            mtm,
            &named("host-menu-quit"),
            Some(sel!(terminate:)),
            Some(("q", NSEventModifierFlags::Command)),
        ));

        let file = submenu(mtm, &menu_bar, &I18nManager::lookup_plain("host-menu-file"));
        file.addItem(&item(
            mtm,
            &I18nManager::lookup_plain("host-menu-close"),
            Some(sel!(performClose:)),
            Some(("w", NSEventModifierFlags::Command)),
        ));
        let edit = submenu(
            mtm,
            &menu_bar,
            &I18nManager::lookup_plain(MenuKind::Edit.title_key()),
        );
        let view = submenu(
            mtm,
            &menu_bar,
            &I18nManager::lookup_plain(MenuKind::View.title_key()),
        );
        let history = submenu(
            mtm,
            &menu_bar,
            &I18nManager::lookup_plain(MenuKind::History.title_key()),
        );
        // The item just added, hidden while the app has no History to show.
        let history_item = menu_bar.itemAtIndex(menu_bar.numberOfItems() - 1);
        if let Some(item) = &history_item {
            item.setHidden(true);
        }
        let window = submenu(
            mtm,
            &menu_bar,
            &I18nManager::lookup_plain("host-menu-window"),
        );
        window.addItem(&item(
            mtm,
            &I18nManager::lookup_plain("host-menu-minimize"),
            Some(sel!(performMiniaturize:)),
            Some(("m", NSEventModifierFlags::Command)),
        ));
        window.addItem(&item(
            mtm,
            &I18nManager::lookup_plain("host-menu-zoom"),
            Some(sel!(performZoom:)),
            None,
        ));
        window.addItem(&NSMenuItem::separatorItem(mtm));
        window.addItem(&item(
            mtm,
            &I18nManager::lookup_plain("host-menu-front"),
            Some(sel!(arrangeInFront:)),
            None,
        ));
        let help = submenu(mtm, &menu_bar, &I18nManager::lookup_plain("host-menu-help"));
        Self {
            mtm,
            target,
            menu_bar,
            edit,
            view,
            history,
            history_item,
            window,
            help,
            shape: RefCell::new(Vec::new()),
            toolbar: RefCell::new(None),
        }
    }

    /// Replace winit's default menu with this one. AppKit then adds its own
    /// window list, full-screen item and Help search.
    pub(crate) fn install(&self, app: &NSApplication) {
        app.setMainMenu(Some(&self.menu_bar));
        app.setWindowsMenu(Some(&self.window));
        app.setHelpMenu(Some(&self.help));
    }

    /// Give `view`'s window a unified title bar with the system search field.
    pub(crate) fn attach_toolbar(&self, view: &NSView) {
        let Some(window) = view.window() else {
            return;
        };
        let toolbar =
            NSToolbar::initWithIdentifier(NSToolbar::alloc(self.mtm), ns_string!("so.tilo.host"));
        toolbar.setDelegate(Some(ProtocolObject::from_ref(&*self.target)));
        toolbar.setAllowsUserCustomization(false);
        window.setToolbar(Some(&toolbar));
        window.setToolbarStyle(NSWindowToolbarStyle::Unified);
        *self.toolbar.borrow_mut() = Some(toolbar);
    }

    /// Present `commands`. Menus are rebuilt only when an item's title, place
    /// or shortcut changed; enabled state is read live by `validateMenuItem:`.
    pub(crate) fn update(&self, commands: Vec<HostCommand>) {
        let shape: Vec<_> = commands
            .iter()
            .map(|command| {
                (
                    command.label.clone(),
                    command.group,
                    command.shortcut.clone(),
                )
            })
            .collect();
        let rebuild = *self.shape.borrow() != shape;
        *self.target.ivars().commands.borrow_mut() = commands;
        if rebuild {
            self.rebuild();
            *self.shape.borrow_mut() = shape;
        }
    }

    fn rebuild(&self) {
        let mtm = self.mtm;
        for menu in [&self.edit, &self.view, &self.history] {
            menu.removeAllItems();
        }
        let commands = self.target.ivars().commands.borrow();
        for (kind, menu) in MenuKind::ALL
            .into_iter()
            .zip([&self.edit, &self.view, &self.history])
        {
            for entry in menus::entries(&commands, kind) {
                let Entry::Command(index) = entry else {
                    menu.addItem(&NSMenuItem::separatorItem(mtm));
                    continue;
                };
                let command = &commands[index];
                let key = command.shortcut.as_ref().map(|shortcut| {
                    let mut flags = NSEventModifierFlags::Command;
                    if shortcut.has_shift() {
                        flags |= NSEventModifierFlags::Shift;
                    }
                    if shortcut.has_alt() {
                        flags |= NSEventModifierFlags::Option;
                    }
                    (shortcut.key().to_lowercase(), flags)
                });
                let item = item(
                    mtm,
                    &command.label,
                    Some(sel!(performHostCommand:)),
                    key.as_ref().map(|(key, flags)| (key.as_str(), *flags)),
                );
                item.setTag(isize::try_from(index).unwrap_or(isize::MAX));
                // SAFETY: `Target` implements `performHostCommand:` and
                // outlives the menus, which `MacChrome` owns alongside it.
                let target: &AnyObject = &self.target;
                unsafe { item.setTarget(Some(target)) };
                menu.addItem(&item);
            }
        }
        if let Some(item) = &self.history_item {
            item.setHidden(self.history.numberOfItems() == 0);
        }
    }
}

/// The search field at the trailing edge, as in Finder and Mail.
fn toolbar_items() -> Retained<NSArray<NSString>> {
    NSArray::from_retained_slice(&[
        NSString::from_str("NSToolbarFlexibleSpaceItem"),
        NSString::from_str(SEARCH_ITEM),
    ])
}

fn submenu(mtm: MainThreadMarker, bar: &NSMenu, title: &str) -> Retained<NSMenu> {
    let title = NSString::from_str(title);
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &title);
    let holder = NSMenuItem::new(mtm);
    holder.setTitle(&title);
    holder.setSubmenu(Some(&menu));
    bar.addItem(&holder);
    menu
}

fn item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<Sel>,
    key: Option<(&str, NSEventModifierFlags)>,
) -> Retained<NSMenuItem> {
    let (equivalent, flags) = key.unwrap_or(("", NSEventModifierFlags::Command));
    // SAFETY: `action` is either a standard responder-chain selector or one
    // `Target` implements; the key equivalent is a plain string.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(equivalent),
        )
    };
    item.setKeyEquivalentModifierMask(flags);
    item
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use automerge::ROOT;
    use automerge::transaction::Transactable;
    use objc2::{MainThreadMarker, msg_send};
    use objc2_app_kit::{NSEventModifierFlags, NSMenu, NSMenuItem};

    use super::{ChromeRequest, MacChrome};
    use crate::app::{App, framework_history_tests};

    fn titles(menu: &NSMenu) -> Vec<String> {
        (0..menu.numberOfItems())
            .filter_map(|index| menu.itemAtIndex(index))
            .map(|item| item.title().to_string())
            .collect()
    }

    fn submenu(bar: &NSMenu, title: &str) -> objc2::rc::Retained<NSMenu> {
        bar.itemWithTitle(&objc2_foundation::NSString::from_str(title))
            .and_then(|item| item.submenu())
            .unwrap_or_else(|| panic!("{title} menu"))
    }

    #[test]
    fn history_menu_lists_the_command_model_and_dispatches_the_chosen_item() {
        // SAFETY: the test harness runs tests off the process main thread.
        // These menus are built, read and dropped on this one thread and are
        // never installed into a running NSApplication.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let mut state = crate::State::new();
        for title in ["first", "second"] {
            state.doc.put(ROOT, "title", title).unwrap();
            state.commit_action(None, None);
        }
        let mut app =
            App::new_with_controlled_inputs(state, Box::new(crate::widget::Text::new("Document")));
        app.enable_document_history_chrome();
        app.init_layout();

        let chosen: Rc<RefCell<Vec<ChromeRequest>>> = Rc::default();
        let sink = Rc::clone(&chosen);
        let chrome = MacChrome::new(mtm, move |request| sink.borrow_mut().push(request));
        chrome.update(app.host_commands(true));

        let bar = &chrome.menu_bar;
        assert_eq!(
            titles(bar)[1..],
            ["File", "Edit", "View", "History", "Window", "Help"],
            "History sits between View and Window"
        );
        let history = submenu(bar, "History");
        let items = titles(&history);
        assert_eq!(items[0], "Show History");
        assert!(items[1].is_empty(), "recent changes follow a separator");
        assert_eq!(items.len(), 4, "two recent changes: {items:?}");
        assert!(items[2..].iter().all(|title| title.contains(" · ")));
        let show: objc2::rc::Retained<NSMenuItem> = history.itemAtIndex(0).unwrap();
        assert_eq!(show.keyEquivalent().to_string(), "y");
        assert_eq!(
            show.keyEquivalentModifierMask(),
            NSEventModifierFlags::Command
        );

        let edit = titles(&submenu(bar, "Edit"));
        assert_eq!(
            edit[..2],
            ["Undo Edited title", "Redo"],
            "Undo names its change"
        );
        assert!(edit.contains(&"Search…".to_owned()));

        // AppKit asks the target whether an item is enabled, then sends its
        // action there: exactly what a click or ⌘Y does.
        let target = &*chrome.target;
        let enabled: bool = unsafe { msg_send![target, validateMenuItem: &*show] };
        assert!(enabled);
        let redo = submenu(bar, "Edit").itemAtIndex(1).unwrap();
        let enabled: bool = unsafe { msg_send![target, validateMenuItem: &*redo] };
        assert!(!enabled, "nothing to redo");
        let _: () = unsafe { msg_send![target, performHostCommand: &*show] };
        let Some(ChromeRequest::Command(invocation)) = chosen.borrow_mut().pop() else {
            panic!("choosing Show History dispatches its command");
        };
        assert!(app.invoke_host_command(&invocation));
        assert!(
            app.document_history_snapshot().unwrap().is_some(),
            "History opened"
        );

        chrome.update(app.host_commands(true));
        assert_eq!(
            titles(&submenu(bar, "History"))[..2],
            ["Hide History", "Compare Selection"]
        );
    }

    #[test]
    fn framework_history_menu_lists_recent_changes_and_opens_at_the_chosen_one() {
        // SAFETY: as above; the menus never leave this thread.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let mut app = framework_history_tests::native_chrome_app();

        let chosen: Rc<RefCell<Vec<ChromeRequest>>> = Rc::default();
        let sink = Rc::clone(&chosen);
        let chrome = MacChrome::new(mtm, move |request| sink.borrow_mut().push(request));
        chrome.update(app.host_commands(true));

        let history = submenu(&chrome.menu_bar, "History");
        let items = titles(&history);
        assert_eq!(items[0], "Show History");
        assert!(items[1].is_empty(), "recent changes follow a separator");
        assert_eq!(items.len(), 4, "two recent changes: {items:?}");
        assert!(items[2..].iter().all(|title| title.contains(" · ")));

        let newest = history.itemAtIndex(2).unwrap();
        let target = &*chrome.target;
        let enabled: bool = unsafe { msg_send![target, validateMenuItem: &*newest] };
        assert!(enabled);
        let _: () = unsafe { msg_send![target, performHostCommand: &*newest] };
        let Some(ChromeRequest::Command(invocation)) = chosen.borrow_mut().pop() else {
            panic!("choosing a recent change dispatches its command");
        };
        assert!(app.invoke_host_command(&invocation));
        framework_history_tests::layout(&mut app);
        chrome.update(app.host_commands(true));
        assert_eq!(
            titles(&submenu(&chrome.menu_bar, "History"))[0],
            "Hide History"
        );
    }

    #[test]
    fn framework_history_apps_search_from_the_title_bar_and_the_edit_menu() {
        // SAFETY: as above; the menus never leave this thread.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let mut app = framework_history_tests::native_chrome_app();
        let chosen: Rc<RefCell<Vec<ChromeRequest>>> = Rc::default();
        let sink = Rc::clone(&chosen);
        let chrome = MacChrome::new(mtm, move |request| sink.borrow_mut().push(request));
        chrome.update(app.host_commands(false));
        assert!(titles(&submenu(&chrome.menu_bar, "Edit")).contains(&"Search…".to_owned()));

        let field = objc2_app_kit::NSSearchField::new(mtm);
        field.setStringValue(&objc2_foundation::NSString::from_str("notes"));
        let target = &*chrome.target;
        let _: () = unsafe { msg_send![target, searchFieldDidStartSearching: &*field] };
        let Some(ChromeRequest::Search(query)) = chosen.borrow_mut().pop() else {
            panic!("the title-bar field hands its query over");
        };
        assert_eq!(query, "notes");
        // What the runner does with it: the Search command, then the query.
        assert!(
            app.invoke_host_shortcut(crate::platform::HostShortcut::Search),
            "the palette opens"
        );
    }
}
