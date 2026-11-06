//! Persistent device identity: UUID, display name, platform and a self-signed TLS certificate.

use std::{fs, path::Path};

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{protocol::DeviceInfo, security, PROTOCOL_VERSION};

#[derive(Serialize, Deserialize)]
struct Stored {
    id: Uuid,
    name: String,
}

pub struct Identity {
    pub id: Uuid,
    pub name: String,
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
}

impl Identity {
    /// Loads the identity from `dir`, generating and persisting it on first run.
    /// `name` is used only on first run; otherwise the computer name is used.
    pub fn load_or_create(dir: &Path, name: Option<&str>) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let (meta, cert, key) = (
            dir.join("identity.json"),
            dir.join("cert.der"),
            dir.join("key.der"),
        );

        if !meta.exists() {
            let ck = rcgen::generate_simple_self_signed(vec!["rustdrop".into()])?;
            // shortcut: key is a plain file in the user's app-data dir; move to DPAPI / Android Keystore before release (SECURITY.md §4).
            fs::write(&key, ck.key_pair.serialize_der())?;
            fs::write(&cert, ck.cert.der())?;
            let stored = Stored {
                id: Uuid::new_v4(),
                name: name.map_or_else(default_name, Into::into),
            };
            fs::write(&meta, serde_json::to_vec_pretty(&stored)?)?;
        }

        let stored: Stored = serde_json::from_slice(&fs::read(&meta)?)?;
        Ok(Self {
            id: stored.id,
            name: stored.name,
            cert: CertificateDer::from(fs::read(&cert)?),
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(fs::read(&key)?)),
        })
    }

    pub fn fingerprint(&self) -> String {
        security::fingerprint(&self.cert)
    }

    pub fn info(&self) -> DeviceInfo {
        DeviceInfo {
            device_id: self.id,
            name: self.name.clone(),
            platform: std::env::consts::OS.into(),
            version: PROTOCOL_VERSION,
        }
    }
}

fn default_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "RustDrop device".into())
}
