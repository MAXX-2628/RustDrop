//! File transfer engine (ARCHITECTURE.md §7, SECURITY.md §7-9).
//!
//! Receiver side: validate the request, ask the user (bound to the transfer ID),
//! stream into a staging folder inside the destination, verify SHA-256, then move
//! files into place without overwriting anything.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, ensure, Context, Result};
use ring::digest;
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot},
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
}

/// Receiver state shared by all sessions.
pub struct Inbox {
    dest: PathBuf,
    events: mpsc::Sender<Event>,
    used_ids: Mutex<HashSet<Uuid>>,
}

impl Inbox {
    pub fn new(dest: PathBuf, events: mpsc::Sender<Event>) -> Self {
        Self {
            dest,
            events,
            used_ids: Mutex::new(HashSet::new()),
        }
    }

    pub(crate) async fn notify(&self, event: Event) {
        let _ = self.events.send(event).await;
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
    Declined,
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
        let due = self.done == self.total || self.last.elapsed() >= PROGRESS_EVERY;
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

    let (decision, answer) = oneshot::channel();
    let request = Incoming {
        transfer_id,
        peer: peer.clone(),
        trusted_name: trusted_name.into(),
        files,
        decision,
    };
    let _ = inbox.events.send(Event::Incoming(request)).await;
    let accepted = matches!(timeout(DECISION_TIMEOUT, answer).await, Ok(Ok(true)));
    if !accepted {
        protocol::write(stream, &Message::Decline { transfer_id }).await?;
        return Ok(());
    }
    protocol::write(stream, &Message::Accept { transfer_id }).await?;

    let staging = inbox.dest.join(format!(".rustdrop-{transfer_id}"));
    let result = receive_files(stream, transfer_id, &paths, &sizes, total, &staging, inbox).await;
    let _ = fs::remove_dir_all(&staging).await;

    match result {
        Ok((saved, sha256)) => {
            let reply = Message::TransferComplete {
                transfer_id,
                sha256,
            };
            protocol::write(stream, &reply).await?;
            let _ = inbox
                .events
                .send(Event::Completed { transfer_id, saved })
                .await;
            Ok(())
        }
        Err(e) => {
            // Generic reason on the wire: local paths and errors stay on this device.
            let reply = Message::TransferFailed {
                transfer_id,
                reason: "receiver could not complete the transfer".into(),
            };
            let _ = timeout(Duration::from_secs(2), protocol::write(stream, &reply)).await;
            let reason = format!("{e:#}");
            let _ = inbox
                .events
                .send(Event::Failed {
                    transfer_id,
                    reason,
                })
                .await;
            Err(e)
        }
    }
}

async fn receive_files<S: AsyncRead + Unpin>(
    stream: &mut S,
    transfer_id: Uuid,
    paths: &[PathBuf],
    sizes: &[u64],
    total: u64,
    staging: &Path,
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
            let want = left.min(BUF as u64) as usize;
            let n = timeout(IDLE_TIMEOUT, stream.read(&mut buf[..want])).await??;
            ensure!(n > 0, "sender disconnected");
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
/// Dropping the future cancels the transfer; the receiver discards everything it staged.
pub async fn send(
    addr: std::net::SocketAddr,
    identity: &Identity,
    trust: Arc<TrustStore>,
    paths: &[PathBuf],
    mut progress: impl FnMut(u64, u64),
) -> Result<Outcome> {
    let owned = paths.to_vec();
    let (sources, files): (Vec<PathBuf>, Vec<FileMeta>) =
        tokio::task::spawn_blocking(move || collect(&owned))
            .await??
            .into_iter()
            .unzip();
    let (_, total) = validate(&files)?; // same rules as the receiver: fail before connecting

    let (mut tls, _peer) = server::connect(addr, identity, trust).await?;
    let transfer_id = Uuid::new_v4();
    let request = Message::SendRequest {
        transfer_id,
        files: files.clone(),
    };
    protocol::write(&mut tls, &request).await?;
    match timeout(DECISION_TIMEOUT + IDLE_TIMEOUT, protocol::read(&mut tls)).await?? {
        Message::Accept { transfer_id: id } if id == transfer_id => {}
        Message::Decline { transfer_id: id } if id == transfer_id => return Ok(Outcome::Declined),
        other => bail!("expected ACCEPT or DECLINE, got {other:?}"),
    }

    let mut buf = vec![0u8; BUF];
    let mut hashes = Vec::with_capacity(files.len());
    let mut ticker = Progress::new(total);
    for (src, meta) in sources.iter().zip(&files) {
        let mut file = fs::File::open(src).await?;
        let mut hash = digest::Context::new(&digest::SHA256);
        let mut left = meta.size;
        while left > 0 {
            let want = left.min(BUF as u64) as usize;
            let n = file.read(&mut buf[..want]).await?;
            ensure!(n > 0, "{} changed while sending", meta.path);
            hash.update(&buf[..n]);
            timeout(IDLE_TIMEOUT, tls.write_all(&buf[..n])).await??;
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
    protocol::write(&mut tls, &done).await?;

    let result = match timeout(IDLE_TIMEOUT, protocol::read(&mut tls)).await?? {
        Message::TransferComplete {
            transfer_id: id, ..
        } if id == transfer_id => Ok(Outcome::Completed),
        Message::TransferFailed {
            transfer_id: id,
            reason,
        } if id == transfer_id => Err(anyhow!("transfer failed: {reason}")),
        other => Err(anyhow!("expected TRANSFER_COMPLETE, got {other:?}")),
    };
    let _ = tls.shutdown().await;
    result
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
        let outcome = send(addr, &a, a_trust.clone(), &paths, |_, _| {}).await;
        assert_eq!(outcome.unwrap(), Outcome::Completed);
        assert_eq!(std::fs::read(dest.join("one.txt")).unwrap(), b"one");
        assert_eq!(std::fs::read(dest.join("folder/sub/two.bin")).unwrap(), big);

        let again = send(addr, &a, a_trust, &paths[..1], |_, _| {}).await;
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
        let outcome = send(addr, &a, a_trust, &[src.join("one.txt")], |_, _| {}).await;
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
