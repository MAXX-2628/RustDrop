//! File transfer engine (ARCHITECTURE.md §7, SECURITY.md §7-9).
//!
//! Receiver side: validate the request, ask the user (bound to the transfer ID),
//! stream into a staging folder inside the destination, verify SHA-256, then move
//! files into place without overwriting anything.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, ensure, Context, Result};
use ring::digest;
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
    time::timeout,
};
use uuid::Uuid;

use crate::{
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
const BUF: usize = 64 * 1024;

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
}

impl Inbox {
    pub fn new(dest: PathBuf, events: mpsc::Sender<Event>) -> Self {
        Self {
            dest,
            events,
            used_ids: Mutex::new(HashSet::new()),
            cancels: Mutex::default(),
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

/// File data travels as frames: u32 length (1..=BUF) then the bytes. This length means
/// "the sender cancelled"; the stream ends after it.
const CANCEL_MARK: u32 = u32::MAX;

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
    Declined,
    /// Someone cancelled after accepting; `by_peer`: the receiver did.
    Cancelled {
        by_peer: bool,
    },
}

/// Rate-limits progress reports so UIs are not flooded (DESIGN.md §12).
struct Progress {
    done: u64,
    total: u64,
    last: Instant,
}

impl Progress {
    fn new(total: u64) -> Self {
        Self {
            done: 0,
            total,
            last: Instant::now(),
        }
    }

    /// Records `n` bytes; true when a report is due.
    fn tick(&mut self, n: usize) -> bool {
        self.done += n as u64;
        // Report the first chunk at once, so UIs show progress as soon as bytes move.
        let first = self.done == n as u64;
        let due = first || self.done == self.total || self.last.elapsed() >= PROGRESS_EVERY;
        if due {
            self.last = Instant::now();
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

pub(crate) async fn receive<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    peer: &DeviceInfo,
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
            protocol::write(stream, &Message::Decline { transfer_id }).await?;
            return Err(e.context("rejected transfer request"));
        }
    };
    let sizes: Vec<u64> = files.iter().map(|f| f.size).collect();

    let (cancel_tx, mut cancel) = watch::channel(false);
    inbox.cancels.lock().unwrap().insert(transfer_id, cancel_tx);
    let result = run_receive(
        stream,
        peer,
        trusted_name,
        transfer_id,
        files,
        &paths,
        &sizes,
        total,
        &mut cancel,
        inbox,
    )
    .await;
    inbox.cancels.lock().unwrap().remove(&transfer_id);
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_receive<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    peer: &DeviceInfo,
    trusted_name: &str,
    transfer_id: Uuid,
    files: Vec<FileMeta>,
    paths: &[PathBuf],
    sizes: &[u64],
    total: u64,
    cancel: &mut watch::Receiver<bool>,
    inbox: &Inbox,
) -> Result<()> {
    let (decision, answer) = oneshot::channel();
    let request = Incoming {
        transfer_id,
        peer: peer.clone(),
        trusted_name: trusted_name.into(),
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
        protocol::write(stream, &Message::Decline { transfer_id }).await?;
        return Ok(());
    }
    protocol::write(stream, &Message::Accept { transfer_id }).await?;

    let staging = inbox.dest.join(format!(".rustdrop-{transfer_id}"));
    let result = receive_files(
        stream,
        transfer_id,
        paths,
        sizes,
        total,
        &staging,
        cancel,
        inbox,
    )
    .await;
    let _ = fs::remove_dir_all(&staging).await;

    match result {
        Ok((saved, sha256)) => {
            let reply = Message::TransferComplete {
                transfer_id,
                sha256,
            };
            protocol::write(stream, &reply).await?;
            inbox.notify(Event::Completed { transfer_id, saved }).await;
            Ok(())
        }
        Err(e) if e.is::<Cancelled>() => {
            let by_peer = e.downcast_ref::<Cancelled>().is_some_and(|c| c.by_peer);
            if !by_peer {
                // Tell the sender, which reads replies while it streams.
                let reply = Message::Cancel { transfer_id };
                let _ = timeout(Duration::from_secs(2), protocol::write(stream, &reply)).await;
                // Keep reading until the sender hangs up. Closing with unread data makes the OS
                // reset the connection, and the sender may then never see the CANCEL.
                let mut sink = vec![0u8; BUF];
                let drain =
                    async { while matches!(stream.read(&mut sink).await, Ok(n) if n > 0) {} };
                let _ = timeout(Duration::from_secs(2), drain).await;
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
            // Generic reason on the wire: local paths and errors stay on this device.
            let reply = Message::TransferFailed {
                transfer_id,
                reason: "receiver could not complete the transfer".into(),
            };
            let _ = timeout(Duration::from_secs(2), protocol::write(stream, &reply)).await;
            let reason = format!("{e:#}");
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

#[allow(clippy::too_many_arguments)]
async fn receive_files<S: AsyncRead + Unpin>(
    stream: &mut S,
    transfer_id: Uuid,
    paths: &[PathBuf],
    sizes: &[u64],
    total: u64,
    staging: &Path,
    cancel: &mut watch::Receiver<bool>,
    inbox: &Inbox,
) -> Result<(Vec<PathBuf>, Vec<String>)> {
    let mut buf = vec![0u8; BUF];
    let mut hashes = Vec::with_capacity(paths.len());
    let mut progress = Progress::new(total);

    for (rel, &size) in paths.iter().zip(sizes) {
        let tmp = staging.join(rel);
        fs::create_dir_all(tmp.parent().unwrap_or(staging)).await?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .await?;
        let mut hash = digest::Context::new(&digest::SHA256);
        let mut left = size;
        while left > 0 {
            // Dropping a half-read frame on cancel is fine: the session ends right after.
            let len = tokio::select! {
                biased;
                _ = cancelled(cancel) => return Err(Cancelled { by_peer: false }.into()),
                r = timeout(IDLE_TIMEOUT, stream.read_u32()) => r??,
            };
            if len == CANCEL_MARK {
                return Err(Cancelled { by_peer: true }.into());
            }
            let n = len as usize;
            ensure!(
                n > 0 && n <= BUF && len as u64 <= left,
                "bad data frame of {len} bytes"
            );
            timeout(IDLE_TIMEOUT, stream.read_exact(&mut buf[..n])).await??;
            hash.update(&buf[..n]);
            file.write_all(&buf[..n]).await?;
            left -= n as u64;
            if progress.tick(n) {
                let _ = inbox.events.try_send(Event::Progress {
                    transfer_id,
                    done: progress.done,
                    total,
                });
            }
        }
        file.sync_all().await?;
        hashes.push(hex(hash.finish().as_ref()));
    }

    let claimed = match timeout(IDLE_TIMEOUT, protocol::read(stream)).await?? {
        Message::TransferComplete {
            transfer_id: id,
            sha256,
        } if id == transfer_id => sha256,
        other => bail!("expected TRANSFER_COMPLETE, got {other:?}"),
    };
    ensure!(claimed == hashes, "integrity check failed");

    let mut saved = Vec::with_capacity(paths.len());
    for rel in paths {
        let target = unique(&inbox.dest.join(rel)).await?;
        fs::create_dir_all(target.parent().unwrap_or(&inbox.dest)).await?;
        fs::rename(staging.join(rel), &target).await?;
        saved.push(target);
    }
    Ok((saved, hashes))
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

/// Expands files and folders into (source, metadata). Folders keep their relative
/// structure; symlinks and special files are skipped, never followed (SECURITY.md §9).
pub fn collect(paths: &[PathBuf]) -> Result<Vec<(PathBuf, FileMeta)>> {
    let mut out = Vec::new();
    for p in paths {
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("unsupported file name: {}", p.display()))?;
        add(p, name.into(), &mut out)?;
    }
    Ok(out)
}

fn add(path: &Path, rel: String, out: &mut Vec<(PathBuf, FileMeta)>) -> Result<()> {
    ensure!(
        rel.matches('/').count() < MAX_DEPTH,
        "folder too deep: {rel}"
    );
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_file() {
        ensure!(out.len() < MAX_FILES, "more than {MAX_FILES} files");
        let size = meta.len();
        out.push((path.into(), FileMeta { path: rel, size }));
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

/// Sends files/folders to a trusted peer. `progress(done, total)` is called at most every 200 ms.
/// Setting `cancel` to true stops the transfer cleanly: the receiver is told and discards what it staged.
pub async fn send(
    addr: std::net::SocketAddr,
    identity: &Identity,
    trust: Arc<TrustStore>,
    paths: &[PathBuf],
    mut cancel: watch::Receiver<bool>,
    mut progress: impl FnMut(u64, u64),
) -> Result<Outcome> {
    let owned = paths.to_vec();
    let (sources, files): (Vec<PathBuf>, Vec<FileMeta>) =
        tokio::task::spawn_blocking(move || collect(&owned))
            .await??
            .into_iter()
            .unzip();
    let (_, total) = validate(&files)?; // same rules as the receiver: fail before connecting

    let (tls, _peer) = server::connect(addr, identity, trust).await?;
    // Split so the receiver's CANCEL can be read while file data is being written.
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
    match reply {
        Message::Accept { transfer_id: id } if id == transfer_id => {}
        Message::Decline { transfer_id: id } if id == transfer_id => return Ok(Outcome::Declined),
        other => bail!("expected ACCEPT or DECLINE, got {other:?}"),
    }

    // One read stays pending for the whole stream: the receiver's verdict, or its CANCEL.
    let mut reply = Box::pin(protocol::read(&mut rd));
    let mut buf = vec![0u8; BUF];
    let mut hashes = Vec::with_capacity(files.len());
    let mut ticker = Progress::new(total);
    for (src, meta) in sources.iter().zip(&files) {
        let mut file = fs::File::open(src).await?;
        let mut hash = digest::Context::new(&digest::SHA256);
        let mut left = meta.size;
        while left > 0 {
            // Checked between frames, never mid-frame, so the stream stays parseable.
            if *cancel.borrow() {
                let _ = wr.write_u32(CANCEL_MARK).await;
                let _ = wr.shutdown().await;
                return Ok(Outcome::Cancelled { by_peer: false });
            }
            let want = left.min(BUF as u64) as usize;
            let n = file.read(&mut buf[..want]).await?;
            ensure!(n > 0, "{} changed while sending", meta.path);
            hash.update(&buf[..n]);
            let written = tokio::select! {
                biased;
                msg = &mut reply => return verdict_mid_stream(msg),
                r = timeout(IDLE_TIMEOUT, write_frame(&mut wr, &buf[..n])) => r,
            };
            if let Err(e) = written
                .map_err(anyhow::Error::from)
                .and_then(|r| r.map_err(Into::into))
            {
                // A write fails once the receiver has hung up; its CANCEL may already be waiting.
                return match timeout(Duration::from_millis(500), &mut reply).await {
                    Ok(msg @ Ok(Message::Cancel { .. } | Message::TransferFailed { .. })) => {
                        verdict_mid_stream(msg)
                    }
                    _ => Err(e),
                };
            }
            left -= n as u64;
            if ticker.tick(n) {
                progress(ticker.done, total);
            }
        }
        hashes.push(hex(hash.finish().as_ref()));
    }
    let done = Message::TransferComplete {
        transfer_id,
        sha256: hashes,
    };
    protocol::write(&mut wr, &done).await?;

    let result = match timeout(IDLE_TIMEOUT, &mut reply).await?? {
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
    let _ = wr.shutdown().await;
    result
}

/// What the receiver said while we were still streaming.
fn verdict_mid_stream(msg: Result<Message>) -> Result<Outcome> {
    match msg? {
        Message::Cancel { .. } => Ok(Outcome::Cancelled { by_peer: true }),
        Message::TransferFailed { reason, .. } => Err(anyhow!("transfer failed: {reason}")),
        other => Err(anyhow!("unexpected {other:?} during transfer")),
    }
}

async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> std::io::Result<()> {
    w.write_u32(data.len() as u32).await?;
    w.write_all(data).await
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

    #[test]
    fn rejects_duplicates_and_oversize() {
        let f = |p: &str, size| FileMeta {
            path: p.into(),
            size,
        };
        assert!(validate(&[f("a.txt", 1), f("A.TXT", 1)]).is_err());
        assert!(validate(&[f("a", u64::MAX), f("b", 1)]).is_err());
        assert!(validate(&[]).is_err());
    }

    type Node = (Arc<Identity>, Arc<TrustStore>);

    fn trusted_pair() -> (Node, Node) {
        let (a, b) = (node("a"), node("b"));
        a.1.add(&b.0.fingerprint(), "b").unwrap();
        b.1.add(&a.0.fingerprint(), "a").unwrap();
        (a, b)
    }

    fn never() -> watch::Receiver<bool> {
        watch::channel(false).1
    }

    fn big_file(tag: &str, mb: usize) -> PathBuf {
        let src = temp_dir(tag);
        std::fs::create_dir_all(&src).unwrap();
        let path = src.join("big.bin");
        std::fs::write(&path, vec![7u8; mb << 20]).unwrap();
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
        let path = big_file("src", 8);

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
        // Cancel as soon as the first chunk has gone out.
        let outcome = send(s.addr, &a, a_trust, &[path], cancel, move |_, _| {
            let _ = cancel_tx.send(true);
        })
        .await;
        assert_eq!(outcome.unwrap(), Outcome::Cancelled { by_peer: false });
        assert_eq!(
            receiver.await.unwrap(),
            Some(true),
            "receiver sees the sender's cancel"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(staging_is_gone(&dest));
    }

    #[tokio::test]
    async fn receiver_cancels_mid_transfer() {
        let ((a, a_trust), (b, b_trust)) = trusted_pair();
        let dest = temp_dir("dest");
        let s = start(b, b_trust, dest.clone()).await;
        let (mut events, inbox) = (s.events, s.inbox);
        let path = big_file("src", 16);

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
        let outcome = send(s.addr, &a, a_trust, &[path], never(), |_, _| {}).await;
        assert_eq!(outcome.unwrap(), Outcome::Cancelled { by_peer: true });
        assert_eq!(receiver.await.unwrap(), Some(false));
        tokio::time::sleep(Duration::from_millis(100)).await;
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
        let outcome = send(s.addr, &a, a_trust, &[path], cancel, |_, _| {}).await;
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

    #[tokio::test]
    async fn sends_file_and_folder_without_overwriting() {
        let ((a, a_trust), (b, b_trust)) = trusted_pair();
        let dest = temp_dir("dest");
        let s = start(b, b_trust, dest.clone()).await;
        let (addr, events) = (s.addr, s.events);
        auto_answer(events, true);

        let src = temp_dir("src");
        std::fs::create_dir_all(src.join("folder/sub")).unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
        std::fs::write(src.join("one.txt"), "one").unwrap();
        std::fs::write(src.join("folder/sub/two.bin"), &big).unwrap();

        let paths = [src.join("one.txt"), src.join("folder")];
        let outcome = send(addr, &a, a_trust.clone(), &paths, never(), |_, _| {}).await;
        assert_eq!(outcome.unwrap(), Outcome::Completed);
        assert_eq!(std::fs::read(dest.join("one.txt")).unwrap(), b"one");
        assert_eq!(std::fs::read(dest.join("folder/sub/two.bin")).unwrap(), big);

        let again = send(addr, &a, a_trust, &paths[..1], never(), |_, _| {}).await;
        assert_eq!(again.unwrap(), Outcome::Completed);
        assert_eq!(std::fs::read(dest.join("one (1).txt")).unwrap(), b"one");
        assert_eq!(std::fs::read(dest.join("one.txt")).unwrap(), b"one");
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
            &a,
            a_trust,
            &[src.join("one.txt")],
            never(),
            |_, _| {},
        )
        .await;
        assert_eq!(outcome.unwrap(), Outcome::Declined);
        assert!(!dest.join("one.txt").exists());
    }

    #[tokio::test]
    async fn hash_mismatch_fails_and_saves_nothing() {
        let dest = temp_dir("dest");
        let (tx, mut events) = mpsc::channel(16);
        let inbox = Inbox::new(dest.clone(), tx);
        let (mut sender, mut receiver) = tokio::io::duplex(BUF);
        let id = Uuid::new_v4();
        let peer = DeviceInfo {
            device_id: Uuid::new_v4(),
            name: "peer".into(),
            platform: "test".into(),
            version: crate::PROTOCOL_VERSION,
            port: 0,
        };
        let files = vec![FileMeta {
            path: "a.txt".into(),
            size: 5,
        }];
        let recv =
            tokio::spawn(
                async move { receive(&mut receiver, &peer, "peer", id, files, &inbox).await },
            );

        let Some(Event::Incoming(req)) = events.recv().await else {
            panic!("expected request")
        };
        req.decision.send(true).unwrap();
        let accept = protocol::read(&mut sender).await.unwrap();
        assert_eq!(accept, Message::Accept { transfer_id: id });
        sender.write_all(b"hello").await.unwrap();
        let forged = Message::TransferComplete {
            transfer_id: id,
            sha256: vec!["00".repeat(32)],
        };
        protocol::write(&mut sender, &forged).await.unwrap();

        let reply = protocol::read(&mut sender).await.unwrap();
        assert!(matches!(reply, Message::TransferFailed { .. }));
        assert!(recv.await.unwrap().is_err());
        assert!(!dest.join("a.txt").exists());
        assert!(!dest.join(format!(".rustdrop-{id}")).exists());
    }
}
