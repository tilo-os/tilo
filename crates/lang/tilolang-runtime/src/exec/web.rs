//! The web capability: a request an action makes, and what a reply offers.
//!
//! Each request is journaled as the run makes it, so a run that replays makes each one once. A
//! host the package did not declare, one the workspace revoked, and any network failure are
//! values the run carries on with.

use super::{Args, Core, HResult, Imp, JournalEntry, Rc, Value, Wasm, ret_i32, trap};

impl Core {
    /// `web.get`, `web.post` and what a reply is asked: its status, headers and body.
    pub(crate) fn web_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::WebGet => {
                let url = self.text(a.h(0)?)?;
                let headers = self.web_headers(a.h(1)?)?;
                let if_none_match = match self.val(a.h(2)?)? {
                    Value::Opt(Some(tag)) => Some(match *tag {
                        Value::Text(tag) => tag,
                        other => {
                            return trap(format!(
                                "if_none_match takes text, got {}",
                                other.kind_name()
                            ));
                        }
                    }),
                    Value::Opt(None) => None,
                    other => {
                        return trap(format!(
                            "if_none_match takes text, got {}",
                            other.kind_name()
                        ));
                    }
                };
                let request = crate::run::WebRequest {
                    method: "GET".to_owned(),
                    url,
                    headers,
                    body: None,
                    if_none_match,
                };
                let response = self.web(request)?;
                self.ret_h(Value::Response(Rc::new(response)))
            }
            Imp::WebPost => {
                let url = self.text(a.h(0)?)?;
                let body = self.text(a.h(1)?)?;
                let headers = self.web_headers(a.h(2)?)?;
                let request = crate::run::WebRequest {
                    method: "POST".to_owned(),
                    url,
                    headers,
                    body: Some(body),
                    if_none_match: None,
                };
                let response = self.web(request)?;
                self.ret_h(Value::Response(Rc::new(response)))
            }
            Imp::WebOk => {
                let response = self.response_arg(a.h(0)?)?;
                ret_i32(i32::from(response.ok()))
            }
            Imp::WebStatus => {
                let response = self.response_arg(a.h(0)?)?;
                crate::core::ret_i64(i64::from(response.status))
            }
            Imp::WebStatusText => {
                let response = self.response_arg(a.h(0)?)?;
                self.ret_h(Value::text(response.status_text.clone()))
            }
            Imp::WebHeaders => {
                let response = self.response_arg(a.h(0)?)?;
                let headers = response
                    .headers
                    .iter()
                    .map(|line| Value::text(line.clone()))
                    .collect();
                self.ret_h(Value::List(headers))
            }
            Imp::WebBody => {
                let response = self.response_arg(a.h(0)?)?;
                let bytes = Rc::new(response.bytes());
                self.ret_h(Value::Bytes(bytes))
            }
            Imp::WebText => {
                let text = match self.val(a.h(0)?)? {
                    Value::Response(response) => {
                        String::from_utf8_lossy(&response.bytes()).into_owned()
                    }
                    Value::Bytes(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    other => {
                        return trap(format!("expected bytes, got {}", other.kind_name()));
                    }
                };
                self.ret_h(Value::Text(text))
            }
            // The bytes a base64 text holds: how a picture or any other bytes an action came by
            // as text (a vCard's PHOTO, a body written inline) becomes a file it can keep.
            Imp::Base64Decode => {
                let text = self.text(a.h(0)?)?;
                use base64::Engine as _;
                let bytes = match base64::engine::general_purpose::STANDARD.decode(text.trim()) {
                    Ok(bytes) => bytes,
                    Err(why) => return trap(format!("this is not base64: {why}")),
                };
                self.ret_h(Value::Bytes(Rc::new(bytes)))
            }
            Imp::BytesLength => {
                let n = match self.val(a.h(0)?)? {
                    Value::Bytes(bytes) => bytes.len(),
                    other => {
                        return trap(format!("expected bytes, got {}", other.kind_name()));
                    }
                };
                crate::core::ret_i64(i64::try_from(n).unwrap_or(i64::MAX))
            }
            _ => trap("not a web import"),
        }
    }

    /// The headers a call was given: each `Name: value`.
    fn web_headers(&self, h: i32) -> HResult<Vec<String>> {
        match self.val(h)? {
            Value::List(items) => items
                .iter()
                .map(|item| match item {
                    Value::Text(line) => Ok(line.clone()),
                    other => trap(format!(
                        "headers take text like `Name: value`, got {}",
                        other.kind_name()
                    )),
                })
                .collect(),
            other => trap(format!("headers take a list, got {}", other.kind_name())),
        }
    }

    fn response_arg(&self, h: i32) -> HResult<Rc<crate::run::WebResponse>> {
        self.with_val(h, |v| match v {
            Value::Response(response) => Ok(Rc::clone(response)),
            other => trap(format!("expected a response, got {}", other.kind_name())),
        })
    }

    /// Fetch `request` for the run running, once: its journal answers a request it made before,
    /// else the transport does and the answer is recorded, so a run that replays makes each
    /// request once. A host the package did not declare, one the workspace revoked, and any
    /// network failure are values: the run carries on and sees why.
    fn web(&self, request: crate::run::WebRequest) -> HResult<crate::run::WebResponse> {
        {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut() else {
                return trap("a request is only made in an action");
            };
            if let Some(entry) = run.journal.entries.get(run.cursor) {
                let JournalEntry::Web {
                    request: before,
                    response,
                } = entry
                else {
                    return trap(
                        "the run diverged from its journal: expected a web request's entry",
                    );
                };
                if *before != request {
                    return trap(format!(
                        "the run diverged from its journal: it now asks for {} {}, not {} {}",
                        request.method, request.url, before.method, before.url
                    ));
                }
                let response = response.clone();
                run.cursor = run.cursor.saturating_add(1);
                return Ok(response);
            }
        }
        let response = self.web_response(&request);
        self.journal_push(JournalEntry::Web {
            request,
            response: response.clone(),
        })?;
        Ok(response)
    }

    /// The answer `request` gets now: the transport's, or why it was not made.
    fn web_response(&self, request: &crate::run::WebRequest) -> crate::run::WebResponse {
        let Ok(module) = self.module_of_frame() else {
            return crate::run::WebResponse::of(crate::web::WebReply::failed(
                "web is only available in actions",
            ));
        };
        let package = self.registry.borrow().package(&module);
        let Some(package) = package else {
            return crate::run::WebResponse::of(crate::web::WebReply::failed(format!(
                "no package wrote the code at {}",
                module.0
            )));
        };
        let app = package.meta.app.clone();
        let Some(host) = crate::web::host_of(&request.url) else {
            return crate::run::WebResponse::of(crate::web::WebReply::failed(
                "only http and https requests are made",
            ));
        };
        if !crate::web::granted(&package.meta.web, &host) {
            return crate::run::WebResponse::of(crate::web::WebReply::failed(format!(
                "`{app}` may not reach `{host}`: it declares web: [{}]",
                package.meta.web.join(", ")
            )));
        }
        if !self.authority.may_fetch(&app, &host) {
            return crate::run::WebResponse::of(crate::web::WebReply::failed(format!(
                "the workspace revoked `{host}` for `{app}`"
            )));
        }
        let reply = self.web.borrow().as_ref().map_or_else(
            || crate::web::WebReply::failed("this host reaches no services"),
            |transport| transport.fetch(request),
        );
        crate::run::WebResponse::of(reply)
    }
}
