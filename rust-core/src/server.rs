//! Authenticated sessions: accept peers (HELLO -> HELLO_ACK, then PINGs and transfers) and connect to peers.

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use rustls::pki_types::ServerName;
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::timeout,
};
use tokio_rustls::{client, TlsAcceptor, TlsConnector};

use crate::{
    identity::Identity,
    protocol::{self, check_version, DeviceInfo, Message},
    security::{self, TrustStore},
    transfer::{self, Inbox},
};

const MAX_CONNECTIONS: usize = 16;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn listen(
    listener: TcpListener,
    identity: Arc<Identity>,
    trust: Arc<TrustStore>,
    inbox: Arc<Inbox>,
) -> Result<()> {
    let acceptor = TlsAcceptor::from(security::server_config(&identity, trust.clone())?);
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (tcp, addr) = listener.accept().await?;
        // Over the limit: drop the socket instead of queueing it.
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let (acceptor, me) = (acceptor.clone(), identity.info());
        let (inbox, trust) = (inbox.clone(), trust.clone());
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = serve(acceptor, tcp, me, &inbox, &trust).await {
                tracing::warn!(%addr, "session closed: {e:#}");
            }
        });
    }
}

async fn serve(
    acceptor: TlsAcceptor,
    tcp: TcpStream,
    me: DeviceInfo,
    inbox: &Inbox,
    trust: &TrustStore,
) -> Result<()> {
    let mut tls = timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await??;
    // The handshake only succeeds for pinned certificates, so this lookup always finds the peer.
    let trusted_name = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certs| certs.first())
        .and_then(|cert| trust.get(&security::fingerprint(cert)))
        .map(|device| device.name)
        .context("peer certificate missing from trust store")?;
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
            Message::SendRequest { transfer_id, files } => {
                transfer::receive(&mut tls, &peer, &trusted_name, transfer_id, files, inbox)
                    .await?;
            }
            other => bail!("unexpected message {other:?}"),
        }
    }
}

fn is_eof(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::UnexpectedEof)
}

/// Opens an authenticated session with a trusted peer and exchanges HELLO.
pub async fn connect(
    addr: SocketAddr,
    identity: &Identity,
    trust: Arc<TrustStore>,
) -> Result<(client::TlsStream<TcpStream>, DeviceInfo)> {
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
    Ok((tls, peer))
}

/// Connects to a trusted peer and exchanges one PING/PONG. Returns the peer and round-trip time.
pub async fn ping(
    addr: SocketAddr,
    identity: &Identity,
    trust: Arc<TrustStore>,
) -> Result<(DeviceInfo, Duration)> {
    let (mut tls, peer) = connect(addr, identity, trust).await?;
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
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::sync::mpsc;

    use crate::transfer::Event;

    pub(crate) fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rustdrop-test-{tag}-{}", uuid::Uuid::new_v4()))
    }

    pub(crate) fn node(tag: &str) -> (Arc<Identity>, Arc<TrustStore>) {
        let dir = temp_dir(tag);
        let id = Identity::load_or_create(&dir).unwrap();
        let trust = TrustStore::load(dir.join("trusted.json")).unwrap();
        (Arc::new(id), Arc::new(trust))
    }

    /// Starts a listener; returns its address and the receiver's event stream.
    pub(crate) async fn start(
        id: Arc<Identity>,
        trust: Arc<TrustStore>,
        dest: PathBuf,
    ) -> (SocketAddr, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel(16);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(listen(listener, id, trust, Arc::new(Inbox::new(dest, tx))));
        (addr, rx)
    }

    #[tokio::test]
    async fn trusted_peers_ping() {
        let (a, a_trust) = node("a");
        let (b, b_trust) = node("b");
        a_trust.add(&b.fingerprint(), "b").unwrap();
        b_trust.add(&a.fingerprint(), "a").unwrap();

        let (addr, _events) = start(b.clone(), b_trust, temp_dir("dest")).await;
        let (peer, _) = ping(addr, &a, a_trust).await.unwrap();
        assert_eq!(peer.device_id, b.id);
    }

    #[tokio::test]
    async fn untrusted_client_rejected() {
        let (a, a_trust) = node("a");
        let (b, b_trust) = node("b");
        a_trust.add(&b.fingerprint(), "b").unwrap(); // a trusts b, b does not trust a

        let (addr, _events) = start(b, b_trust, temp_dir("dest")).await;
        assert!(ping(addr, &a, a_trust).await.is_err());
    }

    #[tokio::test]
    async fn untrusted_server_rejected() {
        let (a, a_trust) = node("a");
        let (b, b_trust) = node("b");
        b_trust.add(&a.fingerprint(), "a").unwrap(); // b trusts a, a does not trust b

        let (addr, _events) = start(b, b_trust, temp_dir("dest")).await;
        assert!(ping(addr, &a, a_trust).await.is_err());
    }
}
