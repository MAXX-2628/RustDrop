//! File transfer engine (ARCHITECTURE.md §7, SECURITY.md §7-9).
//!
//! Receiver side: validate the request, check free space, ask the user (bound to the transfer
//! ID), then take chunks over several connections into a staging folder inside the
//! destination. Every file is re-read and checked against the sender's SHA-256 before it is
//! moved into place, and nothing existing is overwritten. A transfer that breaks (rather than
//! being cancelled or failing its check) keeps its staging folder, so sending the same files
//! again continues where it stopped.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, ensure, Context, Result};
use ring::digest;
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
    time::timeout,
};
use uuid::Uuid;

use crate::{
    disk,
    identity::Identity,
    protocol::{self, DeviceInfo, FileMeta, Message},
    security::{hex, TrustStore},
    server::{self, IDLE_TIMEOUT},
};

pub const MAX_FILES: usize = 1000;
pub const MAX_TOTAL: u64 = 256 << 30; // 256 GiB
const MAX_DEPTH: usize = 32;
const MAX_PATH: usize = 512;
const DECISION_TIMEOUT: Duration = Duration::from_secs(60);
const PROGRESS_EVERY: Duration = Duration::from_millis(200);
/// Bytes per chunk: the unit that is sent in parallel, acknowledged, and kept for resume.
pub const CHUNK: u64 = 1 << 20;
/// Data connections a sender opens for one transfer.
pub const MAX_STREAMS: usize = 4;
/// Data connections a receiver allows at once per transfer (room for reconnects).
const MAX_JOINED: usize = 2 * MAX_STREAMS;
/// Times a sender's data connection may fail before it gives up.
const STREAM_RETRIES: u32 = 3;
/// The receiver gives up when no chunk arrives for this long.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// How often a receiver saves which chunks it has, for resume.
const CHECKPOINT_EVERY: Duration = Duration::from_secs(5);
/// Staging folders of broken transfers older than this are deleted.
const PARTIAL_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
/// Free space a receiver keeps after accepting, so the device does not fill up completely.
const SPACE_MARGIN: u64 = 64 << 20;
/// Slowest verification speed assumed when waiting for the receiver's verdict.
const VERIFY_BYTES_PER_SEC: u64 = 20 << 20;

/// An incoming request waiting for the user. Send `true` on `decision` to accept;
/// dropping it or not answering within 60 s declines.
#[derive(Debug)]
pub struct Incoming {
    pub transfer_id: Uuid,
    pub peer: DeviceInfo,
    /// Label from the local trust store, the only name proven by the TLS handshake.
    pub trusted_name: String,
    pub files: Vec<FileMeta>,
    pub decision: oneshot::Sender<bool>,
}

impl Incoming {
    pub fn total(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

#[derive(Debug)]
pub enum Event {
    Incoming(Incoming),
    Progress {
        transfer_id: Uuid,
        done: u64,
        total: u64,
    },
    Completed {
        transfer_id: Uuid,
        saved: Vec<PathBuf>,
    },
    Failed {
        transfer_id: Uuid,
        reason: String,
    },
    /// A device completed QR pairing with this one; `name` is its new trust-store label.
    Paired {
        name: String,
    },
    /// Clipboard text from the paired device `from` (contents are never logged).
    Clipboard {
        text: String,
        from: String,
    },
    /// The transfer stopped because someone cancelled it. `by_peer`: the sender did, not this device.
    Cancelled {
        transfer_id: Uuid,
        by_peer: bool,
    },
}

/// Receiver state shared by all sessions.
pub struct Inbox {
    dest: PathBuf,
    events: mpsc::Sender<Event>,
    used_ids: Mutex<HashSet<Uuid>>,
    cancels: Mutex<HashMap<Uuid, watch::Sender<bool>>>,
    /// Accepted transfers that data connections may join.
    active: Mutex<HashMap<Uuid, Arc<Active>>>,
    /// Staging folders in use, so two transfers of the same files never share one.
    busy: Mutex<HashSet<String>>,
    /// Test hook: drop the next data connection after reading a chunk, before acknowledging it.
    #[cfg(test)]
    pub(crate) drop_next_stream: AtomicBool,
}

impl Inbox {
    pub fn new(dest: PathBuf, events: mpsc::Sender<Event>) -> Self {
        Self {
            dest,
            events,
            used_ids: Mutex::new(HashSet::new()),
            cancels: Mutex::default(),
            active: Mutex::default(),
            busy: Mutex::default(),
            #[cfg(test)]
            drop_next_stream: AtomicBool::new(false),
        }
    }

    pub(crate) async fn notify(&self, event: Event) {
        let _ = self.events.send(event).await;
    }

    /// Cancels an incoming transfer (or declines it if it is still waiting). False if unknown.
    pub fn cancel(&self, transfer_id: Uuid) -> bool {
        let cancels = self.cancels.lock().unwrap();
        cancels
            .get(&transfer_id)
            .is_some_and(|tx| tx.send(true).is_ok())
    }
}

/// Marks a transfer stopped by a cancel (not a failure), so callers can report it as such.
#[derive(Debug)]
pub(crate) struct Cancelled {
    pub by_peer: bool,
}

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.by_peer {
            "cancelled by the other device"
        } else {
            "cancelled"
        })
    }
}

impl std::error::Error for Cancelled {}

