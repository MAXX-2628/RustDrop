//! RustDrop for Windows: a thin Tauri shell over `rustdrop-core`.
//! The UI only shows state and issues commands; networking, trust and transfers stay in the core.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::HashMap,
    net::SocketAddr,
    ops::ControlFlow,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use rustdrop_core::{
    config, discovery,
    identity::Identity,
    protocol::FileMeta,
    security::TrustStore,
    server,
    transfer::{self, Event, Inbox, Outcome, TransferState},
};
use serde::Serialize;
use tauri::{async_runtime::JoinHandle, AppHandle, Emitter, State};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

struct Node {
    identity: Arc<Identity>,
    trust: Arc<TrustStore>,
    downloads: PathBuf,
    peers: Mutex<HashMap<Uuid, Peer>>,
    pending: Mutex<HashMap<Uuid, oneshot::Sender<bool>>>,
    sends: Mutex<HashMap<Uuid, JoinHandle<()>>>,
    error: Mutex<Option<String>>,
}

type Shared = Arc<Node>;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Peer {
    id: Uuid,
    name: String,
    platform: String,
    #[serde(skip)]
    addr: SocketAddr,
    /// `None` while checking; `Some(true)` once an authenticated PING succeeded.
    trusted: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Me {
    name: String,
    id: Uuid,
    fingerprint: String,
    downloads: PathBuf,
    version: &'static str,
    error: Option<String>,
}

#[derive(Serialize)]
struct Trusted {
    fingerprint: String,
    name: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct IncomingView {
    id: Uuid,
    sender: String,
    platform: String,
    files: Vec<FileMeta>,
    count: usize,
    total: u64,
}

/// Partial transfer update; the UI merges the non-null fields into the transfer with this `id`.
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Update {
    id: Uuid,
    outgoing: Option<bool>,
    peer: Option<String>,
    items: Option<usize>,
    total: Option<u64>,
    done: Option<u64>,
    state: Option<TransferState>,
    reason: Option<String>,
    saved: Option<Vec<PathBuf>>,
}

fn emit_update(h: &AppHandle, update: Update) {
    let _ = h.emit("transfer", update);
}

fn state(id: Uuid, state: TransferState, reason: Option<&str>) -> Update {
    Update {
        id,
        state: Some(state),
        reason: reason.map(Into::into),
        ..Default::default()
    }
}

fn progress(id: Uuid, done: u64, total: u64) -> Update {
    let s = if done == total {
        TransferState::Verifying
    } else {
        TransferState::Transferring
    };
    Update {
        id,
        done: Some(done),
        total: Some(total),
        state: Some(s),
        ..Default::default()
    }
}

fn peer_list(node: &Node) -> Vec<Peer> {
    let mut peers: Vec<Peer> = node.peers.lock().unwrap().values().cloned().collect();
    peers.sort_by_key(|p| p.name.to_lowercase());
    peers
}

fn emit_peers(h: &AppHandle, node: &Node) {
    let _ = h.emit("peers", peer_list(node));
}

/// Marks the peer as checking, then tries an authenticated PING to learn whether we are paired.
fn check(h: AppHandle, node: Shared, id: Uuid) {
    let addr = node.peers.lock().unwrap().get_mut(&id).map(|p| {
        p.trusted = None;
        p.addr
    });
    let Some(addr) = addr else { return };
    emit_peers(&h, &node);
    tauri::async_runtime::spawn(async move {
        let ok = server::ping(addr, &node.identity, node.trust.clone())
            .await
            .is_ok();
        if let Some(p) = node.peers.lock().unwrap().get_mut(&id) {
            p.trusted = Some(ok);
        }
        emit_peers(&h, &node);
    });
}

fn recheck_all(h: &AppHandle, node: &Shared) {
    let ids: Vec<Uuid> = node.peers.lock().unwrap().keys().copied().collect();
    for id in ids {
        check(h.clone(), node.clone(), id);
    }
}

/// Short, actionable failure text for the UI (DESIGN.md §14).
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

#[tauri::command]
fn me(node: State<Shared>) -> Me {
    Me {
        name: node.identity.name.clone(),
        id: node.identity.id,
        fingerprint: node.identity.fingerprint(),
        downloads: node.downloads.clone(),
        version: rustdrop_core::VERSION,
        error: node.error.lock().unwrap().clone(),
    }
}

#[tauri::command]
fn peers(node: State<Shared>) -> Vec<Peer> {
    peer_list(&node)
}

#[tauri::command]
fn trusted(node: State<Shared>) -> Vec<Trusted> {
    let mut list: Vec<Trusted> = node
        .trust
        .list()
        .into_iter()
        .map(|(fingerprint, d)| Trusted {
            fingerprint,
            name: d.name,
        })
        .collect();
    list.sort_by_key(|t| t.name.to_lowercase());
    list
}

#[tauri::command]
fn pair(
    h: AppHandle,
    node: State<Shared>,
    fingerprint: String,
    name: String,
) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err("Enter a device name of up to 64 characters.".into());
    }
    let fingerprint: String = fingerprint.chars().filter(|c| !c.is_whitespace()).collect();
    node.trust
        .add(&fingerprint, name)
        .map_err(|_| "That isn't a valid fingerprint. It has 64 letters and digits.".to_string())?;
    recheck_all(&h, &node);
    Ok(())
}

