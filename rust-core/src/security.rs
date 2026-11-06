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
        let addr = devices.get(&fingerprint).and_then(|d| d.addr.clone());
        devices.insert(
            fingerprint,
            TrustedDevice {
                name: name.into(),
                addr,
            },
        );
        fs::write(&self.path, serde_json::to_vec_pretty(&*devices)?)?;
        Ok(())
    }

    /// Remembers where a trusted device listens. Unknown fingerprints are ignored.
    pub fn set_addr(&self, fingerprint: &str, addr: SocketAddr) -> Result<()> {
        let mut devices = self.devices.write().unwrap();
        let Some(device) = devices.get_mut(fingerprint) else {
            return Ok(());
        };
        let addr = addr.to_string();
        if device.addr.as_deref() == Some(addr.as_str()) {
            return Ok(());
        }
        device.addr = Some(addr);
        fs::write(&self.path, serde_json::to_vec_pretty(&*devices)?)?;
        Ok(())
    }

    /// Revokes trust; the device's next handshake is rejected.
    pub fn remove(&self, fingerprint: &str) -> Result<()> {
        let mut devices = self.devices.write().unwrap();
        devices.remove(fingerprint);
        fs::write(&self.path, serde_json::to_vec_pretty(&*devices)?)?;
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
    fn check(&self, cert: &CertificateDer<'_>) -> Result<(), Error> {
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
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        self.check(end_entity)
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
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        self.check(end_entity)
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
