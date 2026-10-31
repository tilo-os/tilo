//! The store's contents held with its schema table installed on the thread: what a read or a
//! write resolves a record's type by is the package version this store loaded, not another
//! store's on the same thread (`TiloStore::held`, `TiloStore::changing`).

use super::Inner;

/// The store's contents, held for a read with the store's schema table installed.
pub(super) struct Held<'a> {
    inner: std::cell::Ref<'a, Inner>,
    _scope: crate::schema::Current,
}

impl<'a> Held<'a> {
    /// The store's contents with its schema table installed: what a read resolves a
    /// record's type by is this store's package version, never another store's.
    pub(super) fn of(
        schemas: std::sync::Arc<crate::schema::Schemas>,
        inner: &'a std::cell::RefCell<Inner>,
    ) -> Self {
        let scope = schemas.enter();
        Self {
            inner: inner.borrow(),
            _scope: scope,
        }
    }
}

impl std::ops::Deref for Held<'_> {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.inner
    }
}

/// The store's contents, held to change with the store's schema table installed.
pub(super) struct Changing<'a> {
    inner: std::cell::RefMut<'a, Inner>,
    _scope: crate::schema::Current,
}

impl<'a> Changing<'a> {
    /// The store's contents, to change, with its schema table installed.
    pub(super) fn of(
        schemas: std::sync::Arc<crate::schema::Schemas>,
        inner: &'a std::cell::RefCell<Inner>,
    ) -> Self {
        let scope = schemas.enter();
        Self {
            inner: inner.borrow_mut(),
            _scope: scope,
        }
    }
}

impl std::ops::Deref for Changing<'_> {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.inner
    }
}

impl std::ops::DerefMut for Changing<'_> {
    fn deref_mut(&mut self) -> &mut Inner {
        &mut self.inner
    }
}
