//! Platform-neutral app node: runs the listener, discovery, pairing checks and transfers,
//! and reports to the UI through a [`Listener`]. The Windows app uses it directly;
//! Android uses it through UniFFI (feature `uniffi`).

use std::{
    collections::HashMap,
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::ControlFlow,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tokio::{
    runtime::{Builder, Handle, Runtime},
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    discovery,
    identity::Identity,
    protocol::Message,
    security::{PairingWindow, TrustStore},
    server,
    transfer::{self, Event, Inbox, Outcome, SendOptions, Source, TransferState},
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
    /// Transfers from this device start without asking.
    pub auto_accept: bool,
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
    /// Name of the first file or folder, for lists and history.
    pub name: Option<String>,
    pub items: Option<u64>,
    pub total: Option<u64>,
    pub done: Option<u64>,
    pub state: Option<TransferState>,
    pub reason: Option<String>,
    pub saved: Option<Vec<String>>,
}

/// A finished (or paused) transfer, kept in `history.json` across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct HistoryEntry {
    pub id: String,
    pub outgoing: bool,
    pub peer: String,
    pub name: String,
    pub items: u64,
    pub total: u64,
    pub state: TransferState,
    pub reason: Option<String>,
    pub saved: Vec<String>,
    /// Unix seconds.
    pub finished: u64,
}

/// Transfers kept in history; older ones are dropped.
const HISTORY_MAX: usize = 200;
/// Largest clipboard image synced (PNG bytes).
pub const MAX_CLIPBOARD_IMAGE: u64 = 32 << 20;

/// A QR pairing code to show: `modules` holds `size` x `size` cells, row by row, true = dark.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PairingOffer {
    pub link: String,
    pub size: u32,
    pub modules: Vec<bool>,
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
    /// Clipboard text arrived from the paired device `from`; put it on the system clipboard.
    /// Only called while clipboard sync is on. Never log `text`.
    fn clipboard_received(&self, text: String, from: String);
    /// A PNG image copied on `from` arrived at `path`; put it on the clipboard, then delete the file.
    /// Only called while clipboard sync is on.
    fn clipboard_image_received(&self, path: String, from: String);
    /// The paired device `from` sent a link or text on purpose. Show it with Open (for http/https
    /// links) and Copy; never open it without the user asking.
    fn text_received(&self, text: String, from: String);
    /// A device finished QR pairing with this one (it scanned this device's code).
    fn paired(&self, name: String);
    /// The node stopped working (for example, it could not open a network socket).
    fn failed(&self, message: String);
}

struct Discovery {
    daemon: mdns_sd::ServiceDaemon,
    task: JoinHandle<()>,
    /// Drops devices that stopped answering (see `watch_peers`).
    watch: JoinHandle<()>,
}

/// How often listed devices are checked while discovery runs.
const PEER_CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(20);
/// Checks in a row a device may miss before it leaves the list.
const PEER_MISSES: u32 = 2;

struct Peer {
    info: PeerInfo,
    addr: SocketAddr,
}

/// A running send: raise the watch to stop it; set the flag first to pause instead.
type Running = (watch::Sender<bool>, Arc<AtomicBool>);

/// An outgoing transfer the user paused: what to send again, and to whom.
struct PausedSend {
    device: Uuid,
    sources: Vec<Source>,
}

struct Inner {
    identity: Arc<Identity>,
    trust: Arc<TrustStore>,
    downloads: Mutex<PathBuf>,
    listener: Arc<dyn Listener>,
    rt: Handle,
    peers: Mutex<HashMap<Uuid, Peer>>,
    pending: Mutex<HashMap<Uuid, oneshot::Sender<bool>>>,
    /// Running sends: raise the watch to stop; set the flag first to pause instead of cancel.
    sends: Mutex<HashMap<Uuid, Running>>,
    paused: Mutex<HashMap<Uuid, PausedSend>>,
    /// Incoming clipboard images in flight (never shown as transfers): id -> sender label.
    silent: Mutex<HashMap<Uuid, String>>,
    inbox: Mutex<Option<Arc<Inbox>>>,
    error: Mutex<Option<String>>,
    pairing: Arc<PairingWindow>,
    port: Mutex<Option<u16>>,
    /// Wanted state; the UI turns discovery off when nobody is looking (saves battery).
    discover: AtomicBool,
    discovery: Mutex<Option<Discovery>>,
    settings_path: PathBuf,
    settings: Mutex<Settings>,
    clipboard_sync: AtomicBool,
    /// SHA-256 of the last clipboard text sent or received: stops echoes between devices.
    last_clip: Mutex<Option<Vec<u8>>>,
    /// What is known so far about each unfinished transfer, merged from its updates.
    live: Mutex<HashMap<String, HistoryEntry>>,
    history_path: PathBuf,
    /// Newest first.
    history: Mutex<Vec<HistoryEntry>>,
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct Node {
    inner: Arc<Inner>,
    _runtime: Runtime,
}

/// User settings kept in `settings.json` in the data folder.
#[derive(Serialize, Deserialize, Default)]
struct Settings {
    /// SECURITY.md §10: clipboard sync is opt-in.
    #[serde(default)]
    clipboard_sync: bool,
    /// Folder the user chose for received files (Windows); None = the platform default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    downloads: Option<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Preferred listening port (falls back to any free port).
pub const DEFAULT_PORT: u16 = 47821;

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

fn cancelled_update(id: Uuid, by_peer: bool) -> TransferUpdate {
    let reason = if by_peer {
        "Cancelled by the other device"
    } else {
        "You cancelled"
    };
    state(id, TransferState::Cancelled, Some(reason))
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
        let settings_path = data_dir.join("settings.json");
        // Missing or unreadable settings fall back to the defaults (everything opt-in is off).
        let settings: Settings = std::fs::read(&settings_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let trust = TrustStore::load(data_dir.join("trusted.json"))
            .map_err(|e| fail(format!("Couldn't load trusted devices: {e:#}")))?;
        let history_path = data_dir.join("history.json");
        let history: Vec<HistoryEntry> = std::fs::read(&history_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        // A chosen folder that is gone (unplugged drive) falls back to the default.
        let downloads = settings
            .downloads
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| downloads_dir.into());
        // Two workers are plenty for a LAN app; one per core would only add idle threads.
        let runtime = Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rustdrop")
            .enable_all()
            .build()
            .map_err(|e| fail(e.to_string()))?;
        let inner = Arc::new(Inner {
            identity: Arc::new(identity),
            trust: Arc::new(trust),
            downloads: Mutex::new(downloads),
            listener,
            rt: runtime.handle().clone(),
            peers: Mutex::default(),
            pending: Mutex::default(),
            sends: Mutex::default(),
            paused: Mutex::default(),
            silent: Mutex::default(),
            inbox: Mutex::default(),
            error: Mutex::default(),
            pairing: Arc::default(),
            port: Mutex::default(),
            discover: AtomicBool::new(true),
            discovery: Mutex::default(),
            clipboard_sync: AtomicBool::new(settings.clipboard_sync),
            settings: Mutex::new(settings),
            settings_path,
            last_clip: Mutex::default(),
            live: Mutex::default(),
            history_path,
            history: Mutex::new(history),
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
            downloads: i.downloads.lock().unwrap().display().to_string(),
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
                auto_accept: d.auto_accept,
            })
            .collect();
        list.sort_by_key(|t| t.name.to_lowercase());
        list
    }

    /// Changes the name this device shows for a paired device (lists, requests, history).
    pub fn rename_device(&self, fingerprint: String, name: String) -> Result<(), NodeError> {
        let name = name.trim().to_string();
        if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
            return Err(fail("Enter a device name of up to 64 characters."));
        }
        self.inner
            .trust
            .update(&fingerprint, |d| {
                let changed = d.name != name;
                d.name = name;
                changed
            })
            .map_err(|e| fail(format!("Couldn't save the name: {e}")))?;
        self.inner.recheck_all();
        Ok(())
    }

