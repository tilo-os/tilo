//! Transitional frontend-owned native state. These containers are not backend
//! extraction: their payloads, dispatch and downcasts still belong to the imager.
//! Do not use them to represent portable backend authority.
use imager_backend::journey_media::Unsupported;
use std::any::Any;

/// Native payload ownership is never serialized or reconstructed from a path.
pub struct FrontendNativeCustody(Box<dyn Any + Send + Sync>);
impl FrontendNativeCustody {
    pub fn new<T: Any + Send + Sync>(value: T) -> Self {
        Self(Box::new(value))
    }
    pub fn get<T: Any>(&self) -> Option<&T> {
        self.0.downcast_ref()
    }
    pub fn get_mut<T: Any>(&mut self) -> Option<&mut T> {
        self.0.downcast_mut()
    }
}

/// An optional native actor. Missing support is a typed error on requests, not
/// a successful empty native operation. Presentation may still show idle state.
pub struct FrontendNativeActor<T: ?Sized>(Option<Box<T>>);
impl<T: ?Sized> Default for FrontendNativeActor<T> {
    fn default() -> Self {
        Self(None)
    }
}
impl<T: ?Sized> FrontendNativeActor<T> {
    pub fn new(actor: Box<T>) -> Self {
        Self(Some(actor))
    }
    pub fn get(&self) -> Result<&T, Unsupported> {
        self.0.as_deref().ok_or(Unsupported)
    }
    pub fn get_mut(&mut self) -> Result<&mut T, Unsupported> {
        match &mut self.0 {
            Some(actor) => Ok(actor.as_mut()),
            None => Err(Unsupported),
        }
    }
}
