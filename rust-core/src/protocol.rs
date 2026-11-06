//! Wire messages: a big-endian u32 length prefix followed by a JSON body.

use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

use crate::PROTOCOL_VERSION;

/// Upper bound for any single control message; peers sending more are dropped.
pub const MAX_MESSAGE: usize = 64 * 1024;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub device_id: Uuid,
    pub name: String,
    pub platform: String,
    pub version: u16,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Message {
    Hello(DeviceInfo),
    HelloAck(DeviceInfo),
    Ping { nonce: u64 },
    Pong { nonce: u64 },
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
