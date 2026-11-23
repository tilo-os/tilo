//! Narrowing a declared action's parameter choices to what this instance
//! offers: the sessions a greeter can start now, the challenge waiting for an
//! answer.

use super::ActionSpec;

impl ActionSpec {
    /// This action with `name`'s accepted values narrowed to `choices`: the
    /// sessions a greeter offers now, the challenge waiting for an answer. Its
    /// other parameters, operation and contract stay as declared, and an
    /// unknown name leaves it as it is.
    #[must_use]
    pub fn with_param_choices(
        mut self,
        name: &str,
        choices: impl IntoIterator<Item = std::borrow::Cow<'static, str>>,
    ) -> Self {
        let choices: Vec<std::borrow::Cow<'static, str>> = choices.into_iter().collect();
        if let Some(param) = self
            .type_mut()
            .params
            .iter_mut()
            .find(|param| param.name == name)
        {
            param.choices = choices;
        }
        self
    }
}
