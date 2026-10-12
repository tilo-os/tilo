//! Hand-written [`zbus::object_server::Interface`] plumbing for D-Bus
//! interfaces whose methods carry more wire arguments than a Rust signature
//! should. Each method's arguments decode into a named request type and the
//! reply, `NO_REPLY_EXPECTED` handling and error mapping follow what zbus's
//! `#[interface]` macro generates, so a hand-dispatched interface looks the
//! same on the bus.

use std::fmt::Write;
use std::future::Future;

use serde::Serialize;
use serde::de::DeserializeOwned;
use zbus::message::{Flags, Header, Message};
use zbus::object_server::DispatchResult2;
use zbus::zvariant::{DynamicType, Type};
use zbus::{Connection, fdo};

/// Write `xml`, an interface's introspection rendered at level 0, indented by
/// `level`. Blank lines (paragraph breaks in doc comments) stay unindented,
/// as the macro writes them.
pub(crate) fn write_introspection(writer: &mut dyn Write, level: usize, xml: &str) {
    for line in xml.lines() {
        let _ = if line.is_empty() {
            writeln!(writer)
        } else {
            writeln!(writer, "{:level$}{line}", "")
        };
    }
}

/// Implement [`zbus::object_server::Interface`] for `$ty`, a property-less
/// interface named `$name` and introspected as `$xml`. Method calls go to the
/// type's inherent `dispatch_call(connection, message, member)`, which
/// routes each member through [`dispatch`] or [`answer`].
macro_rules! method_interface {
    ($ty:ty, $name:literal, $xml:expr) => {
        #[zbus::export::async_trait::async_trait]
        impl zbus::object_server::Interface for $ty {
            fn name() -> zbus::names::InterfaceName<'static> {
                zbus::names::InterfaceName::from_static_str_unchecked($name)
            }

            async fn get(
                &self,
                _property: &str,
                _server: &zbus::ObjectServer,
                _connection: &zbus::Connection,
                _header: Option<&zbus::message::Header<'_>>,
                _emitter: &zbus::object_server::SignalEmitter<'_>,
            ) -> Option<zbus::fdo::Result<zbus::zvariant::OwnedValue>> {
                None
            }

            async fn get_all(
                &self,
                _server: &zbus::ObjectServer,
                _connection: &zbus::Connection,
                _header: Option<&zbus::message::Header<'_>>,
                _emitter: &zbus::object_server::SignalEmitter<'_>,
            ) -> zbus::fdo::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>
            {
                Ok(std::collections::HashMap::new())
            }

            fn set<'call>(
                &'call self,
                _property: &'call str,
                _value: &'call zbus::zvariant::Value<'_>,
                _server: &'call zbus::ObjectServer,
                _connection: &'call zbus::Connection,
                _header: Option<&'call zbus::message::Header<'_>>,
                _emitter: &'call zbus::object_server::SignalEmitter<'_>,
            ) -> zbus::object_server::DispatchResult2<'call> {
                zbus::object_server::DispatchResult2::NotFound
            }

            async fn set_mut(
                &mut self,
                _property: &str,
                _value: &zbus::zvariant::Value<'_>,
                _server: &zbus::ObjectServer,
                _connection: &zbus::Connection,
                _header: Option<&zbus::message::Header<'_>>,
                _emitter: &zbus::object_server::SignalEmitter<'_>,
            ) -> Option<zbus::fdo::Result<()>> {
                None
            }

            fn call<'call>(
                &'call self,
                _server: &'call zbus::ObjectServer,
                connection: &'call zbus::Connection,
                message: &'call zbus::message::Message,
                name: zbus::names::MemberName<'call>,
            ) -> zbus::object_server::DispatchResult2<'call> {
                self.dispatch_call(connection, message, name.as_str())
            }

            fn call_mut<'call>(
                &'call mut self,
                _server: &'call zbus::ObjectServer,
                _connection: &'call zbus::Connection,
                _message: &'call zbus::message::Message,
                _name: zbus::names::MemberName<'call>,
            ) -> zbus::object_server::DispatchResult2<'call> {
                zbus::object_server::DispatchResult2::NotFound
            }

            fn introspect_to_writer(&self, writer: &mut dyn std::fmt::Write, level: usize) {
                $crate::portal::interface_dispatch::write_introspection(writer, level, $xml);
            }
        }
    };
}
pub(crate) use method_interface;

