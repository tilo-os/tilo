//! The Google Cloud projects a Vertex AI sign-in can bill, for the Agent
//! Models picker.
//!
//! Listing is a convenience: it needs the Cloud Resource Manager API, which
//! the user's organisation (or Tilo's client project) may not have enabled.
//! Callers must still let the user type a project ID by hand when this
//! fails or comes back empty.

use std::time::Duration;

use crate::CustodyError;

/// Resource Manager's project list, active projects only.
pub const GOOGLE_PROJECTS_ENDPOINT: &str =
    "https://cloudresourcemanager.googleapis.com/v1/projects";

/// A user with more projects than this types the ID; the picker is not a
/// project browser, and every page is a round trip while the user waits.
const MAX_PAGES: usize = 5;

/// One project the signed-in user can see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudProject {
    /// The ID Vertex AI URLs and billing headers name (`my-project-123`).
    pub id: String,
    /// The display name, which is not unique; empty when unset.
    pub name: String,
}

/// The active projects `access_token` (a `cloud-platform` grant) can see,
/// sorted by ID, from `endpoint` ([`GOOGLE_PROJECTS_ENDPOINT`] outside
/// tests).
///
/// # Errors
/// [`CustodyError::Offline`] when Google cannot be reached,
/// [`CustodyError::NeedsAuthentication`] when it refuses the token, and
/// [`CustodyError::SignIn`] for any other answer (most often the Resource
/// Manager API being disabled).
pub fn google_cloud_projects(
    endpoint: &str,
    access_token: &str,
) -> Result<Vec<CloudProject>, CustodyError> {
    let http = tilo_net::http::blocking_client()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| CustodyError::Offline)?;
    let mut projects = Vec::new();
    let mut page_token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut url = url::Url::parse(endpoint)
            .map_err(|_| CustodyError::SignIn("invalid Google Cloud projects endpoint".into()))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("filter", "lifecycleState:ACTIVE");
            if let Some(token) = page_token.take() {
                query.append_pair("pageToken", &token);
            }
        }
        let response = http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .map_err(|_| CustodyError::Offline)?;
        let status = response.status().as_u16();
        if status == 401 {
            return Err(CustodyError::NeedsAuthentication);
        }
        if !response.status().is_success() {
            return Err(CustodyError::SignIn(format!(
                "Google Cloud projects could not be listed (HTTP {status})"
            )));
        }
        let page: serde_json::Value = response
            .json()
            .map_err(|_| CustodyError::SignIn("Google returned an invalid project list".into()))?;
        let (found, next) = parse_page(&page);
        projects.extend(found);
        match next {
            Some(next) => page_token = Some(next),
            None => break,
        }
    }
    projects.sort_by(|a, b| a.id.cmp(&b.id));
    projects.dedup_by(|a, b| a.id == b.id);
    Ok(projects)
}

/// The projects of one Resource Manager v1 page and its continuation
/// token. Entries without a usable ID are skipped rather than failing the
/// whole list.
fn parse_page(page: &serde_json::Value) -> (Vec<CloudProject>, Option<String>) {
    let projects = page
        .get("projects")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let id = entry.get("projectId")?.as_str()?;
                    if id.is_empty() || id.contains(|c: char| c == '/' || c.is_whitespace()) {
                        return None;
                    }
                    let name = entry
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    Some(CloudProject {
                        id: id.to_string(),
                        name: name.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let next = page
        .get("nextPageToken")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_string);
    (projects, next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_http::{Reply, Responder};

    #[test]
    fn a_page_yields_its_usable_projects_and_continuation() {
        let page = serde_json::json!({
            "projects": [
                {"projectId": "alpha-1", "name": "Alpha", "lifecycleState": "ACTIVE"},
                {"projectId": "", "name": "Broken"},
                {"name": "No id"},
                {"projectId": "beta-2"},
            ],
            "nextPageToken": "next",
        });
        let (projects, next) = parse_page(&page);
        assert_eq!(
            projects,
            [
                CloudProject {
                    id: "alpha-1".into(),
                    name: "Alpha".into()
                },
                CloudProject {
                    id: "beta-2".into(),
                    name: String::new()
                },
            ]
        );
        assert_eq!(next.as_deref(), Some("next"));
        let (none, end) = parse_page(&serde_json::json!({}));
        assert!(none.is_empty());
        assert_eq!(end, None);
    }

    #[test]
    fn pages_are_followed_and_a_disabled_api_is_an_error() {
        let server = Responder::start(vec![
            Reply::status(200).body(
                serde_json::json!({
                    "projects": [{"projectId": "zeta", "name": "Z"}],
                    "nextPageToken": "p2",
                })
                .to_string(),
            ),
            Reply::status(200).body(
                serde_json::json!({"projects": [{"projectId": "alpha", "name": "A"}]}).to_string(),
            ),
        ]);
        let projects = google_cloud_projects(&format!("{}/v1/projects", server.url()), "tok")
            .expect("projects");
        assert_eq!(
            projects.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        let requests = server.drain_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].header("Authorization"), Some("Bearer tok"));
        assert!(
            requests[0]
                .target
                .contains("filter=lifecycleState%3AACTIVE")
        );
        assert!(requests[1].target.contains("pageToken=p2"));

        let disabled = Responder::start(vec![Reply::status(403).body("{}")]);
        assert!(matches!(
            google_cloud_projects(&disabled.url(), "tok"),
            Err(CustodyError::SignIn(_))
        ));
        let refused = Responder::start(vec![Reply::status(401).body("{}")]);
        assert!(matches!(
            google_cloud_projects(&refused.url(), "tok"),
            Err(CustodyError::NeedsAuthentication)
        ));
    }
}
