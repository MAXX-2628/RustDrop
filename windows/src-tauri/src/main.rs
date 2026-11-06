//! RustDrop for Windows: a thin Tauri shell over `rustdrop_core::node`.
//! The UI only shows state and issues commands; networking, trust and transfers stay in the core.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{path::PathBuf, sync::Arc};

use rustdrop_core::{
    config,
    node::{IncomingRequest, Listener, Me, Node, PeerInfo, TransferUpdate, TrustedInfo},
};
use tauri::{AppHandle, Emitter, Manager, State};

type Shared = Arc<Node>;
type Answer<T> = Result<T, String>;

/// Forwards core events to the webview.
struct Events(AppHandle);

impl Listener for Events {
    fn peers_changed(&self, peers: Vec<PeerInfo>) {
        let _ = self.0.emit("peers", peers);
    }
    fn incoming(&self, request: IncomingRequest) {
        let _ = self.0.emit("incoming", request);
    }
    fn transfer_changed(&self, update: TransferUpdate) {
        let _ = self.0.emit("transfer", update);
    }
    fn failed(&self, message: String) {
        let _ = self.0.emit("fatal", message);
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

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let node = Node::new(
                config::data_dir().display().to_string(),
                config::downloads_dir().display().to_string(),
                None,
                Arc::new(Events(app.handle().clone())),
            )?;
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
            open_downloads,
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("run RustDrop");
}
