//! Wire messages: a big-endian u32 length prefix followed by a JSON body.

use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

use crate::PROTOCOL_VERSION;

/// Upper bound for any single control message; peers sending more are dropped.
/// Sized for a SEND_REQUEST listing `transfer::MAX_FILES` paths.
pub const MAX_MESSAGE: usize = 1024 * 1024;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub device_id: Uuid,
    pub name: String,
    pub platform: String,
    pub version: u16,
    /// TCP port this device listens on, so the other side can reach it later without mDNS.
    /// 0 = unknown (older peers).
    #[serde(default)]
    pub port: u16,
}

/// One file in a transfer. `path` is relative and `/`-separated; the receiver validates it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FileMeta {
    pub path: String,
    pub size: u64,
    /// Last-modified time (Unix seconds, 0 = unknown). Only used to match a resumed transfer.
    #[serde(default)]
    pub modified: u64,
}

/// The control connection carries SEND_REQUEST, then ACCEPT (with a token and the chunks the
/// receiver already holds from an earlier, broken attempt) or DECLINE.
///
/// File data then flows over up to `transfer::MAX_STREAMS` extra connections to the same
/// receiver. Each one says DATA_HELLO with the transfer ID and token, then carries chunk frames.
/// A frame is a u32 chunk index, a u32 length and the bytes. The receiver answers each frame
/// with the u32 index once the bytes are written. Files are split into
/// `transfer::CHUNK`-sized chunks, numbered across all files in request order.
///
/// When every chunk is acknowledged, the sender sends TRANSFER_COMPLETE with each file's
/// SHA-256 on the control connection. The receiver re-reads and hashes what it wrote, then
/// answers TRANSFER_COMPLETE (verified and saved) or TRANSFER_FAILED. Either side may send
/// CANCEL on the control connection at any time.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Message {
    Hello(DeviceInfo),
    HelloAck(DeviceInfo),
    /// Either side stops a transfer (before or after ACCEPT).
    Cancel {
        transfer_id: Uuid,
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    SendRequest {
        transfer_id: Uuid,
        files: Vec<FileMeta>,
    },
    Accept {
        transfer_id: Uuid,
        /// Secret the data connections present in DATA_HELLO (32 hex).
        token: String,
        /// Chunks already received in an earlier attempt: a bitset, lowercase hex, bit i of
        /// byte i/8 (least significant first) = chunk i. Empty = none.
        #[serde(default)]
        have: String,
    },
    Decline {
        transfer_id: Uuid,
        /// Why it was declined automatically (for example, not enough free space). None = the user said no.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// First message on a data connection of an accepted transfer.
    DataHello {
        transfer_id: Uuid,
        token: String,
    },
    TransferComplete {
        transfer_id: Uuid,
        sha256: Vec<String>,
    },
    TransferFailed {
        transfer_id: Uuid,
        reason: String,
    },
    /// Clipboard sync: plain text copied on the sender (only between paired devices, opt-in on both).
    Clipboard {
        text: String,
    },
    /// QR pairing: the one-time code read from the QR. The only message an untrusted peer may send.
    PairRequest {
        code: String,
    },
    PairAccept,
    PairReject,
}

pub async fn write<W: AsyncWrite + Unpin>(w: &mut W, msg: &Message) -> Result<()> {
    let body = serde_json::to_vec(msg)?;
    ensure!(body.len() <= MAX_MESSAGE, "message too large");
    w.write_u32(body.len() as u32).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read<R: AsyncRead + Unpin>(r: &mut R) -> Result<Message> {
    let len = r.read_u32().await? as usize;
    ensure!(len <= MAX_MESSAGE, "message too large: {len} bytes");
    let mut body = vec![0; len];
    r.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

pub fn check_version(peer: &DeviceInfo) -> Result<()> {
    if peer.version != PROTOCOL_VERSION {
        bail!("unsupported protocol version {}", peer.version);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write(&mut a, &Message::Ping { nonce: 7 }).await.unwrap();
        assert_eq!(read(&mut b).await.unwrap(), Message::Ping { nonce: 7 });
    }

    #[tokio::test]
    async fn rejects_oversized_length() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_u32(MAX_MESSAGE as u32 + 1).await.unwrap();
        assert!(read(&mut b).await.is_err());
    }
}
