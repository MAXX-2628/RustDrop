use std::{
    collections::HashSet, io::Write, ops::ControlFlow, path::PathBuf, sync::Arc, time::Duration,
};

use anyhow::{bail, Result};
use rustdrop_core::{
    config::{data_dir, downloads_dir},
    discovery,
    identity::Identity,
    node,
    security::TrustStore,
    server,
    transfer::{self, Event, Inbox, Outcome},
    PROTOCOL_VERSION, VERSION,
};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage:
  rustdrop id                          show this device and its fingerprint
  rustdrop trust <fingerprint> <name>  trust a device (compare fingerprints on both screens first)
  rustdrop run                         advertise, receive files, and ping discovered devices
  rustdrop send <device> <path>...     send files/folders to a device (name or id prefix)

RUSTDROP_HOME overrides the data directory, RUSTDROP_DOWNLOADS the receive folder.";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let home = data_dir();
    let identity = Arc::new(Identity::load_or_create(&home, None)?);
    let trust = Arc::new(TrustStore::load(home.join("trusted.json"))?);
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["id"] => {
            println!("RustDrop core v{VERSION} (protocol {PROTOCOL_VERSION})");
            println!("name:        {}", identity.name);
            println!("id:          {}", identity.id);
            println!("fingerprint: {}", identity.fingerprint());
            println!("data:        {}", home.display());
            println!("downloads:   {}", downloads_dir().display());
        }
        ["trust", fingerprint, name] => {
            trust.add(fingerprint, name)?;
            println!("trusted {name}");
        }
        ["run"] => run(identity, trust).await?,
        ["send", device, ref paths @ ..] if !paths.is_empty() => {
            send(identity, trust, device, paths).await?
        }
        _ => bail!("{USAGE}"),
    }
    Ok(())
}

async fn run(identity: Arc<Identity>, trust: Arc<TrustStore>) -> Result<()> {
    // shortcut: IPv4 only; add an IPv6 listener when testing on IPv6-only networks.
    // Same port as the apps, so paired devices that saved this address can reach it again.
    let listener = match tokio::net::TcpListener::bind(("0.0.0.0", node::DEFAULT_PORT)).await {
        Ok(l) => l,
        Err(_) => tokio::net::TcpListener::bind("0.0.0.0:0").await?,
    };
    let port = listener.local_addr()?.port();
    identity
        .listen_port
        .store(port, std::sync::atomic::Ordering::Relaxed); // announced in HELLO
    let (tx, events) = mpsc::channel(16);
    let inbox = Arc::new(Inbox::new(downloads_dir(), tx));
    tokio::spawn(server::listen(
        listener,
        identity.clone(),
        trust.clone(),
        inbox,
        Default::default(), // the CLI never opens a QR pairing window
    ));
    tokio::spawn(show_events(events));

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
            return ControlFlow::Continue(());
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
        ControlFlow::Continue(())
    })
    .await
}

/// Console stand-in for the platform UI: asks ACCEPT/DECLINE and prints progress.
async fn show_events(mut events: mpsc::Receiver<Event>) {
    while let Some(event) = events.recv().await {
        match event {
            Event::Incoming(req) => {
                println!(
                    "\n{} (trusted) wants to send {} file(s), {} bytes:",
                    req.trusted_name,
                    req.files.len(),
                    req.total()
                );
                for f in req.files.iter().take(10) {
                    println!("  {} ({} bytes)", f.path, f.size);
                }
                print!("Accept? [y/N] ");
                let _ = std::io::stdout().flush();
                let yes = tokio::task::spawn_blocking(|| {
                    let mut line = String::new();
                    let _ = std::io::stdin().read_line(&mut line);
                    line.trim().eq_ignore_ascii_case("y")
                })
                .await
                .unwrap_or(false);
                let _ = req.decision.send(yes);
            }
            Event::Progress { done, total, .. } => {
                print!("\rreceiving {}%", done * 100 / total.max(1));
                let _ = std::io::stdout().flush();
            }
            Event::Completed { saved, .. } => {
                println!("\nreceived and verified {} file(s):", saved.len());
                for path in saved.iter().take(10) {
                    println!("  {}", path.display());
                }
            }
            Event::Failed { reason, .. } => println!("\ntransfer failed: {reason}"),
            Event::Paired { name } => println!("\npaired with {name}"),
            Event::Cancelled { .. } => println!("\ntransfer cancelled"),
            Event::Clipboard { .. } => {} // the CLI does not sync the clipboard
        }
    }
}

async fn send(
    identity: Arc<Identity>,
    trust: Arc<TrustStore>,
    device: &str,
    paths: &[&str],
) -> Result<()> {
    let daemon = mdns_sd::ServiceDaemon::new()?;
    let mut found = None;
    let search = discovery::browse(&daemon, identity.id, |peer| {
        let wanted = peer.info.name.eq_ignore_ascii_case(device)
            || peer.info.device_id.to_string().starts_with(device);
        match peer.addrs.iter().find(|a| a.is_ipv4()) {
            Some(addr) if wanted => {
                found = Some((*addr, peer.info.name.clone()));
                ControlFlow::Break(())
            }
            _ => ControlFlow::Continue(()),
        }
    });
    let _ = tokio::time::timeout(Duration::from_secs(10), search).await;
    let Some((addr, name)) = found else {
        bail!("device {device:?} not found on the network");
    };

    println!("sending to {name}, waiting for them to accept...");
    let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let (_keep, never_cancel) = tokio::sync::watch::channel(false);
    let outcome = transfer::send(
        addr,
        identity,
        trust,
        transfer::paths(&paths),
        never_cancel,
        |done, total| {
            print!("\rsending {}%", done * 100 / total.max(1));
            let _ = std::io::stdout().flush();
        },
    )
    .await?;
    match outcome {
        Outcome::Completed => println!("\ndone: {name} received and verified everything"),
        Outcome::Declined { reason: None } => println!("{name} declined"),
        Outcome::Declined {
            reason: Some(reason),
        } => println!("{name} declined: {reason}"),
        Outcome::Cancelled { .. } => println!("\n{name} cancelled the transfer"),
    }
    Ok(())
}
