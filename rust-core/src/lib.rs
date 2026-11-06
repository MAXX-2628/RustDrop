//! RustDrop shared core: identity, discovery, security and transfer engine.

pub mod config;
pub mod discovery;
pub mod identity;
pub mod protocol;
pub mod security;
pub mod server;
pub mod transfer;

/// Application version, shared with platform front-ends.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Wire protocol version advertised during discovery and HELLO.
pub const PROTOCOL_VERSION: u16 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_manifest() {
        assert_eq!(VERSION, "0.1.0");
    }
}
