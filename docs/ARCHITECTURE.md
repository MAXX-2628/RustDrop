RustDrop
Complete Development Guide — Android + Windows

Version: 0.1.0
Project: RustDrop
Core: Rust
Platforms: Android and Windows
Distribution: Android APK / Google Play and Windows EXE installer
License: MIT (proposed)

1. Project Overview

RustDrop is a lightweight, cross-platform, peer-to-peer file-sharing application inspired by AirDrop.

Its purpose is to make transferring files between Android and Windows devices extremely simple.

The primary workflow is:

SEND → SELECT DEVICE → ACCEPT → DONE

No account registration or cloud service is required for local-network transfers.

Main Features
Automatic nearby-device discovery.
Android-to-Windows file transfers.
Windows-to-Android file transfers.
Multiple-file sharing.
Complete folder sharing.
Incoming transfer acceptance.
Transfer progress and speed.
Transfer cancellation.
Interrupted-transfer recovery.
Two-way clipboard synchronization.
Link sharing.
QR-code device pairing.
Trusted-device management.
Encrypted communication.
Optional remote-network transfers.
2. Technology Stack
Component	Technology
Core engine	Rust
Async runtime	Tokio
Device discovery	mDNS / DNS-SD
Transport	QUIC with TCP fallback
Encryption	TLS 1.3
Serialization	Serde
Device identifiers	UUID
File verification	SHA-256
Windows framework	Tauri 2
Windows frontend	React + TypeScript
Android framework	Kotlin
Android UI	Jetpack Compose
Rust Android bridge	JNI or UniFFI
Build system	Cargo + Gradle
Architecture

Android and Windows share one Rust networking and transfer engine.

The native platform interfaces communicate with that engine through platform-specific bridges.

text
                RUSTDROP
                    |
          Shared Rust Core
                    |
       +------------+------------+
       |                         |
   Windows                    Android
       |                         |
     Tauri                   Kotlin/JNI
       |                         |
    React UI               Compose UI
       |                         |
       +------------+------------+
                    |
             Network Engine
                    |
          Discovery + Security
                    |
          Peer-to-Peer Transfer

3. Install Development Tools
3.1 Install Rust

Download Rust:

https://rustup.rs/

Verify:

bash
rustc --version
cargo --version
rustup show

3.2 Install Git

https://git-scm.com/downloads

Verify:

bash
git --version

3.3 Windows development requirements

Install:

Visual Studio Build Tools.
MSVC C++ build tools.
Windows SDK.
Node.js LTS.
pnpm.
Rust MSVC target.

Install the target:

bash
rustup target add x86_64-pc-windows-msvc

3.4 Android development requirements

Install Android Studio:

https://developer.android.com/studio

Install:

Android SDK.
Android SDK Platform Tools.
Android SDK Build Tools.
Android NDK.
CMake.
JDK supported by the selected Android Gradle Plugin.

Install Rust Android targets:

bash
rustup target add aarch64-linux-android
rustup target add armv7-linux-androideabi
rustup target add x86_64-linux-android


Configure the Android NDK and Rust linker for native builds.

4. Create the Project

Create the main folder:

bash
mkdir RustDrop
cd RustDrop


Create the Rust workspace:

bash
cargo new rust-core --lib


Create the root Cargo workspace file.