    /// Accept every transfer from this paired device without asking (off by default).
    pub fn set_auto_accept(&self, fingerprint: String, enabled: bool) -> Result<(), NodeError> {
        self.inner
            .trust
            .update(&fingerprint, |d| {
                let changed = d.auto_accept != enabled;
                d.auto_accept = enabled;
                changed
            })
            .map_err(|e| fail(format!("Couldn't save the setting: {e}")))
    }

    /// Finished and paused transfers, newest first (kept across restarts).
    pub fn history(&self) -> Vec<HistoryEntry> {
        self.inner.history.lock().unwrap().clone()
    }

    pub fn clear_history(&self) -> Result<(), NodeError> {
        let mut history = self.inner.history.lock().unwrap();
        history.clear();
        crate::disk::write_atomic(&self.inner.history_path, b"[]")
            .map_err(|e| fail(format!("Couldn't clear the history: {e}")))
    }

    /// Saves received files in `path` from now on (an existing folder). Transfers already
    /// running finish where they started.
    pub fn set_downloads(&self, path: String) -> Result<(), NodeError> {
        let dir = PathBuf::from(&path);
        if !dir.is_dir() {
            return Err(fail("Choose a folder that exists."));
        }
        self.inner
            .save_settings(|s| s.downloads = Some(path))
            .map_err(|e| fail(format!("Couldn't save the setting: {e}")))?;
        if let Some(inbox) = self.inner.inbox.lock().unwrap().as_ref() {
            inbox.set_dest(dir.clone());
        }
        *self.inner.downloads.lock().unwrap() = dir;
        Ok(())
    }

    /// Sends a link or text to a nearby paired device. Blocks for up to a few seconds;
    /// call it off the UI thread.
    pub fn send_text(&self, device: String, text: String) -> Result<(), NodeError> {
        let text = text.trim().to_string();
        if text.is_empty() || text.len() > server::MAX_CLIPBOARD {
            return Err(fail("Enter a link or text of up to 64 KB."));
        }
        let addr = self.inner.peer_addr(&device)?;
        let node = self.inner.clone();
        self.inner
            .rt
            .block_on(async move {
                let msg = Message::Text { text };
                server::send_one(addr, &node.identity, node.trust.clone(), &msg).await
            })
            .map_err(|e| fail(friendly(&e)))
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
        self.inner.probe_known();
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
        self.inner.report(update);
    }

    /// Starts sending files/folders to a nearby paired device. Returns the transfer id.
    pub fn send(&self, device: String, paths: Vec<String>) -> Result<String, NodeError> {
        let sources = paths.into_iter().map(|p| Source::Path(p.into())).collect();
        self.start_send(device, sources)
    }

    /// Sends a PNG image just copied on this device to every paired device listed, if
    /// clipboard sync is on. The file is opened at once, so the caller may delete it after.
    pub fn clipboard_image(&self, path: String) -> Result<(), NodeError> {
        let file = std::fs::File::open(&path).map_err(|e| fail(e.to_string()))?;
        self.inner.share_clipboard_image(file)
    }

    /// Like `clipboard_image`, for an open PNG file (Android). Takes ownership of `fd`.
    pub fn clipboard_image_fd(&self, fd: i32) -> Result<(), NodeError> {
        #[cfg(unix)]
        {
            use std::os::fd::FromRawFd;
            // SAFETY: the caller hands over an open descriptor it no longer uses (detachFd).
            let file = unsafe { std::fs::File::from_raw_fd(fd) };
            self.inner.share_clipboard_image(file)
        }
        #[cfg(not(unix))]
        {
            let _ = fd;
            Err(fail("Sending open files is only supported on Android."))
        }
    }

    /// Stops an outgoing transfer but lets the receiver keep what arrived; `resume` continues.
    pub fn pause(&self, id: String) {
        let Ok(id) = Uuid::parse_str(&id) else { return };
        if let Some((stop, pause)) = self.inner.sends.lock().unwrap().get(&id) {
            pause.store(true, Ordering::SeqCst);
            let _ = stop.send(true);
        }
    }

    /// Sends a paused transfer again; the receiver continues where it stopped, without asking.
    pub fn resume(&self, id: String) -> Result<(), NodeError> {
        let uid = Uuid::parse_str(&id).map_err(|_| fail("Unknown transfer."))?;
        let paused = self
            .inner
            .paused
            .lock()
            .unwrap()
            .remove(&uid)
            .ok_or_else(|| fail("This transfer can't be resumed. Send the files again."))?;
        let addr = match self.inner.peer_addr(&paused.device.to_string()) {
            Ok(addr) => addr,
            Err(e) => {
                self.inner.paused.lock().unwrap().insert(uid, paused);
                return Err(e);
            }
        };
        self.inner
            .report(state(uid, TransferState::WaitingForAccept, None));
        self.inner
            .spawn_send(uid, paused.device, addr, paused.sources, false);
        Ok(())
    }