#[tauri::command]
fn unpair(h: AppHandle, node: State<Shared>, fingerprint: String) -> Result<(), String> {
    node.trust.remove(&fingerprint).map_err(|e| e.to_string())?;
    recheck_all(&h, &node);
    Ok(())
}

#[tauri::command]
fn refresh(h: AppHandle, node: State<Shared>) {
    recheck_all(&h, &node);
}

#[tauri::command]
fn respond(h: AppHandle, node: State<Shared>, id: Uuid, accept: bool) {
    let delivered = node
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
    emit_update(&h, update);
}

#[tauri::command]
fn send(
    h: AppHandle,
    node: State<Shared>,
    device: Uuid,
    paths: Vec<PathBuf>,
) -> Result<Uuid, String> {
    if paths.is_empty() {
        return Err("Choose at least one file or folder.".into());
    }
    let peer = node
        .peers
        .lock()
        .unwrap()
        .get(&device)
        .cloned()
        .ok_or("That device is no longer nearby.")?;

    let id = Uuid::new_v4();
    emit_update(
        &h,
        Update {
            id,
            outgoing: Some(true),
            peer: Some(peer.name.clone()),
            items: Some(paths.len()),
            done: Some(0),
            state: Some(TransferState::WaitingForAccept),
            ..Default::default()
        },
    );

    let node = node.inner().clone();
    // Hold the lock across spawn so the task cannot remove its entry before it is inserted.
    let mut sends = node.sends.lock().unwrap();
    let task = {
        let node = node.clone();
        tauri::async_runtime::spawn(async move {
            let tick = h.clone();
            let result = transfer::send(
                peer.addr,
                &node.identity,
                node.trust.clone(),
                &paths,
                move |done, total| emit_update(&tick, progress(id, done, total)),
            )
            .await;
            node.sends.lock().unwrap().remove(&id);
            let update = match result {
                Ok(Outcome::Completed) => state(id, TransferState::Completed, None),
                Ok(Outcome::Declined) => state(id, TransferState::Cancelled, Some("Declined")),
                Err(e) => state(id, TransferState::Failed, Some(&friendly(&e))),
            };
            emit_update(&h, update);
        })
    };
    sends.insert(id, task);
    Ok(id)
}

/// Cancels an outgoing transfer; the receiver discards everything it staged.
#[tauri::command]
fn cancel(h: AppHandle, node: State<Shared>, id: Uuid) {
    if let Some(task) = node.sends.lock().unwrap().remove(&id) {
        task.abort();
        emit_update(&h, state(id, TransferState::Cancelled, Some("Cancelled")));
    }
}