/// Resolves once `rx` is set to true. A dropped sender never cancels.
async fn cancelled(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Transfer states shown by every UI (ARCHITECTURE.md §11).
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TransferState {
    WaitingForAccept,
    Transferring,
    Verifying,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Completed,
    /// `reason` is set when the receiver declined automatically (for example, no free space).
    Declined {
        reason: Option<String>,
    },
    /// Someone cancelled after accepting; `by_peer`: the receiver did.
    Cancelled {
        by_peer: bool,
    },
}

/// Rate-limits progress reports so UIs are not flooded (DESIGN.md §12).
struct Progress {
    last: Option<Instant>,
}

impl Progress {
    /// True when a report is due: the first one at once, then every 200 ms, and the last one.
    fn due(&mut self, done: u64, total: u64) -> bool {
        let due = done == total || self.last.is_none_or(|t| t.elapsed() >= PROGRESS_EVERY);
        if due {
            self.last = Some(Instant::now());
        }
        due
    }
}

/// Turns a remote `/`-separated relative path into a safe relative `PathBuf`, or rejects it.
pub fn safe_relative(rel: &str) -> Result<PathBuf> {
    ensure!(
        !rel.is_empty() && rel.len() <= MAX_PATH,
        "invalid path length"
    );
    let parts: Vec<&str> = rel.split('/').collect();
    ensure!(parts.len() <= MAX_DEPTH, "path too deep");
    let mut out = PathBuf::new();
    for part in parts {
        ensure!(valid_component(part), "invalid path component {part:?}");
        out.push(part);
    }
    Ok(out)
}

/// Rejects empty, `.`/`..`, separators, drive/stream colons, control characters
/// (incl. NUL), Windows-reserved names and trailing dots/spaces, on every platform.
fn valid_component(c: &str) -> bool {
    if c.is_empty() || c == "." || c == ".." || c.len() > 255 {
        return false;
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return false;
    }
    if c.chars()
        .any(|ch| ch.is_control() || r#"<>:"/\|?*"#.contains(ch))
    {
        return false;
    }
    let stem = c
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    !device
}

/// Checks counts, sizes, paths and duplicates. Returns the safe paths and total size.
fn validate(files: &[FileMeta]) -> Result<(Vec<PathBuf>, u64)> {
    ensure!(
        !files.is_empty() && files.len() <= MAX_FILES,
        "invalid file count {}",
        files.len()
    );
    let mut seen = HashSet::new();
    let mut total = 0u64;
    let mut paths = Vec::with_capacity(files.len());
    for f in files {
        // Case-insensitive: Windows and Android storage treat a.txt and A.TXT as one file.
        ensure!(
            seen.insert(f.path.to_lowercase()),
            "duplicate path {:?}",
            f.path
        );
        total = total
            .checked_add(f.size)
            .filter(|t| *t <= MAX_TOTAL)
            .context("transfer too large")?;
        paths.push(safe_relative(&f.path)?);
    }
    Ok((paths, total))
}

/// Where each chunk lives: the files in request order, each cut into `CHUNK`-sized pieces.
/// Empty files have no chunks.
struct Layout {
    sizes: Vec<u64>,
    /// Index of each file's first chunk.
    first: Vec<u32>,
    count: u32,
}

impl Layout {
    fn new(sizes: &[u64]) -> Self {
        let mut first = Vec::with_capacity(sizes.len());
        let mut n = 0u64;
        for s in sizes {
            first.push(n as u32); // MAX_TOTAL / CHUNK + MAX_FILES fits easily in u32
            n += s.div_ceil(CHUNK);
        }
        Self {
            sizes: sizes.into(),
            first,
            count: n as u32,
        }
    }

    /// File index, byte offset and length of chunk `i`; None if there is no such chunk.
    fn locate(&self, i: u32) -> Option<(usize, u64, usize)> {
        if i >= self.count {
            return None;
        }
        // The last file starting at or before `i`; empty files share their start with the next one.
        let f = self.first.partition_point(|&s| s <= i) - 1;
        let offset = u64::from(i - self.first[f]) * CHUNK;
        let len = (self.sizes[f] - offset).min(CHUNK) as usize;
        Some((f, offset, len))
    }

    fn bitset_len(&self) -> usize {
        (self.count as usize).div_ceil(8)
    }

    /// Bytes covered by the chunks set in `have`.
    fn bytes_in(&self, have: &[u8]) -> u64 {
        (0..self.count)
            .filter(|&i| bit(have, i))
            .filter_map(|i| self.locate(i))
            .map(|(_, _, len)| len as u64)
            .sum()
    }
}

fn bit(set: &[u8], i: u32) -> bool {
    set[(i / 8) as usize] & (1 << (i % 8)) != 0
}

/// Sets bit `i`; true if it was not set before.
fn set_bit(set: &mut [u8], i: u32) -> bool {
    let (byte, mask) = ((i / 8) as usize, 1u8 << (i % 8));
    let new = set[byte] & mask == 0;
    set[byte] |= mask;
    new
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// "3.2 GB": sizes in messages people read.
fn human(bytes: u64) -> String {
    let units = ["bytes", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < units.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

/// SHA-256 of the first `size` bytes of `file`, read with positional reads (safe on a shared handle).
fn hash_file(file: &File, size: u64, stop: &AtomicBool) -> io::Result<String> {
    let mut hash = digest::Context::new(&digest::SHA256);
    let mut buf = vec![0u8; CHUNK as usize];
    let mut offset = 0;
    while offset < size {
        if stop.load(Ordering::Relaxed) {
            return Err(io::Error::other("stopped"));
        }
        let n = (size - offset).min(CHUNK) as usize;
        disk::read_exact_at(file, &mut buf[..n], offset)?;
        hash.update(&buf[..n]);
        offset += n as u64;
    }
    Ok(hex(hash.finish().as_ref()))
}

/// Names the staging folder after the sender and the exact files, so a re-send finds it again.
fn resume_key(fingerprint: &str, files: &[FileMeta]) -> String {
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(fingerprint.as_bytes());
    for f in files {
        ctx.update(format!("\n{}\n{}\n{}", f.path, f.size, f.modified).as_bytes());
    }
    hex(&ctx.finish().as_ref()[..16])
}

/// An accepted incoming transfer that data connections fill in.
struct Active {
    fingerprint: String,
    token: String,
    layout: Layout,
    files: Mutex<Vec<Arc<File>>>,
    /// Files written since the last checkpoint (synced before the chunk list is saved).
    dirty: Vec<AtomicBool>,
    have: Mutex<Vec<u8>>,
    done: AtomicU64,
    total: u64,
    joined: AtomicUsize,
    stop: watch::Sender<bool>,
    last_data: Mutex<Instant>,
    progress: Mutex<Progress>,
}

impl Active {
    fn complete(&self) -> bool {
        let have = self.have.lock().unwrap();
        (0..self.layout.count).all(|i| bit(&have, i))
    }

    fn file(&self, i: usize) -> Result<Arc<File>> {
        self.files
            .lock()
            .unwrap()
            .get(i)
            .cloned()
            .context("transfer already finished")
    }
}

/// Opens (or reopens, when resuming) one staging file per entry, each at its final size.
/// Returns the files and whether they all already existed at the right size.
fn open_staging(dir: &Path, paths: &[PathBuf], sizes: &[u64]) -> io::Result<(Vec<File>, bool)> {
    let mut intact = true;
    let mut files = Vec::with_capacity(paths.len());
    for (rel, &size) in paths.iter().zip(sizes) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap_or(dir))?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        if file.metadata()?.len() != size {
            intact = false;
            file.set_len(size)?;
        }
        files.push(file);
    }
    Ok((files, intact))
}

/// Syncs written data, then records which chunks are on disk, so a crash never claims more.
fn checkpoint(active: &Active, list: &Path) -> io::Result<()> {
    let files = active.files.lock().unwrap().clone();
    for (file, dirty) in files.iter().zip(&active.dirty) {
        if dirty.swap(false, Ordering::Relaxed) {
            file.sync_data()?;
        }
    }
    let have = active.have.lock().unwrap().clone();
    disk::write_atomic(list, &have)
}

/// Deletes staging folders of broken transfers nobody resumed for a week.
async fn clean_stale(dest: &Path, busy: &HashSet<String>) {
    let Ok(mut dir) = fs::read_dir(dest).await else {
        return;
    };
    while let Ok(Some(entry)) = dir.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(key) = name.strip_prefix(".rustdrop-") else {
            continue;
        };
        let old = entry
            .metadata()
            .await
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().unwrap_or_default() > PARTIAL_MAX_AGE);
        if old && !busy.contains(key) {
            let _ = fs::remove_dir_all(entry.path()).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn receive<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    peer: &DeviceInfo,
    fingerprint: &str,
    trusted_name: &str,
    transfer_id: Uuid,
    files: Vec<FileMeta>,
    inbox: &Inbox,
) -> Result<()> {
    let checked = validate(&files).and_then(|v| {
        ensure!(
            inbox.used_ids.lock().unwrap().insert(transfer_id),
            "transfer id reused"
        );
        Ok(v)
    });
    let (paths, total) = match checked {
        Ok(v) => v,
        Err(e) => {
            let reply = Message::Decline {
                transfer_id,
                reason: None,
            };
            protocol::write(stream, &reply).await?;
            return Err(e.context("rejected transfer request"));
        }
    };

    // Resume needs a staging folder of its own; a second copy of the same files starts fresh.
    let key = resume_key(fingerprint, &files);
    let resumable = inbox.busy.lock().unwrap().insert(key.clone());
    let busy = inbox.busy.lock().unwrap().clone();
    clean_stale(&inbox.dest, &busy).await;
    let (cancel_tx, mut cancel) = watch::channel(false);
    inbox.cancels.lock().unwrap().insert(transfer_id, cancel_tx);
    let job = Job {
        peer,
        fingerprint,
        trusted_name,
        transfer_id,
        paths,
        sizes: files.iter().map(|f| f.size).collect(),
        total,
        staging: inbox.dest.join(format!(
            ".rustdrop-{}",
            if resumable {
                key.clone()
            } else {
                transfer_id.to_string()
            }
        )),
        resumable,
    };
    let result = run_receive(stream, &job, files, &mut cancel, inbox).await;
    inbox.cancels.lock().unwrap().remove(&transfer_id);
    if resumable {
        inbox.busy.lock().unwrap().remove(&key);
    }
    result
}

/// What the receiver knows about one incoming transfer.
struct Job<'a> {
    peer: &'a DeviceInfo,
    fingerprint: &'a str,
    trusted_name: &'a str,
    transfer_id: Uuid,
    paths: Vec<PathBuf>,
    sizes: Vec<u64>,
    total: u64,
    staging: PathBuf,
    resumable: bool,
}

impl Job<'_> {
    fn chunk_list(&self) -> PathBuf {
        self.staging.join("chunks")
    }

    fn files_dir(&self) -> PathBuf {
        self.staging.join("files")
    }
}

async fn run_receive<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    job: &Job<'_>,
    files: Vec<FileMeta>,
    cancel: &mut watch::Receiver<bool>,
    inbox: &Inbox,
) -> Result<()> {
    let transfer_id = job.transfer_id;
    let layout = Layout::new(&job.sizes);
    let mut have = match fs::read(job.chunk_list()).await {
        Ok(saved) if job.resumable && saved.len() == layout.bitset_len() => saved,
        _ => vec![0u8; layout.bitset_len()],
    };

    // Not enough room: decline before bothering the user, and say why on both devices.
    fs::create_dir_all(&inbox.dest).await?;
    let mut need = job.total - layout.bytes_in(&have);
    if cfg!(target_os = "android") {
        // The app then copies each file into Downloads on the same storage before deleting its own.
        need += job.sizes.iter().max().copied().unwrap_or(0);
    }
    let free = disk::available_space(&inbox.dest).unwrap_or(u64::MAX);
    if free < need.saturating_add(SPACE_MARGIN) {
        let (need, free) = (human(need), human(free.saturating_sub(SPACE_MARGIN)));
        let reply = Message::Decline {
            transfer_id,
            reason: Some(format!(
                "The other device doesn't have enough free space ({need} needed, {free} free)."
            )),
        };
        protocol::write(stream, &reply).await?;
        let reason = format!(
            "{} tried to send {need}, but only {free} is free. Free up space and ask them to send again.",
            job.trusted_name
        );
        inbox
            .notify(Event::Failed {
                transfer_id,
                reason,
            })
            .await;
        return Ok(());
    }

    let (decision, answer) = oneshot::channel();
    let request = Incoming {
        transfer_id,
        peer: job.peer.clone(),
        trusted_name: job.trusted_name.into(),
        files,
        decision,
    };
    let _ = inbox.events.send(Event::Incoming(request)).await;
    // While the user decides, the sender may cancel (or disconnect); cancelling here = declining.
    let accepted = tokio::select! {
        r = timeout(DECISION_TIMEOUT, answer) => matches!(r, Ok(Ok(true))),
        _ = cancelled(cancel) => false,
        _ = protocol::read(stream) => {
            inbox.notify(Event::Cancelled { transfer_id, by_peer: true }).await;
            return Err(Cancelled { by_peer: true }.into());
        }
    };
    if !accepted {
        let reply = Message::Decline {
            transfer_id,
            reason: None,
        };
        protocol::write(stream, &reply).await?;
        return Ok(());
    }

    let (dir, paths, sizes) = (job.files_dir(), job.paths.clone(), job.sizes.clone());
    let staged = tokio::task::spawn_blocking(move || open_staging(&dir, &paths, &sizes))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r.map_err(Into::into));
    let (opened, intact) = match staged {
        Ok(v) => v,
        Err(e) => {
            let reply = Message::Decline {
                transfer_id,
                reason: Some("The other device couldn't prepare to save the files.".into()),
            };
            let _ = protocol::write(stream, &reply).await;
            let reason = format!("Couldn't prepare to save the files: {e:#}");
            inbox
                .notify(Event::Failed {
                    transfer_id,
                    reason,
                })
                .await;
            return Err(e);
        }
    };
    if !intact {
        have.fill(0); // staged files are missing or the wrong size: start over
    }
    let mut token = [0u8; 16];
    ::ring::rand::SecureRandom::fill(&::ring::rand::SystemRandom::new(), &mut token)
        .map_err(|_| anyhow!("no secure random source"))?;
    let done = layout.bytes_in(&have);
    let accept = Message::Accept {
        transfer_id,
        token: hex(&token),
        have: hex(&have),
    };
    let active = Arc::new(Active {
        fingerprint: job.fingerprint.into(),
        token: hex(&token),
        dirty: opened.iter().map(|_| AtomicBool::new(false)).collect(),
        files: Mutex::new(opened.into_iter().map(Arc::new).collect()),
        layout,
        have: Mutex::new(have),
        done: AtomicU64::new(done),
        total: job.total,
        joined: AtomicUsize::new(0),
        stop: watch::channel(false).0,
        last_data: Mutex::new(Instant::now()),
        progress: Mutex::new(Progress { last: None }),
    });
    inbox
        .active
        .lock()
        .unwrap()
        .insert(transfer_id, active.clone());
    let result = async {
        protocol::write(stream, &accept).await?;
        let claimed = wait_for_data(stream, job, &active, cancel).await?;
        finish(job, &active, claimed, inbox).await
    }
    .await;
    // Data connections stop and release the files.
    inbox.active.lock().unwrap().remove(&transfer_id);
    active.stop.send_replace(true);

    match result {
        Ok((saved, sha256)) => {
            remove_staging(&job.staging).await;
            let reply = Message::TransferComplete {
                transfer_id,
                sha256,
            };
            protocol::write(stream, &reply).await?;
            inbox.notify(Event::Completed { transfer_id, saved }).await;
            Ok(())
        }
        Err(e) if e.is::<Cancelled>() => {
            active.files.lock().unwrap().clear();
            remove_staging(&job.staging).await;
            let by_peer = e.downcast_ref::<Cancelled>().is_some_and(|c| c.by_peer);
            if !by_peer {
                // Tell the sender, which reads the control connection while it streams.
                let reply = Message::Cancel { transfer_id };
                let _ = timeout(Duration::from_secs(2), protocol::write(stream, &reply)).await;
            }
            inbox
                .notify(Event::Cancelled {
                    transfer_id,
                    by_peer,
                })
                .await;
            Err(e)
        }
        Err(e) => {
            let corrupt = e.is::<Corrupt>();
            if job.resumable && !corrupt {
                // Keep what arrived, so sending the same files again continues from here.
                let (active, list) = (active.clone(), job.chunk_list());
                let _ = tokio::task::spawn_blocking(move || checkpoint(&active, &list)).await;
            }
            active.files.lock().unwrap().clear();
            if !job.resumable || corrupt {
                remove_staging(&job.staging).await;
            }
            // Generic reason on the wire: local paths and errors stay on this device.
            let reply = Message::TransferFailed {
                transfer_id,
                reason: "receiver could not complete the transfer".into(),
            };
            let _ = timeout(Duration::from_secs(2), protocol::write(stream, &reply)).await;
            let reason = if corrupt {
                format!("{e:#}")
            } else {
                format!("{e:#}. Send the files again to continue where it stopped.")
            };
            inbox
                .notify(Event::Failed {
                    transfer_id,
                    reason,
                })
                .await;
            Err(e)
        }
    }
}

