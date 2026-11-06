use std::{collections::HashSet, path::PathBuf, sync::Arc};

use anyhow::{bail, Result};
use rustdrop_core::{
    discovery, identity::Identity, security::TrustStore, server, PROTOCOL_VERSION, VERSION,
};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage:
  rustdrop id                          show this device and its fingerprint
  rustdrop trust <fingerprint> <name>  trust a device (compare fingerprints on both screens first)
  rustdrop run                         advertise, listen, and ping discovered devices

RUSTDROP_HOME overrides the data directory.";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let home = data_dir();
    let identity = Arc::new(Identity::load_or_create(&home)?);
    let trust = Arc::new(TrustStore::load(home.join("trusted.json"))?);
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["id"] => {
            println!("RustDrop core v{VERSION} (protocol {PROTOCOL_VERSION})");
            println!("name:        {}", identity.name);
            println!("id:          {}", identity.id);
            println!("fingerprint: {}", identity.fingerprint());
            println!("data:        {}", home.display());
        }
        ["trust", fingerprint, name] => {
            trust.add(fingerprint, name)?;
            println!("trusted {name}");
        }
        ["run"] => run(identity, trust).await?,
        _ => bail!("{USAGE}"),
    }
    Ok(())
}

async fn run(identity: Arc<Identity>, trust: Arc<TrustStore>) -> Result<()> {
    // shortcut: IPv4 only; add an IPv6 listener when testing on IPv6-only networks.
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(server::listen(listener, identity.clone(), trust.clone()));

    let daemon = mdns_sd::ServiceDaemon::new()?;
    discovery::advertise(&daemon, &identity.info(), port)?;
    println!(
        "{} listening on port {port}, fingerprint {}",
        identity.name,
        identity.fingerprint()
    );

    let mut seen = HashSet::new();
    discovery::browse(&daemon, identity.id, |peer| {
        let addrs: Vec<_> = peer.addrs.into_iter().filter(|a| a.is_ipv4()).collect();
        // Early resolutions may carry only IPv6; wait for one with IPv4 before marking seen.
        if addrs.is_empty() || !seen.insert(peer.info.device_id) {
            return;
        }
        println!("found {} ({})", peer.info.name, peer.info.platform);
        let (identity, trust) = (identity.clone(), trust.clone());
        tokio::spawn(async move {
            for addr in &addrs {
                match server::ping(*addr, &identity, trust.clone()).await {
                    Ok((info, rtt)) => {
                        println!("PONG from {} in {rtt:?} (encrypted, trusted)", info.name);
                        return;
                    }
                    Err(e) => tracing::debug!(%addr, "ping failed: {e:#}"),
                }
            }
            println!(
                "{}: not trusted or unreachable - run `rustdrop trust` on both devices",
                peer.info.name
            );
        });
    })
    .await
}

fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RUSTDROP_HOME") {
        return dir.into();
    }
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("HOME"))
        .unwrap_or_else(|| ".".into());
    PathBuf::from(base).join("RustDrop")
}
