# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Environment (do this first)

Every toolchain is portable and lives in `Tools\` (git-ignored). Each new PowerShell session must activate it, or a different Rust from C: is picked up and the Android tools won't be found:

```powershell
cd Z:\RustDrop; . .\env.ps1
```

- Keep everything inside `Z:\RustDrop`. Never install tools system-wide (no winget, no user-PATH or registry changes). A new tool goes under `Tools\<name>`, and `env.ps1` gets its PATH and home variable.
- System dependencies that are already present and can't be moved: VS 2022 Build Tools (MSVC 14.44, Windows SDK 10.0.26100) and the WebView2 runtime.
- Android: the SDK is in `Tools\android-sdk` (platform android-37.0, build-tools 36.0.0 and 37.0.0, NDK 30.0.16248370, CMake 3.31.6). Gradle 9.8.2 is in `Tools\gradle`; use the wrapper. `sdkmanager` is deprecated; install packages with `android --sdk=$env:ANDROID_HOME sdk install <pkg>/<ver>`.
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

Windows app (run these in `windows\`):

```powershell
pnpm install
pnpm tauri dev                # Vite dev server + app with hot reload
pnpm build                    # type-check (tsc) + Vite build only
pnpm tauri build --no-bundle  # release exe at target\release\rustdrop-windows.exe
pnpm tauri build              # NSIS installer
```

- pnpm 12 enforces a minimum release age. Don't add `minimumReleaseAgeExclude`; pin an older version instead. Project rule: npm packages and crates must be at least 14 days old.
- A running `rustdrop-windows.exe` locks the release build. Close the app before rebuilding it.

Android app (run these in `android\`):

```powershell
.\gradlew assembleDebug     # app\build\outputs\apk\debug\app-debug.apk
.\gradlew assembleRelease   # minified with R8; unsigned until a signing config exists
.\gradlew lintDebug         # Android lint; keep it free of errors
```

- `preBuild` runs `cargo ndk` (rustdrop-core with `--features uniffi`, 3 ABIs) and then `uniffi-bindgen`. Their output goes to `app\build\generated\{jniLibs,uniffi}`.
- AGP 9 has Kotlin built in. Generated sources must go in `kotlin.directories`, not `java`. Source sets reject Provider values, so pass plain paths.
- Keep `/*` out of KDoc comments: Kotlin nests block comments, so it opens a second comment that never closes.

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
  - `protocol` frames messages as a u32 BE length plus JSON, with a 1 MiB cap (sized for a SEND_REQUEST listing 1000 files).
  - `config` resolves the data and downloads folders (`RUSTDROP_HOME` and `RUSTDROP_DOWNLOADS` override them). The CLI and the apps share it.
  - `discovery` uses `mdns-sd`.
  - `server` handles HELLO/HELLO_ACK, PING/PONG, SEND_REQUEST dispatch, connection limits and timeouts.
  - QR pairing: `Node::start_pairing` opens a `security::PairingWindow` and returns a `rustdrop://pair?v=1&fp=..&code=..&addr=..` link plus a QR matrix.
    - The code is 128-bit, single use, expires after 120 s and closes after 3 wrong tries.
    - While the window is open, the TLS verifier admits unknown client certificates. `server::serve` then lets an untrusted session send only PAIR_REQUEST.
    - The scanning side (`Node::pair_with_link`, which blocks) pins the fingerprint from the QR.
    - On success both sides add each other to `trusted.json`. A device that's already trusted keeps its existing label.
  - `Node::set_discovery(false)` stops mDNS (with a goodbye packet) but keeps receiving. Android turns it off 30 s after the app leaves the screen and releases the multicast lock at the same time.
  - Finding devices across subnets (mDNS doesn't cross routers):
    - The listener prefers port 47821 (`node::DEFAULT_PORT`), and HELLO carries the sender's listening port (`DeviceInfo.port`, 0 from older peers).
    - The server saves each trusted peer's `ip:port` in `trusted.json` (`TrustedDevice.addr`, optional, so older files still load).
    - `probe_known` PINGs those saved addresses on start, on discovery resume, on refresh and after pairing, and lists the devices that answer.
  - `transfer` works in this order: SEND_REQUEST, then ACCEPT/DECLINE bound to the transfer ID (60 s expiry), then the raw file bytes in order, then TRANSFER_COMPLETE with the SHA-256 hashes, then the receiver's verdict.
    - Protocol v2: file data travels as frames (u32 length + up to 64 KiB). A length of `u32::MAX` means the sender cancelled. The sender splits the stream, so the receiver's CANCEL message can arrive mid-transfer.
    - `Node::cancel(id)` works in both directions; for an unanswered request it means Decline. The other side gets "Cancelled by the other device".
    - A receiver that cancels drains the stream before closing; otherwise the OS resets the connection and the sender never sees the CANCEL.
    - The receiver validates every remote path with `safe_relative`.
    - Files are staged in `<dest>/.rustdrop-<id>/` and only moved into place after verification. Existing files are never overwritten; a new file gets a ` (n)` suffix instead.
    - The UI receives an `Event` stream and approves a request by sending `true` on its `oneshot` channel.
- Core modules still to build (ARCHITECTURE §5): QUIC transport (TCP+TLS for now), transfer resume, receiver-side cancel, clipboard and device_manager.
- `node` holds the app logic both platforms share.
  - `Node::new(data_dir, downloads_dir, device_name, listener)` owns a tokio runtime. It runs listen, advertise and browse, then PINGs each discovered peer to learn whether it's paired.
  - It exposes `me`, `peers`, `trusted`, `pair`, `unpair`, `refresh`, `respond`, `send` and `cancel`.
  - It reports back through the `Listener` trait: `peers_changed`, `incoming`, `transfer_changed` and `failed`.
  - `TransferUpdate` is a partial update; UIs merge its non-null fields by `id`.
  - With `--features uniffi`, the same types are exported to Kotlin in package `app.rustdrop.core`. Settings are in `rust-core\uniffi.toml`, including `android = true`. `NodeError` is a `flat_error`, so Kotlin sees its Display text.
- Windows app: `windows\src-tauri\src\main.rs` is a thin Tauri shell over `node`.
  - Its `Listener` emits webview events (`peers`, `incoming`, `transfer`, `fatal`), and each command calls the matching `Node` method.
  - Two commands live only in the shell: `open_downloads` and `reveal`.
- Android app: `android\app\src\main\java\app\rustdrop\`.
  - `Core` (in `RustDropApp.kt`) creates the `Node` and exposes `StateFlow`s to Compose.
  - The `Application` holds a multicast lock; without it Android drops mDNS packets.
  - `Files.kt` copies picked or shared content URIs into cache before sending. Received files are verified in app storage, then published to `Download/RustDrop` through MediaStore; a local copy is deleted only after it's fully written.
  - `TransferService` is a dataSync foreground service that runs while any transfer is active.
  - minSdk is 29, the lowest that allows MediaStore Downloads without a storage permission.
  - QR scanning uses `zxing-android-embedded`, whose scanner asks for the camera permission itself. The manifest makes the camera optional and lets the scanner rotate.
  - Don't name a command after a local variable in `main()`. The generated macro picks up the variable instead (this happened with `trust`).
  - `reveal` only opens paths inside the downloads folder.
  - The UI reads its styles from `shared\design\*.json`, applied as CSS variables by `windows\src\tokens.ts`.
  - Filled buttons use the `primaryButton` colour token, because white text on the brand `#2677F5` is only 4.17:1 contrast.
- Search the code for `shortcut:` comments; each marks a known gap to close before release, such as the private key stored as a plain file instead of DPAPI/Keystore.
- Try it with two local instances, each with a different `RUSTDROP_HOME`: run `rustdrop id` in each, then `rustdrop trust <other-fingerprint> <name>` in each, then `rustdrop run` in both. To test a transfer, run `rustdrop send <name-or-id-prefix> <paths...>` from one instance; setting `RUSTDROP_DOWNLOADS` changes the receiving folder.
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
