//! Offering a parameter's current value: what the action's own state knows
//! it would act with, which a surface fills and a caller may change.

use crate::{ActionSpec, ActionType};

impl ActionType {
    /// Offer `value` in the field of the parameter `name`: its schema's
    /// default, which a surface fills and a caller may change. A parameter
    /// the action does not take is left as it is.
    pub fn prefilled(&mut self, name: &str, value: serde_json::Value) {
        if let Some(param) = self
            .params
            .iter_mut()
            .find(|param| param.name.as_ref() == name)
        {
            *param = param.clone().with_default(value);
        }
    }
}

impl ActionSpec {
    /// [`ActionType::prefilled`] on the action this spec publishes: this
    /// item's, copied first when other items share the type.
    pub fn prefilled(&mut self, name: &str, value: serde_json::Value) {
        self.type_mut().prefilled(name, value);
    }
}
