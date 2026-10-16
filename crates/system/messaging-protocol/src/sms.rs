//! `ModemManager` SMS contract. No phone-number guessing or modem selection.
//! Values are independently mapped from `ModemManager`-enums.h and the SMS
//! D-Bus interface, not from UI labels or a successful Send method return.

use crate::{Error, Route, Service};
use zeroize::Zeroizing;

/// Typed create properties, sent in a D-Bus body rather than command arguments.
pub struct CreateText {
    pub number: String,
    pub text: Zeroizing<String>,
    pub delivery_report_request: bool,
}

/// Prepare one explicitly selected international recipient. Short codes and
/// national-format numbers require separate user-resolved routes, not guessing.
///
/// # Errors
/// Refuses other services, unresolved numbers and empty/oversized text.
pub fn create_text(route: &Route, text: &str) -> Result<CreateText, Error> {
    if route.service() != Service::Sms {
        return Err(Error::Unsupported);
    }
    let Some(digits) = route.recipient().strip_prefix('+') else {
        return Err(Error::InvalidIdentity);
    };
    if digits.is_empty()
        || digits.len() > 15
        || digits.starts_with('0')
        || !digits.bytes().all(|digit| digit.is_ascii_digit())
    {
        return Err(Error::InvalidIdentity);
    }
    if text.trim().is_empty() || text.len() > crate::signal::MAX_TEXT_BYTES {
        return Err(Error::InvalidRequest);
    }
    Ok(CreateText {
        number: route.recipient().into(),
        text: Zeroizing::new(text.into()),
        delivery_report_request: true,
    })
}

/// Raw modem evidence supplied by the authorized broker's selected modem/SIM.
/// Account, ICCID and modem binding remain outside this raw property decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observation {
    Unknown,
    Stored,
    ReceivingParts,
    IncomingComplete,
    QueuedForNetwork,
    SentToNetwork,
    /// A report is evidence for reconciliation, not an automatically matched
    /// message. `MessageReference` alone wraps/reuses and must not grant delivery.
    DeliveryReport {
        reference: u32,
        state: u32,
    },
}

/// Interpret actual state/PDU properties without inventing delivery from Send.
///
/// # Errors
/// Refuses unknown state/PDU combinations. Reports need a complete inbound PDU.
pub fn observe(
    state: u32,
    pdu_type: u32,
    reference: u32,
    delivery_state: u32,
) -> Result<Observation, Error> {
    match (state, pdu_type) {
        (0, _) => Ok(Observation::Unknown),
        (1, 2 | 33) => Ok(Observation::Stored),
        (2, 1 | 3 | 32) => Ok(Observation::ReceivingParts),
        (3, 1 | 32) => Ok(Observation::IncomingComplete),
        (3, 3) => Ok(Observation::DeliveryReport {
            reference,
            state: delivery_state,
        }),
        (4, 2 | 33) => Ok(Observation::QueuedForNetwork),
        (5, 2 | 33) => Ok(Observation::SentToNetwork),
        _ => Err(Error::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn send_properties_preserve_exact_recipient_without_normalization() {
        let route = Route::new(Service::Sms, "sim-account".into(), "+12025550123".into()).unwrap();
        let properties = create_text(&route, "hello").unwrap();
        assert_eq!(properties.number, "+12025550123");
        assert_eq!(properties.text.as_str(), "hello");
        assert!(properties.delivery_report_request);
        for number in ["2025550123", "+1(202)5550123", "+0123", "+1234567890123456"] {
            let route = Route::new(Service::Sms, "sim-account".into(), number.into()).unwrap();
            assert!(matches!(
                create_text(&route, "hello"),
                Err(Error::InvalidIdentity)
            ));
        }
    }
    #[test]
    fn multipart_receive_and_network_send_are_not_delivery_reports() {
        assert_eq!(observe(2, 1, 7, 0), Ok(Observation::ReceivingParts));
        assert_eq!(observe(3, 1, 7, 0), Ok(Observation::IncomingComplete));
        assert_eq!(observe(4, 2, 7, 0), Ok(Observation::QueuedForNetwork));
        assert_eq!(observe(5, 2, 7, 0), Ok(Observation::SentToNetwork));
        assert_eq!(
            observe(3, 3, 7, 0),
            Ok(Observation::DeliveryReport {
                reference: 7,
                state: 0
            })
        );
        assert_eq!(
            observe(3, 3, 7, 1),
            Ok(Observation::DeliveryReport {
                reference: 7,
                state: 1
            })
        );
        assert_eq!(observe(5, 3, 7, 0), Err(Error::Unsupported));
    }
}
