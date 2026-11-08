//! TLS 1.3 with mutual certificate pinning: a peer is accepted only if its
//! certificate fingerprint is in the local trust store. There is no CA; the
//! handshake signature is still fully verified by rustls. QR pairing (`PairingWindow`) is the
//! only way an unknown device gets past the handshake, and only to present a one-time code.

use std::{
    collections::HashMap,
    fs,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use anyhow::Result;
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{ring, verify_tls13_signature, CryptoProvider},
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
    CertificateError, ClientConfig, DigitallySignedStruct, DistinguishedName, Error, ServerConfig,
    SignatureScheme,
};
use serde::{Deserialize, Serialize};

use crate::identity::Identity;

/// One DER element: (tag, contents, rest). None if malformed.
fn der(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let len = rest[..n].iter().fold(0usize, |l, b| (l << 8) | *b as usize);
        (len, &rest[n..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// UTCTime (YYMMDDHHMMSSZ) or GeneralizedTime (YYYYMMDDHHMMSSZ) as Unix seconds.
fn der_time(tag: u8, value: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(value).ok()?.strip_suffix('Z')?;
    let digits = |s: &str| s.parse::<u64>().ok();
    let (year, rest) = match (tag, text.len()) {
        (0x17, 12) => {
            let yy = digits(&text[..2])?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &text[2..])
        }
        (0x18, 14) => (digits(&text[..4])?, &text[4..]),
        _ => return None,
    };
    if !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let field = |i: usize| digits(&rest[i..i + 2]);
    let (month, day) = (field(0)?, field(2)?);
    let (h, m, s) = (field(4)?, field(6)?, field(8)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || h > 23 || m > 59 || s > 60 {
        return None;
    }
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = year as i64 - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + (h * 3600 + m * 60 + s) as i64).ok()
}

/// The certificate's notBefore and notAfter, as Unix seconds.
fn validity(cert: &[u8]) -> Option<(u64, u64)> {
    let (_, certificate, _) = der(cert)?;
    let (_, tbs, _) = der(certificate)?;
    let mut rest = tbs;
    let (tag, _, after) = der(rest)?;
    if tag == 0xa0 {
        rest = after; // explicit version
    }
    for _ in 0..3 {
        rest = der(rest)?.2; // serial number, signature algorithm, issuer
    }
    let (_, validity, _) = der(rest)?;
    let (t1, v1, rest) = der(validity)?;
    let (t2, v2, _) = der(rest)?;
    Some((der_time(t1, v1)?, der_time(t2, v2)?))
}

/// SHA-256 of the DER certificate, lowercase hex.
pub fn fingerprint(cert: &CertificateDer<'_>) -> String {
    hex(::ring::digest::digest(&::ring::digest::SHA256, cert).as_ref())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TrustedDevice {
    pub name: String,
    /// Last address ("ip:port") the device was reached at, so it can be found without mDNS
    /// (for example across Wi-Fi and wired subnets). Absent in files from older versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    /// The user chose to accept every transfer from this device without asking.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_accept: bool,
}

/// Fingerprint -> trusted device, persisted as `trusted.json`.
#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    devices: RwLock<HashMap<String, TrustedDevice>>,
}

impl TrustStore {
    pub fn load(path: PathBuf) -> Result<Self> {
        let devices = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path,
            devices: RwLock::new(devices),
        })
    }

    pub fn get(&self, fingerprint: &str) -> Option<TrustedDevice> {
        self.devices.read().unwrap().get(fingerprint).cloned()
    }

    pub fn add(&self, fingerprint: &str, name: &str) -> Result<()> {
        let fingerprint = fingerprint.trim().to_ascii_lowercase();
        anyhow::ensure!(
            fingerprint.len() == 64 && fingerprint.bytes().all(|b| b.is_ascii_hexdigit()),
            "fingerprint must be 64 hex characters"
        );
        let mut devices = self.devices.write().unwrap();
        // Re-pairing renames; the address and auto-accept choice are kept.
        let device = devices.entry(fingerprint).or_insert(TrustedDevice {
            name: String::new(),
            addr: None,
            auto_accept: false,
        });
        device.name = name.into();
        crate::disk::write_atomic(&self.path, &serde_json::to_vec_pretty(&*devices)?)?;
        Ok(())
    }

    /// Changes a known device and saves, only if `change` returns true. Unknown fingerprints are ignored.
    pub fn update(
        &self,
        fingerprint: &str,
        change: impl FnOnce(&mut TrustedDevice) -> bool,
    ) -> Result<()> {
        let mut devices = self.devices.write().unwrap();
        let Some(device) = devices.get_mut(fingerprint) else {
            return Ok(());
        };
        if change(device) {
            crate::disk::write_atomic(&self.path, &serde_json::to_vec_pretty(&*devices)?)?;
        }
        Ok(())
    }

    /// Remembers where a trusted device listens. Unknown fingerprints are ignored.
    pub fn set_addr(&self, fingerprint: &str, addr: SocketAddr) -> Result<()> {
        let addr = addr.to_string();
        self.update(fingerprint, |d| {
            let changed = d.addr.as_deref() != Some(addr.as_str());
            d.addr = Some(addr);
            changed
        })
    }

    /// Revokes trust; the device's next handshake is rejected.
    pub fn remove(&self, fingerprint: &str) -> Result<()> {
        let mut devices = self.devices.write().unwrap();
        devices.remove(fingerprint);
        crate::disk::write_atomic(&self.path, &serde_json::to_vec_pretty(&*devices)?)?;
        Ok(())
    }

    /// (fingerprint, device) pairs.
    pub fn list(&self) -> Vec<(String, TrustedDevice)> {
        let devices = self.devices.read().unwrap();
        devices
            .iter()
            .map(|(f, d)| (f.clone(), d.clone()))
            .collect()
    }
}

