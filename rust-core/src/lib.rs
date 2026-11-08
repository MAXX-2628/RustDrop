//! RustDrop shared core: identity, discovery, security and transfer engine.

pub mod config;
pub mod discovery;
pub mod disk;
pub mod identity;
pub mod node;
pub mod protocol;
pub mod security;
pub mod server;
pub mod transfer;

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!();

/// Application version, shared with platform front-ends.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Wire protocol version advertised during discovery and HELLO.
/// 2: file data is framed so either side can cancel mid-transfer.
/// 3: chunked data over parallel connections, resume of broken transfers, decline reasons.
/// 4: pause, links/text (TEXT), clipboard images (SEND_REQUEST.clipboard).
pub const PROTOCOL_VERSION: u16 = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_manifest() {
        assert_eq!(VERSION, "0.2.0");
    }
}
