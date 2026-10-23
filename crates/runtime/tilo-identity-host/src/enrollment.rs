//! Redeeming an enrollment token: the one place a token becomes a device
//! certificate.
//!
//! A trusted device mints a short-lived token ([`crate::device_fleet`]); the
//! device that receives it returns its public key, and the approving device
//! redeems the token for a root-signed certificate here. Every redeemer — the
//! desktop's fleet and the nearby-pairing broker alike — goes through
//! [`redeem`], so every one of them applies the same rules:
//!
//! * the token is genuine, unexpired and grants the shape of its
//!   [`EnrollmentRole`];
//! * its issuer is this identity's device, was certified both when the token
//!   was minted and now, and is not revoked; it must be allowed to admit
//!   devices unless the person's devices that hold the identity's key
//!   approved the certificate themselves ([`Certifier::Approved`]);
//! * its nonce has never been redeemed, recorded durably in the one
//!   [`NonceStore`] before the certificate leaves this function.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, EnrollmentToken, verify_device_cert, verify_device_cert_at,
    verify_enrollment_token,
};

use crate::device_fleet::DeviceFleetStore;

/// What a token admits a device as. Each role has one capability preset, and
/// a token whose capabilities have the other role's shape is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EnrollmentRole {
    /// One of the person's own devices: it owns workspaces and does not host.
    PersonalDevice,
    /// An always-on home server: it hosts and reads every workspace.
    HomePeer,
}

impl EnrollmentRole {
    /// The capabilities a token minted for this role grants.
    #[must_use]
    pub fn preset(self) -> DeviceCapabilities {
        match self {
            Self::PersonalDevice => DeviceCapabilities::member(),
            Self::HomePeer => DeviceCapabilities::host(),
        }
    }

    /// Whether `capabilities` have the shape of this role: a home peer hosts
    /// and reads everything, a personal device does neither.
    #[must_use]
    pub fn admits(self, capabilities: &DeviceCapabilities) -> bool {
        let hosts = capabilities.can_act_as_host;
        let reads_all = capabilities.can_read_all_workspaces;
        match self {
            Self::PersonalDevice => !hosts && !reads_all,
            Self::HomePeer => hosts && reads_all,
        }
    }

    /// The stable name callers pass, `personal-device` or `home-peer`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PersonalDevice => "personal-device",
            Self::HomePeer => "home-peer",
        }
    }

    /// The role named by [`Self::as_str`].
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "personal-device" => Some(Self::PersonalDevice),
            "home-peer" => Some(Self::HomePeer),
            _ => None,
        }
    }
}

fn refused(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message.into())
}

/// Check that `issuer` may have minted `token`: it belongs to the token's
/// identity, is the device that signed the token, was valid when the token
/// was minted and is valid now, and is not revoked. When it `admits` the
/// device on its own authority it must also be allowed to admit devices.
pub fn check_issuer(
    token: &EnrollmentToken,
    issuer: &DeviceCert,
    issuer_revoked: bool,
    admits: bool,
    now_ns: u64,
) -> io::Result<()> {
    if issuer.user_public_key != token.user_public_key {
        return Err(refused("enrollment token belongs to another identity"));
    }
    if issuer.device_public_key != token.issuer_device_public_key {
        return Err(refused("enrollment token was minted by another device"));
    }
    verify_device_cert_at(issuer, token.issued_at_ns)
        .map_err(|error| refused(format!("issuer at the token's issue time: {error}")))?;
    verify_device_cert_at(issuer, now_ns).map_err(|error| refused(error.to_string()))?;
    if issuer_revoked || (admits && !issuer.capabilities.can_admit_devices) {
        return Err(refused("issuer is revoked or cannot admit devices"));
    }
    Ok(())
}

/// Consumed enrollment nonces: one durable marker file per redeemed token,
/// named by its nonce and holding the key of the device it admitted.
///
/// The markers live under the identity root (`enrollment-nonces/`), which
/// both the session and the privileged nearby-pairing broker reach, so a
/// token redeemed by either is spent for both. Markers are kept permanently:
/// a wall clock set back must not revive a token.
#[derive(Clone, Debug)]
pub struct NonceStore {
    directory: PathBuf,
}

