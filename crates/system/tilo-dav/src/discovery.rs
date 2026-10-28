//! Bounded `CalDAV` setup, resolving server-owned identities rather than guessing
//! a calendar path from a login name. Credentials never leave the input origin.

use crate::{DavClient, Depth, Limits, Multistatus, PropName, Url, props};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalendarCollection {
    pub href: String,
    pub name: String,
    pub read_only: bool,
}

/// Discover actual calendar collections from a server, principal or home URL.
/// The client must refuse automatic redirects. Every followed URL is checked
/// before sending credentials; at most eight requests and three redirects per
/// step are made, with bounded bodies and XML complexity.
pub fn calendars(client: &DavClient, server: &Url) -> Result<Vec<CalendarCollection>, String> {
    calendars_checked(client, server, &|_| Ok(()))
}

pub(crate) fn calendars_checked(
    client: &DavClient,
    server: &Url,
    check: &dyn Fn(&Url) -> Result<(), String>,
) -> Result<Vec<CalendarCollection>, String> {
    check(server)?;
    if !client.refuses_redirects() {
        return Err("CalDAV discovery requires a client with automatic redirects disabled".into());
    }
    safe_url(server, server.as_str())?;
    let mut budget = 8;
    let endpoint = home(client, server, &mut budget, check)?;
    let (endpoint, listing) = find(
        client,
        server,
        endpoint,
        Depth::One,
        &[
            props::RESOURCETYPE,
            props::DISPLAYNAME,
            PropName::dav("current-user-privilege-set"),
        ],
        &mut budget,
        check,
    )?;
    let mut calendars = Vec::new();
    for response in listing.responses {
        if response
            .status
            .is_some_and(|status| !(200..300).contains(&status))
            || !response
                .prop("resourcetype")
                .is_some_and(|p| p.has_child("calendar"))
        {
            continue;
        }
        let href = safe_url(&endpoint, &response.href)?;
        check(&href)?;
        // A depth-one listing cannot nominate an unrelated path as a calendar.
        if href.path() != endpoint.path()
            && !href
                .path()
                .starts_with(&format!("{}/", endpoint.path().trim_end_matches('/')))
        {
            return Err("calendar collection is outside its home".into());
        }
        let privileges = response.prop("current-user-privilege-set");
        if privileges.is_some_and(|p| !p.has_child("read") && !p.has_child("all")) {
            continue;
        }
        let read_only = privileges.is_some_and(|p| !p.has_child("write") && !p.has_child("all"));
        let name = response
            .prop_text("displayname")
            .unwrap_or_else(|| href.path())
            .to_owned();
        if !calendars
            .iter()
            .any(|c: &CalendarCollection| c.href == href.as_str())
        {
            calendars.push(CalendarCollection {
                href: href.into(),
                name,
                read_only,
            });
        }
    }
    if calendars.is_empty() {
        return Err(
            "No calendars were found. Check the CalDAV server address and calendar access.".into(),
        );
    }
    Ok(calendars)
}

fn home(
    client: &DavClient,
    server: &Url,
    budget: &mut usize,
    check: &dyn Fn(&Url) -> Result<(), String>,
) -> Result<Url, String> {
    let properties = [
        PropName::dav("current-user-principal"),
        PropName::caldav("calendar-home-set"),
        props::RESOURCETYPE,
    ];
    let initial = find(
        client,
        server,
        server.clone(),
        Depth::Zero,
        &properties,
        budget,
        check,
    );
    let (mut endpoint, mut listing) = match initial {
        Ok(found) => found,
        Err(error)
            if error == "CalDAV discovery returned HTTP 404"
                || error == "CalDAV discovery returned HTTP 200" =>
        {
            (server.clone(), Multistatus::default())
        }
        Err(error) => return Err(error),
    };
    // Some installations expose discovery only at the well-known path. Keep
    // the user's deployment prefix rather than assuming a root installation.
    if !has_discovery(&listing) {
        let base = format!("{}/", server.as_str().trim_end_matches('/'));
        let well_known = Url::parse(&base)
            .map_err(|_| "invalid server URL")?
            .join(".well-known/caldav")
            .map_err(|_| "invalid discovery URL")?;
        (endpoint, listing) = find(
            client,
            server,
            well_known,
            Depth::Zero,
            &properties,
            budget,
            check,
        )?;
    }
    if let Some(principal) = property_href(&listing, "current-user-principal") {
        let principal = safe_url(&endpoint, principal)?;
        (endpoint, listing) = find(
            client,
            server,
            principal,
            Depth::Zero,
            &properties,
            budget,
            check,
        )?;
    }
    if let Some(home) = property_href(&listing, "calendar-home-set") {
        endpoint = safe_url(&endpoint, home)?;
    }
    Ok(endpoint)
}

fn has_discovery(listing: &Multistatus) -> bool {
    listing.responses.iter().any(|r| {
        r.prop("current-user-principal").is_some()
            || r.prop("calendar-home-set").is_some()
            || r.is_collection()
    })
}

fn property_href<'a>(listing: &'a Multistatus, name: &str) -> Option<&'a str> {
    listing.responses.iter().find_map(|r| r.prop_text(name))
}

pub(crate) fn safe_url(base: &Url, href: &str) -> Result<Url, String> {
    if href.len() > 4096 || href.chars().any(char::is_control) || href.contains('\\') {
        return Err("unsafe DAV URL".into());
    }
    let normalized = href.to_ascii_lowercase().replace("%2e", ".");
    if normalized.split('/').any(|part| part == "..") {
        return Err("unsafe DAV path traversal".into());
    }
    let url = base.join(href).map_err(|_| "invalid DAV URL")?;
    if url.origin() != base.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.scheme(), "https" | "http")
    {
        return Err("DAV discovery URL must stay on the configured origin".into());
    }
    let path = url.path().to_ascii_lowercase();
    if path.contains("%2f") || path.contains("%5c") || path.contains("%00") {
        return Err("unsafe DAV path".into());
    }
    Ok(url)
}

fn find(
    client: &DavClient,
    origin: &Url,
    mut url: Url,
    depth: Depth,
    props: &[PropName],
    budget: &mut usize,
    check: &dyn Fn(&Url) -> Result<(), String>,
) -> Result<(Url, Multistatus), String> {
    for _ in 0..4 {
        if *budget == 0 {
            return Err("DAV discovery request limit reached".into());
        }
        check(&url)?;
        *budget = budget.saturating_sub(1);
        let response = client
            .propfind(url.clone(), depth, props)
            .map_err(|_| "could not reach the CalDAV server")?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .header("location")
                .ok_or("DAV redirect has no location")?;
            url = safe_url(&url, &location)?;
            safe_url(origin, url.as_str())?;
            continue;
        }
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err("server rejected the saved credential or calendar access".into());
        }
        if status.as_u16() != 207 {
            return Err(format!(
                "CalDAV discovery returned HTTP {}",
                status.as_u16()
            ));
        }
        let limits = Limits {
            max_bytes: 1024 * 1024,
            max_responses: 512,
            max_nodes: 32768,
            ..Limits::default()
        };
        return response
            .multistatus(&limits)
            .map(|listing| (url, listing))
            .map_err(|error| format!("invalid CalDAV discovery response: {error}"));
    }
    Err("CalDAV discovery redirect limit reached".into())
}
