//! A page action's success, as the answer its caller reads: the page's own
//! report, the resources it names, and a picture of the page kept as content.

use super::BrowserWidget;
use ui::webview::{PageReply, PageRequest};

impl BrowserWidget {
    pub(super) fn page_success(
        &self,
        invocation: ui::tilo_actions::InvocationId,
        reply: &PageReply,
        request: &PageRequest,
        note: Option<&str>,
    ) -> ui::tilo_actions::InvocationUpdate {
        // After a navigation the tab's committed address is newer than the
        // engine's answer, which was taken as the navigation began.
        let url = if reply.loading && !self.current_uri.is_empty() {
            self.current_uri.clone()
        } else {
            reply.url.clone()
        };
        let message = note.map_or_else(
            || reply.message.clone(),
            |note| format!("{}; {note}", reply.message),
        );
        let mut value = serde_json::Map::new();
        value.insert("url".into(), url.into());
        value.insert("title".into(), self.title.as_str().into());
        value.insert("message".into(), message.into());
        value.insert("navigation".into(), reply.navigation.as_str().into());
        if let Some(outline) = &reply.outline {
            value.insert("outline".into(), outline.as_str().into());
        }
        // A find answers an element, not a reading of the page: its ref is
        // what a later step binds to, so it is the answer's own field rather
        // than only a finding inside it. One found in a frame says the frame:
        // the ref acts there.
        if let (PageRequest::Find { .. }, Some(data)) = (request, &reply.data) {
            for key in ["element", "role", "name", "line", "nth", "matches", "frame"] {
                if let Some(field) = data.get(key) {
                    value.insert(key.into(), field.clone());
                }
            }
        }
        // The site's own question is part of the page it waits on: it goes in
        // the outline beside the elements, and as the request the page's
        // answer actions name, so an agent answers it as the person does.
        if let Some((request, origin, rights)) = self.capabilities.pending() {
            let ask = format!("{origin} asks to use your {rights}");
            // The line names the answers where a reader finds the page's other
            // buttons, so the question is answered without looking further.
            let line = format!(
                "- permission \"{ask}\" [request={request}, resource={}; answer it with the \
                 page's answer-permission actions]\n",
                crate::capabilities::RESOURCE
            );
            let outline = value
                .get("outline")
                .and_then(|outline| outline.as_str())
                .unwrap_or_default();
            let joined = if outline.is_empty() {
                line
            } else {
                format!("{}\n{line}", outline.trim_end_matches('\n'))
            };
            value.insert("outline".into(), joined.into());
            value.insert(
                "permission".into(),
                serde_json::json!({
                    "request": request,
                    "origin": origin,
                    "rights": rights,
                    "resource": crate::capabilities::RESOURCE,
                }),
            );
            if let Some(serde_json::Value::String(message)) = value.get_mut("message") {
                message.push_str("; the site asks for permission");
            }
        }
        if let Some(data) = &reply.data {
            value.insert("data".into(), data.clone());
        }
        // The engine counts what waits behind the request it runs. This tab
        // sends it one at a time, so the actions waiting their turn here are
        // the ones a reader of the record is asking about.
        if let (PageRequest::Ledger { .. }, Some(serde_json::Value::Object(data))) =
            (request, value.get_mut("data"))
        {
            let behind_the_engine = data
                .get("waiting")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let waiting = u64::try_from(self.page_waiting.len())
                .unwrap_or(u64::MAX)
                .saturating_add(behind_the_engine);
            data.insert("waiting".into(), waiting.into());
        }
        // Several actors may work in one tab: where this action stands in
        // the tab's record, and what the others did since this actor last
        // heard, so it learns the page moved under it.
        if let Some(serde_json::Value::Object(record)) = Self::page_record_details(&reply.record) {
            value.extend(record);
        }
        // Everything here came from the page: data to reason about, never
        // instructions to follow.
        value.insert("provenance".into(), "web-content".into());
        // A picture is not a finding: its bytes travel as the action's own
        // content, which a reader that can see shows and one that cannot
        // leaves alone. A picture nothing could keep says so, rather than
        // answering as if the page had none. Said before `agent` because a
        // picture that could not be kept is no content at all.
        let resources = match (&reply.image, &self.content) {
            (Some(image), Some(content)) => match Self::kept_picture(image, content) {
                Ok(address) => vec![address],
                Err(error) => {
                    value.insert("picture".into(), error.into());
                    Vec::new()
                }
            },
            (Some(_), None) => {
                value.insert(
                    "picture".into(),
                    "this window has no place to keep a picture of the page".into(),
                );
                Vec::new()
            }
            (None, _) => Vec::new(),
        };
        // What the action was, for a reader holding content this answer
        // names: a read of the page, whose picture is the page to look at.
        // Said here because the answer's resource is the id this window
        // bound `browser.page` under (`tilo-ui-bound:v1:…`), which resolves
        // to no action, so a reader asking the host afterwards could not tell
        // a read from an effect and would treat the picture as an artifact
        // the app had made. Only an answer with content to show says it: no
        // other answer has a picture whose kind a reader must know.
        if !resources.is_empty() {
            value.insert(
                "agent".into(),
                serde_json::json!({"kind": "view", "action": request.action()}),
            );
        }
        ui::tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: ui::tilo_actions::ActionOutput {
                value: Some(value.into()),
                resources,
            },
        }
    }

    /// Why a page request outside its bounds is refused before it reaches the
    /// page, in the words its caller needs to ask again rightly.
    pub(super) fn refused_page_request(request: &PageRequest) -> String {
        if !request.frame_valid() {
            return format!(
                "`{}` was given a frame id the page outline never printed",
                request.action()
            );
        }
        match request {
            PageRequest::Scroll { .. } => "`scroll` goes to an end of the page, by a distance, \
                                          or brings an element into view: one of them, and no two"
                .into(),
            PageRequest::Outline { .. } => format!(
                "`outline` reads on from a line between 1 and {}: the line a read that stopped \
                 short named, or the line of a `search` match",
                ui::webview::page::MAX_OUTLINE_FROM
            ),
            PageRequest::Search { .. } => format!(
                "`search` takes the words to find: 1 to {} characters",
                ui::webview::page::MAX_SEARCH_UNITS
            ),
            PageRequest::Settle { .. } => format!(
                "`settle` waits for the page to hold still for {} ms or more, {} ms at most in \
                 all, and takes the words to wait for, and the words to see gone, as 1 to {} \
                 characters each",
                ui::webview::page::MIN_SETTLE_QUIET_MS,
                ui::webview::page::MAX_SETTLE_MS,
                ui::webview::page::MAX_SEARCH_UNITS
            ),
            PageRequest::FillForm { .. } => format!(
                "`fill_form` takes a form's id or signature of at most {} characters and at \
                 most {} answers by the names the form asks under, each a text, a number, true \
                 or false, or a list of texts; the form is sent with its first submit button \
                 or with the one a label names, not both ways at once",
                ui::webview::page::MAX_FORM_UNITS,
                ui::webview::page::MAX_FORM_ANSWERS
            ),
            _ => format!(
                "`{}` names no element of the page outline, or arguments the page cannot take",
                request.action()
            ),
        }
    }

    /// What an answer says of the tab's record, as the fields an action's
    /// output and a refusal's details both carry: `seq`, the action's own
    /// place; `others`, what the other actors did since this one last heard;
    /// `others_earlier`, how many older ones that leaves out. `None` when it
    /// says nothing. The entries are page content, like the rest.
    pub(super) fn page_record_details(
        record: &ui::webview::PageRecordNote,
    ) -> Option<serde_json::Value> {
        if record.is_empty() {
            return None;
        }
        let mut fields = serde_json::Map::new();
        if let Some(seq) = record.seq {
            fields.insert("seq".into(), seq.into());
        }
        if !record.others.is_empty() {
            fields.insert("others".into(), record.others.clone().into());
        }
        if record.others_earlier > 0 {
            fields.insert("others_earlier".into(), record.others_earlier.into());
        }
        Some(fields.into())
    }

    /// A picture of the page kept as content the answer can name, or why it
    /// could not be kept.
    pub(super) fn kept_picture(
        image: &[u8],
        content: &ui::content::ContentStore,
    ) -> Result<String, &'static str> {
        let hash = content
            .put(image)
            .map_err(|_| "this window could not keep a picture of the page")?;
        ui::tilo_actions::content_resource(&hash)
            .ok_or("this window kept the picture under no address")
    }
}