/// What a migrated marker holds: the admitted device is not known, so no
/// device can ever re-record the nonce as its own.
const MIGRATED_MARKER: &str = "migrated";

impl NonceStore {
    /// The store under `identity_root`, the directory holding
    /// `user-identity/` and `device-private/`.
    #[must_use]
    pub fn under(identity_root: &Path) -> Self {
        Self {
            directory: identity_root.join("enrollment-nonces"),
        }
    }

    fn path(&self, nonce: &[u8; 16]) -> PathBuf {
        self.directory.join(hex::encode(nonce))
    }

    /// Whether `nonce` has been redeemed.
    ///
    /// # Errors
    ///
    /// Returns an error when the marker cannot be examined.
    pub fn is_consumed(&self, nonce: &[u8; 16]) -> io::Result<bool> {
        match fs::symlink_metadata(self.path(nonce)) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Spend `nonce` for `device`. A token can be spent once: any existing
    /// marker refuses, including one for the same device.
    ///
    /// # Errors
    ///
    /// `PermissionDenied` when the nonce is already spent; otherwise the
    /// error writing or syncing the marker.
    pub fn claim(&self, nonce: &[u8; 16], device: [u8; 32]) -> io::Result<()> {
        match self.create(nonce, &hex::encode(device)) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(refused("enrollment token has already been consumed"))
            }
            other => other,
        }
    }

    /// Record that `device` used `nonce`, idempotently: recording it again for
    /// the same device succeeds, so a retried join completes.
    ///
    /// # Errors
    ///
    /// `PermissionDenied` when another device spent the nonce; otherwise the
    /// error reading or writing the marker.
    pub fn record(&self, nonce: &[u8; 16], device: [u8; 32]) -> io::Result<()> {
        let device = hex::encode(device);
        match self.create(nonce, &device) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if fs::read_to_string(self.path(nonce))?.trim() == device {
                    Ok(())
                } else {
                    Err(refused(
                        "enrollment token has already been consumed by another device",
                    ))
                }
            }
            other => other,
        }
    }

    /// Carry a nonce spent in an older store into this one.
    pub(crate) fn import(&self, nonce: &[u8; 16]) -> io::Result<()> {
        match self.create(nonce, MIGRATED_MARKER) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            other => other,
        }
    }

    /// Create the marker for `nonce` holding `contents`, durably: the file
    /// and its directory entry are on disk before this returns.
    fn create(&self, nonce: &[u8; 16], contents: &str) -> io::Result<()> {
        fs::create_dir_all(&self.directory)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path(nonce))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        File::open(&self.directory)?.sync_all()
    }
}

/// One token presented for redemption.
#[derive(Clone, Copy, Debug)]
pub struct Redemption<'a> {
    pub token: &'a EnrollmentToken,
    /// What the approving person admits the device as.
    pub role: EnrollmentRole,
    /// The key of the device being admitted.
    pub device_public_key: [u8; 32],
    pub label: &'a str,
    pub now_ns: u64,
    /// When the new certificate starts to be valid: `now_ns`, or a little
    /// earlier so a device whose clock runs behind accepts it.
    pub valid_from_ns: u64,
    /// The approving device's own certificate, used as the issuer when the
    /// fleet does not list it (a host that keeps no fleet).
    pub local_issuer: Option<&'a DeviceCert>,
}

/// Signs a certificate digest with the identity's root.
pub type RootSign<'a> = &'a dyn Fn(&[u8]) -> io::Result<[u8; 64]>;

/// Where a redeemed certificate's root signature comes from.
#[derive(Clone, Copy)]
pub enum Certifier<'a> {
    /// The root this device holds signs it, so its issuer must be allowed to
    /// admit devices.
    Root(RootSign<'a>),
    /// The person's devices that hold the identity's key signed exactly this
    /// certificate, each after asking the person
    /// ([`crate::certification_ceremony`]); the issuer only introduced the
    /// device.
    Approved(&'a DeviceCert),
}

impl Certifier<'_> {
    /// Whether the issuer admits the device on its own authority.
    #[must_use]
    pub const fn issuer_admits(&self) -> bool {
        matches!(self, Self::Root(_))
    }
}

