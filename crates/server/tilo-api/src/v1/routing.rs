//! Which operation a request is: its method and path matched against the
//! templates of [`super::operations::OPERATIONS`].

use super::operations::{OPERATIONS, Operation};
use std::collections::HashMap;

/// The values a request's path gave the parameters of its template, decoded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Params(HashMap<&'static str, String>);

impl Params {
    /// The parameter `name`; empty when the template has none of that name.
    #[must_use]
    pub fn get(&self, name: &str) -> &str {
        self.0.get(name).map_or("", String::as_str)
    }
}

/// How a request matched no operation.
#[derive(Debug, PartialEq, Eq)]
pub enum Unmatched {
    /// No template takes the path.
    NotFound,
    /// A template takes the path, by other methods: these.
    Method(Vec<&'static str>),
}

/// The operation `method` on `path` is, and its parameters.
///
/// # Errors
/// No template takes the path, or none with that method.
pub fn find(method: &hyper::Method, path: &str) -> Result<(&'static Operation, Params), Unmatched> {
    let segments: Vec<&str> = path.trim_end_matches('/').split('/').skip(1).collect();
    let mut allowed = Vec::new();
    for operation in OPERATIONS.iter() {
        let template: Vec<&'static str> = operation.path.split('/').skip(1).collect();
        let mut params = HashMap::new();
        if !matches(&template, &segments, &mut params) {
            continue;
        }
        if operation.method == *method {
            return Ok((operation, Params(params)));
        }
        allowed.push(operation.method.as_str());
    }
    if allowed.is_empty() {
        Err(Unmatched::NotFound)
    } else {
        Err(Unmatched::Method(allowed))
    }
}

/// Whether `segments` fit `template`, filling `params`. `{path}` takes one or
/// more segments, as few as let the rest of the template fit.
fn matches(
    template: &[&'static str],
    segments: &[&str],
    params: &mut HashMap<&'static str, String>,
) -> bool {
    let Some((&first, template_rest)) = template.split_first() else {
        return segments.is_empty();
    };
    let Some(name) = first
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    else {
        return segments.first() == Some(&first)
            && matches(template_rest, segments.get(1..).unwrap_or_default(), params);
    };
    let most = if name == "path" { segments.len() } else { 1 };
    for taken in 1..=most.min(segments.len()) {
        let (these, rest) = segments.split_at(taken);
        let Some(value) = decode(these) else {
            return false;
        };
        if matches(template_rest, rest, params) {
            params.insert(name, value);
            return true;
        }
    }
    false
}

/// `segments`, each percent-decoded, joined by `/`: `None` when one is empty
/// or not UTF-8.
fn decode(segments: &[&str]) -> Option<String> {
    let mut decoded = Vec::with_capacity(segments.len());
    for segment in segments {
        let segment = percent_encoding::percent_decode_str(segment)
            .decode_utf8()
            .ok()?;
        if segment.is_empty() {
            return None;
        }
        decoded.push(segment.into_owned());
    }
    Some(decoded.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::Method;

    const WORKSPACE: &str = "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b";

    #[test]
    fn a_resource_path_spans_segments_and_keeps_encoded_slashes() {
        let (operation, params) = find(
            &Method::POST,
            &format!("/v1/workspaces/{WORKSPACE}/resources/notes/Q3%2F4%20plan/actions/rename"),
        )
        .unwrap();
        assert_eq!(operation.id, "runAction");
        assert_eq!(params.get("workspace"), WORKSPACE);
        assert_eq!(params.get("path"), "notes/Q3/4 plan");
        assert_eq!(params.get("action"), "rename");

        let (operation, params) = find(
            &Method::GET,
            &format!("/v1/workspaces/{WORKSPACE}/resources/notes/actions/Todo"),
        )
        .unwrap();
        assert_eq!(operation.id, "readResource");
        assert_eq!(params.get("path"), "notes/actions/Todo");
    }

    #[test]
    fn another_method_is_named_and_an_unknown_path_is_not_found() {
        assert_eq!(
            find(&Method::PUT, "/v1/workspaces").unwrap_err(),
            Unmatched::Method(vec!["GET", "POST"])
        );
        assert_eq!(
            find(&Method::GET, "/v1/nothing").unwrap_err(),
            Unmatched::NotFound
        );
        assert_eq!(
            find(
                &Method::GET,
                &format!("/v1/workspaces/{WORKSPACE}/resources//x")
            )
            .unwrap_err(),
            Unmatched::NotFound
        );
    }
}
