//! What a carrier action names to one caller: only the actions their own call of it is not
//! refused for.

use std::sync::Arc;

use super::ActionSpec;

impl ActionSpec {
    /// Take the actions `refused` names out of what this action says it carries
    /// ([`Self::carrying`]): out of the ids the schemas of its parameters name. Whoever
    /// lists a resource to a caller calls it with the actions it drops for them, or lists
    /// to them as not allowed, so a carrier names to that caller only what their own call
    /// of it is not refused for. An action that carries none, or names none of them, stays
    /// as it is and keeps the schema it shares.
    pub fn without_carried(&mut self, refused: &[&str]) {
        if !self.carrier {
            return;
        }
        let cut: Vec<(usize, serde_json::Value)> = self
            .params
            .iter()
            .enumerate()
            .filter_map(|(at, param)| {
                let schema = param.schema.as_deref()?;
                Some((at, crate::params::without_named(schema, refused)?))
            })
            .collect();
        if cut.is_empty() {
            return;
        }
        let params = &mut self.type_mut().params;
        for (at, schema) in cut {
            if let Some(param) = params.get_mut(at) {
                param.schema = Some(Arc::new(schema));
            }
        }
    }
}