    /// Like `send`, for files the app already opened (Android content URIs): no copy is made.
    /// Takes ownership of every `fd`, even on error, and closes them when the transfer ends.
    /// Each must be a regular, seekable file (several connections read it at once).
    pub fn send_fds(&self, device: String, files: Vec<FdFile>) -> Result<String, NodeError> {
        #[cfg(unix)]
        {
            use std::os::fd::FromRawFd;
            let sources = files
                .into_iter()
                .map(|f| Source::Open {
                    name: f.name,
                    // SAFETY: the caller hands over an open descriptor it no longer uses (detachFd).
                    file: unsafe { std::fs::File::from_raw_fd(f.fd) },
                })
                .collect();
            self.start_send(device, sources)
        }
        #[cfg(not(unix))]
        {
            let _ = (device, files);
            Err(fail("Sending open files is only supported on Android."))
        }
    }
}

/// An already-open file to send, named as the receiver should see it (`/` for folders).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FdFile {
    pub name: String,
    pub fd: i32,
}

impl Node {
    fn start_send(&self, device: String, sources: Vec<Source>) -> Result<String, NodeError> {
        if sources.is_empty() {
            return Err(fail("Choose at least one file or folder."));
        }
        let addr = self.inner.peer_addr(&device)?;
        let device = Uuid::parse_str(&device).map_err(|_| fail("Unknown device."))?;
        let peer = self
            .inner
            .peers
            .lock()
            .unwrap()
            .get(&device)
            .map(|p| p.info.name.clone());

        let id = Uuid::new_v4();
        self.inner.report(TransferUpdate {
            id: id.to_string(),
            outgoing: Some(true),
            peer,
            name: Some(sources[0].name()),
            items: Some(sources.len() as u64),
            done: Some(0),
            state: Some(TransferState::WaitingForAccept),
            ..Default::default()
        });
        self.inner.spawn_send(id, device, addr, sources, false);
        Ok(id.to_string())
    }
}

impl Inner {
    /// Address of a listed device, by id.
    fn peer_addr(&self, device: &str) -> Result<SocketAddr, NodeError> {
        let device = Uuid::parse_str(device).map_err(|_| fail("Unknown device."))?;
        self.peers
            .lock()
            .unwrap()
            .get(&device)
            .map(|p| p.addr)
            .ok_or_else(|| fail("That device is no longer nearby."))
    }

    /// Runs one send in the background and reports it as transfer `id`. A `clipboard` send
    /// (an image for clipboard sync) is never reported and can't be paused.
    fn spawn_send(
        self: &Arc<Self>,
        id: Uuid,
        device: Uuid,
        addr: SocketAddr,
        sources: Vec<Source>,
        clipboard: bool,
    ) {
        // Kept to send again if the user pauses; dropped (closing any duplicates) otherwise.
        let again: Option<Vec<Source>> = (!clipboard)
            .then(|| {
                sources
                    .iter()
                    .map(Source::try_clone)
                    .collect::<Result<_, _>>()
                    .ok()
            })
            .flatten();
        let (stop, cancel) = watch::channel(false);
        let options = SendOptions {
            clipboard,
            ..Default::default()
        };
        if !clipboard {
            let entry = (stop, options.pause.clone());
            self.sends.lock().unwrap().insert(id, entry);
        }
        let node = self.clone();
        self.rt.spawn(async move {
            let tick = node.clone();
            let (identity, trust) = (node.identity.clone(), node.trust.clone());
            let result = transfer::send_with(
                addr,
                identity,
                trust,
                sources,
                cancel,
                move |done, total| {
                    if !clipboard {
                        tick.report(progress(id, done, total))
                    }
                },
                &options,
            )
            .await;
            node.sends.lock().unwrap().remove(&id);
            if clipboard {
                if let Err(e) = result {
                    tracing::debug!(%addr, "clipboard image not delivered: {e:#}");
                }
                return;
            }
            let update = match result {
                Ok(Outcome::Completed) => state(id, TransferState::Completed, None),
                Ok(Outcome::Paused) => match again {
                    Some(sources) => {
                        let paused = PausedSend { device, sources };
                        node.paused.lock().unwrap().insert(id, paused);
                        state(id, TransferState::Paused, Some("Paused"))
                    }
                    None => state(id, TransferState::Cancelled, Some("You cancelled")),
                },
                Ok(Outcome::Declined { reason: None }) => {
                    state(id, TransferState::Cancelled, Some("Declined"))
                }
                Ok(Outcome::Declined {
                    reason: Some(reason),
                }) => state(id, TransferState::Failed, Some(&reason)),
                Ok(Outcome::Cancelled { by_peer }) => cancelled_update(id, by_peer),
                Err(e) => state(id, TransferState::Failed, Some(&friendly(&e))),
            };
            node.report(update);
        });
    }

    fn share_clipboard_image(self: &Arc<Self>, file: std::fs::File) -> Result<(), NodeError> {
        if !self.clipboard_sync.load(Ordering::SeqCst) {
            return Ok(());
        }
        let size = file.metadata().map_err(|e| fail(e.to_string()))?.len();
        if size == 0 || size > MAX_CLIPBOARD_IMAGE {
            return Ok(()); // too big to sync: skipped like long text
        }
        let targets: Vec<(Uuid, SocketAddr)> = {
            let peers = self.peers.lock().unwrap();
            peers
                .iter()
                .filter(|(_, p)| p.info.trusted == Some(true))
                .map(|(id, p)| (*id, p.addr))
                .collect()
        };
        for (device, addr) in targets {
            let file = file.try_clone().map_err(|e| fail(e.to_string()))?;
            let source = Source::Open {
                name: "Clipboard.png".into(),
                file,
            };
            self.spawn_send(Uuid::new_v4(), device, addr, vec![source], true);
        }
        Ok(())
    }

    fn save_settings(&self, change: impl FnOnce(&mut Settings)) -> std::io::Result<()> {
        let mut settings = self.settings.lock().unwrap();
        change(&mut settings);
        let bytes = serde_json::to_vec_pretty(&*settings)?;
        crate::disk::write_atomic(&self.settings_path, &bytes)
    }