/// Deletes a staging folder. A data connection may still hold a file open for a moment
/// (Windows refuses to delete open files), so it retries briefly.
async fn remove_staging(dir: &Path) {
    for _ in 0..20 {
        match fs::remove_dir_all(dir).await {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                tokio::time::sleep(Duration::from_millis(100)).await
            }
            _ => return,
        }
    }
}

/// The received data does not match the sender's hashes. Never resumed.
#[derive(Debug)]
struct Corrupt;

impl std::fmt::Display for Corrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("integrity check failed: the received files were discarded")
    }
}

impl std::error::Error for Corrupt {}

/// Waits on the control connection while data connections deliver chunks. Returns the
/// sender's hashes once it says it is done.
async fn wait_for_data<S: AsyncRead + Unpin>(
    stream: &mut S,
    job: &Job<'_>,
    active: &Arc<Active>,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Vec<String>> {
    // One read stays pending throughout (a dropped half-read message would corrupt the stream).
    let mut msg = Box::pin(protocol::read(stream));
    let mut tick = tokio::time::interval(CHECKPOINT_EVERY);
    tick.tick().await;
    loop {
        tokio::select! {
            biased;
            _ = cancelled(cancel) => return Err(Cancelled { by_peer: false }.into()),
            m = &mut msg => {
                return match m.context("connection to the sender was lost")? {
                    Message::TransferComplete { transfer_id: id, sha256 } if id == job.transfer_id => {
                        Ok(sha256)
                    }
                    Message::Cancel { .. } => Err(Cancelled { by_peer: true }.into()),
                    other => bail!("expected TRANSFER_COMPLETE, got {other:?}"),
                };
            }
            _ = tick.tick() => {
                if job.resumable {
                    let (active, list) = (active.clone(), job.chunk_list());
                    tokio::task::spawn_blocking(move || checkpoint(&active, &list)).await??;
                }
                // The sender may still be hashing once every chunk is in; give it longer then.
                let quiet = active.last_data.lock().unwrap().elapsed();
                let limit = if active.complete() { 10 * STALL_TIMEOUT } else { STALL_TIMEOUT };
                ensure!(quiet < limit, "the sender stopped sending");
            }
        }
    }
}

/// Checks every chunk arrived, re-reads each file against the sender's SHA-256, then moves
/// the files into place. Returns where they were saved and their hashes.
async fn finish(
    job: &Job<'_>,
    active: &Arc<Active>,
    claimed: Vec<String>,
    inbox: &Inbox,
) -> Result<(Vec<PathBuf>, Vec<String>)> {
    ensure!(
        active.complete(),
        "the sender finished before sending everything"
    );
    ensure!(claimed.len() == job.paths.len(), Corrupt);
    let _ = inbox.events.try_send(Event::Progress {
        transfer_id: job.transfer_id,
        done: job.total,
        total: job.total,
    });
    let files = active.files.lock().unwrap().clone();
    let sizes = job.sizes.clone();
    let hashes = tokio::task::spawn_blocking(move || -> io::Result<Vec<String>> {
        let never = AtomicBool::new(false);
        files
            .iter()
            .zip(sizes)
            .map(|(file, size)| {
                file.sync_all()?;
                hash_file(file, size, &never)
            })
            .collect()
    })
    .await??;
    ensure!(claimed == hashes, Corrupt);

    // Close the staged files (Windows cannot rename open ones), then move them into place.
    active.files.lock().unwrap().clear();
    let staged = job.files_dir();
    let mut saved = Vec::with_capacity(job.paths.len());
    for rel in &job.paths {
        let target = unique(&inbox.dest.join(rel)).await?;
        fs::create_dir_all(target.parent().unwrap_or(&inbox.dest)).await?;
        fs::rename(staged.join(rel), &target).await?;
        saved.push(target);
    }
    Ok((saved, hashes))
}

/// Receiver side of one data connection: writes each chunk where it belongs and acknowledges it.
pub(crate) async fn receive_stream<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    fingerprint: &str,
    transfer_id: Uuid,
    token: &str,
    inbox: &Inbox,
) -> Result<()> {
    let active = inbox
        .active
        .lock()
        .unwrap()
        .get(&transfer_id)
        .cloned()
        .context("data connection for an unknown transfer")?;
    #[allow(deprecated)] // still the constant-time compare ring ships in 0.17
    let token_ok =
        ::ring::constant_time::verify_slices_are_equal(active.token.as_bytes(), token.as_bytes())
            .is_ok();
    // Only the device that made the request may send its data.
    ensure!(
        token_ok && active.fingerprint == fingerprint,
        "data connection not allowed for this transfer"
    );
    let joined = active.joined.fetch_add(1, Ordering::SeqCst);
    let result = async {
        ensure!(joined < MAX_JOINED, "too many data connections");
        let mut stop = active.stop.subscribe();
        let mut buf = vec![0u8; CHUNK as usize];
        loop {
            let mut header = [0u8; 8];
            let read = tokio::select! {
                biased;
                _ = cancelled(&mut stop) => return Ok(()),
                r = timeout(IDLE_TIMEOUT, stream.read_exact(&mut header)) => r?,
            };
            match read {
                Ok(_) => {}
                // The sender closes a data connection when it has nothing left to send.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e.into()),
            }
            let index = u32::from_be_bytes(header[..4].try_into().unwrap());
            let len = u32::from_be_bytes(header[4..].try_into().unwrap()) as usize;
            let (f, offset, want) = active.layout.locate(index).context("bad chunk index")?;
            ensure!(len == want, "bad chunk length {len} for chunk {index}");
            timeout(IDLE_TIMEOUT, stream.read_exact(&mut buf[..len])).await??;
            #[cfg(test)]
            if inbox.drop_next_stream.swap(false, Ordering::SeqCst) {
                bail!("test: connection dropped");
            }

            let file = active.file(f)?;
            let data = std::mem::take(&mut buf);
            buf = tokio::task::spawn_blocking(move || {
                disk::write_all_at(&file, &data[..len], offset).map(|_| data)
            })
            .await??;
            active.dirty[f].store(true, Ordering::Relaxed);
            *active.last_data.lock().unwrap() = Instant::now();
            if set_bit(&mut active.have.lock().unwrap(), index) {
                let done = active.done.fetch_add(len as u64, Ordering::SeqCst) + len as u64;
                if active.progress.lock().unwrap().due(done, active.total) {
                    let _ = inbox.events.try_send(Event::Progress {
                        transfer_id,
                        done,
                        total: active.total,
                    });
                }
            }
            stream.write_all(&index.to_be_bytes()).await?;
            stream.flush().await?;
        }
    }
    .await;
    active.joined.fetch_sub(1, Ordering::SeqCst);
    result
}

