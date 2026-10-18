//! Private, selected website credentials sent only to an isolated engine profile.
use browser_continuity_core::sessions::{Service, SessionCookies};
use serde_json::{Value, json};

#[derive(Clone, PartialEq, Eq)]
pub struct ServiceSessionRequest {
    pub request_id: u32,
    pub verify_only: bool,
    pub session: SessionCookies,
}
impl std::fmt::Debug for ServiceSessionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServiceSessionRequest { private credentials }")
    }
}
impl ServiceSessionRequest {
    pub(crate) fn valid(&self) -> bool {
        let now = crate::time::wall()
            .duration_since(crate::time::UNIX_EPOCH)
            .ok()
            .and_then(|v| i64::try_from(v.as_secs()).ok());
        self.request_id > 0
            && i32::try_from(self.request_id).is_ok()
            && self.session.validate().is_ok()
            && now.is_some_and(|now| {
                self.session.cookies.iter().all(|cookie| {
                    cookie
                        .expires_unix_seconds
                        .is_none_or(|expiry| expiry > now)
                })
            })
    }
    pub(crate) fn encode(&self) -> Option<Value> {
        self.valid().then(|| {
            json!({
                "op": "service_session_import", "request_id": self.request_id,
                "session_id": self.session.session_id, "service": self.session.service,
                "cookies": self.session.cookies,
                "verify_only": self.verify_only,
                "origin_storage": self.session.origin_storage.iter().map(|storage| json!({
                    "origin": storage.origin, "key": storage.key, "value": storage.value,
                })).collect::<Vec<_>>(),
            })
        })
    }
}
/// Import completion proves storage writes, never provider authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSessionReply {
    pub request_id: u32,
    pub session_id: String,
    pub service: Service,
    pub imported: bool,
    pub cookie_count: usize,
    pub origin_storage_count: usize,
}
impl ServiceSessionReply {
    pub(crate) fn decode(value: &Value) -> Option<Self> {
        let fields = value.as_object()?;
        if value["event"].as_str()? != "service_session_result"
            || fields.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "event"
                        | "tab"
                        | "request_id"
                        | "session_id"
                        | "service"
                        | "imported"
                        | "cookie_count"
                        | "origin_storage_count"
                )
            })
            || value.get("tab").is_some_and(|tab| {
                !tab.as_u64()
                    .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
            })
        {
            return None;
        }
        let request_id = u32::try_from(value["request_id"].as_u64()?).ok()?;
        let session_id = value["session_id"].as_str()?.to_owned();
        let service = serde_json::from_value(value["service"].clone()).ok()?;
        let imported = value["imported"].as_bool()?;
        let cookie_count = usize::try_from(value["cookie_count"].as_u64()?).ok()?;
        let origin_storage_count = value
            .get("origin_storage_count")
            .map_or(Some(0), |count| count.as_u64())
            .and_then(|count| usize::try_from(count).ok())?;
        (request_id > 0
            && i32::try_from(request_id).is_ok()
            && browser_continuity_core::sessions::valid_id(&session_id)
            && cookie_count <= browser_continuity_core::sessions::MAX_COOKIES
            && origin_storage_count <= 1
            && (origin_storage_count == 0 || service == Service::Discord)
            && (!imported || cookie_count > 0 || origin_storage_count > 0))
            .then_some(Self {
                request_id,
                session_id,
                service,
                imported,
                cookie_count,
                origin_storage_count,
            })
    }
}