/// Redeem `request` for a root-signed certificate.
///
/// With a `fleet`, the issuer comes from it (or `local_issuer`), revocation
/// is checked against it, and the new certificate is recorded in it under
/// the fleet lock. Without one — a host approving with a root seed and no
/// person directory — the issuer is `local_issuer`. Either way the nonce is
/// spent in `nonces` before the certificate is returned.
///
/// # Errors
///
/// `PermissionDenied` when the token is not genuine, unexpired and of
/// `role`'s shape, when the issuer may not admit the device, or when the
/// token was already redeemed; otherwise the error loading, signing or
/// persisting.
pub fn redeem(
    fleet: Option<&DeviceFleetStore>,
    nonces: &NonceStore,
    request: &Redemption<'_>,
    certifier: Certifier<'_>,
) -> io::Result<DeviceCert> {
    let token = request.token;
    verify_enrollment_token(token, request.now_ns).map_err(|error| refused(error.to_string()))?;
    if !request.role.admits(&token.capabilities) {
        return Err(refused(format!(
            "enrollment token does not grant the {} role",
            request.role.as_str()
        )));
    }
    if request.valid_from_ns > request.now_ns {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a certificate cannot start to be valid in the future",
        ));
    }
    if let Some(fleet) = fleet {
        return fleet.redeem_locked(nonces, request, certifier);
    }
    let issuer = request
        .local_issuer
        .ok_or_else(|| refused("issuer not enrolled"))?;
    check_issuer(
        token,
        issuer,
        false,
        certifier.issuer_admits(),
        request.now_ns,
    )?;
    if nonces.is_consumed(&token.nonce)? {
        return Err(refused("enrollment token has already been consumed"));
    }
    let cert = certify(request, certifier)?;
    nonces.claim(&token.nonce, request.device_public_key)?;
    Ok(cert)
}