/// `name.ext`, else `name (1).ext`, `name (2).ext`, ... — never an existing file.
async fn unique(path: &Path) -> Result<PathBuf> {
    // shortcut: exists-then-rename can race with another local writer; fine for a single-user downloads folder.
    if !fs::try_exists(path).await? {
        return Ok(path.into());
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 1..10_000 {
        let candidate = path.with_file_name(format!("{stem} ({n}){ext}"));
        if !fs::try_exists(&candidate).await? {
            return Ok(candidate);
        }
    }
    bail!("too many files with the same name")
}

/// Something to send.
pub enum Source {
    /// A file or folder on disk. Folders keep their structure.
    Path(PathBuf),
    /// A file the platform already opened (Android content URIs), sent under `name`,
    /// which may contain `/` for files inside a folder.
    Open { name: String, file: File },
}

/// Where a file's bytes are read from.
enum Reader {
    Path(PathBuf),
    Open(Arc<File>),
}

impl Reader {
    fn open(&self) -> io::Result<Arc<File>> {
        match self {
            Self::Path(p) => File::open(p).map(Arc::new),
            Self::Open(f) => Ok(f.clone()),
        }
    }
}

fn modified(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// Expands files and folders into what to read and the metadata to announce. Symlinks and
/// special files are skipped, never followed (SECURITY.md §9).
fn collect(sources: Vec<Source>) -> Result<Vec<(Reader, FileMeta)>> {
    let mut out = Vec::new();
    for s in sources {
        match s {
            Source::Path(p) => {
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .with_context(|| format!("unsupported file name: {}", p.display()))?
                    .to_string();
                add(&p, name, &mut out)?;
            }
            Source::Open { name, file } => {
                let meta = file.metadata()?;
                ensure!(meta.is_file(), "{name} can't be read as a file");
                ensure!(out.len() < MAX_FILES, "more than {MAX_FILES} files");
                let info = FileMeta {
                    path: name,
                    size: meta.len(),
                    modified: modified(&meta),
                };
                out.push((Reader::Open(Arc::new(file)), info));
            }
        }
    }
    Ok(out)
}

fn add(path: &Path, rel: String, out: &mut Vec<(Reader, FileMeta)>) -> Result<()> {
    ensure!(
        rel.matches('/').count() < MAX_DEPTH,
        "folder too deep: {rel}"
    );
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_file() {
        ensure!(out.len() < MAX_FILES, "more than {MAX_FILES} files");
        let info = FileMeta {
            path: rel,
            size: meta.len(),
            modified: modified(&meta),
        };
        out.push((Reader::Path(path.into()), info));
    } else if meta.is_dir() {
        let mut entries = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e
                .file_name()
                .into_string()
                .map_err(|n| anyhow!("unsupported file name: {n:?}"))?;
            add(&e.path(), format!("{rel}/{name}"), out)?;
        }
    }
    Ok(())
}

/// Shared by the sender's data connections.
struct Outbound {
    transfer_id: Uuid,
    token: String,
    layout: Layout,
    readers: Vec<Reader>,
    names: Vec<String>,
    queue: Mutex<VecDeque<u32>>,
    done: AtomicU64,
    acked: AtomicU32,
}

/// Sends files/folders to a trusted peer over up to `MAX_STREAMS` connections.
/// `progress(done, total)` is called at most every 200 ms. Setting `cancel` to true stops the
/// transfer cleanly: the receiver is told and discards what it staged. If the network breaks,
/// the receiver keeps what arrived, and sending the same files again continues from there.
pub async fn send(
    addr: std::net::SocketAddr,
    identity: Arc<Identity>,
    trust: Arc<TrustStore>,
    sources: Vec<Source>,
    mut cancel: watch::Receiver<bool>,
    mut progress: impl FnMut(u64, u64),
) -> Result<Outcome> {
    let (readers, files): (Vec<Reader>, Vec<FileMeta>) =
        tokio::task::spawn_blocking(move || collect(sources))
            .await??
            .into_iter()
            .unzip();
    let (_, total) = validate(&files)?; // same rules as the receiver: fail before connecting
    let layout = Layout::new(&files.iter().map(|f| f.size).collect::<Vec<_>>());

    let (tls, _peer) = server::connect(addr, &identity, trust.clone()).await?;
    let (mut rd, mut wr) = tokio::io::split(tls);
    let transfer_id = Uuid::new_v4();
    let request = Message::SendRequest {
        transfer_id,
        files: files.clone(),
    };
    protocol::write(&mut wr, &request).await?;
    let reply = tokio::select! {
        r = timeout(DECISION_TIMEOUT + IDLE_TIMEOUT, protocol::read(&mut rd)) => r??,
        _ = cancelled(&mut cancel) => {
            let _ = protocol::write(&mut wr, &Message::Cancel { transfer_id }).await;
            return Ok(Outcome::Cancelled { by_peer: false });
        }
    };
    let (token, have) = match reply {
        Message::Accept {
            transfer_id: id,
            token,
            have,
        } if id == transfer_id => (token, have),
        Message::Decline {
            transfer_id: id,
            reason,
        } if id == transfer_id => {
            // Shown to the user: keep it short and printable.
            let reason = reason.map(|r| r.chars().filter(|c| !c.is_control()).take(200).collect());
            return Ok(Outcome::Declined { reason });
        }
        other => bail!("expected ACCEPT or DECLINE, got {other:?}"),
    };
    let have = unhex(&have)
        .filter(|h| h.len() == layout.bitset_len())
        .unwrap_or_else(|| vec![0u8; layout.bitset_len()]);
    let queue: VecDeque<u32> = (0..layout.count).filter(|&i| !bit(&have, i)).collect();
    let needed = queue.len() as u32;
    let start = layout.bytes_in(&have);

    let out = Arc::new(Outbound {
        transfer_id,
        token,
        names: files.iter().map(|f| f.path.clone()).collect(),
        readers,
        layout,
        queue: Mutex::new(queue),
        done: AtomicU64::new(start),
        acked: AtomicU32::new(0),
    });
    // Hash while the chunks go out; the receiver checks every byte it wrote against these.
    let stop_hashing = Arc::new(AtomicBool::new(false));
    let mut hasher = {
        let (out, stop) = (out.clone(), stop_hashing.clone());
        tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            out.readers
                .iter()
                .zip(&out.layout.sizes)
                .zip(&out.names)
                .map(|((r, &size), name)| {
                    let file = r.open().with_context(|| format!("couldn't open {name}"))?;
                    hash_file(&file, size, &stop).with_context(|| format!("couldn't read {name}"))
                })
                .collect()
        })
    };
    let _stop_on_exit = StopOnDrop(stop_hashing);

    let mut workers = JoinSet::new();
    for _ in 0..MAX_STREAMS.min(needed as usize) {
        let (out, identity, trust) = (out.clone(), identity.clone(), trust.clone());
        workers.spawn(async move { data_worker(addr, &identity, trust, &out).await });
    }

    // One read stays pending for the whole transfer: the receiver's verdict, or its CANCEL.
    let mut reply = Box::pin(protocol::read(&mut rd));
    let mut ticker = tokio::time::interval(PROGRESS_EVERY);
    let mut reported = None;
    let mut last_error = None;
    loop {
        tokio::select! {
            biased;
            msg = &mut reply => return verdict_mid_stream(msg),
            _ = cancelled(&mut cancel) => {
                workers.abort_all();
                drop(reply);
                let _ = protocol::write(&mut wr, &Message::Cancel { transfer_id }).await;
                let _ = wr.shutdown().await;
                return Ok(Outcome::Cancelled { by_peer: false });
            }
            joined = workers.join_next() => match joined {
                None => break,
                Some(Ok(Ok(()))) => {}
                Some(Ok(Err(e))) => last_error = Some(e),
                Some(Err(e)) => last_error = Some(e.into()),
            },
            _ = ticker.tick() => {
                let done = out.done.load(Ordering::SeqCst);
                if reported != Some(done) {
                    reported = Some(done);
                    progress(done, total);
                }
            }
        }
    }
    progress(out.done.load(Ordering::SeqCst), total);
    if out.acked.load(Ordering::SeqCst) < needed {
        // Every data connection gave up. The receiver may have said why (or cancelled).
        if let Ok(msg) = timeout(Duration::from_secs(2), &mut reply).await {
            if matches!(
                msg,
                Ok(Message::Cancel { .. } | Message::TransferFailed { .. })
            ) {
                return verdict_mid_stream(msg);
            }
        }
        let e = last_error.unwrap_or_else(|| anyhow!("connection lost"));
        return Err(e.context("transfer interrupted; send the same files again to resume"));
    }

    let hashes = tokio::select! {
        biased;
        msg = &mut reply => return verdict_mid_stream(msg),
        h = &mut hasher => h??,
    };
    let done = Message::TransferComplete {
        transfer_id,
        sha256: hashes,
    };
    protocol::write(&mut wr, &done).await?;

    // The receiver re-reads everything before answering; allow for a slow disk.
    let verify = IDLE_TIMEOUT + Duration::from_secs(total / VERIFY_BYTES_PER_SEC);
    let result = match timeout(verify, &mut reply).await?? {
        Message::TransferComplete {
            transfer_id: id, ..
        } if id == transfer_id => Ok(Outcome::Completed),
        Message::TransferFailed {
            transfer_id: id,
            reason,
        } if id == transfer_id => Err(anyhow!("transfer failed: {reason}")),
        Message::Cancel { .. } => Ok(Outcome::Cancelled { by_peer: true }),
        other => Err(anyhow!("expected TRANSFER_COMPLETE, got {other:?}")),
    };
    drop(reply);
    let _ = wr.shutdown().await;
    result
}

struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// One data connection, reconnecting a few times if it breaks. Chunks in flight on a broken
/// connection go back on the queue for any connection to send again.
async fn data_worker(
    addr: std::net::SocketAddr,
    identity: &Identity,
    trust: Arc<TrustStore>,
    out: &Outbound,
) -> Result<()> {
    let mut failures = 0;
    loop {
        match stream_chunks(addr, identity, trust.clone(), out).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                failures += 1;
                if failures >= STREAM_RETRIES || out.queue.lock().unwrap().is_empty() {
                    return Err(e);
                }
                tracing::debug!("data connection failed, retrying: {e:#}");
                tokio::time::sleep(Duration::from_secs(failures.into())).await;
            }
        }
    }
}

async fn stream_chunks(
    addr: std::net::SocketAddr,
    identity: &Identity,
    trust: Arc<TrustStore>,
    out: &Outbound,
) -> Result<()> {
    let (mut tls, _) = server::connect(addr, identity, trust).await?;
    let hello = Message::DataHello {
        transfer_id: out.transfer_id,
        token: out.token.clone(),
    };
    protocol::write(&mut tls, &hello).await?;
    let mut buf = vec![0u8; 8 + CHUNK as usize];
    let mut open: Option<(usize, Arc<File>)> = None;
    loop {
        let Some(index) = out.queue.lock().unwrap().pop_front() else {
            let _ = tls.shutdown().await;
            return Ok(());
        };
        let sent = async {
            let (f, offset, len) = out.layout.locate(index).context("bad chunk")?;
            let file = match &open {
                Some((i, file)) if *i == f => file.clone(),
                _ => {
                    let file = out.readers[f]
                        .open()
                        .with_context(|| format!("couldn't open {}", out.names[f]))?;
                    open = Some((f, file.clone()));
                    file
                }
            };
            let mut data = std::mem::take(&mut buf);
            data = tokio::task::spawn_blocking(move || {
                data[..4].copy_from_slice(&index.to_be_bytes());
                data[4..8].copy_from_slice(&(len as u32).to_be_bytes());
                disk::read_exact_at(&file, &mut data[8..8 + len], offset).map(|_| data)
            })
            .await?
            .with_context(|| {
                format!(
                    "{} changed or became unreadable while sending",
                    out.names[f]
                )
            })?;
            buf = data;
            timeout(IDLE_TIMEOUT, tls.write_all(&buf[..8 + len])).await??;
            tls.flush().await?;
            let ack = timeout(IDLE_TIMEOUT, tls.read_u32()).await??;
            ensure!(ack == index, "receiver acknowledged the wrong chunk");
            out.done.fetch_add(len as u64, Ordering::SeqCst);
            out.acked.fetch_add(1, Ordering::SeqCst);
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = sent {
            out.queue.lock().unwrap().push_front(index);
            return Err(e);
        }
    }
}

/// What the receiver said while we were still streaming.
fn verdict_mid_stream(msg: Result<Message>) -> Result<Outcome> {
    match msg? {
        Message::Cancel { .. } => Ok(Outcome::Cancelled { by_peer: true }),
        Message::TransferFailed { reason, .. } => Err(anyhow!("transfer failed: {reason}")),
        other => Err(anyhow!("unexpected {other:?} during transfer")),
    }
}

/// Convenience for callers that send paths.
pub fn paths(paths: &[PathBuf]) -> Vec<Source> {
    paths.iter().cloned().map(Source::Path).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tests::{node, start, temp_dir};

    #[test]
    fn accepts_normal_relative_paths() {
        for ok in [
            "a.txt",
            "folder/sub/b.bin",
            ".hidden",
            "photo (1).jpg",
            "日本語.txt",
        ] {
            assert!(safe_relative(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_hostile_paths() {
        let deep = vec!["d"; MAX_DEPTH + 1].join("/");
        for bad in [
            "",
            "../x",
            "a/../../x",
            "/etc/passwd",
            "C:/Windows/x",
            "C:x",
            r"\\server\share",
            r"a\..\b",
            "a//b",
            "a/./b",
            "x\0y",
            "trail.",
            "trail ",
            "CON",
            "con.txt",
            "LPT1.log",
            "file.txt:stream",
            &deep,
        ] {
            assert!(safe_relative(bad).is_err(), "{bad:?}");
        }
    }

    fn meta(p: &str, size: u64) -> FileMeta {
        FileMeta {
            path: p.into(),
            size,
            modified: 0,
        }
    }

    #[test]
    fn rejects_duplicates_and_oversize() {
        assert!(validate(&[meta("a.txt", 1), meta("A.TXT", 1)]).is_err());
        assert!(validate(&[meta("a", u64::MAX), meta("b", 1)]).is_err());
        assert!(validate(&[]).is_err());
    }

    #[test]
    fn layout_maps_every_byte_once() {
        let sizes = [0, CHUNK * 2 + 5, 0, 7, CHUNK];
        let l = Layout::new(&sizes);
        assert_eq!(l.count, 3 + 1 + 1);
        let mut covered = vec![0u64; sizes.len()];
        for i in 0..l.count {
            let (f, offset, len) = l.locate(i).unwrap();
            assert_eq!(offset, covered[f], "chunks of a file are contiguous");
            covered[f] += len as u64;
        }
        assert_eq!(covered, sizes);
        assert!(l.locate(l.count).is_none());

        let mut have = vec![0u8; l.bitset_len()];
        assert!(set_bit(&mut have, 2) && !set_bit(&mut have, 2));
        assert_eq!(l.bytes_in(&have), 5);
        assert_eq!(unhex(&hex(&have)).unwrap(), have);
        assert!(unhex("abc").is_none() && unhex("zz").is_none());
    }

    type Peer = (Arc<Identity>, Arc<TrustStore>);

    fn trusted_pair() -> (Peer, Peer) {
        let (a, b) = (node("a"), node("b"));
        a.1.add(&b.0.fingerprint(), "b").unwrap();
        b.1.add(&a.0.fingerprint(), "a").unwrap();
        (a, b)
    }

    fn never() -> watch::Receiver<bool> {
        watch::channel(false).1
    }

    /// Bytes that differ at every position, so a chunk written in the wrong place is caught.
    fn pattern(len: usize) -> Vec<u8> {
        (0..len as u64)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect()
    }

    fn big_file(tag: &str, mb: usize) -> PathBuf {
        let src = temp_dir(tag);
        std::fs::create_dir_all(&src).unwrap();
        let path = src.join("big.bin");
        std::fs::write(&path, pattern(mb << 20)).unwrap();
        path
    }

    fn staging_is_gone(dest: &Path) -> bool {
        std::fs::read_dir(dest).map_or(true, |d| d.count() == 0)
    }

    #[tokio::test]
    async fn sender_cancels_mid_transfer() {
        let ((a, a_trust), (b, b_trust)) = trusted_pair();
        let dest = temp_dir("dest");
        let s = start(b, b_trust, dest.clone()).await;
        let mut events = s.events;
        let path = big_file("src", 64);

        let (cancel_tx, cancel) = watch::channel(false);
        let receiver = tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                match ev {
                    Event::Incoming(req) => {
                        let _ = req.decision.send(true);
                    }
                    Event::Cancelled { by_peer, .. } => return Some(by_peer),
                    Event::Completed { .. } | Event::Failed { .. } => return None,
                    _ => {}
                }
            }
            None
        });
        // Cancel as soon as data starts moving.
        let outcome = send(s.addr, a, a_trust, paths(&[path]), cancel, move |_, _| {
            let _ = cancel_tx.send(true);
        })
        .await;
        assert_eq!(outcome.unwrap(), Outcome::Cancelled { by_peer: false });
        assert_eq!(
            receiver.await.unwrap(),
            Some(true),
            "receiver sees the sender's cancel"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(staging_is_gone(&dest));
    }

    #[tokio::test]
    async fn receiver_cancels_mid_transfer() {
        let ((a, a_trust), (b, b_trust)) = trusted_pair();
        let dest = temp_dir("dest");
        let s = start(b, b_trust, dest.clone()).await;
        let (mut events, inbox) = (s.events, s.inbox);
        let path = big_file("src", 64);

        let receiver = tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                match ev {
                    Event::Incoming(req) => {
                        let _ = req.decision.send(true);
                    }
                    Event::Progress { transfer_id, .. } => {
                        inbox.cancel(transfer_id);
                    }
                    Event::Cancelled { by_peer, .. } => return Some(by_peer),
                    Event::Completed { .. } | Event::Failed { .. } => return None,
                    _ => {}
                }
            }
            None
        });
        let outcome = send(s.addr, a, a_trust, paths(&[path]), never(), |_, _| {}).await;
        assert_eq!(outcome.unwrap(), Outcome::Cancelled { by_peer: true });
        assert_eq!(receiver.await.unwrap(), Some(false));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(staging_is_gone(&dest));
    }

    #[tokio::test]
    async fn sender_cancels_while_receiver_is_asked() {
        let ((a, a_trust), (b, b_trust)) = trusted_pair();
        let s = start(b, b_trust, temp_dir("dest")).await;
        let mut events = s.events;
        let path = big_file("src", 1);
        let (cancel_tx, cancel) = watch::channel(false);

        let receiver = tokio::spawn(async move {
            let mut held = Vec::new(); // dropping a request would count as Decline
            while let Some(ev) = events.recv().await {
                match ev {
                    // Keep the request open (unanswered); the sender gives up meanwhile.
                    Event::Incoming(req) => {
                        held.push(req);
                        let _ = cancel_tx.send(true);
                    }
                    Event::Cancelled { by_peer, .. } => return Some(by_peer),
                    _ => {}
                }
            }
            None
        });
        let outcome = send(s.addr, a, a_trust, paths(&[path]), cancel, |_, _| {}).await;
        assert_eq!(outcome.unwrap(), Outcome::Cancelled { by_peer: false });
        assert_eq!(receiver.await.unwrap(), Some(true));
    }

    /// Answers every incoming request with `accept`.
    fn auto_answer(mut events: mpsc::Receiver<Event>, accept: bool) {
        tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                if let Event::Incoming(req) = ev {
                    let _ = req.decision.send(accept);
                }
            }
        });
    }

    /// Loopback throughput: `cargo test --release -p rustdrop-core throughput -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn throughput() {
        let ((a, a_trust), (b, b_trust)) = trusted_pair();
        let s = start(b, b_trust, temp_dir("dest")).await;
        auto_answer(s.events, true);
        let path = big_file("src", 512);
        let t = Instant::now();
        let outcome = send(s.addr, a, a_trust, paths(&[path]), never(), |_, _| {}).await;
        assert_eq!(outcome.unwrap(), Outcome::Completed);
        let secs = t.elapsed().as_secs_f64();
        println!("{} MB/s", (512.0 * 1.048576 / secs) as u64);
    }

    #[tokio::test]
    async fn sends_file_and_folder_without_overwriting() {
        let ((a, a_trust), (b, b_trust)) = trusted_pair();
        let dest = temp_dir("dest");
        let s = start(b, b_trust, dest.clone()).await;
        let (addr, events) = (s.addr, s.events);
        auto_answer(events, true);

        let src = temp_dir("src");
        std::fs::create_dir_all(src.join("folder/sub")).unwrap();
        // Several chunks, split across the parallel connections, plus an empty file.
        let big = pattern(5 * CHUNK as usize + 12_345);
        std::fs::write(src.join("one.txt"), "one").unwrap();
        std::fs::write(src.join("folder/sub/two.bin"), &big).unwrap();
        std::fs::write(src.join("folder/empty.txt"), "").unwrap();

        let list = [src.join("one.txt"), src.join("folder")];
        let outcome = send(
            addr,
            a.clone(),
            a_trust.clone(),
            paths(&list),
            never(),
            |_, _| {},
        )
        .await;
        assert_eq!(outcome.unwrap(), Outcome::Completed);
        assert_eq!(std::fs::read(dest.join("one.txt")).unwrap(), b"one");
        assert_eq!(std::fs::read(dest.join("folder/sub/two.bin")).unwrap(), big);
        assert_eq!(std::fs::read(dest.join("folder/empty.txt")).unwrap(), b"");

        let again = send(addr, a, a_trust, paths(&list[..1]), never(), |_, _| {}).await;
        assert_eq!(again.unwrap(), Outcome::Completed);
        assert_eq!(std::fs::read(dest.join("one (1).txt")).unwrap(), b"one");
        assert_eq!(std::fs::read(dest.join("one.txt")).unwrap(), b"one");
        assert!(!std::fs::read_dir(&dest).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".rustdrop-")));
    }

    #[tokio::test]
    async fn broken_data_connection_is_retried_and_file_arrives_intact() {
        let ((a, a_trust), (b, b_trust)) = trusted_pair();
        let dest = temp_dir("dest");
        let s = start(b, b_trust, dest.clone()).await;
        s.inbox.drop_next_stream.store(true, Ordering::SeqCst);
        auto_answer(s.events, true);
        let path = big_file("src", 8);

        let outcome = send(
            s.addr,
            a,
            a_trust,
            paths(std::slice::from_ref(&path)),
            never(),
            |_, _| {},
        )
        .await;
        assert_eq!(outcome.unwrap(), Outcome::Completed);
        assert!(
            !s.inbox.drop_next_stream.load(Ordering::SeqCst),
            "a connection was dropped"
        );
        assert_eq!(
            std::fs::read(dest.join("big.bin")).unwrap(),
            std::fs::read(path).unwrap()
        );
    }

    #[tokio::test]
    async fn declined_transfer_writes_nothing() {
        let ((a, a_trust), (b, b_trust)) = trusted_pair();
        let dest = temp_dir("dest");
        let s = start(b, b_trust, dest.clone()).await;
        let (addr, events) = (s.addr, s.events);
        auto_answer(events, false);

        let src = temp_dir("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("one.txt"), "one").unwrap();
        let outcome = send(
            addr,
            a,
            a_trust,
            paths(&[src.join("one.txt")]),
            never(),
            |_, _| {},
        )
        .await;
        assert_eq!(outcome.unwrap(), Outcome::Declined { reason: None });
        assert!(!dest.join("one.txt").exists());
    }

    /// Plays the sender by hand: control messages on one stream, chunks on another.
    struct FakeSender {
        control: tokio::io::DuplexStream,
        inbox: Arc<Inbox>,
        events: mpsc::Receiver<Event>,
        id: Uuid,
        fingerprint: String,
        received: tokio::task::JoinHandle<Result<()>>,
    }

    impl FakeSender {
        fn start(dest: &Path, fingerprint: &str, files: Vec<FileMeta>) -> Self {
            let (tx, events) = mpsc::channel(64);
            let inbox = Arc::new(Inbox::new(dest.into(), tx));
            let (control, mut theirs) = tokio::io::duplex(1 << 16);
            let id = Uuid::new_v4();
            let peer = DeviceInfo {
                device_id: Uuid::new_v4(),
                name: "peer".into(),
                platform: "test".into(),
                version: crate::PROTOCOL_VERSION,
                port: 0,
            };
            let (i, fp) = (inbox.clone(), fingerprint.to_string());
            let received = tokio::spawn(async move {
                receive(&mut theirs, &peer, &fp, "peer", id, files, &i).await
            });
            Self {
                control,
                inbox,
                events,
                id,
                fingerprint: fingerprint.into(),
                received,
            }
        }

        /// Accepts the request; returns the token and the already-held bitset.
        async fn accepted(&mut self) -> (String, Vec<u8>) {
            loop {
                match self.events.recv().await.expect("receiver stopped") {
                    Event::Incoming(req) => req.decision.send(true).unwrap(),
                    _ => continue,
                }
                break;
            }
            match protocol::read(&mut self.control).await.unwrap() {
                Message::Accept { token, have, .. } => (token, unhex(&have).unwrap()),
                other => panic!("expected ACCEPT, got {other:?}"),
            }
        }

        /// Opens a data connection and sends `chunks` as (index, bytes), checking each ack.
        async fn send_chunks(&self, token: &str, chunks: &[(u32, &[u8])]) {
            let (mut mine, mut theirs) = tokio::io::duplex(1 << 16);
            let (inbox, fp, id, token) = (
                self.inbox.clone(),
                self.fingerprint.clone(),
                self.id,
                token.to_string(),
            );
            let task =
                tokio::spawn(
                    async move { receive_stream(&mut theirs, &fp, id, &token, &inbox).await },
                );
            for (index, bytes) in chunks {
                mine.write_u32(*index).await.unwrap();
                mine.write_u32(bytes.len() as u32).await.unwrap();
                mine.write_all(bytes).await.unwrap();
                assert_eq!(mine.read_u32().await.unwrap(), *index);
            }
            drop(mine);
            task.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn hash_mismatch_fails_and_saves_nothing() {
        let dest = temp_dir("dest");
        let mut s = FakeSender::start(&dest, &"ab".repeat(32), vec![meta("a.txt", 5)]);
        let (token, _) = s.accepted().await;

        // A data connection from another device, or with a wrong token, is refused.
        let (_m, mut t) = tokio::io::duplex(1024);
        assert!(
            receive_stream(&mut t, &"cd".repeat(32), s.id, &token, &s.inbox)
                .await
                .is_err()
        );
        assert!(receive_stream(&mut t, &s.fingerprint, s.id, "00", &s.inbox)
            .await
            .is_err());

        s.send_chunks(&token, &[(0, b"hello")]).await;
        let forged = Message::TransferComplete {
            transfer_id: s.id,
            sha256: vec!["00".repeat(32)],
        };
        protocol::write(&mut s.control, &forged).await.unwrap();

        let reply = protocol::read(&mut s.control).await.unwrap();
        assert!(matches!(reply, Message::TransferFailed { .. }));
        assert!(s.received.await.unwrap().is_err());
        assert!(!dest.join("a.txt").exists());
        assert!(
            staging_is_gone(&dest),
            "a corrupt transfer is never kept for resume"
        );
    }

    #[tokio::test]
    async fn broken_transfer_resumes_with_only_the_missing_chunks() {
        let ((a, a_trust), (b, b_trust)) = trusted_pair();
        let dest = temp_dir("dest");
        let path = big_file("src", 4);
        let data = std::fs::read(&path).unwrap();
        let (_, files): (Vec<Reader>, Vec<FileMeta>) = collect(paths(std::slice::from_ref(&path)))
            .unwrap()
            .into_iter()
            .unzip();

        // First attempt: chunks 0 and 2 arrive, then the sender's connection drops.
        let mut s = FakeSender::start(&dest, &a.fingerprint(), files);
        let (token, have) = s.accepted().await;
        assert!(have.iter().all(|b| *b == 0));
        let c = CHUNK as usize;
        s.send_chunks(&token, &[(0, &data[..c]), (2, &data[2 * c..3 * c])])
            .await;
        drop(s.control);
        assert!(s.received.await.unwrap().is_err());
        while let Ok(ev) = s.events.try_recv() {
            if let Event::Failed { reason, .. } = ev {
                assert!(reason.contains("continue"), "{reason}");
            }
        }

        // Second attempt, same files: only chunks 1 and 3 travel.
        let srv = start(b, b_trust, dest.clone()).await;
        auto_answer(srv.events, true);
        let mut first = None;
        let outcome = send(srv.addr, a, a_trust, paths(&[path]), never(), |done, _| {
            first.get_or_insert(done);
        })
        .await;
        assert_eq!(outcome.unwrap(), Outcome::Completed);
        assert_eq!(
            first,
            Some(2 * CHUNK),
            "progress starts at what was already there"
        );
        assert_eq!(std::fs::read(dest.join("big.bin")).unwrap(), data);
        assert_eq!(
            std::fs::read_dir(&dest).unwrap().count(),
            1,
            "only big.bin is left"
        );
    }

    #[tokio::test]
    async fn not_enough_space_is_declined_with_a_reason() {
        let dest = temp_dir("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let free = disk::available_space(&dest).unwrap();
        if free > MAX_TOTAL {
            return; // a request this large is refused by validation before the space check
        }
        let mut s = FakeSender::start(&dest, &"ab".repeat(32), vec![meta("huge.bin", free)]);
        match protocol::read(&mut s.control).await.unwrap() {
            Message::Decline {
                reason: Some(r), ..
            } => assert!(r.contains("free space"), "{r}"),
            other => panic!("expected DECLINE with a reason, got {other:?}"),
        }
        assert!(matches!(s.events.recv().await, Some(Event::Failed { .. })));
        assert!(s.received.await.unwrap().is_ok());
    }
}
