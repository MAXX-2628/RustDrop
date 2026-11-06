//! Platform-neutral app node: runs the listener, discovery, pairing checks and transfers,
//! and reports to the UI through a [`Listener`]. The Windows app uses it directly;
//! Android uses it through UniFFI (feature `uniffi`).

use std::{
    collections::HashMap,
    fmt,
    net::SocketAddr,
    ops::ControlFlow,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::Serialize;
use tokio::{
    runtime::{Handle, Runtime},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    discovery,
    identity::Identity,
    security::TrustStore,
    server,
    transfer::{self, Event, Inbox, Outcome, TransferState},
    VERSION,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PeerInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
    /// `None` while checking; `Some(true)` once an authenticated PING succeeded.
    pub trusted: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Me {
    pub name: String,
    pub id: String,
    pub fingerprint: String,
    pub downloads: String,
    pub version: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TrustedInfo {
    pub fingerprint: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FileItem {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct IncomingRequest {
    pub id: String,
    /// Label from the local trust store, the only name proven by the TLS handshake.
    pub sender: String,
    pub platform: String,
    /// At most the first 50 files.
    pub files: Vec<FileItem>,
    pub count: u64,
    pub total: u64,
}

/// Partial transfer update; UIs merge the non-null fields into the transfer with this `id`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TransferUpdate {
    pub id: String,
    pub outgoing: Option<bool>,
    pub peer: Option<String>,
    pub items: Option<u64>,
    pub total: Option<u64>,
    pub done: Option<u64>,
    pub state: Option<TransferState>,
    pub reason: Option<String>,
    pub saved: Option<Vec<String>>,
}

#[derive(Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error), uniffi(flat_error))]
pub enum NodeError {
    Failed { message: String },
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self::Failed { message } = self;
        f.write_str(message)
    }
}

impl std::error::Error for NodeError {}

fn fail(message: impl Into<String>) -> NodeError {
    NodeError::Failed {
        message: message.into(),
    }
}

/// Implemented by each platform UI. Called from background threads.
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait Listener: Send + Sync {
    fn peers_changed(&self, peers: Vec<PeerInfo>);
    fn incoming(&self, request: IncomingRequest);
    fn transfer_changed(&self, update: TransferUpdate);
    /// The node stopped working (for example, it could not open a network socket).
    fn failed(&self, message: String);
}

struct Peer {
    info: PeerInfo,
    addr: SocketAddr,
}

struct Inner {
    identity: Arc<Identity>,
    trust: Arc<TrustStore>,
    downloads: PathBuf,
    listener: Arc<dyn Listener>,
    rt: Handle,
    peers: Mutex<HashMap<Uuid, Peer>>,
    pending: Mutex<HashMap<Uuid, oneshot::Sender<bool>>>,
    sends: Mutex<HashMap<Uuid, JoinHandle<()>>>,
    error: Mutex<Option<String>>,
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct Node {
    inner: Arc<Inner>,
    _runtime: Runtime,
}

fn state(id: Uuid, state: TransferState, reason: Option<&str>) -> TransferUpdate {
    TransferUpdate {
        id: id.to_string(),
        state: Some(state),
        reason: reason.map(Into::into),
        ..Default::default()
    }
}

fn progress(id: Uuid, done: u64, total: u64) -> TransferUpdate {
    let s = if done == total {
        TransferState::Verifying
    } else {
        TransferState::Transferring
    };
    TransferUpdate {
        id: id.to_string(),
        done: Some(done),
        total: Some(total),
        state: Some(s),
        ..Default::default()
    }
}

/// Short, actionable failure text for UIs (DESIGN.md §14).
fn friendly(e: &anyhow::Error) -> String {
    let text = format!("{e:#}");
    if text.contains("certificate") || text.contains("CertificateError") {
        "This device isn't paired with you. Add each other's fingerprint on both devices.".into()
    } else if text.contains("refused") || text.contains("deadline") || text.contains("timed out") {
        "Couldn't reach the device. Check your network and try again.".into()
    } else {
        e.to_string()
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl Node {
    /// Loads (or creates) the identity in `data_dir` and starts receiving and discovery.
    /// `device_name` is only used the first time, when the identity is created.
    #[cfg_attr(feature = "uniffi", uniffi::constructor)]
    pub fn new(
        data_dir: String,
        downloads_dir: String,
        device_name: Option<String>,
        listener: Arc<dyn Listener>,
    ) -> Result<Arc<Self>, NodeError> {
        let data_dir = PathBuf::from(data_dir);
        let identity = Identity::load_or_create(&data_dir, device_name.as_deref())
            .map_err(|e| fail(format!("Couldn't load this device's identity: {e:#}")))?;
        let trust = TrustStore::load(data_dir.join("trusted.json"))
            .map_err(|e| fail(format!("Couldn't load trusted devices: {e:#}")))?;
        let runtime = Runtime::new().map_err(|e| fail(e.to_string()))?;
        let inner = Arc::new(Inner {
            identity: Arc::new(identity),
            trust: Arc::new(trust),
            downloads: downloads_dir.into(),
            listener,
            rt: runtime.handle().clone(),
            peers: Mutex::default(),
            pending: Mutex::default(),
            sends: Mutex::default(),
            error: Mutex::default(),
        });
        let node = inner.clone();
        runtime.spawn(async move {
            if let Err(e) = run(node.clone()).await {
                let message = format!("{e:#}");
                *node.error.lock().unwrap() = Some(message.clone());
                node.listener.failed(message);
            }
        });
        Ok(Arc::new(Self {
            inner,
            _runtime: runtime,
        }))
    }

    pub fn me(&self) -> Me {
        let i = &self.inner;
        Me {
            name: i.identity.name.clone(),
            id: i.identity.id.to_string(),
            fingerprint: i.identity.fingerprint(),
            downloads: i.downloads.display().to_string(),
            version: VERSION.into(),
            error: i.error.lock().unwrap().clone(),
        }
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        self.inner.peer_list()
    }

    pub fn trusted(&self) -> Vec<TrustedInfo> {
        let mut list: Vec<TrustedInfo> = self
            .inner
            .trust
            .list()
            .into_iter()
            .map(|(fingerprint, d)| TrustedInfo {
                fingerprint,
                name: d.name,
            })
            .collect();
        list.sort_by_key(|t| t.name.to_lowercase());
        list
    }

    pub fn pair(&self, fingerprint: String, name: String) -> Result<(), NodeError> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(fail("Enter a device name of up to 64 characters."));
        }
        let fingerprint: String = fingerprint.chars().filter(|c| !c.is_whitespace()).collect();
        self.inner
            .trust
            .add(&fingerprint, name)
            .map_err(|_| fail("That isn't a valid fingerprint. It has 64 letters and digits."))?;
        self.inner.recheck_all();
        Ok(())
    }

    pub fn unpair(&self, fingerprint: String) -> Result<(), NodeError> {
        self.inner
            .trust
            .remove(&fingerprint)
            .map_err(|e| fail(e.to_string()))?;
        self.inner.recheck_all();
        Ok(())
    }

    /// Re-checks whether each nearby device is paired.
    pub fn refresh(&self) {
        self.inner.recheck_all();
    }

    pub fn respond(&self, id: String, accept: bool) {
        let Ok(id) = Uuid::parse_str(&id) else { return };
        let delivered = self
            .inner
            .pending
            .lock()
            .unwrap()
            .remove(&id)
            .is_some_and(|tx| tx.send(accept).is_ok());
        let update = match (delivered, accept) {
            (false, _) => state(id, TransferState::Failed, Some("The request expired.")),
            (true, true) => state(id, TransferState::Transferring, None),
            (true, false) => state(id, TransferState::Cancelled, Some("Declined")),
        };
        self.inner.listener.transfer_changed(update);
    }

    /// Starts sending files/folders to a nearby paired device. Returns the transfer id.
    pub fn send(&self, device: String, paths: Vec<String>) -> Result<String, NodeError> {
        if paths.is_empty() {
            return Err(fail("Choose at least one file or folder."));
        }
        let device = Uuid::parse_str(&device).map_err(|_| fail("Unknown device."))?;
        let (name, addr) = {
            let peers = self.inner.peers.lock().unwrap();
            let peer = peers
                .get(&device)
                .ok_or_else(|| fail("That device is no longer nearby."))?;
            (peer.info.name.clone(), peer.addr)
        };

        let id = Uuid::new_v4();
        self.inner.listener.transfer_changed(TransferUpdate {
            id: id.to_string(),
            outgoing: Some(true),
            peer: Some(name),
            items: Some(paths.len() as u64),
            done: Some(0),
            state: Some(TransferState::WaitingForAccept),
            ..Default::default()
        });

        let node = self.inner.clone();
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        // Hold the lock across spawn so the task cannot remove its entry before it is inserted.
        let mut sends = self.inner.sends.lock().unwrap();
        let task = self.inner.rt.spawn(async move {
            let tick = node.listener.clone();
            let result = transfer::send(addr, &node.identity, node.trust.clone(), &paths, {
                move |done, total| tick.transfer_changed(progress(id, done, total))
            })
            .await;
            node.sends.lock().unwrap().remove(&id);
            let update = match result {
                Ok(Outcome::Completed) => state(id, TransferState::Completed, None),
                Ok(Outcome::Declined) => state(id, TransferState::Cancelled, Some("Declined")),
                Err(e) => state(id, TransferState::Failed, Some(&friendly(&e))),
            };
            node.listener.transfer_changed(update);
        });
        sends.insert(id, task);
        Ok(id.to_string())
    }

    /// Cancels an outgoing transfer; the receiver discards everything it staged.
    pub fn cancel(&self, id: String) {
        let Ok(id) = Uuid::parse_str(&id) else { return };
        if let Some(task) = self.inner.sends.lock().unwrap().remove(&id) {
            task.abort();
            let update = state(id, TransferState::Cancelled, Some("Cancelled"));
            self.inner.listener.transfer_changed(update);
        }
    }
}

impl Inner {
    fn peer_list(&self) -> Vec<PeerInfo> {
        let peers = self.peers.lock().unwrap();
        let mut list: Vec<PeerInfo> = peers.values().map(|p| p.info.clone()).collect();
        list.sort_by_key(|p| p.name.to_lowercase());
        list
    }

    fn emit_peers(&self) {
        self.listener.peers_changed(self.peer_list());
    }

    /// Marks the peer as checking, then tries an authenticated PING to learn whether we are paired.
    fn check(self: &Arc<Self>, id: Uuid) {
        let addr = self.peers.lock().unwrap().get_mut(&id).map(|p| {
            p.info.trusted = None;
            p.addr
        });
        let Some(addr) = addr else { return };
        self.emit_peers();
        let node = self.clone();
        self.rt.spawn(async move {
            let ok = server::ping(addr, &node.identity, node.trust.clone())
                .await
                .is_ok();
            if let Some(p) = node.peers.lock().unwrap().get_mut(&id) {
                p.info.trusted = Some(ok);
            }
            node.emit_peers();
        });
    }

    fn recheck_all(self: &Arc<Self>) {
        let ids: Vec<Uuid> = self.peers.lock().unwrap().keys().copied().collect();
        for id in ids {
            self.check(id);
        }
    }

    fn forward(&self, event: Event) {
        let update = match event {
            Event::Incoming(req) => {
                let id = req.transfer_id;
                let view = IncomingRequest {
                    id: id.to_string(),
                    sender: req.trusted_name.clone(),
                    platform: req.peer.platform.clone(),
                    count: req.files.len() as u64,
                    total: req.total(),
                    files: req
                        .files
                        .iter()
                        .take(50)
                        .map(|f| FileItem {
                            path: f.path.clone(),
                            size: f.size,
                        })
                        .collect(),
                };
                self.listener.transfer_changed(TransferUpdate {
                    id: view.id.clone(),
                    outgoing: Some(false),
                    peer: Some(view.sender.clone()),
                    items: Some(view.count),
                    total: Some(view.total),
                    done: Some(0),
                    state: Some(TransferState::WaitingForAccept),
                    ..Default::default()
                });
                self.pending.lock().unwrap().insert(id, req.decision);
                self.listener.incoming(view);
                return;
            }
            Event::Progress {
                transfer_id,
                done,
                total,
            } => progress(transfer_id, done, total),
            Event::Completed { transfer_id, saved } => TransferUpdate {
                saved: Some(saved.iter().map(|p| p.display().to_string()).collect()),
                ..state(transfer_id, TransferState::Completed, None)
            },
            Event::Failed {
                transfer_id,
                reason,
            } => state(transfer_id, TransferState::Failed, Some(&reason)),
        };
        self.listener.transfer_changed(update);
    }
}

async fn run(node: Arc<Inner>) -> anyhow::Result<()> {
    // shortcut: IPv4 only; add an IPv6 listener for IPv6-only networks.
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
    let port = listener.local_addr()?.port();
    let (tx, mut events) = mpsc::channel(32);
    let inbox = Arc::new(Inbox::new(node.downloads.clone(), tx));
    tokio::spawn(server::listen(
        listener,
        node.identity.clone(),
        node.trust.clone(),
        inbox,
    ));
    let forwarder = node.clone();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            forwarder.forward(event);
        }
    });

    let daemon = mdns_sd::ServiceDaemon::new()?;
    discovery::advertise(&daemon, &node.identity.info(), port)?;
    // shortcut: devices that leave the network stay listed until restart; handle ServiceRemoved for "Unavailable".
    discovery::browse(&daemon, node.identity.id, |peer| {
        let Some(addr) = peer.addrs.iter().copied().find(|a| a.is_ipv4()) else {
            return ControlFlow::Continue(());
        };
        let id = peer.info.device_id;
        let changed = {
            let mut peers = node.peers.lock().unwrap();
            match peers.get_mut(&id) {
                Some(p) if p.addr == addr => false,
                Some(p) => {
                    p.addr = addr;
                    true
                }
                None => {
                    let info = PeerInfo {
                        id: id.to_string(),
                        name: peer.info.name,
                        platform: peer.info.platform,
                        trusted: None,
                    };
                    peers.insert(id, Peer { info, addr });
                    true
                }
            }
        };
        if changed {
            node.check(id);
        }
        ControlFlow::Continue(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tests::temp_dir;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<TransferUpdate>>);

    impl Listener for Recorder {
        fn peers_changed(&self, _: Vec<PeerInfo>) {}
        fn incoming(&self, _: IncomingRequest) {}
        fn transfer_changed(&self, update: TransferUpdate) {
            self.0.lock().unwrap().push(update);
        }
        fn failed(&self, _: String) {}
    }

    #[test]
    fn rejects_bad_input_and_reports_expired_requests() {
        let dir = temp_dir("node");
        let recorder = Arc::new(Recorder::default());
        let node = Node::new(
            dir.join("data").display().to_string(),
            dir.join("dl").display().to_string(),
            Some("Pixel".into()),
            recorder.clone(),
        )
        .unwrap();

        assert_eq!(node.me().name, "Pixel");
        assert!(node.pair("not-hex".into(), "x".into()).is_err());
        assert!(node.pair("ab".repeat(32), " ".into()).is_err());
        node.pair("ab".repeat(32), "Laptop".into()).unwrap();
        assert_eq!(node.trusted()[0].name, "Laptop");
        assert!(node
            .send(Uuid::new_v4().to_string(), vec!["x".into()])
            .is_err());

        let id = Uuid::new_v4().to_string();
        node.respond(id.clone(), true);
        let updates = recorder.0.lock().unwrap();
        assert_eq!(updates[0].id, id);
        assert_eq!(updates[0].state, Some(TransferState::Failed));
    }
}
