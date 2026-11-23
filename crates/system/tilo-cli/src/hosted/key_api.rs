//! Which API server `tilo keys create` and `tilo keys export` ask for a key: the one named, else
//! the one of the site this computer logged in with.

use anyhow::{Result, bail};
use serde_json::Value;

use super::{MAX_ANSWER, text};
use crate::api;

/// Where a key is asked for: the API server `--api` or `TILO_API_URL` names, else the one of the
/// site this computer logged in with, as that site names it, and tilo.sh's only for a computer
/// that logged in nowhere else. A computer logged in elsewhere is never sent to production
/// unasked: where its site names no API server, it is told to name one.
pub(super) async fn key_api(named: Option<&str>) -> Result<String> {
    let named = named
        .map(str::to_owned)
        .or_else(|| std::env::var(api::API_URL_ENV).ok())
        .filter(|api| !api.trim().is_empty());
    if let Some(api) = named {
        return Ok(api.trim_end_matches('/').to_owned());
    }
    let Some(device) = crate::login::home() else {
        return Ok(api::DEFAULT_API_URL.to_owned());
    };
    if device.server_address.is_some() {
        return api_of_site(&device.site, None, true);
    }
    let site = device.site.trim_end_matches('/');
    if site == crate::login::DEFAULT_SITE {
        return api_of_site(site, None, false);
    }
    let entry = crate::login::http()
        .get(format!("{site}/entry.json"))
        .send()
        .await
        .ok();
    let named = match entry {
        Some(entry) if entry.status().as_u16() == 200 => entry
            .bytes_limited(MAX_ANSWER)
            .await
            .ok()
            .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
            .and_then(|entry| entry.get("api").and_then(Value::as_str).map(str::to_owned)),
        _ => None,
    };
    api_of_site(site, named.as_deref(), false)
}

/// The API server a computer logged in with `site` asks for keys: the one the site names
/// (`named`), tilo.sh's own for tilo.sh, and otherwise none, with what to do. A computer that
/// joined a server reached over iroh only (`over_iroh`) has no API server its site names.
fn api_of_site(site: &str, named: Option<&str>, over_iroh: bool) -> Result<String> {
    let site = site.trim_end_matches('/');
    if let Some(api) = named.filter(|api| !api.trim().is_empty()) {
        return Ok(api.trim_end_matches('/').to_owned());
    }
    if over_iroh {
        bail!(
            "This computer joined a workspace on a server reached over iroh, which has no API \
             server it names for keys. Name the one to ask with --api or {} (a local stack's: \
             `stack.sh env`).",
            api::API_URL_ENV
        );
    }
    if site == crate::login::DEFAULT_SITE {
        return Ok(api::DEFAULT_API_URL.to_owned());
    }
    bail!(
        "This computer is logged in with {site}, which names no API server for keys. Name the one \
         to ask with --api or {} (a local stack's: `stack.sh env`).",
        api::API_URL_ENV
    )
}

/// The grant of the key `asked` names in `listed` (`GET /v1/keys`): the key whose grant it is, or
/// the one key called that. A name two keys share is refused with both grants, to name one.
pub(super) fn key_named(listed: &Value, asked: &str) -> Result<String> {
    let keys = listed
        .get("keys")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if keys.iter().any(|key| text(key, "grant") == asked) {
        return Ok(asked.to_owned());
    }
    let named: Vec<&Value> = keys
        .iter()
        .filter(|key| text(key, "name") == asked)
        .collect();
    match named.as_slice() {
        [one] => Ok(text(one, "grant").to_owned()),
        [] => bail!("no key is called {asked}: `tilo keys ls` lists them"),
        many => bail!(
            "{} keys are called {asked}: revoke one by its grant ({})",
            many.len(),
            many.iter()
                .map(|key| text(key, "grant"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    /// A computer logged in with a site asks that site's API server for keys, as the site names
    /// it; tilo.sh's own only for tilo.sh; and a site that names none, or a server reached over
    /// iroh only, is never quietly swapped for production.
    #[test]
    fn a_logged_in_computer_asks_its_own_sites_api_server_for_keys() {
        assert_eq!(
            super::api_of_site(
                "http://tilo.localhost:33720/",
                Some("http://localhost:33706/"),
                false
            )
            .unwrap(),
            "http://localhost:33706"
        );
        assert_eq!(
            super::api_of_site("https://tilo.sh", None, false).unwrap(),
            "https://mcp.tilo.sh"
        );
        let unnamed = super::api_of_site("http://tilo.localhost:33720", None, false)
            .unwrap_err()
            .to_string();
        assert!(
            unnamed.contains("http://tilo.localhost:33720") && unnamed.contains("TILO_API_URL"),
            "{unnamed}"
        );
        assert!(!unnamed.contains("tilo.sh/"), "{unnamed}");
        let iroh = super::api_of_site("https://tilo.sh/", None, true)
            .unwrap_err()
            .to_string();
        assert!(iroh.contains("TILO_API_URL"), "{iroh}");
    }

    /// `tilo keys revoke` takes a key by its grant or by its name as `tilo keys ls` lists it, and
    /// asks for the grant when two keys share the name.
    #[test]
    fn a_key_is_revoked_by_its_grant_or_its_name() {
        let listed = serde_json::json!({"keys": [
            {"grant": "grant-1", "name": "Claude Code on laptop"},
            {"grant": "grant-2", "name": "Linear import via https://mcp.tilo.test"},
            {"grant": "grant-3", "name": "Nightly"},
            {"grant": "grant-4", "name": "Nightly"},
        ]});
        assert_eq!(super::key_named(&listed, "grant-2").unwrap(), "grant-2");
        assert_eq!(
            super::key_named(&listed, "Linear import via https://mcp.tilo.test").unwrap(),
            "grant-2"
        );
        let shared = super::key_named(&listed, "Nightly")
            .unwrap_err()
            .to_string();
        assert!(
            shared.contains("grant-3") && shared.contains("grant-4"),
            "{shared}"
        );
        assert!(super::key_named(&listed, "Nobody").is_err());
    }
}
