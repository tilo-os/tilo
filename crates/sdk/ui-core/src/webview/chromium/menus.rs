//! Menus the engine offers: a page's context menu (`context_menu`) and an
//! extension's toolbar button menu (`extension_menu`), checked and bounded
//! as they arrive.
use serde_json::Value;

use super::{extension_id, valid_id};
use crate::webview::WebViewEvent;

/// Bounds on a menu the engine offers: rows at each level, levels, and rows
/// in all.
const MENU_LEVEL_ROWS: usize = 24;
const MENU_DEPTH: usize = 4;
pub(super) const MENU_ROWS: usize = 200;

/// Take one row from what is left of a menu's rows; `None` when none is.
fn take_row(left: &mut usize) -> Option<()> {
    *left = left.checked_sub(1)?;
    Some(())
}

/// A page's context menu rows (`context_menu`): commands by id, separators,
/// and submenus at most [`MENU_DEPTH`] deep.
pub(super) fn decode_context_items(
    value: &Value,
    depth: usize,
    left: &mut usize,
) -> Option<Vec<crate::webview::ContextMenuItem>> {
    let items = value.as_array()?;
    if items.len() > MENU_LEVEL_ROWS || depth > MENU_DEPTH {
        return None;
    }
    items
        .iter()
        .map(|item| {
            take_row(left)?;
            if item.get("separator").is_some() {
                return (item["separator"] == true)
                    .then_some(crate::webview::ContextMenuItem::Separator);
            }
            if !valid_id(item) {
                return None;
            }
            let label = item["label"]
                .as_str()
                .filter(|s| s.len() <= 4096)?
                .to_owned();
            let enabled = match item.get("enabled") {
                None => true,
                Some(enabled) => enabled.as_bool()?,
            };
            if let Some(nested) = item.get("items") {
                return Some(crate::webview::ContextMenuItem::Submenu {
                    label,
                    enabled,
                    items: decode_context_items(nested, depth + 1, left)?,
                });
            }
            Some(crate::webview::ContextMenuItem::Command {
                id: u32::try_from(item["id"].as_u64()?).ok()?,
                label,
                checked: match item.get("checked") {
                    None => None,
                    Some(checked) => Some(checked.as_bool()?),
                },
                enabled,
            })
        })
        .collect()
}

/// An extension's toolbar button menu (`extension_menu`): its rows by key.
pub(super) fn decode_extension_menu(value: &Value) -> Option<WebViewEvent> {
    fn rows(
        value: &Value,
        depth: usize,
        left: &mut usize,
    ) -> Option<Vec<crate::webview::ExtensionMenuItem>> {
        let items = value.as_array()?;
        if items.len() > MENU_LEVEL_ROWS || depth > MENU_DEPTH {
            return None;
        }
        items
            .iter()
            .map(|item| {
                take_row(left)?;
                let kind = item["type"].as_str()?;
                if kind == "separator" {
                    return Some(crate::webview::ExtensionMenuItem::Separator);
                }
                let label = item["label"]
                    .as_str()
                    .filter(|s| s.len() <= 4096)?
                    .to_owned();
                let enabled = item["enabled"].as_bool()?;
                if let Some(nested) = item.get("items") {
                    if kind != "normal" {
                        return None;
                    }
                    return Some(crate::webview::ExtensionMenuItem::Submenu {
                        label,
                        enabled,
                        items: rows(nested, depth + 1, left)?,
                    });
                }
                let key = item["key"]
                    .as_str()
                    .filter(|key| !key.is_empty() && key.len() <= 1024)?
                    .to_owned();
                let checked = match kind {
                    "normal" => None,
                    "checkbox" | "radio" => Some(item["checked"].as_bool()?),
                    _ => return None,
                };
                Some(crate::webview::ExtensionMenuItem::Command {
                    key,
                    label,
                    checked,
                    enabled,
                })
            })
            .collect()
    }
    Some(WebViewEvent::ExtensionMenu {
        id: value["id"].as_str().filter(|id| extension_id(id))?.into(),
        items: rows(&value["items"], 1, &mut { MENU_ROWS })?,
    })
}
