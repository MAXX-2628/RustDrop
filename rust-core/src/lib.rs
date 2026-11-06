//! RustDrop shared core: identity, discovery, security and transfer engine.

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
