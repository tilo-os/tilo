//! Windows host chrome: the window's menu bar (Edit, View, History) and the
//! taskbar Jump List, both built from the host command model.
//!
//! A chosen menu item hands its command's invocation back to the app through
//! `dispatch`; a Jump List task starts the app with `--host-command`, and
//! that process runs the invocation once its window exists. Either way it
//! enters the same door as the palette.

use std::cell::RefCell;
use std::sync::Mutex;

use tilo_actions::ControlInvocation;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, PROPERTYKEY, RPC_E_CHANGED_MODE, WPARAM};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    DefSubclassProc, DestinationList, EnumerableObjectCollection, ICustomDestinationList,
    IShellLinkW, RemoveWindowSubclass, SHStrDupW, SetWindowSubclass, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateMenu, CreatePopupMenu, DestroyMenu, DrawMenuBar, EnableMenuItem, HMENU,
    MF_BYCOMMAND, MF_ENABLED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, SetMenu, WM_COMMAND,
    WM_INITMENUPOPUP,
};
use windows::core::{GUID, HSTRING, Interface, PCWSTR};

use super::menus::{self, Entry, MenuKind};
use super::model::HostCommand;
use super::windows_items::{JumpTask, jump_tasks, menu_text};
use crate::i18n::I18nManager;
use crate::platform::{Platform, PlatformConventions};

/// Menu item ids start above the range Windows reserves for system commands
/// a window might forward.
const FIRST_ID: usize = 0x100;
const SUBCLASS_ID: usize = 0x7110;
/// `PKEY_Title`: the text a Jump List shows for a task.
const PKEY_TITLE: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9),
    pid: 2,
};

static STARTUP_COMMAND: Mutex<Option<ControlInvocation>> = Mutex::new(None);

/// The invocation this process was launched with, from a Jump List task.
pub(crate) fn request_startup_command(invocation: ControlInvocation) {
    if let Ok(mut pending) = STARTUP_COMMAND.lock() {
        *pending = Some(invocation);
    }
}

pub(crate) fn take_startup_command() -> Option<ControlInvocation> {
    STARTUP_COMMAND.lock().ok()?.take()
}

struct State {
    hwnd: HWND,
    commands: RefCell<Vec<HostCommand>>,
    shape: RefCell<Vec<String>>,
    tasks: RefCell<Vec<JumpTask>>,
    menu: RefCell<Option<HMENU>>,
    dispatch: Box<dyn Fn(ControlInvocation)>,
}

/// The menu bar and Jump List of one window, rebuilt when commands change.
pub(crate) struct WindowsChrome {
    state: Box<State>,
}

impl WindowsChrome {
    /// Take over `hwnd`'s menu messages. `hwnd` must be a live window of
    /// this thread, and this value must be dropped before it is destroyed.
    pub(crate) fn install(hwnd: HWND, dispatch: impl Fn(ControlInvocation) + 'static) -> Self {
        let state = Box::new(State {
            hwnd,
            commands: RefCell::new(Vec::new()),
            shape: RefCell::new(Vec::new()),
            tasks: RefCell::new(Vec::new()),
            menu: RefCell::new(None),
            dispatch: Box::new(dispatch),
        });
        // SAFETY: the reference data points into `state`'s heap allocation,
        // which lives until `Drop` removes this subclass.
        unsafe {
            let _ = SetWindowSubclass(
                hwnd,
                Some(subclass),
                SUBCLASS_ID,
                std::ptr::from_ref::<State>(&state) as usize,
            );
        }
        Self { state }
    }

    /// Present `commands`: the menu bar is rebuilt only when an item's text
    /// changed, the Jump List only when its tasks did.
    pub(crate) fn update(&self, commands: Vec<HostCommand>) {
        let conventions = PlatformConventions::new(Platform::Windows);
        let shape: Vec<_> = commands
            .iter()
            .map(|command| menu_text(command, conventions))
            .collect();
        let tasks = jump_tasks(&commands);
        *self.state.commands.borrow_mut() = commands;
        if *self.state.shape.borrow() != shape {
            self.rebuild_menu(conventions);
            *self.state.shape.borrow_mut() = shape;
        }
        if *self.state.tasks.borrow() != tasks {
            if let Err(error) = publish_jump_list(&tasks) {
                tracing::debug!(%error, "the Jump List was not published");
            }
            *self.state.tasks.borrow_mut() = tasks;
        }
    }

