//! RustDrop for Windows: a thin Tauri shell over `rustdrop_core::node`.
//! The UI only shows state and issues commands; networking, trust and transfers stay in the core.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use arboard::SetExtWindows;
use rustdrop_core::{
    config,
    node::{
        IncomingRequest, Listener, Me, Node, PairingOffer, PeerInfo, TransferUpdate, TrustedInfo,
    },
    transfer::TransferState,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;

type Shared = Arc<Node>;
type Answer<T> = Result<T, String>;

/// Forwards core events to the webview, and raises Windows notifications for what the user
/// must see even when RustDrop is in the background: a request to accept, and cancellations.
struct Events {
    app: AppHandle,
    /// Transfer id -> other device's name (the first update of each transfer carries it).
    peers: Mutex<HashMap<String, String>>,
}

impl Events {
    fn notify(&self, title: &str, body: &str) {
        let _ = self
            .app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show();
    }

    fn focused(&self) -> bool {
        self.app
            .get_webview_window("main")
            .and_then(|w| w.is_focused().ok())
            .unwrap_or(false)
    }
}

impl Listener for Events {
    fn peers_changed(&self, peers: Vec<PeerInfo>) {
        let _ = self.app.emit("peers", peers);
    }
    fn incoming(&self, request: IncomingRequest) {
        if !self.focused() {
            let what = if request.count == 1 {
                "a file".into()
            } else {
                format!("{} files", request.count)
            };
            self.notify(
                &format!("{} wants to send you {what}", request.sender),
                "Open RustDrop to accept or decline.",
            );
        }
        let _ = self.app.emit("incoming", request);
    }
    fn transfer_changed(&self, update: TransferUpdate) {
        let mut peers = self.peers.lock().unwrap();
        if let Some(peer) = &update.peer {
            peers.insert(update.id.clone(), peer.clone());
        }
        if update.state == Some(TransferState::Cancelled) {
            let who = peers
                .remove(&update.id)
                .unwrap_or_else(|| "The other device".into());
            match update.reason.as_deref() {
                Some("Cancelled by the other device") => self.notify(
                    "Transfer cancelled",
                    &format!("{who} cancelled the transfer."),
                ),
                Some("You cancelled") => self.notify(
                    "Transfer cancelled",
                    &format!("The transfer with {who} was cancelled."),
                ),
                _ => {}
            }
        }
        drop(peers);
        let _ = self.app.emit("transfer", update);
    }
    fn clipboard_received(&self, text: String, from: String) {
        // Kept out of Win+V history and cloud clipboard (SECURITY.md §10). The watcher sees this
        // write, but the core ignores it because it matches the text just received.
        let written =
            arboard::Clipboard::new().and_then(|mut c| c.set().exclude_from_history().text(text));
        if written.is_ok() {
            let _ = self.app.emit("clipboard", from);
        }
    }
    fn paired(&self, name: String) {
        let _ = self.app.emit("paired", name);
    }
    fn failed(&self, message: String) {
        let _ = self.app.emit("fatal", message);
    }
}

#[tauri::command]
fn me(node: State<Shared>) -> Me {
    node.me()
}

#[tauri::command]
fn peers(node: State<Shared>) -> Vec<PeerInfo> {
    node.peers()
}

#[tauri::command]
fn trusted(node: State<Shared>) -> Vec<TrustedInfo> {
    node.trusted()
}

#[tauri::command]
fn pair(node: State<Shared>, fingerprint: String, name: String) -> Answer<()> {
    node.pair(fingerprint, name).map_err(|e| e.to_string())
}

#[tauri::command]
fn unpair(node: State<Shared>, fingerprint: String) -> Answer<()> {
    node.unpair(fingerprint).map_err(|e| e.to_string())
}

#[tauri::command]
fn refresh(node: State<Shared>) {
    node.refresh()
}

#[tauri::command]
fn respond(node: State<Shared>, id: String, accept: bool) {
    node.respond(id, accept)
}

#[tauri::command]
fn send(node: State<Shared>, device: String, paths: Vec<String>) -> Answer<String> {
    node.send(device, paths).map_err(|e| e.to_string())
}

#[tauri::command]
fn start_pairing(node: State<Shared>) -> Answer<PairingOffer> {
    node.start_pairing().map_err(|e| e.to_string())
}

#[tauri::command]
fn stop_pairing(node: State<Shared>) {
    node.stop_pairing()
}

#[tauri::command]
fn cancel(node: State<Shared>, id: String) {
    node.cancel(id)
}

#[tauri::command]
fn open_downloads() -> Answer<()> {
    let dir = config::downloads_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer")
        .arg(&dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Shows a received file in Explorer. Only paths inside the downloads folder are allowed.
#[tauri::command]
fn reveal(path: PathBuf) -> Answer<()> {
    let root = config::downloads_dir()
        .canonicalize()
        .map_err(|e| e.to_string())?;
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

#[tauri::command]
fn clipboard_sync(node: State<Shared>) -> bool {
    node.clipboard_sync()
}

#[tauri::command]
fn set_clipboard_sync(node: State<Shared>, enabled: bool) -> Answer<()> {
    node.set_clipboard_sync(enabled).map_err(|e| e.to_string())
}

/// Polls the clipboard every 500 ms while sync is on (DESIGN target: under 500 ms latency).
/// shortcut: polling, not AddClipboardFormatListener; switch if idle CPU ever matters.
fn watch_clipboard(node: Shared) {
    std::thread::spawn(move || {
        let mut last: Option<String> = None;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if !node.clipboard_sync() {
                last = None;
                continue;
            }
            let Ok(text) = arboard::Clipboard::new().and_then(|mut c| c.get_text()) else {
                continue; // not text, or another app holds the clipboard right now
            };
            if last.as_deref() != Some(text.as_str()) {
                // The first read after turning sync on is what was already there: don't send it.
                if last.is_some() {
                    node.clipboard_changed(text.clone());
                }
                last = Some(text);
            }
        }
    });
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let node = Node::new(
                config::data_dir().display().to_string(),
                config::downloads_dir().display().to_string(),
                None,
                Arc::new(Events {
                    app: app.handle().clone(),
                    peers: Mutex::default(),
                }),
            )?;
            watch_clipboard(node.clone());
            app.manage(node);
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
            start_pairing,
            stop_pairing,
            clipboard_sync,
            set_clipboard_sync,
            open_downloads,
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("run RustDrop");
}