    /// Tells the UI, and records the transfer in history once it ends (or pauses).
    fn report(&self, update: TransferUpdate) {
        self.record(&update);
        self.listener.transfer_changed(update);
    }

    fn record(&self, u: &TransferUpdate) {
        let mut live = self.live.lock().unwrap();
        let entry = live.entry(u.id.clone()).or_insert_with(|| {
            // A resumed transfer continues its paused history entry.
            let history = self.history.lock().unwrap();
            history
                .iter()
                .find(|h| h.id == u.id)
                .cloned()
                .unwrap_or(HistoryEntry {
                    id: u.id.clone(),
                    outgoing: false,
                    peer: String::new(),
                    name: String::new(),
                    items: 0,
                    total: 0,
                    state: TransferState::WaitingForAccept,
                    reason: None,
                    saved: Vec::new(),
                    finished: 0,
                })
        });
        if let Some(v) = u.outgoing {
            entry.outgoing = v;
        }
        if let Some(v) = &u.peer {
            entry.peer.clone_from(v);
        }
        if let Some(v) = &u.name {
            entry.name.clone_from(v);
        }
        if let Some(v) = u.items {
            entry.items = v;
        }
        if let Some(v) = u.total {
            entry.total = v;
        }
        if let Some(v) = &u.saved {
            entry.saved.clone_from(v);
        }
        let Some(state) = u.state else { return };
        entry.state = state;
        entry.reason.clone_from(&u.reason);
        let ended = !matches!(
            state,
            TransferState::WaitingForAccept
                | TransferState::Transferring
                | TransferState::Verifying
        );
        if !ended {
            return;
        }
        let mut entry = live.remove(&u.id).expect("inserted above");
        drop(live);
        if entry.peer.is_empty() {
            return; // an update for a transfer this node never saw start (an expired request)
        }
        entry.finished = now();
        let mut history = self.history.lock().unwrap();
        history.retain(|h| h.id != entry.id);
        history.insert(0, entry);
        history.truncate(HISTORY_MAX);
        let saved = serde_json::to_vec(&*history)
            .map_err(std::io::Error::from)
            .and_then(|b| crate::disk::write_atomic(&self.history_path, &b));
        if let Err(e) = saved {
            tracing::warn!("couldn't save transfer history: {e}");
        }
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl Node {
    /// Cancels a transfer in either direction (an unanswered incoming request is declined).
    /// The other device is told, shows it as cancelled, and discards anything partly received.
    pub fn cancel(&self, id: String) {
        let Ok(id) = Uuid::parse_str(&id) else { return };
        if let Some((tx, _)) = self.inner.sends.lock().unwrap().get(&id) {
            let _ = tx.send(true);
            return;
        }
        // A paused send: forget its files. The receiver drops what it kept after a week.
        if self.inner.paused.lock().unwrap().remove(&id).is_some() {
            self.inner.report(cancelled_update(id, false));
            return;
        }
        let incoming = self.inner.inbox.lock().unwrap().clone();
        if let Some(inbox) = incoming {
            inbox.cancel(id);
        }
    }

    /// Whether this device shares its clipboard with paired devices (off until the user turns it on).
    pub fn clipboard_sync(&self) -> bool {
        self.inner.clipboard_sync.load(Ordering::SeqCst)
    }

    pub fn set_clipboard_sync(&self, enabled: bool) -> Result<(), NodeError> {
        self.inner
            .save_settings(|s| s.clipboard_sync = enabled)
            .map_err(|e| fail(format!("Couldn't save the setting: {e}")))?;
        self.inner.clipboard_sync.store(enabled, Ordering::SeqCst);
        if !enabled {
            *self.inner.last_clip.lock().unwrap() = None;
        }
        Ok(())
    }

    /// Call when this device's clipboard text changed. If sync is on, sends it to every paired
    /// device currently listed. Text that just arrived from another device is not sent back.
    pub fn clipboard_changed(&self, text: String) {
        if !self.clipboard_sync() || text.is_empty() || text.len() > server::MAX_CLIPBOARD {
            return;
        }
        if !self.inner.remember_clip(&text) {
            return;
        }
        let targets: Vec<SocketAddr> = {
            let peers = self.inner.peers.lock().unwrap();
            peers
                .values()
                .filter(|p| p.info.trusted == Some(true))
                .map(|p| p.addr)
                .collect()
        };
        let msg = Arc::new(Message::Clipboard { text });
        for addr in targets {
            let (node, msg) = (self.inner.clone(), msg.clone());
            self.inner.rt.spawn(async move {
                if let Err(e) =
                    server::send_one(addr, &node.identity, node.trust.clone(), &msg).await
                {
                    tracing::debug!(%addr, "clipboard not delivered: {e:#}"); // the error, never the text
                }
            });
        }
    }

    /// Turns mDNS announce/browse on or off. Receiving stays on; paired devices that already
    /// know this device can still reach it. Mobile UIs turn this off in the background to save battery.
    pub fn set_discovery(&self, enabled: bool) {
        self.inner.discover.store(enabled, Ordering::SeqCst);
        if enabled {
            self.inner.start_discovery();
        } else {
            self.inner.stop_discovery();
        }
    }

    /// Opens a 2-minute QR pairing window. Show `link` as a QR code (from `size` x `size` `modules`,
    /// row by row, true = dark) for the other device to scan.
    pub fn start_pairing(&self) -> Result<PairingOffer, NodeError> {
        let port = (*self.inner.port.lock().unwrap())
            .ok_or_else(|| fail("MaxxDrop isn't ready to pair yet. Try again in a moment."))?;
        let addrs = local_addrs(port);
        if addrs.is_empty() {
            return Err(fail("Connect to a Wi-Fi or wired network to pair."));
        }
        let code = self.inner.pairing.open().map_err(|e| fail(e.to_string()))?;
        let mut link = format!(
            "rustdrop://pair?v=1&fp={}&code={code}",
            self.inner.identity.fingerprint()
        );
        for addr in addrs {
            link.push_str(&format!("&addr={addr}"));
        }
        let qr = qrcode::QrCode::new(link.as_bytes()).map_err(|e| fail(e.to_string()))?;
        Ok(PairingOffer {
            size: qr.width() as u32,
            modules: qr
                .to_colors()
                .into_iter()
                .map(|c| c == qrcode::Color::Dark)
                .collect(),
            link,
        })
    }

    pub fn stop_pairing(&self) {
        self.inner.pairing.close();
    }

    /// Pairs using a scanned QR link and returns the other device's name. Both devices end up
    /// trusting each other. Blocks for up to a few seconds per address; call it off the UI thread
    /// and never from inside an async runtime.
    pub fn pair_with_link(&self, link: String) -> Result<String, NodeError> {
        let link = parse_link(&link).ok_or_else(|| fail("That isn't a MaxxDrop pairing code."))?;
        if link.fp == self.inner.identity.fingerprint() {
            return Err(fail("That's this device's own pairing code."));
        }
        let node = self.inner.clone();
        let result = self.inner.rt.block_on(async move {
            let mut last = None;
            for addr in &link.addrs {
                match server::pair(
                    *addr,
                    &node.identity,
                    node.trust.clone(),
                    &link.fp,
                    &link.code,
                )
                .await
                {
                    Ok(peer) => return Ok(peer),
                    Err(e) => last = Some(e),
                }
            }
            Err(last.expect("parse_link guarantees at least one address"))
        });
        match result {
            Ok(peer) => {
                self.inner.recheck_all();
                self.inner.probe_known();
                Ok(peer.name)
            }
            Err(e) if format!("{e:#}").contains("certificate") => Err(fail(
                "Couldn't verify the other device. Show a new QR code on it and scan again.",
            )),
            Err(e) => Err(fail(friendly(&e))),
        }
    }
}

impl Inner {
    /// Records `text` as the last synced clipboard; false if it already was (an echo).
    fn remember_clip(&self, text: &str) -> bool {
        let hash = ::ring::digest::digest(&::ring::digest::SHA256, text.as_bytes())
            .as_ref()
            .to_vec();
        let mut last = self.last_clip.lock().unwrap();
        if last.as_deref() == Some(hash.as_slice()) {
            return false;
        }
        *last = Some(hash);
        true
    }

    fn start_discovery(self: &Arc<Self>) {
        let Some(port) = *self.port.lock().unwrap() else {
            return; // run() starts discovery once the listener is bound
        };
        let mut slot = self.discovery.lock().unwrap();
        if slot.is_some() {
            return;
        }
        let daemon = match mdns_sd::ServiceDaemon::new() {
            Ok(d) => d,
            Err(e) => {
                return self
                    .listener
                    .failed(format!("Couldn't start device discovery: {e}"))
            }
        };
        if let Err(e) = discovery::advertise(&daemon, &self.identity.info(), port) {
            let _ = daemon.shutdown();
            return self
                .listener
                .failed(format!("Couldn't announce this device: {e:#}"));
        }
        let (node, browsing) = (self.clone(), daemon.clone());
        let task = self.rt.spawn(async move {
            let _ = discovery::browse(&browsing, node.identity.id, |peer| node.on_peer(peer)).await;
        });
        let watch = self.rt.spawn(self.clone().watch_peers());
        *slot = Some(Discovery {
            daemon,
            task,
            watch,
        });
        drop(slot);
        self.probe_known();
    }

    /// Removes devices that left the network: every 20 s each listed device gets a plain TCP
    /// connect (no handshake, so it costs almost nothing), and one that misses two in a row is
    /// dropped. Paired devices that come back are found again by `probe_known`.
    async fn watch_peers(self: Arc<Self>) {
        let mut misses: HashMap<Uuid, u32> = HashMap::new();
        let mut tick = tokio::time::interval(PEER_CHECK_EVERY);
        tick.tick().await;
        loop {
            tick.tick().await;
            let listed: Vec<(Uuid, SocketAddr)> = {
                let peers = self.peers.lock().unwrap();
                peers.iter().map(|(id, p)| (*id, p.addr)).collect()
            };
            let mut checks = tokio::task::JoinSet::new();
            for (id, addr) in listed {
                checks.spawn(async move {
                    let connect = tokio::net::TcpStream::connect(addr);
                    let up = tokio::time::timeout(std::time::Duration::from_secs(3), connect)
                        .await
                        .is_ok_and(|r| r.is_ok());
                    (id, up)
                });
            }
            let mut gone = Vec::new();
            while let Some(Ok((id, up))) = checks.join_next().await {
                if up {
                    misses.remove(&id);
                } else {
                    let n = misses.entry(id).or_default();
                    *n += 1;
                    if *n >= PEER_MISSES {
                        gone.push(id);
                    }
                }
            }
            if !gone.is_empty() {
                let mut peers = self.peers.lock().unwrap();
                for id in &gone {
                    peers.remove(id);
                    misses.remove(id);
                }
                drop(peers);
                self.emit_peers();
            }
            self.probe_known();
        }
    }

    /// PINGs every paired device at its remembered address and lists the ones that answer.
    /// Finds devices mDNS cannot (different subnets, multicast blocked).
    fn probe_known(self: &Arc<Self>) {
        for (_, device) in self.trust.list() {
            let Some(addr) = device.addr.and_then(|a| a.parse::<SocketAddr>().ok()) else {
                continue;
            };
            if self.peers.lock().unwrap().values().any(|p| p.addr == addr) {
                continue; // already listed
            }
            let node = self.clone();
            self.rt.spawn(async move {
                let Ok((info, _, fp)) =
                    server::ping(addr, &node.identity, node.trust.clone()).await
                else {
                    return;
                };
                let id = info.device_id;
                {
                    let mut peers = node.peers.lock().unwrap();
                    if peers.contains_key(&id) || id == node.identity.id {
                        return;
                    }
                    let info = PeerInfo {
                        id: id.to_string(),
                        name: node.trust.get(&fp).map_or(info.name, |d| d.name),
                        platform: info.platform,
                        trusted: Some(true),
                    };
                    peers.insert(id, Peer { info, addr });
                }
                node.emit_peers();
            });
        }
    }

    /// Stops announcing and browsing; the nearby list empties until discovery resumes.
    fn stop_discovery(&self) {
        if let Some(d) = self.discovery.lock().unwrap().take() {
            d.task.abort();
            d.watch.abort();
            // Goodbye packet first, so other devices drop us; the daemon runs commands in order.
            let _ = d
                .daemon
                .unregister(&format!("{}.{}", self.identity.id, discovery::SERVICE));
            let _ = d.daemon.shutdown();
        }
        self.peers.lock().unwrap().clear();
        self.emit_peers();
    }

    fn on_peer(self: &Arc<Self>, peer: discovery::Peer) -> ControlFlow<()> {
        let Some(addr) = peer.addrs.iter().copied().find(|a| a.is_ipv4()) else {
            return ControlFlow::Continue(());
        };
        let id = peer.info.device_id;
        let changed = {
            let mut peers = self.peers.lock().unwrap();
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
            self.check(id);
        }
        ControlFlow::Continue(())
    }

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
            let pong = server::ping(addr, &node.identity, node.trust.clone()).await;
            // A paired device shows the name the user gave it, not the one it announces.
            let label = pong
                .as_ref()
                .ok()
                .and_then(|(_, _, fp)| node.trust.get(fp))
                .map(|d| d.name);
            if let Some(p) = node.peers.lock().unwrap().get_mut(&id) {
                p.info.trusted = Some(pong.is_ok());
                if let Some(label) = label {
                    p.info.name = label;
                }
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

    fn forward(self: &Arc<Self>, event: Event) {
        let update = match event {
            Event::Incoming(req) if req.clipboard => {
                // Clipboard images are never shown as transfers; only taken while sync is on.
                let ok = self.clipboard_sync.load(Ordering::SeqCst)
                    && req.total() <= MAX_CLIPBOARD_IMAGE
                    && req.files[0].path.to_ascii_lowercase().ends_with(".png");
                if ok {
                    let mut silent = self.silent.lock().unwrap();
                    silent.insert(req.transfer_id, req.trusted_name.clone());
                }
                let _ = req.decision.send(ok);
                return;
            }
            Event::Incoming(req) => {
                let id = req.transfer_id;
                // The user accepted this device for good, or these files before the sender paused.
                let auto = req.resumed
                    || self
                        .trust
                        .get(&req.fingerprint)
                        .is_some_and(|d| d.auto_accept);
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
                self.report(TransferUpdate {
                    id: view.id.clone(),
                    outgoing: Some(false),
                    peer: Some(view.sender.clone()),
                    name: view.files.first().map(|f| f.path.clone()),
                    items: Some(view.count),
                    total: Some(view.total),
                    done: Some(0),
                    state: Some(TransferState::WaitingForAccept),
                    ..Default::default()
                });
                if auto {
                    let accepted = req.decision.send(true).is_ok();
                    let update = if accepted {
                        state(id, TransferState::Transferring, None)
                    } else {
                        state(id, TransferState::Failed, Some("The request expired."))
                    };
                    self.report(update);
                    return;
                }
                let mut pending = self.pending.lock().unwrap();
                // Requests that expired unanswered: their receiver side is gone.
                pending.retain(|_, tx| !tx.is_closed());
                pending.insert(id, req.decision);
                drop(pending);
                self.listener.incoming(view);
                return;
            }
            Event::Progress { transfer_id, .. }
                if self.silent.lock().unwrap().contains_key(&transfer_id) =>
            {
                return;
            }
            Event::Progress {
                transfer_id,
                done,
                total,
            } => progress(transfer_id, done, total),
            Event::Completed { transfer_id, saved } => {
                if let Some(from) = self.silent.lock().unwrap().remove(&transfer_id) {
                    if let Some(path) = saved.first() {
                        let path = path.display().to_string();
                        self.listener.clipboard_image_received(path, from);
                    }
                    return;
                }
                TransferUpdate {
                    saved: Some(saved.iter().map(|p| p.display().to_string()).collect()),
                    ..state(transfer_id, TransferState::Completed, None)
                }
            }
            Event::Failed { transfer_id, .. }
            | Event::Cancelled { transfer_id, .. }
            | Event::Paused { transfer_id }
                if self.silent.lock().unwrap().remove(&transfer_id).is_some() =>
            {
                return;
            }
            Event::Failed {
                transfer_id,
                reason,
            } => state(transfer_id, TransferState::Failed, Some(&reason)),
            Event::Cancelled {
                transfer_id,
                by_peer,
            } => {
                self.pending.lock().unwrap().remove(&transfer_id);
                cancelled_update(transfer_id, by_peer)
            }
            Event::Paused { transfer_id } => state(
                transfer_id,
                TransferState::Paused,
                Some("Paused by the other device"),
            ),
            Event::Text { text, from } => {
                self.listener.text_received(text, from);
                return;
            }
            Event::Clipboard { text, from } => {
                if self.clipboard_sync.load(Ordering::SeqCst) && self.remember_clip(&text) {
                    self.listener.clipboard_received(text, from);
                }
                return;
            }
            Event::Paired { name } => {
                self.listener.paired(name);
                self.recheck_all();
                self.probe_known();
                return;
            }
        };
        self.report(update);
    }
}

async fn run(node: Arc<Inner>) -> anyhow::Result<()> {
    // shortcut: IPv4 only; add an IPv6 listener for IPv6-only networks.
    // A fixed port keeps remembered addresses valid across restarts; any free port if it is taken.
    let listener = match tokio::net::TcpListener::bind(("0.0.0.0", DEFAULT_PORT)).await {
        Ok(l) => l,
        Err(_) => tokio::net::TcpListener::bind("0.0.0.0:0").await?,
    };
    let port = listener.local_addr()?.port();
    node.identity.listen_port.store(port, Ordering::Relaxed);
    *node.port.lock().unwrap() = Some(port);
    let (tx, mut events) = mpsc::channel(32);
    let inbox = Arc::new(Inbox::new(node.downloads.lock().unwrap().clone(), tx));
    *node.inbox.lock().unwrap() = Some(inbox.clone());
    tokio::spawn(server::listen(
        listener,
        node.identity.clone(),
        node.trust.clone(),
        inbox,
        node.pairing.clone(),
    ));
    let forwarder = node.clone();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            forwarder.forward(event);
        }
    });
    if node.discover.load(Ordering::SeqCst) {
        node.start_discovery();
    }
    Ok(())
}

