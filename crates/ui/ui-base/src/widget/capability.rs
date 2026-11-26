//! Capabilities: behaviour a widget takes part in beyond pixels and input.
//!
//! A layer above the toolkit (collaboration, history, fullscreen, native
//! views, …) defines a capability as a trait and reaches a widget's
//! implementation through [`capability`] / [`capability_mut`]. A widget
//! declares what it provides in one line inside its `impl Widget`:
//!
//! ```ignore
//! impl Widget for Player {
//!     crate::provides!(dyn FullscreenProvider, dyn Continuable);
//!     // ...
//! }
//! ```
//!
//! A widget that provides nothing needs nothing: the [`Widget`] defaults
//! answer every request with `None`. A wrapper passes requests to the widget
//! it wraps by calling its `provide`/`provide_mut`.

use std::any::Any;
use std::marker::PhantomData;
use std::ptr::NonNull;

use super::Widget;

pub struct Request<'a, 'r> {
    slot: &'r mut dyn Any,
    widget: PhantomData<&'a mut ()>,
}

impl<'a> Request<'a, '_> {
    pub fn wants<T: ?Sized + 'static>(&self) -> bool {
        self.slot.is::<Option<NonNull<T>>>()
    }

    pub fn provide<T: ?Sized + 'static>(&mut self, value: &'a T) -> &mut Self {
        if let Some(slot) = self.slot.downcast_mut::<Option<NonNull<T>>>() {
            *slot = Some(NonNull::from(value));
        }
        self
    }
}

pub struct RequestMut<'a, 'r> {
    slot: &'r mut dyn Any,
    widget: PhantomData<&'a mut ()>,
}

impl<'a> RequestMut<'a, '_> {
    pub fn wants<T: ?Sized + 'static>(&self) -> bool {
        self.slot.is::<Option<NonNull<T>>>()
    }

    pub fn provide<T: ?Sized + 'static>(&mut self, value: &'a mut T) {
        if let Some(slot) = self.slot.downcast_mut::<Option<NonNull<T>>>() {
            *slot = Some(NonNull::from(value));
        }
    }
}

pub fn capability<T: ?Sized + 'static>(widget: &(impl Widget + ?Sized)) -> Option<&T> {
    let mut slot: Option<NonNull<T>> = None;
    widget.provide(&mut Request {
        slot: &mut slot,
        widget: PhantomData,
    });
    // SAFETY: `provide` could only store a reference borrowed from `widget`
    // for as long as the borrow passed in (the request carries that
    // lifetime), so the pointer is valid and shared for the returned borrow.
    slot.map(|pointer| unsafe { pointer.as_ref() })
}

pub fn capability_mut<T: ?Sized + 'static>(widget: &mut (impl Widget + ?Sized)) -> Option<&mut T> {
    let mut slot: Option<NonNull<T>> = None;
    widget.provide_mut(&mut RequestMut {
        slot: &mut slot,
        widget: PhantomData,
    });
    // SAFETY: `provide_mut` could only store a reborrow of the exclusive
    // widget borrow passed in, for that borrow's lifetime, and it consumed the
    // borrow, so the pointer is valid and unaliased for the returned borrow.
    // A second answer replaces the first, so only one reborrow is returned.
    slot.map(|mut pointer| unsafe { pointer.as_mut() })
}

/// Declare the capabilities a widget provides, inside its `impl Widget`:
/// `provides!(dyn FullscreenProvider, dyn Continuable);`
///
/// A wrapper that provides some capabilities itself and passes every other
/// request to the widget in one of its fields names that field:
/// `provides!(dyn CommittedActions; inner = content);`, and a
/// [`Decorator`](super::Decorator) passes them to the widget it decorates:
/// `provides!(dyn CommittedActions; decorated);`
///
/// Capabilities may carry conditional compilation attributes, so a platform-only
/// implementation need not be compiled or advertised on other targets:
/// `provides!(#[cfg(not(target_family = "wasm"))] dyn NativeCapability);`
#[macro_export]
macro_rules! provides {
    (@each [$($(#[$attribute:meta])* $capability:ty),+] $rest:tt) => {
        fn provide<'a>(&'a self, request: &mut $crate::widget::capability::Request<'a, '_>) {
            $(
                $(#[$attribute])*
                if request.wants::<$capability>() {
                    request.provide::<$capability>(self);
                    return;
                }
            )+
            $crate::provides!(@rest self request provide inner $rest);
        }

        fn provide_mut<'a>(
            &'a mut self,
            request: &mut $crate::widget::capability::RequestMut<'a, '_>,
        ) {
            $(
                $(#[$attribute])*
                if request.wants::<$capability>() {
                    request.provide::<$capability>(self);
                    return;
                }
            )+
            $crate::provides!(@rest self request provide_mut inner_mut $rest);
        }
    };
    (@rest $self:ident $request:ident $provide:ident $inner:ident {}) => {};
    (@rest $self:ident $request:ident $provide:ident $inner:ident { decorated }) => {
        $self.$inner().$provide($request)
    };
    (@rest $self:ident $request:ident $provide:ident $inner:ident { $field:ident }) => {
        $self.$field.$provide($request)
    };
    ($($(#[$attribute:meta])* $capability:ty),+ $(,)?) => {
        $crate::provides!(@each [$($(#[$attribute])* $capability),+] {});
    };
    ($($(#[$attribute:meta])* $capability:ty),+ ; inner = $field:ident) => {
        $crate::provides!(@each [$($(#[$attribute])* $capability),+] { $field });
    };
    ($($(#[$attribute:meta])* $capability:ty),+ ; decorated) => {
        $crate::provides!(@each [$($(#[$attribute])* $capability),+] { decorated });
    };
}
