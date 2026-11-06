# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Environment (do this first)

Every toolchain is portable and lives in `Tools\` (git-ignored). Each new PowerShell session must activate it, or a different Rust from C: is picked up and the Android tools won't be found:

```powershell
cd Z:\RustDrop; . .\env.ps1
```

- Keep everything inside `Z:\RustDrop`. Never install tools system-wide (no winget, no user-PATH or registry changes). A new tool goes under `Tools\<name>`, and `env.ps1` gets its PATH and home variable.
- System dependencies that are already present and can't be moved: VS 2022 Build Tools (MSVC 14.44, Windows SDK 10.0.26100) and the WebView2 runtime.
- Android: the SDK is in `Tools\android-sdk` (platform android-37.0, build-tools 37.0.0, NDK 30.0.16248370, CMake 3.31.6). `sdkmanager` is deprecated; install packages with `android --sdk=$env:ANDROID_HOME sdk install <pkg>/<ver>`.
- Shell is Windows PowerShell 5.1: no `&&`, so use `; if ($?) { ... }`.

## Commands

```powershell
cargo check --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings    # CI-level lint; must be clean
cargo test --workspace
cargo test -p rustdrop-core <test_name>                  # single test (substring match)
cargo run -p rustdrop-core                               # `rustdrop` binary
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 build -p rustdrop-core --lib   # Android .so files
cargo audit; cargo outdated                              # dependency security (SECURITY.md §19)
```

Later phases, once those projects exist: `pnpm install; pnpm tauri build` in `windows\`, and `.\gradlew assembleDebug` in `android\`.

## Architecture

The specs in `docs\` are the source of truth. Read the relevant one before designing anything:
- `docs\ARCHITECTURE.md`: phases, the protocol message set, transfer states, project layout, and milestones.
- `docs\SECURITY.md`: mandatory rules (§24) and fail-closed behaviour (§18). Treat these as hard requirements, not suggestions.
- `docs\DESIGN.md`: design tokens, components, UI states and copy for both platforms.

The big picture:
- **One shared Rust core (`rust-core\`, crate `rustdrop-core`, lib name `rustdrop_core`) does all networking, security and transfer work.** The platform UIs only show state and send user commands. The core builds as both a `lib` and a `cdylib`, so Android can load it over JNI/UniFFI and Tauri can link it.
  - Windows: Tauri 2 + React/TypeScript in `windows\`.
  - Android: Kotlin + Jetpack Compose in `android\`.
- Core modules that exist now:
  - `identity` creates a UUID, a display name and a self-signed rcgen certificate on first run and stores them in the data dir (`RUSTDROP_HOME`, otherwise `%APPDATA%\RustDrop`).
  - `security` runs TLS 1.3 only, where both sides present a certificate. A peer is accepted only if its SHA-256 certificate fingerprint is in `trusted.json`; handshake signatures are still verified by rustls. It uses the `ring` crypto backend everywhere, because aws-lc-rs needs extra native tooling.
  - `protocol` frames messages as a u32 BE length plus JSON, with a 64 KiB cap.
  - `discovery` uses `mdns-sd`.
  - `server` handles HELLO/HELLO_ACK, PING/PONG, connection limits and timeouts.
- Core modules still to build (ARCHITECTURE §5): QUIC transport (TCP+TLS for now), config, transfer (chunked, SHA-256, bounded buffers), clipboard and device_manager.
- Search the code for `shortcut:` comments; each marks a known gap to close before release, such as the private key stored as a plain file instead of DPAPI/Keystore.
- Try it with two local instances, each with a different `RUSTDROP_HOME`: run `rustdrop id` in each, then `rustdrop trust <other-fingerprint> <name>` in each, then `rustdrop run` in both.
- **Security boundaries:**
  - Discovery data is untrusted and never authenticates a peer.
  - A UUID or device name is never proof of identity; only the certificate fingerprint is.
  - Every transfer needs explicit receiver approval bound to its transfer ID.
  - Remote paths are validated against the chosen destination directory. Reject absolute, `..`, drive-letter and UNC paths, null bytes and symlinks.
  - Files are written to a temporary location and only reported complete after SHA-256 verification.
  - Clipboard sync is opt-in, trusted devices only, and its contents are never logged.
- Protocol messages are versioned and length-prefixed (`PROTOCOL_VERSION` in `rust-core\src\lib.rs`). Transfer states (IDLE … CANCELLED) must stay identical in the core and both UIs.
- Shared design tokens go in `shared\design\*.json`, and both platforms use the same values. Never hardcode colours in components.
- Current goal (ARCHITECTURE §20): Windows and Android discover each other, show device names, set up a trusted encrypted connection, and exchange PING and PONG. Transfers come after that.

## Skills to use

| Situation | Skill |
|-----------|-------|
| Any coding task (keep the core lightweight, avoid over-building) | `ponytail:ponytail` |
| New core feature or module | `caveman:lean-build`, or `claude-mem:make-plan` then `claude-mem:do` for multi-phase work |
| Bug fix | `caveman:investigate-first` when the cause is unknown, then `caveman:surgical-patch` |
| Changes to protocol, persisted identity/trust records or config formats | `caveman:migration` |
| Restructuring modules | `caveman:safe-refactor` |
| Anything touching security, crypto, paths or the network (most of this repo) | `security-review` before committing |
| Reviewing a change | `ponytail:ponytail-review` or `code-review` |
| Windows (Tauri/React) UI work, which must follow `docs\DESIGN.md` tokens | `frontend-design:frontend-design` |
| Running the app or checking a change works for real | `run` |
| Proving a task is finished | `caveman:verify-and-stop` |
| Long session that needs a handover | `claude-mem:handoff` |
