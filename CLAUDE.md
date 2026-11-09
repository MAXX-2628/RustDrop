# CLAUDE.md

The product is called **MaxxDrop** (renamed from RustDrop in 0.2). Code names keep the old name: crate `rustdrop-core`, Kotlin package `app.rustdrop`, `RUSTDROP_HOME`, mDNS `_rustdrop._tcp`, `rustdrop://` pairing links and `.rustdrop-*` staging folders. `config::data_dir` moves an old `%APPDATA%\RustDrop` to `%APPDATA%\MaxxDrop` once (if the old app still holds it open, it keeps using the old folder until the move works).

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
pnpm tauri build --no-bundle  # release exe at target\release\maxxdrop.exe
pnpm tauri build              # NSIS installer
```

- pnpm 12 enforces a minimum release age. Don't add `minimumReleaseAgeExclude`; pin an older version instead. Project rule: npm packages and crates must be at least 14 days old.
- A running `maxxdrop.exe` locks the release build. Quit it from the tray (closing the window only hides it) before rebuilding.

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
  - `identity` creates a UUID, a display name and a self-signed rcgen certificate on first run and stores them in the data dir (`RUSTDROP_HOME`, otherwise `%APPDATA%\MaxxDrop`).
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
  - `transfer` (protocol v4) works in this order:
    1. On the control connection: SEND_REQUEST, then ACCEPT or DECLINE, bound to the transfer ID (60 s expiry). ACCEPT carries a random token and a hex bitset of chunks the receiver already holds. DECLINE may carry a reason (for example, not enough free space).
    2. The sender opens up to `MAX_STREAMS` (4) data connections. Each one sends DATA_HELLO (transfer ID + token), then chunk frames: u32 index, u32 length, then the bytes.
       - Files are cut into `CHUNK` (1 MiB) pieces, numbered across all files (`Layout`).
       - The receiver writes each chunk at its offset (`disk::write_all_at`) and acknowledges it with its u32 index.
       - A broken data connection reconnects up to 3 times; its unacknowledged chunk goes back on the shared queue.
    3. The sender hashes every file in the background. Once all chunks are acknowledged, it sends TRANSFER_COMPLETE with the SHA-256 hashes.
    4. The receiver re-reads and hashes what it wrote, then answers TRANSFER_COMPLETE or TRANSFER_FAILED.
    - A data connection is accepted only from the same certificate fingerprint as the request, with the right token (compared in constant time), and at most 8 at once per transfer. Every chunk index and length is checked against the layout.
    - `Node::cancel(id)` works in both directions: the CANCEL goes on the control connection, which is otherwise quiet while data flows. For an unanswered request it means Decline. The other side gets "Cancelled by the other device".
    - `Node::pause(id)` (sender only) sends CANCEL with `pause: true`: the receiver checkpoints, keeps the staging folder and remembers its key (`Inbox.paused`, in memory). `Node::resume(id)` sends the same sources again (kept with `Source::try_clone`) under the same UI id; the receiver sees `Incoming.resumed` and the node accepts it without asking.
    - Auto-accept: `TrustedDevice.auto_accept` (`Node::set_auto_accept`). `Node::forward` answers such requests itself, so the UI never sees `incoming`.
    - History: every update goes through `Inner::report`, which merges it into `live` and, on a final state or PAUSED, saves the entry to `history.json` (newest first, 200 max). A PAUSED entry from an earlier run shows as cancelled (its sources are gone).
    - Download folder: `Node::set_downloads` saves `settings.downloads` and calls `Inbox::set_dest`; each transfer keeps the folder it started with (`Job.dest`).
    - The receiver validates every remote path with `safe_relative` and checks free space (`disk::available_space`, with a 64 MiB margin) before asking the user.
    - Staging: files land in `<dest>/.rustdrop-<key>/files/`. `key` is a hash of the sender's fingerprint plus each file's path, size and modified time.
      - Every 5 s the receiver syncs the files and saves its chunk bitset to `chunks`.
      - A transfer that breaks keeps the folder, so sending the same files again resumes it.
      - A cancel or a failed hash check deletes the folder. Folders untouched for 7 days are deleted.
      - Files are only moved into place after verification. Existing files are never overwritten; a new file gets a ` (n)` suffix instead.
    - The UI receives an `Event` stream and approves a request by sending `true` on its `oneshot` channel.
    - `cargo test --release -p rustdrop-core throughput -- --ignored --nocapture` measures loopback speed (about 500 MB/s on the dev PC).
  - `security` also rejects expired or not-yet-valid certificates, even pinned ones. `validity()` is a small DER reader; rcgen's default validity runs from 1975 to 4096.
  - `Node` drops listed devices that miss two TCP connect checks in a row (every 20 s while discovery runs). `serve` treats a handshake-less hang-up as a probe, not an error.
- Clipboard sync (`Node::set_clipboard_sync`, saved in `settings.json`, off by default):
  - Text, at most 64 KiB, goes as a CLIPBOARD message to paired devices that are currently listed. An image goes as a one-file transfer with `SendRequest.clipboard`, PNG only, at most 32 MiB (`MAX_CLIPBOARD_IMAGE`); the receiver takes it only while its own sync is on, never reports it as a transfer (`Inner.silent`), and hands the file to `Listener::clipboard_image_received`, which must delete it.
  - Links and text sent on purpose are a TEXT message (`Node::send_text`, blocking) and arrive at `Listener::text_received`. UIs show them with Copy, and Open only for http(s) links; nothing opens by itself.
  - A SHA-256 of the last text (`remember_clip`) stops a received text from being sent back.
  - Windows checks `GetClipboardSequenceNumber` every 500 ms and reads the clipboard only when it changed. It writes received text and images with `exclude_from_history` (kept out of Win+V and cloud clipboard) and records the sequence number after its own write, so that write is never sent back.
  - Android can read the clipboard only while the app has focus; received content is applied at any time. A received image is served from `cache/clipboard/` by the FileProvider `${applicationId}.files`; clips from that authority are never sent back.
- Core modules still to build (ARCHITECTURE §5): QUIC transport (TCP+TLS for now) and device_manager.
- `node` holds the app logic both platforms share.
  - `Node::new(data_dir, downloads_dir, device_name, listener)` owns a tokio runtime. It runs listen, advertise and browse, then PINGs each discovered peer to learn whether it's paired.
  - It exposes `me`, `peers`, `trusted`, `pair`, `unpair`, `rename_device`, `set_auto_accept`, `refresh`, `respond`, `send`, `send_fds`, `send_text`, `cancel`, `pause`, `resume`, `history`, `clear_history`, `set_downloads`, `set_discovery`, `start_pairing`, `stop_pairing`, `pair_with_link`, `clipboard_sync`, `set_clipboard_sync`, `clipboard_changed`, `clipboard_image` and `clipboard_image_fd`.
  - A listed paired device shows the name from the trust store (proven by the handshake), not the name it announces.
  - It reports back through the `Listener` trait: `peers_changed`, `incoming`, `transfer_changed`, `clipboard_received`, `clipboard_image_received`, `text_received`, `paired` and `failed`.
  - `TransferUpdate` is a partial update; UIs merge its non-null fields by `id`.
  - With `--features uniffi`, the same types are exported to Kotlin in package `app.rustdrop.core`. Settings are in `rust-core\uniffi.toml`, including `android = true`. `NodeError` is a `flat_error`, so Kotlin sees its Display text.
- Windows app: `windows\src-tauri\src\main.rs` is a thin Tauri shell over `node`.
  - Its `Listener` emits webview events (`peers`, `incoming`, `transfer`, `fatal`), and each command calls the matching `Node` method.
  - Commands that live only in the shell: `open_downloads`, `reveal`, `open_link` (http/https only), `autostart`/`set_autostart` (HKCU `Run` value with `--hidden`), `explorer_menu`/`set_explorer_menu` (HKCU `Software\Classes\*\shell\MaxxDrop` and `Software\Classes\Directory\shell\MaxxDrop`) and `take_launch_paths`. Both registry entries are rewritten on start, because the portable exe can move.
  - `tauri-plugin-single-instance` makes a second launch (each Explorer "Send with MaxxDrop" is one) hand its paths to the running app as a `staged` event. Closing the window hides it to the tray; Quit is in the tray menu. The window starts hidden (`visible: false`) and is shown unless `--hidden`.
  - The shell also posts system notifications (a request while the window is unfocused, and cancellations) and runs the clipboard watcher.
- Android app: `android\app\src\main\java\app\rustdrop\`.
  - `Core` (in `RustDropApp.kt`) creates the `Node` and exposes `StateFlow`s to Compose.
  - The `Application` holds a multicast lock; without it Android drops mDNS packets.
  - `Files.kt` opens picked or shared content URIs as `ParcelFileDescriptor`s, and `Core.send` hands duplicates to `Node::send_fds`, so nothing is copied. Only a provider that streams (`statSize` < 0, a pipe) gets copied to cache, and that copy's name is deleted at once. Received files are verified in app storage, then published to `Download/MaxxDrop` through MediaStore; a local copy is deleted only after it's fully written.
  - `TransferService` is a dataSync foreground service that runs while any transfer is active.
  - `Notifications.kt`: the ongoing transfer notification has a Cancel action and is a Live Update (`ProgressStyle` + `EXTRA_REQUEST_PROMOTED_ONGOING`) on API 36+. A request that arrives in the background gets a notification with Decline. `ActionReceiver` handles both buttons.
  - `TransferIsland` (`ui\Screens.kt`) is the in-app "dynamic island": the accept prompt, a progress pill (tap to expand, with Cancel), then the result for 3 s. It always uses the dark palette (`LocalIsland`).
  - minSdk is 29, the lowest that allows MediaStore Downloads without a storage permission.
  - applicationId is `app.maxxdrop`; the Kotlin namespace and package stay `app.rustdrop`.
  - `Shortcuts.kt`: Direct Share (dynamic long-lived shortcuts per listed paired device, category in `res/xml/shortcuts.xml`; the share intent carries the device id as `EXTRA_SHORTCUT_ID`) and the `SendTile` Quick Settings tile (opens the file picker). A shared `EXTRA_TEXT` without files becomes a staged link or text.
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
- Status: discovery, QR and fingerprint pairing, cross-subnet probing, parallel chunked transfer with resume, pause/resume, cancel from either side, auto-accept per device, links and text, history, free-space and certificate-expiry checks, offline-device removal, notifications, clipboard sync (text and images), device rename, the Windows tray/autostart/Explorer menu/download folder, and Android Direct Share and Quick Settings tile all work.
- Backlog: key storage in DPAPI/Keystore, IPv6/QUIC, and code signing for the Windows exe.

## Git

- One branch, `main`. Commit only after the gates pass: `cargo fmt --all --check`, clippy, `cargo test --workspace`, `cargo audit`, Android `assembleDebug` + `lintDebug`, and Windows `pnpm build`.
- `.gitignore` keeps out `Tools\`, `target\`, `node_modules`, build output, APKs and every key or keystore file. Never commit those.
- Remote: `origin`. Push with `git push origin main`.

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
