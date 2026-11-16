//! Where a `settings:` link an app opens goes on the page.
//!
//! On a desktop the shell hands the link to Settings and the app shows the
//! pane it names. A page has no such shell, so the link takes the same road a
//! notice's choice does: the session opens Settings, anchored on the pane
//! ([`crate::shell::HostRequest::ShowInApp`]). This is what that request
//! names, decided without the page's thread-locals so it is tested on the
//! host.

/// Settings' application id, as the workspace's actions name it.
pub(crate) const APP: &str = "so.tilo.settings";

/// The resource a `settings:` link names, as Settings' `open` reads it: a
/// pane row (`settings.section:<slug>`). `settings://keyboard` and
/// `settings.section:keyboard` name the same page, as a link and a person's
/// card do; `None` for anything else, and for a link naming nothing.
pub(crate) fn resource(link: &str) -> Option<String> {
    // A person's card hands the resource already made; a link names the pane.
    if link.starts_with("settings.section:") {
        return (link.len() > "settings.section:".len()).then(|| link.to_owned());
    }
    let rest = link
        .strip_prefix("settings://")
        .or_else(|| link.strip_prefix("settings:"))?;
    if rest.is_empty() {
        return None;
    }
    Some(format!("settings.section:{rest}"))
}

/// What an app's `open` of `uri` asks the page for: Settings on the pane's
/// resource. `None` for a link that names no pane, which the browser's own
/// opener answers as it always has.
pub(crate) fn show_in_app(uri: &str) -> Option<(&'static str, String)> {
    resource(uri).map(|resource| (APP, resource))
}

/// Take from `queue` what the embedding page answers, leaving each request
/// `stays` keeps for the page's own desktop: a `settings:` link queued
/// between frames waits there for the desktop to show it, and the page's
/// status never carries it off to an embedding page that does nothing with
/// it.
pub(crate) fn take_for_page<T>(queue: &mut Vec<T>, stays: impl Fn(&T) -> bool) -> Vec<T> {
    let (kept, taken) = std::mem::take(queue)
        .into_iter()
        .partition(|request| stays(request));
    *queue = kept;
    taken
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link queued after a frame is still there for the desktop once the
    /// page reported its status, which took everything else.
    #[test]
    fn the_status_leaves_a_settings_link_for_the_desktop() {
        let mut queue = vec!["copy", "show-in-app", "named"];
        let taken = take_for_page(&mut queue, |request| *request == "show-in-app");
        assert_eq!(taken, ["copy", "named"]);
        assert_eq!(queue, ["show-in-app"]);
    }

    /// The two spellings of a pane link name the same page, and a link that
    /// names none is no resource at all.
    #[test]
    fn a_link_names_its_pane_however_it_is_spelled() {
        assert_eq!(
            resource("settings://keyboard").as_deref(),
            Some("settings.section:keyboard")
        );
        assert_eq!(
            resource("settings:keyboard").as_deref(),
            Some("settings.section:keyboard")
        );
        assert_eq!(
            resource("settings.section:keyboard").as_deref(),
            Some("settings.section:keyboard"),
            "a resource already made is the page's to take, not a link to re-make"
        );
        assert_eq!(
            resource("settings://models/u2net-subject").as_deref(),
            Some("settings.section:models/u2net-subject")
        );
        assert_eq!(resource("settings://"), None);
        assert_eq!(resource("https://example.com"), None);
        assert_eq!(resource("settings.section:"), None);
    }

    /// A `settings:` link is the page's Settings on the pane it names, never
    /// a browser window; every other link is the browser's.
    #[test]
    fn a_settings_link_opens_settings_on_its_pane() {
        assert_eq!(
            show_in_app("settings://external_accounts"),
            Some((APP, "settings.section:external_accounts".to_owned()))
        );
        assert_eq!(show_in_app("settings://"), None);
        assert_eq!(show_in_app("mailto:ada@example.test"), None);
    }
}
