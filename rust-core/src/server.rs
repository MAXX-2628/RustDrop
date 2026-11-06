//! Authenticated sessions: accept peers (HELLO -> HELLO_ACK, then answer PINGs) and ping peers.

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{bail, Result};
use rustls::pki_types::ServerName;
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::timeout,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::{
    identity::Identity,
    protocol::{self, check_version, DeviceInfo, Message},
    security::{self, TrustStore},
};

const MAX_CONNECTIONS: usize = 16;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn listen(
    listener: TcpListener,
    identity: Arc<Identity>,
    trust: Arc<TrustStore>,
) -> Result<()> {
    let acceptor = TlsAcceptor::from(security::server_config(&identity, trust)?);
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (tcp, addr) = listener.accept().await?;
        // Over the limit: drop the socket instead of queueing it.
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let (acceptor, me) = (acceptor.clone(), identity.info());
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = serve(acceptor, tcp, me).await {
                tracing::warn!(%addr, "session closed: {e:#}");
            }
        });
    }
}

async fn serve(acceptor: TlsAcceptor, tcp: TcpStream, me: DeviceInfo) -> Result<()> {
    let mut tls = timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await??;
    let peer = match timeout(IDLE_TIMEOUT, protocol::read(&mut tls)).await?? {
        Message::Hello(peer) => peer,
        other => bail!("expected HELLO, got {other:?}"),
    };
    check_version(&peer)?;
    protocol::write(&mut tls, &Message::HelloAck(me)).await?;
    tracing::info!(peer = %peer.name, "trusted peer connected");

    loop {
        let msg = match timeout(IDLE_TIMEOUT, protocol::read(&mut tls)).await? {
            Ok(msg) => msg,
            Err(e) if is_eof(&e) => return Ok(()),
            Err(e) => return Err(e),
        };
        match msg {
            Message::Ping { nonce } => {
                protocol::write(&mut tls, &Message::Pong { nonce }).await?;
            }
            other => bail!("unexpected message {other:?}"),
        }
    }
}

fn is_eof(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::UnexpectedEof)
}

/// Connects to a trusted peer, exchanges HELLO and one PING/PONG. Returns the peer and round-trip time.
pub async fn ping(
    addr: SocketAddr,
    identity: &Identity,
    trust: Arc<TrustStore>,
) -> Result<(DeviceInfo, Duration)> {
    let connector = TlsConnector::from(security::client_config(identity, trust)?);
    let tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await??;
    // Peers are pinned by fingerprint, so the name only has to be syntactically valid.
    let name = ServerName::try_from("rustdrop")?;
    let mut tls = timeout(HANDSHAKE_TIMEOUT, connector.connect(name, tcp)).await??;

    protocol::write(&mut tls, &Message::Hello(identity.info())).await?;
    let peer = match timeout(IDLE_TIMEOUT, protocol::read(&mut tls)).await?? {
        Message::HelloAck(peer) => peer,
        other => bail!("expected HELLO_ACK, got {other:?}"),
    };
    check_version(&peer)?;

    let nonce = uuid::Uuid::new_v4().as_u64_pair().0;
    let start = Instant::now();
    protocol::write(&mut tls, &Message::Ping { nonce }).await?;
    let rtt = match timeout(IDLE_TIMEOUT, protocol::read(&mut tls)).await?? {
        Message::Pong { nonce: n } if n == nonce => start.elapsed(),
        other => bail!("expected PONG, got {other:?}"),
    };
    let _ = tls.shutdown().await; // send close_notify; failure here doesn't affect the result
    Ok((peer, rtt))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(tag: &str) -> (Arc<Identity>, Arc<TrustStore>) {
        let dir =
            std::env::temp_dir().join(format!("rustdrop-test-{tag}-{}", uuid::Uuid::new_v4()));
        let id = Identity::load_or_create(&dir).unwrap();
        let trust = TrustStore::load(dir.join("trusted.json")).unwrap();
        (Arc::new(id), Arc::new(trust))
    }

    async fn start(id: Arc<Identity>, trust: Arc<TrustStore>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(listen(listener, id, trust));
        addr
    }

    #[tokio::test]
    async fn trusted_peers_ping() {
        let (a, a_trust) = node("a");
        let (b, b_trust) = node("b");
        a_trust.add(&b.fingerprint(), "b").unwrap();
        b_trust.add(&a.fingerprint(), "a").unwrap();

        let addr = start(b.clone(), b_trust).await;
        let (peer, _) = ping(addr, &a, a_trust).await.unwrap();
        assert_eq!(peer.device_id, b.id);
    }

    #[tokio::test]
    async fn untrusted_client_rejected() {
        let (a, a_trust) = node("a");
        let (b, b_trust) = node("b");
        a_trust.add(&b.fingerprint(), "b").unwrap(); // a trusts b, b does not trust a

        let addr = start(b, b_trust).await;
        assert!(ping(addr, &a, a_trust).await.is_err());
    }

    #[tokio::test]
    async fn untrusted_server_rejected() {
        let (a, a_trust) = node("a");
        let (b, b_trust) = node("b");
        b_trust.add(&a.fingerprint(), "a").unwrap(); // b trusts a, a does not trust b

        let addr = start(b, b_trust).await;
        assert!(ping(addr, &a, a_trust).await.is_err());
    }
}