#[tauri::command]
fn open_downloads(node: State<Shared>) -> Result<(), String> {
    std::fs::create_dir_all(&node.downloads).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer")
        .arg(&node.downloads)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Shows a received file in Explorer. Only paths inside the downloads folder are allowed.
#[tauri::command]
fn reveal(node: State<Shared>, path: PathBuf) -> Result<(), String> {
    let root = node.downloads.canonicalize().map_err(|e| e.to_string())?;
    let target = path
        .canonicalize()
        .map_err(|_| "That file was moved or deleted.".to_string())?;
    if !target.starts_with(&root) {
        return Err("Only files in the RustDrop downloads folder can be shown.".into());
    }
    let mut select = std::ffi::OsString::from("/select,");
    select.push(target.as_os_str());
    std::process::Command::new("explorer")
        .arg(select)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn forward(h: &AppHandle, node: &Node, event: Event) {
    match event {
        Event::Incoming(req) => {
            let view = IncomingView {
                id: req.transfer_id,
                sender: req.trusted_name.clone(),
                platform: req.peer.platform.clone(),
                count: req.files.len(),
                total: req.total(),
                files: req.files.iter().take(50).cloned().collect(),
            };
            emit_update(
                h,
                Update {
                    id: view.id,
                    outgoing: Some(false),
                    peer: Some(view.sender.clone()),
                    items: Some(view.count),
                    total: Some(view.total),
                    done: Some(0),
                    state: Some(TransferState::WaitingForAccept),
                    ..Default::default()
                },
            );
            node.pending
                .lock()
                .unwrap()
                .insert(req.transfer_id, req.decision);
            let _ = h.emit("incoming", view);
        }
        Event::Progress {
            transfer_id,
            done,
            total,
        } => emit_update(h, progress(transfer_id, done, total)),
        Event::Completed { transfer_id, saved } => emit_update(
            h,
            Update {
                saved: Some(saved),
                ..state(transfer_id, TransferState::Completed, None)
            },
        ),
        Event::Failed {
            transfer_id,
            reason,
        } => emit_update(h, state(transfer_id, TransferState::Failed, Some(&reason))),
    }
}

async fn run_node(h: AppHandle, node: Shared) -> anyhow::Result<()> {
    // shortcut: IPv4 only, same as the CLI; add an IPv6 listener for IPv6-only networks.
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
    let port = listener.local_addr()?.port();
    let (tx, mut events) = mpsc::channel(32);
    let inbox = Arc::new(Inbox::new(node.downloads.clone(), tx));
    tauri::async_runtime::spawn(server::listen(
        listener,
        node.identity.clone(),
        node.trust.clone(),
        inbox,
    ));
    {
        let (h, node) = (h.clone(), node.clone());
        tauri::async_runtime::spawn(async move {
            while let Some(event) = events.recv().await {
                forward(&h, &node, event);
            }
        });
    }

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
                    let (name, platform) = (peer.info.name, peer.info.platform);
                    let trusted = None;
                    peers.insert(
                        id,
                        Peer {
                            id,
                            name,
                            platform,
                            addr,
                            trusted,
                        },
                    );
                    true
                }
            }
        };
        if changed {
            check(h.clone(), node.clone(), id);
        }
        ControlFlow::Continue(())
    })
    .await
}

fn main() {
    let home = config::data_dir();
    let identity = Identity::load_or_create(&home).expect("load device identity");
    let trust = TrustStore::load(home.join("trusted.json")).expect("load trusted devices");
    let node: Shared = Arc::new(Node {
        identity: Arc::new(identity),
        trust: Arc::new(trust),
        downloads: config::downloads_dir(),
        peers: Mutex::default(),
        pending: Mutex::default(),
        sends: Mutex::default(),
        error: Mutex::default(),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(node.clone())
        .setup(move |app| {
            let h = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = run_node(h.clone(), node.clone()).await {
                    let message = format!("{e:#}");
                    *node.error.lock().unwrap() = Some(message.clone());
                    let _ = h.emit("fatal", message);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            me,
            peers,
            trusted,
            pair,
            unpair,
            refresh,
            respond,
            send,
            cancel,
            open_downloads,
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("run RustDrop");
}
