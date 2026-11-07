//! Authenticated sessions: accept peers (HELLO -> HELLO_ACK, then PINGs and transfers) and connect to peers.
//! An untrusted peer only gets this far while a QR pairing window is open, and may then only pair.

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{bail, ensure, Context, Result};
use rustls::{pki_types::ServerName, ClientConfig};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::timeout,
};
use tokio_rustls::{client, TlsAcceptor, TlsConnector};

use crate::{
    identity::Identity,
    protocol::{self, check_version, DeviceInfo, Message},
    security::{self, PairingWindow, TrustStore},
    transfer::{self, Event, Inbox},
};

const MAX_CONNECTIONS: usize = 16;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest clipboard text synced (64 KiB); longer copies are not sent.
pub const MAX_CLIPBOARD: usize = 64 * 1024;

pub async fn listen(
    listener: TcpListener,
    identity: Arc<Identity>,
    trust: Arc<TrustStore>,
    inbox: Arc<Inbox>,
    pairing: Arc<PairingWindow>,
) -> Result<()> {
    let config = security::server_config(&identity, trust.clone(), pairing.clone())?;
    let acceptor = TlsAcceptor::from(config);
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (tcp, addr) = listener.accept().await?;
        // Over the limit: drop the socket instead of queueing it.
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let (acceptor, me) = (acceptor.clone(), identity.info());
        let (inbox, trust, pairing) = (inbox.clone(), trust.clone(), pairing.clone());
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = serve(acceptor, tcp, addr, me, &inbox, &trust, &pairing).await {
                tracing::warn!(%addr, "session closed: {e:#}");
            }
        });
    }
}

async fn serve(
    acceptor: TlsAcceptor,
    tcp: TcpStream,
    remote: SocketAddr,
    me: DeviceInfo,
    inbox: &Inbox,
    trust: &TrustStore,
    pairing: &PairingWindow,
) -> Result<()> {
    let mut tls = match timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await? {
        Ok(tls) => tls,
        // A reachability probe connects and hangs up without a handshake: not an error.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let fp = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certs| certs.first())
        .map(|cert| security::fingerprint(cert))
        .context("peer sent no certificate")?;
    let peer = match timeout(IDLE_TIMEOUT, protocol::read(&mut tls)).await?? {
        Message::Hello(peer) => peer,
        other => bail!("expected HELLO, got {other:?}"),
    };
    check_version(&peer)?;
    protocol::write(&mut tls, &Message::HelloAck(me)).await?;

    // The handshake admits unknown certificates only during a pairing window.
    let Some(trusted) = trust.get(&fp) else {
        let msg = timeout(IDLE_TIMEOUT, protocol::read(&mut tls)).await??;
        let Message::PairRequest { code } = msg else {
            bail!("untrusted device sent {msg:?}");
        };
        let pairing_ok = accept_pairing(&mut tls, &peer, &fp, &code, trust, pairing, inbox).await;
        remember(trust, &fp, remote, &peer);
        return pairing_ok;
    };
    remember(trust, &fp, remote, &peer);
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
            Message::Clipboard { text } => {
                ensure!(text.len() <= MAX_CLIPBOARD, "clipboard text too large");
                let from = trusted.name.clone();
                inbox.notify(Event::Clipboard { text, from }).await;
            }
            Message::SendRequest { transfer_id, files } => {
                let received = transfer::receive(
                    &mut tls,
                    &peer,
                    &fp,
                    &trusted.name,
                    transfer_id,
                    files,
                    inbox,
                )
                .await;
                match received {
                    // A cancelled transfer leaves the stream mid-frame: end the session quietly.
                    Err(e) if e.is::<transfer::Cancelled>() => return Ok(()),
                    r => r?,
                }
            }
            // One of the parallel connections carrying an accepted transfer's data.
            Message::DataHello { transfer_id, token } => {
                return transfer::receive_stream(&mut tls, &fp, transfer_id, &token, inbox).await;
            }
            Message::PairRequest { code } => {
                return accept_pairing(&mut tls, &peer, &fp, &code, trust, pairing, inbox).await;
            }
            other => bail!("unexpected message {other:?}"),
        }
    }
}

/// Redeems a QR code and trusts the peer under its HELLO name.
async fn accept_pairing<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    peer: &DeviceInfo,
    fp: &str,
    code: &str,
    trust: &TrustStore,
    pairing: &PairingWindow,
    inbox: &Inbox,
) -> Result<()> {
    if !pairing.redeem(code) {
        let _ = protocol::write(stream, &Message::PairReject).await;
        bail!("wrong or expired pairing code");
    }
    // Keep a name the user already chose; only new devices get the name they announce.
    let name = trust.get(fp).map_or_else(|| label(&peer.name), |d| d.name);
    trust.add(fp, &name)?;
    protocol::write(stream, &Message::PairAccept).await?;
    inbox.notify(Event::Paired { name }).await;
    Ok(())
}