/// The root-signed certificate `request` asks for, verified.
pub(crate) fn certify(
    request: &Redemption<'_>,
    certifier: Certifier<'_>,
) -> io::Result<DeviceCert> {
    let mut cert = DeviceCert {
        user_public_key: request.token.user_public_key,
        device_public_key: request.device_public_key,
        device_label: request.label.to_owned(),
        capabilities: request.token.capabilities.clone(),
        issued_at_ns: request.valid_from_ns,
        expires_at_ns: None,
        signature: [0; 64],
    };
    match certifier {
        Certifier::Root(sign) => {
            let digest = tilo_user_identity::device_cert_signing_digest(&cert)
                .map_err(|error| io::Error::other(error.to_string()))?;
            cert.signature = sign(&digest)?;
        }
        Certifier::Approved(approved) => {
            if approved.user_public_key != cert.user_public_key
                || approved.device_public_key != cert.device_public_key
                || approved.device_label != cert.device_label
                || approved.capabilities != cert.capabilities
                || approved.expires_at_ns.is_some()
                || approved.issued_at_ns > request.now_ns
            {
                return Err(refused(
                    "the certificate the person's devices approved is for another device",
                ));
            }
            cert = approved.clone();
        }
    }
    // A root other than the token's identity cannot produce a signature that
    // verifies under the token's user key.
    verify_device_cert(&cert).map_err(|error| refused(error.to_string()))?;
    Ok(cert)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_fleet::DeviceFleetStore;
    use tilo_user_identity::IdentityPaths;

    struct Fixture {
        _root: tempfile::TempDir,
        store: DeviceFleetStore,
        identity_id: String,
        issued_at_ns: u64,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().expect("temp root");
        let identity_dir = root.path().join("config/identity");
        let paths = IdentityPaths::under(root.path().join("system"));
        crate::provisioning::ensure_collaboration_identity(
            &identity_dir,
            &paths,
            crate::provisioning::IdentityStorage::Files,
        )
        .expect("provision");
        let identity_id = crate::registry::IdentityRegistryStore::at(&identity_dir)
            .load()
            .expect("registry")
            .default_identity
            .expect("default identity");
        let store = DeviceFleetStore::at(&identity_dir, paths);
        let issued_at_ns = store.load(&identity_id).expect("fleet").certificates[0].issued_at_ns;
        Fixture {
            _root: root,
            store,
            identity_id,
            issued_at_ns,
        }
    }

    impl Fixture {
        /// A personal-device code minted by this device, as text and decoded.
        fn mint(&self) -> (String, EnrollmentToken) {
            let encoded = self
                .store
                .mint_enrollment(
                    &self.identity_id,
                    DeviceCapabilities::member(),
                    self.issued_at_ns,
                    self.issued_at_ns.saturating_add(5_000),
                )
                .expect("token");
            let token = tilo_user_identity::decode_enrollment_token(&encoded).expect("decode");
            (encoded, token)
        }
    }

    fn device(seed: u8) -> [u8; 32] {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes()
    }

    #[test]
    fn roles_have_one_preset_and_refuse_the_other_shape() {
        for role in [EnrollmentRole::PersonalDevice, EnrollmentRole::HomePeer] {
            assert!(role.admits(&role.preset()));
            assert_eq!(EnrollmentRole::parse(role.as_str()), Some(role));
        }
        assert!(!EnrollmentRole::HomePeer.admits(&DeviceCapabilities::member()));
        assert!(!EnrollmentRole::PersonalDevice.admits(&DeviceCapabilities::host()));
        assert_eq!(EnrollmentRole::parse("administrator"), None);
    }

    /// One nonce store: a token the nearby-pairing broker redeemed through
    /// [`redeem`] is spent for the fleet's own entry point too, and the
    /// reverse.
    #[test]
    fn token_redeems_once_across_both_entry_points() {
        let fixture = fixture();
        let store = &fixture.store;
        let now = fixture.issued_at_ns;
        let (root, _) = store.identity(&fixture.identity_id).expect("root");
        let sign = |digest: &[u8]| {
            root.sign(digest)
                .map_err(|e| io::Error::other(e.to_string()))
        };

        let (encoded, token) = fixture.mint();
        let cert = redeem(
            Some(store),
            &store.nonces(),
            &Redemption {
                token: &token,
                role: EnrollmentRole::PersonalDevice,
                device_public_key: device(41),
                label: "Laptop",
                now_ns: now,
                valid_from_ns: now,
                local_issuer: None,
            },
            Certifier::Root(&sign),
        )
        .expect("broker redemption");
        assert_eq!(cert.capabilities, DeviceCapabilities::member());
        assert!(store.nonces().is_consumed(&token.nonce).expect("marker"));
        let replay = store.complete_enrollment(
            &fixture.identity_id,
            &encoded,
            EnrollmentRole::PersonalDevice,
            device(42),
            "Replay",
            now + 1,
        );
        let error = replay.expect_err("the fleet must see the broker's redemption");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            error.to_string().contains("already been consumed"),
            "{error}"
        );

        let (encoded, token) = fixture.mint();
        store
            .complete_enrollment(
                &fixture.identity_id,
                &encoded,
                EnrollmentRole::PersonalDevice,
                device(43),
                "Phone",
                now,
            )
            .expect("fleet redemption");
        let replay = redeem(
            None,
            &store.nonces(),
            &Redemption {
                token: &token,
                role: EnrollmentRole::PersonalDevice,
                device_public_key: device(44),
                label: "Replay",
                now_ns: now + 1,
                valid_from_ns: now + 1,
                local_issuer: store
                    .load(&fixture.identity_id)
                    .expect("fleet")
                    .certificates
                    .iter()
                    .find(|cert| cert.device_public_key == token.issuer_device_public_key),
            },
            Certifier::Root(&sign),
        );
        assert_eq!(
            replay
                .expect_err("the broker must see the fleet's redemption")
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            store
                .load(&fixture.identity_id)
                .expect("fleet")
                .certificates
                .len(),
            3,
            "each token admitted exactly one device"
        );
    }

    /// The issuer must have been certified when it minted the token, not just
    /// now, and must still be valid now.
    #[test]
    fn issuer_checked_at_issue_and_now() {
        let fixture = fixture();
        let store = &fixture.store;
        let now = fixture.issued_at_ns;
        let (_, token) = fixture.mint();
        let (root, _) = store.identity(&fixture.identity_id).expect("root");
        let sign = |digest: &[u8]| {
            root.sign(digest)
                .map_err(|e| io::Error::other(e.to_string()))
        };
        let issuer = store
            .load(&fixture.identity_id)
            .expect("fleet")
            .certificates
            .into_iter()
            .find(|cert| cert.device_public_key == token.issuer_device_public_key)
            .expect("issuer");
        let resigned = |mut cert: DeviceCert| {
            let digest = tilo_user_identity::device_cert_signing_digest(&cert).expect("digest");
            cert.signature = sign(&digest).expect("sign");
            cert
        };

        // Certified only after the token claims to have been minted.
        let not_yet_valid = resigned(DeviceCert {
            issued_at_ns: now + 10,
            ..issuer.clone()
        });
        let error = check_issuer(&token, &not_yet_valid, false, true, now + 20)
            .expect_err("an issuer not yet certified at the token's issue time");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("issue time"), "{error}");
        let error = redeem(
            None,
            &store.nonces(),
            &Redemption {
                token: &token,
                role: EnrollmentRole::PersonalDevice,
                device_public_key: device(51),
                label: "Phone",
                now_ns: now + 20,
                valid_from_ns: now + 20,
                local_issuer: Some(&not_yet_valid),
            },
            Certifier::Root(&sign),
        )
        .expect_err("the broker's entry point applies the same rule");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

        let expired = resigned(DeviceCert {
            expires_at_ns: Some(now + 2),
            ..issuer.clone()
        });
        assert!(check_issuer(&token, &expired, false, true, now + 1).is_ok());
        assert_eq!(
            check_issuer(&token, &expired, false, true, now + 3)
                .expect_err("an issuer expired at redemption")
                .kind(),
            io::ErrorKind::PermissionDenied
        );

        // Revoked, or never allowed to admit.
        assert!(check_issuer(&token, &issuer, true, true, now).is_err());
        let mut member = issuer;
        member.capabilities = DeviceCapabilities::member();
        assert!(check_issuer(&token, &resigned(member), false, true, now).is_err());
        assert!(
            !store.nonces().is_consumed(&token.nonce).expect("marker"),
            "a refused redemption spends nothing"
        );
    }

    /// A device that may not admit introduces a new device whose
    /// certificate the person's key holders approved: the approval admits
    /// it, for exactly that device, and never a revoked one.
    #[test]
    fn an_approved_certificate_admits_through_a_device_that_cannot_admit() {
        let fixture = fixture();
        let store = &fixture.store;
        let now = fixture.issued_at_ns;
        let (_, token) = fixture.mint();
        let (root, _) = store.identity(&fixture.identity_id).expect("root");
        let signed = |mut cert: DeviceCert| {
            let digest = tilo_user_identity::device_cert_signing_digest(&cert).expect("digest");
            cert.signature = root.sign(&digest).expect("sign");
            cert
        };
        let mut member = store
            .load(&fixture.identity_id)
            .expect("fleet")
            .certificates
            .into_iter()
            .find(|cert| cert.device_public_key == token.issuer_device_public_key)
            .expect("issuer");
        member.capabilities = DeviceCapabilities::member();
        let member = signed(member);
        let approved = signed(DeviceCert {
            user_public_key: token.user_public_key,
            device_public_key: device(61),
            device_label: "Tablet".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now,
            expires_at_ns: None,
            signature: [0; 64],
        });
        let redemption = |device_public_key, label| Redemption {
            token: &token,
            role: EnrollmentRole::PersonalDevice,
            device_public_key,
            label,
            now_ns: now,
            valid_from_ns: now,
            local_issuer: Some(&member),
        };
        let refuse = |error: io::Error| assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        refuse(
            redeem(
                None,
                &store.nonces(),
                &redemption(device(61), "Tablet"),
                Certifier::Root(&|_| Ok([0; 64])),
            )
            .expect_err("a member may not admit with a root of its own"),
        );
        refuse(
            redeem(
                None,
                &store.nonces(),
                &redemption(device(62), "Tablet"),
                Certifier::Approved(&approved),
            )
            .expect_err("an approval names its device"),
        );
        refuse(
            redeem(
                None,
                &store.nonces(),
                &redemption(device(61), "Laptop"),
                Certifier::Approved(&approved),
            )
            .expect_err("an approval names its label"),
        );
        assert!(!store.nonces().is_consumed(&token.nonce).expect("marker"));
        let cert = redeem(
            None,
            &store.nonces(),
            &redemption(device(61), "Tablet"),
            Certifier::Approved(&approved),
        )
        .expect("the approved certificate admits");
        assert_eq!(cert, approved);
        assert!(store.nonces().is_consumed(&token.nonce).expect("marker"));
    }

    #[test]
    fn a_revoked_device_is_not_admitted_again() {
        let fixture = fixture();
        let store = &fixture.store;
        let now = fixture.issued_at_ns;
        let (encoded, _) = fixture.mint();
        store
            .complete_enrollment(
                &fixture.identity_id,
                &encoded,
                EnrollmentRole::PersonalDevice,
                device(71),
                "Lost phone",
                now,
            )
            .expect("enrolled");
        store
            .revoke(
                &fixture.identity_id,
                device(71),
                tilo_user_identity::RevocationReason::Lost,
                now + 1,
            )
            .expect("revoked");
        let (encoded, _) = fixture.mint();
        let error = store
            .complete_enrollment(
                &fixture.identity_id,
                &encoded,
                EnrollmentRole::PersonalDevice,
                device(71),
                "Found phone",
                now + 2,
            )
            .expect_err("revocation wins over a later enrollment");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_token_of_the_other_role_is_refused_before_anything_is_spent() {
        let fixture = fixture();
        let store = &fixture.store;
        let now = fixture.issued_at_ns;
        let encoded = store
            .mint_enrollment(
                &fixture.identity_id,
                DeviceCapabilities::host(),
                now,
                now + 5_000,
            )
            .expect("token");
        let error = store
            .complete_enrollment(
                &fixture.identity_id,
                &encoded,
                EnrollmentRole::PersonalDevice,
                device(61),
                "Server",
                now,
            )
            .expect_err("a home-peer token cannot admit a personal device");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let token = tilo_user_identity::decode_enrollment_token(&encoded).expect("decode");
        assert!(!store.nonces().is_consumed(&token.nonce).expect("marker"));
        store
            .complete_enrollment(
                &fixture.identity_id,
                &encoded,
                EnrollmentRole::HomePeer,
                device(61),
                "Server",
                now,
            )
            .expect("the same token admits its own role");
    }

    #[test]
    fn nonce_markers_spend_once_and_record_idempotently() {
        let directory = tempfile::tempdir().expect("temp");
        let nonces = NonceStore::under(directory.path());
        nonces.record(&[4; 16], [7; 32]).expect("first record");
        nonces
            .record(&[4; 16], [7; 32])
            .expect("the same device again");
        assert!(nonces.record(&[4; 16], [8; 32]).is_err());
        assert!(nonces.claim(&[4; 16], [7; 32]).is_err());
        nonces.claim(&[5; 16], [7; 32]).expect("a fresh nonce");
        nonces.import(&[6; 16]).expect("migrated");
        assert!(nonces.is_consumed(&[6; 16]).expect("marker"));
        assert!(nonces.record(&[6; 16], [7; 32]).is_err());
    }
}
