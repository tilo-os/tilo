//! An app's typed settings, files and secrets as views and surfaces reach
//! them: from their context, scoped to the app and the settings document its
//! host gave it.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use ui_core::LocalState;
use ui_core::storage::{AppStorage, Settings, SettingsRevision, SettingsSchema};

/// One typed settings value, kept current while the view lives: a write
/// from this window, another window, the Settings app or another of the
/// person's devices (for [`Scope::Personal`](ui_core::storage::Scope))
/// reaches [`Self::state`] on the next frame.
///
/// Bind controls to [`Self::state`] and write with [`Self::set`]. Settings
/// are not the document: they are not undone, not in history and never
/// seen by collaborators.
pub struct Setting<T> {
    settings: Settings<T>,
    value: LocalState<T>,
}

impl<T> Clone for Setting<T> {
    fn clone(&self) -> Self {
        Self {
            settings: self.settings.clone(),
            value: self.value.clone(),
        }
    }
}

impl<T: SettingsSchema + Clone + PartialEq + Send + Sync> Setting<T> {
    /// The current value.
    #[must_use]
    pub fn get(&self) -> T {
        self.value.with(Clone::clone)
    }

    /// The value as state that follows every write.
    #[must_use]
    pub fn state(&self) -> LocalState<T> {
        self.value.clone()
    }

    /// Store `value`.
    ///
    /// # Errors
    ///
    /// When the document refuses it (see [`Settings::set`]); the value shown
    /// stays the stored one.
    pub fn set(&self, value: T) -> std::io::Result<()> {
        self.settings.set(&value)?;
        self.value.set(value);
        Ok(())
    }

    /// Change the stored value with `change`.
    ///
    /// # Errors
    ///
    /// As [`Self::set`].
    pub fn update(&self, change: impl FnOnce(&mut T)) -> std::io::Result<()> {
        let value = self.settings.update(change)?;
        self.value.set(value);
        Ok(())
    }

    /// The underlying settings, for code that runs off the view (a job).
    #[must_use]
    pub fn settings(&self) -> &Settings<T> {
        &self.settings
    }
}

type Refresh = Box<dyn FnMut()>;

/// The app's storage and the settings its views follow. Shared by every
/// scope of one root; the root refreshes it each sync.
pub(crate) struct ViewStorage {
    storage: AppStorage,
    followed: RefCell<Vec<(Weak<()>, Refresh)>>,
}

impl ViewStorage {
    pub(crate) fn new(storage: AppStorage) -> Rc<Self> {
        Rc::new(Self {
            storage,
            followed: RefCell::default(),
        })
    }

    pub(crate) fn storage(&self) -> &AppStorage {
        &self.storage
    }

    /// `T`, followed for as long as `alive` is.
    pub(crate) fn follow<T>(&self, alive: Weak<()>) -> Setting<T>
    where
        T: SettingsSchema + Clone + PartialEq + Send + Sync,
    {
        let settings = self.storage.settings::<T>();
        let mut seen: SettingsRevision = settings.revision();
        let value = LocalState::new(settings.get());
        let setting = Setting {
            settings: settings.clone(),
            value: value.clone(),
        };
        self.followed.borrow_mut().push((
            alive,
            Box::new(move || {
                let revision = settings.revision();
                if revision == seen {
                    return;
                }
                seen = revision;
                let stored = settings.get();
                if value.with(|shown| *shown != stored) {
                    value.set(stored);
                }
            }),
        ));
        setting
    }

    /// Read every followed setting whose document changed, dropping those
    /// of views that are gone.
    pub(crate) fn refresh(&self) {
        // Take the list so a refresh that builds a view (a switch reacting
        // to the new value) may follow more without a reentrant borrow.
        let mut current = std::mem::take(&mut *self.followed.borrow_mut());
        current.retain_mut(|(alive, refresh)| {
            if alive.strong_count() == 0 {
                return false;
            }
            refresh();
            true
        });
        let mut list = self.followed.borrow_mut();
        current.append(&mut list);
        *list = current;
    }
}
