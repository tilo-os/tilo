//! Capabilities: behaviour a widget takes part in beyond pixels and input.
//!
//! A layer above the toolkit (collaboration, history, fullscreen, native
//! views, …) defines a capability as a trait and reaches a widget's
//! implementation through [`capability`] / [`capability_mut`]. A widget
//! declares what it provides in one line inside its `impl Widget`:
//!
//! ```ignore
//! impl Widget for Player {
//!     tilo_ui::provides!(dyn FullscreenProvider, dyn Continuable);
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

/// A shared-borrow request for one capability, answered by
/// [`Widget::provide`].
pub struct Request<'a, 'r> {
    slot: &'r mut dyn Any,
    widget: PhantomData<&'a mut ()>,
}

impl<'a> Request<'a, '_> {
    /// Whether this request asks for `T`.
    pub fn wants<T: ?Sized + 'static>(&self) -> bool {
        self.slot.is::<Option<NonNull<T>>>()
    }

    /// Answer the request with `value` if it asks for `T`.
    pub fn provide<T: ?Sized + 'static>(&mut self, value: &'a T) -> &mut Self {
        if let Some(slot) = self.slot.downcast_mut::<Option<NonNull<T>>>() {
            *slot = Some(NonNull::from(value));
        }
        self
    }
}

/// An exclusive-borrow request for one capability, answered by
/// [`Widget::provide_mut`].
pub struct RequestMut<'a, 'r> {
    slot: &'r mut dyn Any,
    widget: PhantomData<&'a mut ()>,
}

impl<'a> RequestMut<'a, '_> {
    /// Whether this request asks for `T`.
    pub fn wants<T: ?Sized + 'static>(&self) -> bool {
        self.slot.is::<Option<NonNull<T>>>()
    }

    /// Answer the request with `value` if it asks for `T`.
    pub fn provide<T: ?Sized + 'static>(&mut self, value: &'a mut T) {
        if let Some(slot) = self.slot.downcast_mut::<Option<NonNull<T>>>() {
            *slot = Some(NonNull::from(value));
        }
    }
}

/// `widget`'s implementation of capability `T` (a `dyn Trait`), if it
/// provides one.
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

/// Mutable [`capability`].
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
/// `provides!(dyn CommittedActions; inner = content);`
///
/// Capabilities may carry conditional compilation attributes, so a platform-only
/// implementation need not be compiled or advertised on other targets:
/// `provides!(#[cfg(not(target_family = "wasm"))] dyn NativeCapability);`
#[macro_export]
macro_rules! provides {
    (@each [$($(#[$attribute:meta])* $capability:ty),+] { $($field:ident)? }) => {
        fn provide<'a>(&'a self, request: &mut $crate::widget::capability::Request<'a, '_>) {
            $(
                $(#[$attribute])*
                if request.wants::<$capability>() {
                    request.provide::<$capability>(self);
                    return;
                }
            )+
            $(self.$field.provide(request);)?
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
            $(self.$field.provide_mut(request);)?
        }
    };
    ($($(#[$attribute:meta])* $capability:ty),+ $(,)?) => {
        $crate::provides!(@each [$($(#[$attribute])* $capability),+] {});
    };
    ($($(#[$attribute:meta])* $capability:ty),+ ; inner = $field:ident) => {
        $crate::provides!(@each [$($(#[$attribute])* $capability),+] { $field });
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    trait Greeting {
        fn greet(&self) -> String;
        fn rename(&mut self, name: &str);
    }

    trait Unrelated {}

    struct Probe {
        name: String,
    }

    impl Greeting for Probe {
        fn greet(&self) -> String {
            format!("hello {}", self.name)
        }
        fn rename(&mut self, name: &str) {
            self.name = name.to_owned();
        }
    }

    impl Widget for Probe {
        crate::provides!(#[cfg(all())] dyn Greeting, #[cfg(any())] dyn Unrelated);
        crate::testing::inert_widget_methods!(leaf_layout, apply_layout, draw, get_rect);
        fn name(&self) -> &'static str {
            "probe"
        }
    }

    #[test]
    fn a_widget_answers_only_the_capabilities_it_declares() {
        let mut probe = Probe {
            name: "a".to_owned(),
        };
        let widget: &mut dyn Widget = &mut probe;
        assert_eq!(
            capability::<dyn Greeting>(widget).map(Greeting::greet),
            Some("hello a".to_owned())
        );
        assert!(capability::<dyn Unrelated>(widget).is_none());
        assert!(capability_mut::<dyn Unrelated>(widget).is_none());
        capability_mut::<dyn Greeting>(widget)
            .expect("declared")
            .rename("b");
        assert_eq!(probe.greet(), "hello b");
    }
    struct Wrapper {
        inner: Probe,
    }

    impl Widget for Wrapper {
        crate::provides!(#[cfg(any())] dyn Unrelated; inner = inner);
        crate::testing::inert_widget_methods!(leaf_layout, apply_layout, draw, get_rect);
        fn name(&self) -> &'static str {
            "wrapper"
        }
    }

    #[test]
    fn excluded_capabilities_forward_shared_and_mutable_requests_to_the_inner_widget() {
        let mut wrapper = Wrapper {
            inner: Probe {
                name: "a".to_owned(),
            },
        };
        let widget: &mut dyn Widget = &mut wrapper;
        assert!(capability::<dyn Unrelated>(widget).is_none());
        assert!(capability_mut::<dyn Unrelated>(widget).is_none());
        assert_eq!(
            capability::<dyn Greeting>(widget).map(Greeting::greet),
            Some("hello a".to_owned())
        );
        capability_mut::<dyn Greeting>(widget)
            .expect("forwarded")
            .rename("b");
        assert_eq!(wrapper.inner.greet(), "hello b");
    }
}