/// Decode `message`'s body as `Args`, run `handler` with the call's header,
/// and reply with its result. A body that does not decode is answered with
/// the decoding error.
pub(crate) fn dispatch<'call, Args, Reply, Fut>(
    connection: &'call Connection,
    message: &'call Message,
    handler: impl FnOnce(Header<'call>, Args) -> Fut + Send + 'call,
) -> DispatchResult2<'call>
where
    Args: DeserializeOwned + Type + Send + 'call,
    Reply: Serialize + DynamicType + Send + Sync,
    Fut: Future<Output = fdo::Result<Reply>> + Send + 'call,
{
    DispatchResult2::Async(Box::pin(async move {
        let args = match message.body().deserialize::<Args>() {
            Ok(args) => args,
            Err(e) => {
                let hdr = message.header();
                return connection
                    .reply_dbus_error(&hdr, fdo::Error::from(e))
                    .await
                    .map_err(into_fdo);
            }
        };
        send_reply(connection, message, handler(message.header(), args).await).await
    }))
}

/// Reply to an argument-less call with `reply`'s result.
pub(crate) fn answer<'call, Reply, Fut>(
    connection: &'call Connection,
    message: &'call Message,
    reply: Fut,
) -> DispatchResult2<'call>
where
    Reply: Serialize + DynamicType + Send + Sync,
    Fut: Future<Output = fdo::Result<Reply>> + Send + 'call,
{
    DispatchResult2::Async(Box::pin(async move {
        send_reply(connection, message, reply.await).await
    }))
}

async fn send_reply<Reply>(
    connection: &Connection,
    message: &Message,
    reply: fdo::Result<Reply>,
) -> fdo::Result<()>
where
    Reply: Serialize + DynamicType + Send + Sync,
{
    let hdr = message.header();
    if hdr.primary().flags().contains(Flags::NoReplyExpected) {
        return Ok(());
    }
    match reply {
        Ok(r) => connection.reply(&hdr, &r).await,
        Err(e) => connection.reply_dbus_error(&hdr, e).await,
    }
    .map_err(into_fdo)
}

fn into_fdo(e: zbus::Error) -> fdo::Error {
    match e {
        zbus::Error::FDO(e) => *e,
        e => fdo::Error::Failed(format!("{e}")),
    }
}

/// The concatenated signature of the `in` arguments `xml` advertises for
/// `method`, for tests that pin a request type to its introspection.
#[cfg(test)]
pub(crate) fn advertised_in_signature(xml: &str, method: &str) -> String {
    let open = format!("<method name=\"{method}\">");
    let start = xml.find(&open).expect("method is introspected");
    let body = &xml[start..];
    let body = &body[..body.find("</method>").expect("method closes")];
    body.lines()
        .filter(|line| line.contains("direction=\"in\""))
        .map(|line| {
            let ty = &line[line.find("type=\"").expect("arg has a type") + 6..];
            ty[..ty.find('"').expect("type closes")].to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn introspection_is_indented_like_the_interface_macro_writes_it() {
        let xml = "<interface name=\"a.B\">\n  <!--\n   One.\n\n   Two.\n   -->\n  <method name=\"C\">\n  </method>\n</interface>\n";
        let mut out = String::new();
        write_introspection(&mut out, 2, xml);
        assert_eq!(
            out,
            "  <interface name=\"a.B\">\n    <!--\n     One.\n\n     Two.\n     -->\n    <method name=\"C\">\n    </method>\n  </interface>\n"
        );
    }
}
