//! The rich text a record's published action takes, as the runtime writes it: a reference to a
//! member, a resource or a message, and the words after it.

use serde_json::Value as Json;
use tilo_workspace_service::tl_records::RichText;

impl RichText for crate::Service {
    fn reference(&self, label: &str, to: &tilo_reference::Embed) -> Result<Json, String> {
        let mut body = tilolang_tilo::runtime::rich::RichValue::default();
        body.push_reference(label, to);
        tilolang_tilo::runtime::value::to_json(&tilolang_tilo::runtime::Value::Rich(body))
    }

    fn mention(&self, label: &str, to: &tilo_reference::Embed, text: &str) -> Result<Json, String> {
        let mut body = tilolang_tilo::runtime::rich::RichValue::default();
        body.push_reference(label, to);
        body.push_text(&format!(" {text}"));
        tilolang_tilo::runtime::value::to_json(&tilolang_tilo::runtime::Value::Rich(body))
    }
}
