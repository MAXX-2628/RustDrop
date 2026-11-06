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
}

/// After ACCEPT the sender streams each file's raw bytes in order (sizes from SEND_REQUEST),
/// then sends TRANSFER_COMPLETE with their SHA-256 hashes. The receiver answers
/// TRANSFER_COMPLETE (verified and saved) or TRANSFER_FAILED.
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
    },
    Decline {
        transfer_id: Uuid,
    },
    TransferComplete {
        transfer_id: Uuid,
        sha256: Vec<String>,
    },
    TransferFailed {
        transfer_id: Uuid,
        reason: String,
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
