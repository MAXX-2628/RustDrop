//! TLS 1.3 with mutual certificate pinning: a peer is accepted only if its
//! certificate fingerprint is in the local trust store. There is no CA; the
//! handshake signature is still fully verified by rustls.

use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{Arc, RwLock},
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
        devices.insert(fingerprint, TrustedDevice { name: name.into() });
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
}

impl PinnedVerifier {
    fn check(&self, cert: &CertificateDer<'_>) -> Result<(), Error> {
        match self.trust.get(&fingerprint(cert)) {
            Some(_) => Ok(()),
            None => Err(Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            )),
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

fn verifier(trust: Arc<TrustStore>) -> (Arc<CryptoProvider>, Arc<PinnedVerifier>) {
    let provider = Arc::new(ring::default_provider());
    let verifier = Arc::new(PinnedVerifier {
        trust,
        provider: provider.clone(),
    });
    (provider, verifier)
}

pub fn server_config(id: &Identity, trust: Arc<TrustStore>) -> Result<Arc<ServerConfig>> {
    let (provider, verifier) = verifier(trust);
    let cfg = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![id.cert.clone()], id.key.clone_key())?;
    Ok(Arc::new(cfg))
}

pub fn client_config(id: &Identity, trust: Arc<TrustStore>) -> Result<Arc<ClientConfig>> {
    let (provider, verifier) = verifier(trust);
    let cfg = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![id.cert.clone()], id.key.clone_key())?;
    Ok(Arc::new(cfg))
}