/// Saves where a trusted peer listens (its source IP plus the port from its HELLO).
fn remember(trust: &TrustStore, fp: &str, remote: SocketAddr, peer: &DeviceInfo) {
    if peer.port != 0 {
        let _ = trust.set_addr(fp, SocketAddr::new(remote.ip(), peer.port));
    }
}

/// A peer-chosen name, made safe to store and display.
fn label(name: &str) -> String {
    let clean: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    let clean = clean.trim();
    if clean.is_empty() {
        "New device".into()
    } else {
        clean.into()
    }
}

fn is_eof(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::UnexpectedEof)
}

async fn open(
    addr: SocketAddr,
    identity: &Identity,
    config: Arc<ClientConfig>,
) -> Result<(client::TlsStream<TcpStream>, DeviceInfo)> {
    let tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await??;
    // Peers are pinned by fingerprint, so the name only has to be syntactically valid.
    let name = ServerName::try_from("rustdrop")?;
    let mut tls = timeout(
        HANDSHAKE_TIMEOUT,
        TlsConnector::from(config).connect(name, tcp),
    )
    .await??;

    protocol::write(&mut tls, &Message::Hello(identity.info())).await?;
    let peer = match timeout(IDLE_TIMEOUT, protocol::read(&mut tls)).await?? {
        Message::HelloAck(peer) => peer,
        other => bail!("expected HELLO_ACK, got {other:?}"),
    };
    check_version(&peer)?;
    Ok((tls, peer))
}

/// Opens an authenticated session with a trusted peer and exchanges HELLO.
pub async fn connect(
    addr: SocketAddr,
    identity: &Identity,
    trust: Arc<TrustStore>,
) -> Result<(client::TlsStream<TcpStream>, DeviceInfo)> {
    open(
        addr,
        identity,
        security::client_config(identity, trust, None)?,
    )
    .await
}

