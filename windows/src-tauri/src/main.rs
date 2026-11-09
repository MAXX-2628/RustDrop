//! MaxxDrop for Windows: a thin Tauri shell over `rustdrop_core::node`.
//! The UI only shows state and issues commands; networking, trust and transfers stay in the core.
//! The shell adds what only Windows has: the tray, autostart, the Explorer menu and the clipboard.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex,
    },
};

use arboard::SetExtWindows;
use rustdrop_core::{
    config,
    node::{
        HistoryEntry, IncomingRequest, Listener, Me, Node, PairingOffer, PeerInfo, TransferUpdate,
        TrustedInfo,
    },
    transfer::TransferState,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use tauri_plugin_notification::NotificationExt;

type Shared = Arc<Node>;
type Answer<T> = Result<T, String>;

/// Started by autostart: stay in the tray.
const HIDDEN: &str = "--hidden";

/// Forwards core events to the webview, and raises Windows notifications for what the user
/// must see even when MaxxDrop is in the background: a request to accept, links, cancellations.
struct Events {
    app: AppHandle,
    /// Transfer id -> other device's name (the first update of each transfer carries it).
    peers: Mutex<HashMap<String, String>>,
    /// Clipboard sequence number right after MaxxDrop wrote it, so the watcher skips it.
    ours: Arc<AtomicU32>,
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

    /// Writes the clipboard, kept out of Win+V history and cloud clipboard (SECURITY.md §10).
    fn write_clipboard(
        &self,
        write: impl FnOnce(arboard::Set<'_>) -> Result<(), arboard::Error>,
    ) -> bool {
        let written =
            arboard::Clipboard::new().and_then(|mut c| write(c.set().exclude_from_history()));
        self.ours.store(clipboard_seq(), Ordering::SeqCst);
        written.is_ok()
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
                "Open MaxxDrop to accept or decline.",
            );
        }
        let _ = self.app.emit("incoming", request);
    }
    fn transfer_changed(&self, update: TransferUpdate) {
        let mut peers = self.peers.lock().unwrap();
        if let Some(peer) = &update.peer {
            peers.insert(update.id.clone(), peer.clone());
        }
        if matches!(
            update.state,
            Some(TransferState::Completed | TransferState::Failed)
        ) {
            peers.remove(&update.id); // finished: the name is only needed for a cancel notice
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
        if self.write_clipboard(|s| s.text(text)) {
            let _ = self.app.emit("clipboard", from);
        }
    }
    fn clipboard_image_received(&self, path: String, from: String) {
        let image = std::fs::read(&path)
            .ok()
            .and_then(|b| image::load_from_memory_with_format(&b, image::ImageFormat::Png).ok());
        let _ = std::fs::remove_file(&path); // only the clipboard keeps it
        let Some(image) = image else { return };
        let rgba = image.to_rgba8();
        let data = arboard::ImageData {
            width: rgba.width() as usize,
            height: rgba.height() as usize,
            bytes: Cow::Owned(rgba.into_raw()),
        };
        if self.write_clipboard(|s| s.image(data)) {
            let _ = self.app.emit("clipboard", from);
        }
    }
    fn text_received(&self, text: String, from: String) {
        if !self.focused() {
            let short: String = text.chars().take(120).collect();
            self.notify(
                &format!(
                    "{from} sent you {}",
                    if is_link(&text) { "a link" } else { "text" }
                ),
                &short,
            );
        }
        let _ = self.app.emit("text", (text, from));
    }
    fn paired(&self, name: String) {
        let _ = self.app.emit("paired", name);
    }
    fn failed(&self, message: String) {
        let _ = self.app.emit("fatal", message);
    }
}

fn is_link(text: &str) -> bool {
    (text.starts_with("https://") || text.starts_with("http://"))
        && text.len() < 4096
        && !text
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"')
}

fn clipboard_seq() -> u32 {
    // SAFETY: no arguments; reads a counter.
    unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
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
fn rename_device(node: State<Shared>, fingerprint: String, name: String) -> Answer<()> {
    node.rename_device(fingerprint, name)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_auto_accept(node: State<Shared>, fingerprint: String, enabled: bool) -> Answer<()> {
    node.set_auto_accept(fingerprint, enabled)
        .map_err(|e| e.to_string())
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

/// Blocks while connecting, so it runs off the main thread.
#[tauri::command]
async fn send_text(node: State<'_, Shared>, device: String, text: String) -> Answer<()> {
    let node = node.inner().clone();
    tauri::async_runtime::spawn_blocking(move || node.send_text(device, text))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn pause(node: State<Shared>, id: String) {
    node.pause(id)
}

#[tauri::command]
fn resume(node: State<Shared>, id: String) -> Answer<()> {
    node.resume(id).map_err(|e| e.to_string())
}

#[tauri::command]
fn history(node: State<Shared>) -> Vec<HistoryEntry> {
    node.history()
}

#[tauri::command]
fn clear_history(node: State<Shared>) -> Answer<()> {
    node.clear_history().map_err(|e| e.to_string())
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
fn set_downloads(node: State<Shared>, path: String) -> Answer<()> {
    node.set_downloads(path).map_err(|e| e.to_string())
}

#[tauri::command]
fn open_downloads(node: State<Shared>) -> Answer<()> {
    let dir = PathBuf::from(node.me().downloads);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer")
        .arg(&dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Shows a received file in Explorer. Only paths inside the downloads folder are allowed.
#[tauri::command]
fn reveal(node: State<Shared>, path: PathBuf) -> Answer<()> {
    let root = PathBuf::from(node.me().downloads)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let target = path
        .canonicalize()
        .map_err(|_| "That file was moved or deleted.".to_string())?;
    if !target.starts_with(&root) {
        return Err("Only files in the MaxxDrop downloads folder can be shown.".into());
    }
    let mut select = std::ffi::OsString::from("/select,");
    select.push(target.as_os_str());
    std::process::Command::new("explorer")
        .arg(select)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Opens a received link in the default browser. Only plain http(s) links, only when the user asks.
#[tauri::command]
fn open_link(url: String) -> Answer<()> {
    if !is_link(&url) {
        return Err("Only web links (http or https) can be opened.".into());
    }
    std::process::Command::new("explorer")
        .arg(url)
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

#[tauri::command]
fn autostart() -> bool {
    reg::get(RUN_KEY, Some(APP)).is_some()
}

#[tauri::command]
fn set_autostart(enabled: bool) -> Answer<()> {
    if enabled {
        reg::set(RUN_KEY, Some(APP), &format!("\"{}\" {HIDDEN}", exe()?))
    } else {
        reg::delete_value(RUN_KEY, APP)
    }
    .map_err(|e| format!("Couldn't change the startup setting: {e}"))
}

#[tauri::command]
fn explorer_menu() -> bool {
    reg::get(&format!("{}\\command", MENU_KEYS[0]), None).is_some()
}

#[tauri::command]
fn set_explorer_menu(enabled: bool) -> Answer<()> {
    let result = if enabled {
        add_explorer_menu()
    } else {
        MENU_KEYS.iter().try_for_each(|k| reg::delete_tree(k))
    };
    result.map_err(|e| format!("Couldn't change the Explorer menu: {e}"))
}

/// Paths Explorer passed when it started MaxxDrop ("Send with MaxxDrop"), handed to the UI once.
#[tauri::command]
fn take_launch_paths(launch: State<Launch>) -> Vec<String> {
    std::mem::take(&mut *launch.0.lock().unwrap())
}

struct Launch(Mutex<Vec<String>>);

/// File and folder arguments (Explorer quotes each path; flags are skipped).
fn paths_in(args: impl IntoIterator<Item = String>) -> Vec<String> {
    args.into_iter()
        .skip(1)
        .filter(|a| !a.starts_with("--") && Path::new(a).exists())
        .collect()
}

const APP: &str = "MaxxDrop";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// "Send with MaxxDrop" on files and on folders (per user, no admin rights).
const MENU_KEYS: [&str; 2] = [
    r"Software\Classes\*\shell\MaxxDrop",
    r"Software\Classes\Directory\shell\MaxxDrop",
];

fn exe() -> Answer<String> {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())
}

fn add_explorer_menu() -> std::io::Result<()> {
    let exe = exe().map_err(std::io::Error::other)?;
    for key in MENU_KEYS {
        reg::set(key, None, "Send with MaxxDrop")?;
        reg::set(key, Some("Icon"), &exe)?;
        reg::set(
            &format!("{key}\\command"),
            None,
            &format!("\"{exe}\" \"%1\""),
        )?;
    }
    Ok(())
}

/// The portable exe may have moved since autostart or the Explorer menu were turned on.
fn refresh_registered_paths() {
    if autostart() {
        let _ = set_autostart(true);
    }
    if explorer_menu() {
        let _ = add_explorer_menu();
    }
}

/// HKEY_CURRENT_USER string values.
mod reg {
    use std::{io, ptr};
    use windows_sys::Win32::System::Registry::{
        RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER,
        REG_SZ, RRF_RT_REG_SZ,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn check(code: u32) -> io::Result<()> {
        match code {
            0 => Ok(()),
            2 => Ok(()), // ERROR_FILE_NOT_FOUND: deleting what isn't there
            c => Err(io::Error::from_raw_os_error(c as i32)),
        }
    }

    pub fn set(key: &str, name: Option<&str>, value: &str) -> io::Result<()> {
        let (key, value) = (wide(key), wide(value));
        let name = name.map(wide);
        // SAFETY: every pointer is a NUL-terminated UTF-16 buffer that outlives the call.
        let code = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ref().map_or(ptr::null(), |n| n.as_ptr()),
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        };
        check(code)
    }

    pub fn get(key: &str, name: Option<&str>) -> Option<String> {
        let key = wide(key);
        let name = name.map(wide);
        let mut buf = vec![0u16; 2048];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: `buf` holds `size` bytes; the other pointers are NUL-terminated and live.
        let code = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ref().map_or(ptr::null(), |n| n.as_ptr()),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        (code == 0).then(|| String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]))
    }

    pub fn delete_value(key: &str, name: &str) -> io::Result<()> {
        let (key, name) = (wide(key), wide(name));
        // SAFETY: NUL-terminated buffers that outlive the call.
        check(unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) })
    }

    pub fn delete_tree(key: &str) -> io::Result<()> {
        let key = wide(key);
        // SAFETY: a NUL-terminated buffer that outlives the call.
        check(unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr()) })
    }
}

/// Polls the clipboard sequence number every 500 ms while sync is on, and reads the clipboard
/// only when it changed (DESIGN target: under 500 ms latency). Text goes as text; an image is
/// sent as a PNG. Writes MaxxDrop made itself are skipped, so nothing bounces back.
fn watch_clipboard(node: Shared, ours: Arc<AtomicU32>, scratch: PathBuf) {
    std::thread::spawn(move || {
        let mut seen: Option<u32> = None;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if !node.clipboard_sync() {
                seen = None;
                continue;
            }
            let now = clipboard_seq();
            // The first look after turning sync on is what was already there: don't send it.
            let changed = seen.is_some_and(|s| s != now);
            seen = Some(now);
            if !changed || now == ours.load(Ordering::SeqCst) {
                continue;
            }
            let Ok(mut clipboard) = arboard::Clipboard::new() else {
                continue; // another app holds the clipboard right now
            };
            if let Ok(text) = clipboard.get_text() {
                node.clipboard_changed(text);
            } else if let Ok(image) = clipboard.get_image() {
                let path = scratch.join("clipboard-out.png");
                let saved = image::save_buffer(
                    &path,
                    &image.bytes,
                    image.width as u32,
                    image.height as u32,
                    image::ExtendedColorType::Rgba8,
                );
                if saved.is_ok() {
                    let _ = node.clipboard_image(path.display().to_string());
                }
                let _ = std::fs::remove_file(&path); // the core already opened it
            }
        }
    });
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn tray(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open MaxxDrop", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit MaxxDrop", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut tray = TrayIconBuilder::new()
        .tooltip("MaxxDrop")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn main() {
    let launch = paths_in(std::env::args());
    let hidden = std::env::args().any(|a| a == HIDDEN);
    let told_tray = AtomicBool::new(false);
    tauri::Builder::default()
        // First, so a second launch (Explorer menu, Start menu) hands over to this one and exits.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            let paths = paths_in(args);
            show_main(app);
            if !paths.is_empty() {
                let _ = app.emit("staged", paths);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(Launch(Mutex::new(launch)))
        .setup(move |app| {
            let ours = Arc::new(AtomicU32::new(0));
            let data = config::data_dir();
            let node = Node::new(
                data.display().to_string(),
                config::downloads_dir().display().to_string(),
                None,
                Arc::new(Events {
                    app: app.handle().clone(),
                    peers: Mutex::default(),
                    ours: ours.clone(),
                }),
            )?;
            watch_clipboard(node.clone(), ours, data);
            app.manage(node);
            refresh_registered_paths();
            tray(app)?;
            if !hidden {
                show_main(app.handle());
            }
            Ok(())
        })
        // Closing the window keeps MaxxDrop in the tray, so phones can still send to this PC.
        .on_window_event(move |window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                if !told_tray.swap(true, Ordering::SeqCst) {
                    let _ = window
                        .app_handle()
                        .notification()
                        .builder()
                        .title("MaxxDrop is still running")
                        .body("It keeps receiving in the tray. Right-click the tray icon to quit.")
                        .show();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            me,
            peers,
            trusted,
            pair,
            unpair,
            rename_device,
            set_auto_accept,
            refresh,
            respond,
            send,
            send_text,
            pause,
            resume,
            history,
            clear_history,
            cancel,
            start_pairing,
            stop_pairing,
            clipboard_sync,
            set_clipboard_sync,
            set_downloads,
            open_downloads,
            reveal,
            open_link,
            autostart,
            set_autostart,
            explorer_menu,
            set_explorer_menu,
            take_launch_paths
        ])
        .run(tauri::generate_context!())
        .expect("run MaxxDrop");
}
