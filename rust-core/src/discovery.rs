//! mDNS / DNS-SD discovery on `_rustdrop._tcp.local.`. Announcements are untrusted
//! input: they only tell us where to connect; identity is proven by TLS.

use std::net::SocketAddr;

use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use uuid::Uuid;

use crate::protocol::DeviceInfo;

pub const SERVICE: &str = "_rustdrop._tcp.local.";
const MAX_NAME: usize = 64;

#[derive(Debug, Clone)]
pub struct Peer {
    pub info: DeviceInfo,
    pub addrs: Vec<SocketAddr>,
}

pub fn advertise(daemon: &ServiceDaemon, me: &DeviceInfo, port: u16) -> Result<()> {
    let id = me.device_id.to_string();
    let version = me.version.to_string();
    let props = [
        ("id", id.as_str()),
        ("name", me.name.as_str()),
        ("platform", me.platform.as_str()),
        ("v", version.as_str()),
    ];
    let host = format!("{id}.local.");
    let info = ServiceInfo::new(SERVICE, &id, &host, "", port, &props[..])?.enable_addr_auto();
    daemon.register(info)?;
    Ok(())
}

/// Calls `on_peer` for every resolved, well-formed announcement other than our own.
pub async fn browse(daemon: &ServiceDaemon, me: Uuid, mut on_peer: impl FnMut(Peer)) -> Result<()> {
    let events = daemon.browse(SERVICE)?;
    while let Ok(event) = events.recv_async().await {
        if let ServiceEvent::ServiceResolved(info) = event {
            match parse(&info) {
                Some(peer) if peer.info.device_id != me => on_peer(peer),
                Some(_) => {}
                None => {
                    tracing::debug!(name = info.get_fullname(), "ignored malformed announcement")
                }
            }
        }
    }
    Ok(())
}

fn parse(info: &ServiceInfo) -> Option<Peer> {
    let name = info.get_property_val_str("name")?;
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return None;
    }
    let device = DeviceInfo {
        device_id: info.get_property_val_str("id")?.parse().ok()?,
        name: name.into(),
        platform: info
            .get_property_val_str("platform")?
            .chars()
            .take(16)
            .collect(),
        version: info.get_property_val_str("v")?.parse().ok()?,
    };
    let addrs = info
        .get_addresses()
        .iter()
        .map(|ip| SocketAddr::new(*ip, info.get_port()))
        .collect();
    Some(Peer {
        info: device,
        addrs,
    })
}