/// Sends clipboard text to a trusted peer.
pub async fn send_clipboard(
    addr: SocketAddr,
    identity: &Identity,
    trust: Arc<TrustStore>,
    text: &str,
) -> Result<()> {
    let (mut tls, _) = connect(addr, identity, trust).await?;
    protocol::write(&mut tls, &Message::Clipboard { text: text.into() }).await?;
    let _ = tls.shutdown().await;
    Ok(())
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

/// Client side of QR pairing: connects to the device whose fingerprint `pin` came from its QR code,
/// presents the one-time `code`, and on success trusts that device. Returns it.
pub async fn pair(
    addr: SocketAddr,
    identity: &Identity,
    trust: Arc<TrustStore>,
    pin: &str,
    code: &str,
) -> Result<DeviceInfo> {
    let config = security::client_config(identity, trust.clone(), Some(pin.into()))?;
    let (mut tls, peer) = open(addr, identity, config).await?;
    protocol::write(&mut tls, &Message::PairRequest { code: code.into() }).await?;
    match timeout(IDLE_TIMEOUT, protocol::read(&mut tls)).await?? {
        Message::PairAccept => {}
        Message::PairReject => {
            bail!(
                "The pairing code was wrong or has expired. Show a new QR code and scan it again."
            )
        }
        other => bail!("expected PAIR_ACCEPT, got {other:?}"),
    }
    trust.add(pin, &label(&peer.name))?;
    trust.set_addr(pin, addr)?;
    let _ = tls.shutdown().await;
    Ok(peer)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::sync::mpsc;

    pub(crate) fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rustdrop-test-{tag}-{}", uuid::Uuid::new_v4()))
    }

    pub(crate) fn node(tag: &str) -> (Arc<Identity>, Arc<TrustStore>) {
        let dir = temp_dir(tag);
        let id = Identity::load_or_create(&dir, None).unwrap();
        let trust = TrustStore::load(dir.join("trusted.json")).unwrap();
        (Arc::new(id), Arc::new(trust))
    }

    pub(crate) struct Started {
        pub addr: SocketAddr,
        pub events: mpsc::Receiver<Event>,
        pub pairing: Arc<PairingWindow>,
        pub inbox: Arc<Inbox>,
    }

    /// Starts a listener; returns its address, the receiver's events and its pairing window.
    pub(crate) async fn start(id: Arc<Identity>, trust: Arc<TrustStore>, dest: PathBuf) -> Started {
        let (tx, events) = mpsc::channel(16);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let pairing = Arc::new(PairingWindow::default());
        let inbox = Arc::new(Inbox::new(dest, tx));
        tokio::spawn(listen(listener, id, trust, inbox.clone(), pairing.clone()));
        Started {
            addr,
            events,
            pairing,
            inbox,
        }
    }

    #[tokio::test]
    async fn trusted_peers_ping() {
        let (a, a_trust) = node("a");
        let (b, b_trust) = node("b");
        a_trust.add(&b.fingerprint(), "b").unwrap();
        b_trust.add(&a.fingerprint(), "a").unwrap();

        let s = start(b.clone(), b_trust, temp_dir("dest")).await;
        let (peer, _) = ping(s.addr, &a, a_trust).await.unwrap();
        assert_eq!(peer.device_id, b.id);
    }

    #[tokio::test]
    async fn untrusted_client_rejected() {
        let (a, a_trust) = node("a");
        let (b, b_trust) = node("b");
        a_trust.add(&b.fingerprint(), "b").unwrap(); // a trusts b, b does not trust a

        let s = start(b, b_trust, temp_dir("dest")).await;
        assert!(ping(s.addr, &a, a_trust).await.is_err());
    }

    #[tokio::test]
    async fn untrusted_server_rejected() {
        let (a, a_trust) = node("a");
        let (b, b_trust) = node("b");
        b_trust.add(&a.fingerprint(), "a").unwrap(); // b trusts a, a does not trust b

        let s = start(b, b_trust, temp_dir("dest")).await;
        assert!(ping(s.addr, &a, a_trust).await.is_err());
    }

    #[tokio::test]
    async fn qr_pairing_trusts_both_ways_once() {
        let (a, a_trust) = node("a");
        let (b, b_trust) = node("b");
        let mut s = start(b.clone(), b_trust.clone(), temp_dir("dest")).await;
        let code = s.pairing.open().unwrap();

        let peer = pair(s.addr, &a, a_trust.clone(), &b.fingerprint(), &code)
            .await
            .unwrap();
        assert_eq!(peer.device_id, b.id);
        assert!(a_trust.get(&b.fingerprint()).is_some());
        assert!(b_trust.get(&a.fingerprint()).is_some());
        assert!(matches!(s.events.recv().await, Some(Event::Paired { .. })));
        assert!(ping(s.addr, &a, a_trust).await.is_ok());

        // The code is single-use, and the window is closed for everyone else.
        let (c, c_trust) = node("c");
        assert!(pair(s.addr, &c, c_trust, &b.fingerprint(), &code)
            .await
            .is_err());
        assert!(b_trust.get(&c.fingerprint()).is_none());
    }

    #[tokio::test]
    async fn wrong_code_or_other_messages_get_nothing() {
        let (a, a_trust) = node("a");
        let (b, b_trust) = node("b");
        let s = start(b.clone(), b_trust.clone(), temp_dir("dest")).await;
        let code = s.pairing.open().unwrap();

        let wrong = "0".repeat(32);
        assert!(pair(s.addr, &a, a_trust.clone(), &b.fingerprint(), &wrong)
            .await
            .is_err());
        assert!(b_trust.get(&a.fingerprint()).is_none());

        // An unknown device inside the window may not PING (or send files) instead of pairing.
        let config = security::client_config(&a, a_trust.clone(), Some(b.fingerprint())).unwrap();
        let (mut tls, _) = open(s.addr, &a, config).await.unwrap();
        protocol::write(&mut tls, &Message::Ping { nonce: 1 })
            .await
            .unwrap();
        assert!(protocol::read(&mut tls).await.is_err());

        // 1 wrong attempt so far (the PING session does not count); 2 more close the window.
        assert!(pair(s.addr, &a, a_trust.clone(), &b.fingerprint(), &wrong)
            .await
            .is_err());
        assert!(pair(s.addr, &a, a_trust.clone(), &b.fingerprint(), &wrong)
            .await
            .is_err());
        assert!(pair(s.addr, &a, a_trust, &b.fingerprint(), &code)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn closed_window_rejects_unknown_devices_at_handshake() {
        let (a, a_trust) = node("a");
        let (b, b_trust) = node("b");
        let s = start(b.clone(), b_trust, temp_dir("dest")).await;
        let code = s.pairing.open().unwrap();
        s.pairing.close();
        assert!(pair(s.addr, &a, a_trust, &b.fingerprint(), &code)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn re_pairing_keeps_the_label_the_user_chose() {
        let (a, a_trust) = node("a");
        let (b, b_trust) = node("b");
        a_trust.add(&b.fingerprint(), "b").unwrap();
        b_trust.add(&a.fingerprint(), "Mom's phone").unwrap();
        let s = start(b.clone(), b_trust.clone(), temp_dir("dest")).await;
        let code = s.pairing.open().unwrap();

        pair(s.addr, &a, a_trust, &b.fingerprint(), &code)
            .await
            .unwrap();
        assert_eq!(b_trust.get(&a.fingerprint()).unwrap().name, "Mom's phone");
    }

    #[test]
    fn trust_file_from_older_versions_loads_and_keeps_addresses() {
        let dir = temp_dir("trust");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trusted.json");
        let fp = "ab".repeat(32);
        std::fs::write(&path, format!(r#"{{"{fp}":{{"name":"Laptop"}}}}"#)).unwrap();

        let trust = TrustStore::load(path.clone()).unwrap();
        assert!(trust.get(&fp).unwrap().addr.is_none());
        trust
            .set_addr(&fp, "10.10.21.68:47821".parse().unwrap())
            .unwrap();
        trust.add(&fp, "Office PC").unwrap(); // renaming keeps the address

        let again = TrustStore::load(path).unwrap().get(&fp).unwrap();
        assert_eq!(again.name, "Office PC");
        assert_eq!(again.addr.as_deref(), Some("10.10.21.68:47821"));
    }
}