Root Cargo.toml
toml
[workspace]
resolver = "2"
members = ["rust-core"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4", "serde"] }
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

rust-core/Cargo.toml
toml
[package]
name = "rustdrop-core"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
uuid.workspace = true
anyhow.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true

[[bin]]
name = "rustdrop"
path = "src/main.rs"

5. Project Structure

The final project should follow this structure:

text
RustDrop/
│
├── Cargo.toml
├── README.md
├── LICENSE
│
├── rust-core/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs
│       ├── lib.rs
│       ├── identity.rs
│       ├── config.rs
│       ├── protocol.rs
│       ├── discovery.rs
│       ├── server.rs
│       ├── security.rs
│       ├── transfer.rs
│       ├── clipboard.rs
│       └── device_manager.rs
│
├── windows/
│   ├── package.json
│   ├── src/
│   ├── src-tauri/
│   └── index.html
│
├── android/
│   ├── settings.gradle.kts
│   ├── build.gradle.kts
│   └── app/
│       └── src/main/
│
├── shared/
│   └── protocol/
│
├── tests/
│   ├── discovery/
│   ├── transfer/
│   └── security/
│
└── docs/
    ├── ARCHITECTURE.md
    ├── SECURITY.md
    └── BUILD.md

6. Phase 1 — Rust Networking Core

The Rust core is the most important part of RustDrop.

It will manage:

Device identity.
Network discovery.
Connection management.
Message communication.
Security.
Transfer coordination.
6.1 Device identity

Every installation needs a unique, persistent device ID.

The identity contains:

UUID.
Device name.
Platform.
Application version.

The ID should be generated once and securely persisted rather than regenerated on every startup.

6.2 Shared protocol

Both devices must understand the same message types.

Initial protocol:

text
HELLO
HELLO_ACK
PING
PONG
SEND_REQUEST
ACCEPT
DECLINE
TRANSFER_START
TRANSFER_PROGRESS
TRANSFER_COMPLETE
TRANSFER_FAILED
CANCEL


Messages should use versioned, length-prefixed serialization.

Never trust incoming message sizes or file paths.

6.3 Device discovery

Implement mDNS/DNS-SD discovery.

Recommended service:

text
_rustdrop._tcp.local


Advertise:

Device name.
Device ID.
Platform.
Listening port.
Protocol version.

Discovery should work on IPv4 and IPv6 where supported.

Avoid relying exclusively on broadcast packets because many networks block subnet broadcasts.

6.4 Connection management

The connection manager must:

Accept incoming connections.
Authenticate trusted devices.
Exchange device information.
Detect disconnected peers.
Reconnect when appropriate.
Limit concurrent connections.
Handle timeouts.
6.5 Security

Before enabling real file transfers, implement:

TLS 1.3.
Certificate fingerprint verification.
First-time device trust.
Persistent trusted-device records.
Explicit transfer acceptance.
Secure file destination handling.
File size limits.
Connection rate limits.

Never allow unauthenticated peers to write arbitrary files.

7. Phase 2 — File Transfer Engine

The file transfer engine is responsible for moving files between devices.

Required features
Single-file transfer.
Multiple-file transfer.
Folder transfer.
Large-file support.
Chunked transmission.
Progress reporting.
Cancellation.
Resume support.
Integrity verification.
Transfer workflow
text
Sender selects files
        |
        v
Create transfer request
        |
        v
Send metadata to receiver
        |
        v
Receiver displays ACCEPT / DECLINE
        |
        v
Receiver accepts
        |
        v
Encrypted data transfer
        |
        v
Verify file integrity
        |
        v
Save received files
        |
        v
TRANSFER COMPLETE

Transfer protocol

Each transfer should have:

Unique transfer ID.
File metadata.
File size.
Chunk index.
Chunk length.
Integrity information.
Completion status.

Use bounded buffers and backpressure to avoid excessive RAM consumption.

Use SHA-256 to verify completed files.

Folder sharing must preserve relative directory structure while preventing path traversal.

8. Phase 3 — Windows Application

Framework:

Tauri 2.
React.
TypeScript.
Rust.
Main screen

The Windows interface should have:

RustDrop logo.
Nearby devices.
SEND button.
RECEIVE button.
Connection status.
Recent transfers.
Settings.
Windows functionality
Drag and drop files.
Select folders.
Receive files.
Accept incoming requests.
Show transfer speed.
Show transfer progress.
Cancel transfers.
Open received files.
Start automatically with Windows, if enabled.
Minimize to system tray.
UI design

Use a clean dark interface with blue accents.

Avoid unnecessary menus.

The primary workflow should always remain:

SEND → DEVICE → ACCEPT → DONE.

Windows packaging

Build the Windows executable:

bash
cargo build --release


Build the Tauri application:

bash
pnpm install
pnpm tauri build


Configure the Tauri bundler for an EXE installer.

Test installation, updates, firewall behavior, startup and uninstallation.

9. Phase 4 — Android Application

Framework:

Kotlin.
Jetpack Compose.
Android SDK.
Rust JNI or UniFFI bridge.
Android interface

The Android application should include:

SEND.
RECEIVE.
Nearby devices.
Transfer notifications.
Recent transfers.
Settings.
Android functionality
Android share-sheet integration.
Send photos.
Send videos.
Send documents.
Send folders where supported.
Receive incoming files.
Accept or decline.
Display transfer progress.
Background transfer service.
Clipboard synchronization where Android permits.
Android permissions

Request only the permissions required for the features being used.

Potential requirements include:

Nearby Wi-Fi device access.
Network access.
Notifications.
Foreground service permissions.
Media access through the Android photo picker.
File access through Storage Access Framework.

Respect Android background execution restrictions and clipboard privacy rules.

Android packaging

Build a debug APK:

bash
./gradlew assembleDebug


Build a release APK:

bash
./gradlew assembleRelease


For Google Play, generate a signed Android App Bundle:

bash
./gradlew bundleRelease


Securely store signing keys outside the repository.

10. Phase 5 — Clipboard Synchronization

Clipboard synchronization will work in both directions.

Example:

text
Android copies text
        |
        v
RustDrop detects permitted clipboard change
        |
        v
Send encrypted clipboard message
        |
        v
Windows receives text
        |
        v
User pastes text


Reverse direction:

text
Windows copies text
        |
        v
RustDrop detects clipboard change
        |
        v
Android receives text
        |
        v
User pastes text


Requirements:

Text-only initial implementation.
Duplicate-change prevention.
Loop prevention.
Device trust.
User consent.
No clipboard content stored in transfer history.

Android clipboard synchronization must respect foreground and background privacy restrictions.

11. Phase 6 — SEND and ACCEPT Experience

The application should require as few actions as possible.

Sending
Open RustDrop.
Tap SEND.
Select files.
Select a nearby device.
Wait for acceptance.
Transfer starts.
Receiving
Open RustDrop or enable receiving.
Nearby device sends request.
Incoming request appears.
Tap ACCEPT.
Transfer begins.
Transfer states
text
IDLE
DISCOVERING
DEVICE_SELECTED
WAITING_FOR_ACCEPT
ACCEPTED
TRANSFERRING
VERIFYING
COMPLETED
FAILED
CANCELLED


All transfer states must be represented consistently in the Rust core and platform interfaces.

12. Phase 7 — Automatic Startup
Windows

Implement optional automatic startup using a Windows startup entry.

Requirements:

User-controlled setting.
Start minimized.
Lightweight background discovery.
Disable startup option.
Clean shutdown.
Android

Android startup behavior is subject to operating-system restrictions.

Use:

Explicit user-enabled background behavior.
Foreground services for ongoing transfers.
Appropriate notifications.
Platform-compliant restart behavior where permitted.

Do not promise unrestricted background clipboard monitoring.

13. Phase 8 — Performance Optimization

Performance is a core RustDrop requirement.

Targets for benchmarking:

Metric	Engineering target
Idle CPU	Near zero
Desktop core RAM	Under 50 MB
Discovery	Under 3 seconds on typical LAN
Clipboard latency	Under 500 ms when active
File integrity	100% verified transfers
Local transfer	Network-limited throughput
Cloud dependency	None for LAN transfers

These are targets, not measured results.

Optimization methods:

Async Tokio tasks.
Bounded channels.
Streaming file reads.
Reusable buffers.
Minimal background polling.
Efficient device discovery.
No unnecessary database activity.
Avoid duplicate file copies in memory.
14. Phase 9 — Testing

Test these combinations:

Sender	Receiver	Test
Android	Windows	File
Windows	Android	File
Android	Windows	Folder
Windows	Android	Folder
Android	Windows	Clipboard
Windows	Android	Clipboard
Windows	Windows	File
Android	Android	File

Also test:

Wi-Fi disconnection.
Device sleep.
App restart.
Large files.
Duplicate filenames.
Insufficient storage.
Declined requests.
Cancelled transfers.
Malicious file names.
Untrusted devices.
Network changes.
15. Phase 10 — Distribution
Windows

Release:

RustDrop Setup.exe.
Signed installer where feasible.
Uninstaller.
Optional automatic startup.
Update mechanism.
Android

Release:

APK for direct installation.
AAB for Google Play.
Signed release.
Privacy policy.
Permission explanations.
Release checklist
Security review completed.
Transfer verification tested.
Android permissions reviewed.
Windows firewall behavior tested.
Startup behavior tested.
Release signing configured.
Privacy documentation published.
Uninstall tested.
16. Development Timeline

Estimated schedule for one developer with AI assistance:

Phase	Duration
Rust foundation	4–6 days
Transfer engine	4–5 days
Windows application	3–5 days
Android application	4–6 days
Clipboard and actions	3–4 days
Testing and optimization	4–6 days

Estimated total: 22–32 working days.

17. Development Commands

Check the workspace:

bash
cargo check --workspace


Format:

bash
cargo fmt --all


Lint:

bash
cargo clippy --workspace --all-targets


Test:

bash
cargo test --workspace


Release:

bash
cargo build --release


Build Android:

bash
./gradlew assembleRelease


Build Windows:

bash
pnpm tauri build

18. Final Development Milestones
MVP 1.0
Device discovery.
SEND and ACCEPT.
Android and Windows support.
File and folder sharing.
Transfer progress.
Secure connections.
Trusted devices.
File integrity verification.
Version 1.1
Clipboard synchronization.
Link sharing.
QR pairing.
Transfer resume.
Improved notifications.
Version 1.2
Smart quick actions.
Improved background operation.
More efficient discovery.
Better transfer management.
Version 2.0
Optional remote-network sharing.
Secure relay.
Advanced device management.
More platform integrations.
19. Project Principles

RustDrop must always follow these principles:

Lightweight before feature-heavy.
Rust for shared networking and file-transfer logic.
Native platform integration.
Direct transfer whenever possible.
Encryption and explicit receiver consent.
No account requirement.
No cloud dependency for local sharing.
Minimal background CPU usage.
Simple SEND and ACCEPT experience.
Reliable transfers over flashy features.
20. First Development Goal

The first goal is to finish the Rust networking core.

The first demonstrable milestone is:

Start RustDrop on Windows.
Start RustDrop on Android.
Discover both devices.
Display their device names.
Establish a trusted encrypted connection.
Exchange a PING and PONG message.

Once this works, proceed to the file transfer engine.

RustDrop — Share Fast. Share Simply.