    fn rebuild_menu(&self, conventions: PlatformConventions) {
        let commands = self.state.commands.borrow();
        // SAFETY: every handle is created here and either attached to the
        // window or destroyed; the window belongs to this thread.
        unsafe {
            let Ok(bar) = CreateMenu() else {
                return;
            };
            for kind in MenuKind::ALL {
                let entries = menus::entries(&commands, kind);
                if entries.is_empty() {
                    continue;
                }
                let Ok(popup) = CreatePopupMenu() else {
                    continue;
                };
                for entry in entries {
                    let _ = match entry {
                        Entry::Separator => AppendMenuW(popup, MF_SEPARATOR, 0, PCWSTR::null()),
                        Entry::Command(index) => {
                            let command = &commands[index];
                            let flags = if command.enabled {
                                MF_STRING
                            } else {
                                MF_STRING | MF_GRAYED
                            };
                            let text = HSTRING::from(menu_text(command, conventions));
                            AppendMenuW(popup, flags, FIRST_ID + index, &text)
                        }
                    };
                }
                let title = HSTRING::from(I18nManager::lookup_plain(kind.title_key()));
                let _ = AppendMenuW(bar, MF_POPUP, popup.0 as usize, &title);
            }
            if SetMenu(self.state.hwnd, Some(bar)).is_ok() {
                if let Some(old) = self.state.menu.replace(Some(bar)) {
                    let _ = DestroyMenu(old);
                }
                let _ = DrawMenuBar(self.state.hwnd);
            } else {
                let _ = DestroyMenu(bar);
            }
        }
    }
}

impl Drop for WindowsChrome {
    fn drop(&mut self) {
        // SAFETY: removes the subclass installed in `install` before its
        // reference data is freed.
        unsafe {
            let _ = RemoveWindowSubclass(self.state.hwnd, Some(subclass), SUBCLASS_ID);
        }
    }
}

/// Menu choices arrive as `WM_COMMAND`; opening a menu asks for fresh
/// enabled states, as `validateMenuItem:` does on macOS.
unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    // SAFETY: `data` is the `State` `install` registered, alive while the
    // subclass is.
    let state = unsafe { &*(data as *const State) };
    match message {
        // A menu choice, not an accelerator (high word 1) or a control's
        // notification (non-zero lParam).
        WM_COMMAND if lparam.0 == 0 && high_word(wparam.0) == 0 => {
            let index = low_word(wparam.0).wrapping_sub(FIRST_ID);
            let invocation = state
                .commands
                .borrow()
                .get(index)
                .filter(|command| command.enabled)
                .and_then(HostCommand::invocation);
            if let Some(invocation) = invocation {
                (state.dispatch)(invocation);
                return LRESULT(0);
            }
        }
        WM_INITMENUPOPUP => {
            if let Some(menu) = *state.menu.borrow() {
                for (index, command) in state.commands.borrow().iter().enumerate() {
                    let flags = if command.enabled {
                        MF_BYCOMMAND | MF_ENABLED
                    } else {
                        MF_BYCOMMAND | MF_GRAYED
                    };
                    let Ok(id) = u32::try_from(FIRST_ID + index) else {
                        continue;
                    };
                    // SAFETY: `menu` is the live bar attached to this window.
                    let _ = unsafe { EnableMenuItem(menu, id, flags) };
                }
            }
        }
        _ => {}
    }
    // SAFETY: forwards every other message to the window's own procedure.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

fn low_word(value: usize) -> usize {
    value & 0xffff
}

fn high_word(value: usize) -> usize {
    (value >> 16) & 0xffff
}

fn publish_jump_list(tasks: &[JumpTask]) -> windows::core::Result<()> {
    let executable = std::env::current_exe().map_err(|_| windows::core::Error::empty())?;
    let executable = HSTRING::from(executable.as_os_str());
    // SAFETY: COM is initialized on this thread for the calls below; every
    // UTF-16 buffer outlives the call it is passed to, and PROPVARIANT's
    // Drop frees the SHStrDupW string with the matching allocator.
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if initialized.is_err() && initialized != RPC_E_CHANGED_MODE {
            initialized.ok()?;
        }
        let list: ICustomDestinationList =
            CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)?;
        let mut slots = 0u32;
        let _removed: IObjectArray = list.BeginList(&raw mut slots)?;
        let collection: IObjectCollection =
            CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?;
        for task in tasks {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&executable)?;
            link.SetArguments(&HSTRING::from(task.arguments.as_str()))?;
            let store: IPropertyStore = link.cast()?;
            let mut title = PROPVARIANT::default();
            (*title.Anonymous.Anonymous).vt = VT_LPWSTR;
            (*title.Anonymous.Anonymous).Anonymous.pwszVal =
                SHStrDupW(&HSTRING::from(task.title.as_str()))?;
            store.SetValue(&PKEY_TITLE, &raw const title)?;
            store.Commit()?;
            collection.AddObject(&link)?;
        }
        list.AddUserTasks(&collection.cast::<IObjectArray>()?)?;
        list.CommitList()
    }
}
