//! A control's inert request to invoke one advertised operation.

use crate::api::ActionAddress;

/// A UI control's inert request to invoke one advertised operation.
///
/// This deliberately carries an address and typed JSON arguments, not an
/// [`EncodedAction`]. The app process resolves the address against its
/// *fresh* resource tree before opening the action API, so a control cannot
/// retain stale authority when availability or policy changes.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlInvocation {
    pub address: ActionAddress,
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

impl ControlInvocation {
    /// An invocation of `action` on `resource`, with its arguments, or `None`
    /// when either half is empty or contains the address separator.
    ///
    /// This checks the format only. The app process resolves the address
    /// against its fresh resource tree and revalidates the arguments;
    /// constructing a value grants no authority.
    #[must_use]
    pub fn from_parts(
        resource: impl AsRef<str>,
        action: impl AsRef<str>,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<Self> {
        Some(Self {
            address: ActionAddress::checked(resource, action)?,
            arguments,
        })
    }
}