/// This device's reachable IPv4 addresses (private LAN ranges first) for the pairing QR code.
fn local_addrs(port: u16) -> Vec<SocketAddr> {
    let mut ips: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.ip() {
            IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_link_local() => Some(ip),
            _ => None,
        })
        .collect();
    ips.sort_by_key(|ip| !ip.is_private());
    ips.dedup();
    ips.into_iter()
        .take(4)
        .map(|ip| SocketAddr::new(IpAddr::V4(ip), port))
        .collect()
}

#[derive(Debug, PartialEq)]
struct PairLink {
    fp: String,
    code: String,
    addrs: Vec<SocketAddr>,
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Parses `rustdrop://pair?v=1&fp=<64 hex>&code=<32 hex>&addr=<ip:port>...`, which came from a camera: untrusted.
fn parse_link(link: &str) -> Option<PairLink> {
    let query = link.trim().strip_prefix("rustdrop://pair?")?;
    let (mut version, mut fp, mut code, mut addrs) = (None, None, None, Vec::new());
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        match key {
            "v" => version = Some(value),
            "fp" => fp = Some(value),
            "code" => code = Some(value),
            "addr" if addrs.len() < 8 => addrs.push(value.parse().ok()?),
            _ => {}
        }
    }
    let (fp, code) = (fp?, code?);
    let valid = version == Some("1") && is_hex(fp, 64) && is_hex(code, 32) && !addrs.is_empty();
    valid.then(|| PairLink {
        fp: fp.into(),
        code: code.into(),
        addrs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tests::temp_dir;

    /// Updates, pairings, clipboard texts, other events ("text:", "image:", "incoming:").
    #[derive(Default)]
    struct Recorder(
        Mutex<Vec<TransferUpdate>>,
        Mutex<Vec<String>>,
        Mutex<Vec<String>>,
        Mutex<Vec<String>>,
    );

    impl Listener for Recorder {
        fn peers_changed(&self, _: Vec<PeerInfo>) {}
        fn incoming(&self, r: IncomingRequest) {
            self.3.lock().unwrap().push(format!("incoming:{}", r.id));
        }
        fn transfer_changed(&self, update: TransferUpdate) {
            self.0.lock().unwrap().push(update);
        }
        fn paired(&self, name: String) {
            self.1.lock().unwrap().push(name);
        }
        fn clipboard_received(&self, text: String, _from: String) {
            self.2.lock().unwrap().push(text);
        }
        fn clipboard_image_received(&self, path: String, _from: String) {
            self.3.lock().unwrap().push(format!("image:{path}"));
        }
        fn text_received(&self, text: String, from: String) {
            self.3.lock().unwrap().push(format!("text:{from}:{text}"));
        }
        fn failed(&self, _: String) {}
    }

    fn new_node(name: &str, recorder: Arc<Recorder>) -> Arc<Node> {
        let dir = temp_dir("node");
        Node::new(
            dir.join("data").display().to_string(),
            dir.join("dl").display().to_string(),
            Some(name.into()),
            recorder,
        )
        .unwrap()
    }

    #[test]
    fn parses_only_well_formed_pairing_links() {
        let (fp, code) = ("ab".repeat(32), "0f".repeat(16));
        let ok = format!(
            "rustdrop://pair?v=1&fp={fp}&code={code}&addr=192.168.1.5:4000&addr=10.0.0.2:4000"
        );
        let link = parse_link(&ok).unwrap();
        assert_eq!(link.addrs.len(), 2);
        assert_eq!(link.fp, fp);
        for bad in [
            format!("https://pair?v=1&fp={fp}&code={code}&addr=1.2.3.4:5"),
            format!("rustdrop://pair?v=2&fp={fp}&code={code}&addr=1.2.3.4:5"),
            format!(
                "rustdrop://pair?v=1&fp={}&code={code}&addr=1.2.3.4:5",
                "AB".repeat(32)
            ),
            format!("rustdrop://pair?v=1&fp={fp}&code=short&addr=1.2.3.4:5"),
            format!("rustdrop://pair?v=1&fp={fp}&code={code}"),
            format!("rustdrop://pair?v=1&fp={fp}&code={code}&addr=not-an-address"),
            format!("rustdrop://pair?v=1&fp={fp}&code={code}&addr"),
        ] {
            assert!(parse_link(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn qr_pairing_between_two_nodes() {
        let (ra, rb) = (Arc::new(Recorder::default()), Arc::new(Recorder::default()));
        let (a, b) = (new_node("Phone", ra), new_node("Laptop", rb.clone()));
        a.set_discovery(false);
        b.set_discovery(false);

        // The listener binds in the background; pairing is refused until it is ready.
        let offer = (0..100)
            .find_map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(20));
                b.start_pairing().ok()
            })
            .expect("listener never became ready");
        assert_eq!(offer.modules.len(), (offer.size * offer.size) as usize);
        assert!(a.pair_with_link(b.me().id).is_err());

        assert_eq!(a.pair_with_link(offer.link.clone()).unwrap(), "Laptop");
        assert_eq!(a.trusted()[0].fingerprint, b.me().fingerprint);
        assert_eq!(b.trusted()[0].fingerprint, a.me().fingerprint);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(*rb.1.lock().unwrap(), vec!["Phone".to_string()]);

        // Without mDNS, each side lists the other at the address it remembered.
        a.refresh();
        b.refresh();
        let listed = |n: &Node, id: &str| {
            n.peers()
                .iter()
                .any(|p| p.id == id && p.trusted == Some(true))
        };
        let (a_id, b_id) = (a.me().id, b.me().id);
        let both = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            listed(&a, &b_id) && listed(&b, &a_id)
        });
        assert!(both, "a sees b: {:?}, b sees a: {:?}", a.peers(), b.peers());

        // Single use: the same QR code does not work twice.
        let c = new_node("Stranger", Arc::new(Recorder::default()));
        c.set_discovery(false);
        assert!(c.pair_with_link(offer.link).is_err());
        assert_eq!(b.trusted().len(), 1);
    }

    /// Two nodes paired by QR code that list each other (mDNS off).
    fn paired_nodes() -> (Arc<Node>, Arc<Node>, Arc<Recorder>, Arc<Recorder>) {
        let (ra, rb) = (Arc::new(Recorder::default()), Arc::new(Recorder::default()));
        let (a, b) = (
            new_node("Phone", ra.clone()),
            new_node("Laptop", rb.clone()),
        );
        a.set_discovery(false);
        b.set_discovery(false);
        let offer = (0..100)
            .find_map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(20));
                b.start_pairing().ok()
            })
            .expect("listener never became ready");
        a.pair_with_link(offer.link).unwrap();
        a.refresh();
        b.refresh();
        let listed = |n: &Node| n.peers().iter().any(|p| p.trusted == Some(true));
        assert!(
            (0..100).any(|_| {
                std::thread::sleep(std::time::Duration::from_millis(20));
                listed(&a) && listed(&b)
            }),
            "nodes never listed each other"
        );
        (a, b, ra, rb)
    }

    fn wait_for(f: impl Fn() -> bool) -> bool {
        (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            f()
        })
    }

    #[test]
    fn clipboard_sync_is_opt_in_and_does_not_echo() {
        let (a, b, ra, rb) = paired_nodes();

        // Off by default on both sides: nothing moves.
        assert!(!a.clipboard_sync() && !b.clipboard_sync());
        a.clipboard_changed("secret".into());
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(rb.2.lock().unwrap().is_empty());

        a.set_clipboard_sync(true).unwrap();
        b.set_clipboard_sync(true).unwrap();
        a.clipboard_changed("hello from phone".into());
        assert!(wait_for(
            || *rb.2.lock().unwrap() == vec!["hello from phone".to_string()]
        ));

        // b's OS reports the text it just received as a local change: it must not bounce back.
        b.clipboard_changed("hello from phone".into());
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(ra.2.lock().unwrap().is_empty());

        // A new copy on b does reach a.
        b.clipboard_changed("reply from laptop".into());
        assert!(wait_for(
            || *ra.2.lock().unwrap() == vec!["reply from laptop".to_string()]
        ));

        // Turning it off on the receiver stops delivery.
        b.set_clipboard_sync(false).unwrap();
        a.clipboard_changed("after opt-out".into());
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(rb.2.lock().unwrap().len(), 1);
    }

    #[test]
    fn rejects_bad_input_and_reports_expired_requests() {
        let recorder = Arc::new(Recorder::default());
        let node = new_node("Pixel", recorder.clone());

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
        // A transfer this node never saw start is not history.
        assert!(node.history().is_empty());
    }

    fn peer_id(n: &Node) -> String {
        n.peers()[0].id.clone()
    }

    #[test]
    fn auto_accept_history_text_and_rename() {
        let (a, b, ra, rb) = paired_nodes();
        let src = temp_dir("auto");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("report.txt");
        std::fs::write(&file, b"quarterly numbers").unwrap();

        // b accepts everything from a without asking.
        b.set_auto_accept(a.me().fingerprint, true).unwrap();
        assert!(b.trusted()[0].auto_accept);
        let id = a
            .send(peer_id(&a), vec![file.display().to_string()])
            .unwrap();
        let done = |r: &Recorder| {
            r.0.lock()
                .unwrap()
                .iter()
                .any(|u| u.state == Some(TransferState::Completed))
        };
        assert!(
            wait_for(|| done(&ra) && done(&rb)),
            "{:?}",
            rb.0.lock().unwrap()
        );
        assert!(rb.3.lock().unwrap().is_empty(), "b was asked");

        // Both sides keep it in history, across a restart of the list.
        let h = a.history();
        assert_eq!(
            (h[0].id.as_str(), h[0].name.as_str()),
            (id.as_str(), "report.txt")
        );
        assert_eq!(
            (h[0].state, h[0].outgoing, h[0].total),
            (TransferState::Completed, true, 17)
        );
        let h = b.history();
        assert_eq!((h[0].peer.as_str(), h[0].saved.len()), ("Phone", 1));
        b.clear_history().unwrap();
        assert!(b.history().is_empty());

        // A link sent on purpose arrives as text, whatever clipboard sync says.
        a.send_text(peer_id(&a), " https://example.com/x ".into())
            .unwrap();
        assert!(wait_for(|| rb
            .3
            .lock()
            .unwrap()
            .contains(&"text:Phone:https://example.com/x".to_string())));
        assert!(a.send_text(peer_id(&a), "  ".into()).is_err());

        // Renaming a paired device changes what this side lists.
        b.rename_device(a.me().fingerprint, "Work phone".into())
            .unwrap();
        assert!(b.rename_device(a.me().fingerprint, "".into()).is_err());
        b.refresh();
        assert!(wait_for(|| b.peers()[0].name == "Work phone"));
        assert_eq!(b.trusted()[0].name, "Work phone");
        assert!(b.trusted()[0].auto_accept, "rename keeps auto-accept");
    }

    #[test]
    fn clipboard_image_only_moves_when_sync_is_on_and_is_never_a_transfer() {
        let (a, b, ra, rb) = paired_nodes();
        let src = temp_dir("clip");
        std::fs::create_dir_all(&src).unwrap();
        let png = src.join("shot.png");
        std::fs::write(&png, b"\x89PNG fake image bytes").unwrap();

        a.clipboard_image(png.display().to_string()).unwrap(); // sync off: nothing
        b.set_clipboard_sync(true).unwrap();
        a.clipboard_image(png.display().to_string()).unwrap(); // only b on: still nothing
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(rb.3.lock().unwrap().is_empty());

        a.set_clipboard_sync(true).unwrap();
        a.clipboard_image(png.display().to_string()).unwrap();
        assert!(wait_for(|| !rb.3.lock().unwrap().is_empty()));
        let got = rb.3.lock().unwrap()[0].clone();
        let path = got.strip_prefix("image:").expect("an image event");
        assert_eq!(std::fs::read(path).unwrap(), b"\x89PNG fake image bytes");
        assert!(ra.0.lock().unwrap().is_empty() && rb.0.lock().unwrap().is_empty());
        assert!(a.history().is_empty() && b.history().is_empty());
    }

    #[test]
    fn download_folder_can_be_changed_and_persists() {
        let recorder = Arc::new(Recorder::default());
        let dir = temp_dir("dlset");
        let data = dir.join("data").display().to_string();
        let other = dir.join("elsewhere");
        let node = Node::new(
            data.clone(),
            dir.join("dl").display().to_string(),
            None,
            recorder.clone(),
        )
        .unwrap();
        assert!(
            node.set_downloads(other.display().to_string()).is_err(),
            "must exist"
        );
        std::fs::create_dir_all(&other).unwrap();
        node.set_downloads(other.display().to_string()).unwrap();
        assert_eq!(node.me().downloads, other.display().to_string());
        drop(node);
        let again = Node::new(data, dir.join("dl").display().to_string(), None, recorder).unwrap();
        assert_eq!(again.me().downloads, other.display().to_string());
    }
}
