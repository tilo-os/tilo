//! Loopback TLS credentials for disposable registry fixtures.
//!
//! Kept behind the `tls-fixture` feature so the kernel recipe can build the
//! signer without this test-only certificate path as an input.

use zeroize::Zeroize;

/// Ephemeral CA and server credentials for a loopback TLS fixture.
pub struct TlsFixture {
    pub ca_certificate_pem: String,
    pub server_private_key_pem: String,
    pub server_certificate_pem: String,
}

impl Drop for TlsFixture {
    fn drop(&mut self) {
        self.server_private_key_pem.zeroize();
    }
}

/// Generate a constrained ephemeral CA and an IP-SAN server certificate.
///
/// # Errors
///
/// Returns an error if certificate validity bounds overflow, key generation
/// fails, or either certificate cannot be encoded or signed.
pub fn generate_tls_fixture(server_ip: std::net::IpAddr) -> Result<TlsFixture, String> {
    use rcgen::{
        BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose,
        IsCa, Issuer, KeyPair, KeyUsagePurpose,
    };

    let now = time::OffsetDateTime::now_utc();
    let not_before = now
        .checked_sub(time::Duration::hours(1))
        .ok_or("TLS certificate start date overflow")?;
    let not_after = now
        .checked_add(time::Duration::days(2))
        .ok_or("TLS certificate expiry date overflow")?;
    let mut ca_params = CertificateParams::default();
    ca_params.not_before = not_before;
    ca_params.not_after = not_after;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params.distinguished_name = DistinguishedName::new();
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Tilo Disposable Registry CA");
    let ca_key =
        KeyPair::generate().map_err(|error| format!("cannot generate TLS CA key: {error}"))?;
    let ca_certificate = ca_params
        .self_signed(&ca_key)
        .map_err(|error| format!("cannot generate TLS CA certificate: {error}"))?;
    let issuer = Issuer::new(ca_params, ca_key);

    let mut server_params = CertificateParams::new(vec![server_ip.to_string()])
        .map_err(|error| format!("cannot encode TLS server IP: {error}"))?;
    server_params.not_before = not_before;
    server_params.not_after = not_after;
    server_params.is_ca = IsCa::NoCa;
    // Strict RFC 5280 verification requires non-self-signed certificates to
    // identify their issuer's key. Python's default TLS context enforces this.
    server_params.use_authority_key_identifier_extension = true;
    server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    server_params.distinguished_name = DistinguishedName::new();
    server_params
        .distinguished_name
        .push(DnType::CommonName, "Tilo Loopback Registry");
    let server_key =
        KeyPair::generate().map_err(|error| format!("cannot generate TLS server key: {error}"))?;
    let server_certificate = server_params
        .signed_by(&server_key, &issuer)
        .map_err(|error| format!("cannot sign TLS server certificate: {error}"))?;
    Ok(TlsFixture {
        ca_certificate_pem: ca_certificate.pem(),
        server_private_key_pem: server_key.serialize_pem(),
        server_certificate_pem: server_certificate.pem(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tls_fixture_has_constrained_ca_and_loopback_san() {
        use der::{Decode, DecodePem};
        use x509_cert::{
            Certificate,
            ext::pkix::{
                AuthorityKeyIdentifier, BasicConstraints, SubjectAltName, SubjectKeyIdentifier,
                name::GeneralName,
            },
        };

        fn extension<T: for<'a> Decode<'a>>(
            certificate: &Certificate,
            oid: const_oid::ObjectIdentifier,
        ) -> Option<T> {
            certificate
                .tbs_certificate
                .extensions
                .as_deref()?
                .iter()
                .find(|extension| extension.extn_id == oid)
                .map(|extension| T::from_der(extension.extn_value.as_bytes()).unwrap())
        }

        let fixture = generate_tls_fixture("127.0.0.1".parse().unwrap()).expect("TLS fixture");
        assert!(fixture.server_private_key_pem.contains("PRIVATE KEY"));

        let ca = Certificate::from_pem(fixture.ca_certificate_pem.as_bytes()).unwrap();
        let ca_constraints: BasicConstraints =
            extension(&ca, const_oid::db::rfc5280::ID_CE_BASIC_CONSTRAINTS).unwrap();
        assert!(ca_constraints.ca);

        let server = Certificate::from_pem(fixture.server_certificate_pem.as_bytes()).unwrap();
        let ca_key_id: SubjectKeyIdentifier =
            extension(&ca, const_oid::db::rfc5280::ID_CE_SUBJECT_KEY_IDENTIFIER)
                .expect("CA subject key identifier");
        let issuer_key_id = extension::<AuthorityKeyIdentifier>(
            &server,
            const_oid::db::rfc5280::ID_CE_AUTHORITY_KEY_IDENTIFIER,
        )
        .and_then(|identifier| identifier.key_identifier)
        .expect("server authority key identifier");
        assert_eq!(
            issuer_key_id, ca_key_id.0,
            "server identifies the signing CA key"
        );
        assert!(
            !extension::<BasicConstraints>(
                &server,
                const_oid::db::rfc5280::ID_CE_BASIC_CONSTRAINTS
            )
            .is_some_and(|constraints| constraints.ca)
        );
        let san: SubjectAltName =
            extension(&server, const_oid::db::rfc5280::ID_CE_SUBJECT_ALT_NAME).unwrap();
        assert!(san.0.iter().any(
            |name| matches!(name, GeneralName::IpAddress(bytes) if bytes.as_bytes() == [127, 0, 0, 1])
        ));
    }
}