#[derive(Debug)]
struct PinnedVerifier {
    trust: Arc<TrustStore>,
    provider: Arc<CryptoProvider>,
    /// Client side of QR pairing: the server fingerprint read from the QR code.
    pin: Option<String>,
    /// Server side of QR pairing: unknown clients may connect while a window is open.
    pairing: Option<Arc<PairingWindow>>,
}

impl PinnedVerifier {
    fn check(&self, cert: &CertificateDer<'_>, now: UnixTime) -> Result<(), Error> {
        // SECURITY.md §3: never accept expired (or not yet valid) certificates, even pinned ones.
        match validity(cert) {
            Some((from, until)) if from <= now.as_secs() && now.as_secs() <= until => {}
            Some((_, until)) if now.as_secs() > until => {
                return Err(Error::InvalidCertificate(CertificateError::Expired))
            }
            Some(_) => return Err(Error::InvalidCertificate(CertificateError::NotValidYet)),
            None => return Err(Error::InvalidCertificate(CertificateError::BadEncoding)),
        }
        let fp = fingerprint(cert);
        let ok = self.trust.get(&fp).is_some()
            || self.pin.as_deref() == Some(fp.as_str())
            || self.pairing.as_ref().is_some_and(|w| w.is_open());
        if ok {
            Ok(())
        } else {
            Err(Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn tls13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn no_tls12() -> Result<HandshakeSignatureValid, Error> {
    Err(Error::General("TLS 1.2 is not supported".into()))
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        self.check(end_entity, now)
            .map(|_| ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        no_tls12()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.tls13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

impl ClientCertVerifier for PinnedVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        self.check(end_entity, now)
            .map(|_| ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        no_tls12()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.tls13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

fn verifier(
    trust: Arc<TrustStore>,
    pin: Option<String>,
    pairing: Option<Arc<PairingWindow>>,
) -> (Arc<CryptoProvider>, Arc<PinnedVerifier>) {
    let provider = Arc::new(ring::default_provider());
    let verifier = Arc::new(PinnedVerifier {
        trust,
        provider: provider.clone(),
        pin,
        pairing,
    });
    (provider, verifier)
}

pub fn server_config(
    id: &Identity,
    trust: Arc<TrustStore>,
    pairing: Arc<PairingWindow>,
) -> Result<Arc<ServerConfig>> {
    let (provider, verifier) = verifier(trust, None, Some(pairing));
    let cfg = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![id.cert.clone()], id.key.clone_key())?;
    Ok(Arc::new(cfg))
}

/// Client config; `pin` additionally accepts one not-yet-trusted server fingerprint (QR pairing).
pub fn client_config(
    id: &Identity,
    trust: Arc<TrustStore>,
    pin: Option<String>,
) -> Result<Arc<ClientConfig>> {
    let (provider, verifier) = verifier(trust, pin, None);
    let cfg = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![id.cert.clone()], id.key.clone_key())?;
    Ok(Arc::new(cfg))
}
/// One-time QR pairing offer. While open, the TLS server also lets *unknown* devices finish the
/// handshake, but such a session may only send PAIR_REQUEST with this code (see `server::serve`).
#[derive(Debug, Default)]
pub struct PairingWindow {
    offer: Mutex<Option<Offer>>,
}

#[derive(Debug)]
struct Offer {
    code: String,
    expires: Instant,
    attempts: u8,
}

const PAIRING_TTL: Duration = Duration::from_secs(120);
const PAIRING_ATTEMPTS: u8 = 3;

impl PairingWindow {
    /// Opens a 2-minute window and returns a fresh 128-bit code (hex) from the OS CSPRNG.
    pub fn open(&self) -> Result<String> {
        let mut bytes = [0u8; 16];
        ::ring::rand::SecureRandom::fill(&::ring::rand::SystemRandom::new(), &mut bytes)
            .map_err(|_| anyhow::anyhow!("no secure random source"))?;
        let code = hex(&bytes);
        *self.offer.lock().unwrap() = Some(Offer {
            code: code.clone(),
            expires: Instant::now() + PAIRING_TTL,
            attempts: 0,
        });
        Ok(code)
    }

    pub fn close(&self) {
        *self.offer.lock().unwrap() = None;
    }

    pub fn is_open(&self) -> bool {
        self.offer
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|o| Instant::now() < o.expires)
    }

    /// True once for the right code. The offer closes on success, on expiry, and after 3 wrong codes.
    pub fn redeem(&self, code: &str) -> bool {
        let mut offer = self.offer.lock().unwrap();
        let Some(o) = offer.as_mut() else {
            return false;
        };
        if Instant::now() >= o.expires {
            *offer = None;
            return false;
        }
        #[allow(deprecated)] // still the constant-time compare ring ships in 0.17
        let ok = ::ring::constant_time::verify_slices_are_equal(o.code.as_bytes(), code.as_bytes())
            .is_ok();
        o.attempts += 1;
        if ok || o.attempts >= PAIRING_ATTEMPTS {
            *offer = None;
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_certificate_validity() {
        assert_eq!(der_time(0x17, b"700101000000Z"), Some(0));
        assert_eq!(der_time(0x17, b"491231235959Z"), Some(2_524_607_999));
        assert_eq!(der_time(0x18, b"20240229120000Z"), Some(1_709_208_000));
        assert_eq!(der_time(0x17, b"701301000000Z"), None);
        assert_eq!(der_time(0x17, b"7001010000Z"), None);

        // rcgen's default validity (what every RustDrop identity uses) is 1975..4096.
        let ck = rcgen::generate_simple_self_signed(vec!["rustdrop".into()]).unwrap();
        let (from, until) = validity(ck.cert.der()).unwrap();
        assert!(from < 1_000_000_000 && until > 4_000_000_000);
    }

    #[test]
    fn expired_certificate_is_rejected_even_when_trusted() {
        let mut params = rcgen::CertificateParams::new(vec!["rustdrop".into()]).unwrap();
        params.not_before = rcgen::date_time_ymd(2000, 1, 1);
        params.not_after = rcgen::date_time_ymd(2001, 1, 1);
        let cert = params
            .self_signed(&rcgen::KeyPair::generate().unwrap())
            .unwrap();
        let der = cert.der();

        let dir = std::env::temp_dir().join(format!("rustdrop-sec-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let trust = Arc::new(TrustStore::load(dir.join("trusted.json")).unwrap());
        trust.add(&fingerprint(der), "old").unwrap();
        let verifier = PinnedVerifier {
            trust,
            provider: Arc::new(rustls::crypto::ring::default_provider()),
            pin: None,
            pairing: None,
        };
        let in_2000 = UnixTime::since_unix_epoch(std::time::Duration::from_secs(970_000_000));
        assert!(verifier.check(der, in_2000).is_ok());
        assert!(matches!(
            verifier.check(der, UnixTime::now()),
            Err(Error::InvalidCertificate(CertificateError::Expired))
        ));
    }
}
