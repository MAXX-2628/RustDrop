# RustDrop

Send files, folders and clipboard text between your Android phone and your Windows PC over your own network. Nothing goes through the internet or a cloud account, and every transfer is encrypted end to end.

One shared Rust core does all the networking, security and file handling. The Windows app (Tauri + React) and the Android app (Kotlin + Jetpack Compose) are thin screens on top of it.

## Download

Get the latest files from the [Releases page](https://github.com/MAXX-2628/RustDrop/releases/latest):

| File | For | Notes |
|------|-----|-------|
| `RustDrop-<version>-android-universal.apk` | Android 10 or newer | One APK for every phone: 64-bit ARM, 32-bit ARM and x86_64. |
| `RustDrop-<version>-windows-x64.exe` | Windows 10 or 11 (64-bit) | Portable: no installer. Needs the Microsoft Edge WebView2 runtime, which Windows 10/11 already include. |
| `SHA256SUMS.txt` | | Checksums to confirm the files arrived intact. |

### Install on Android

1. Open the APK on the phone. Android asks you to allow installs from your browser or file manager the first time.
2. Allow notifications when RustDrop asks. They show transfer progress and incoming requests.
3. If you installed a development build before, uninstall it first. It is signed with a different key, so Android refuses to update it.

### Run on Windows

1. Put `RustDrop-<version>-windows-x64.exe` anywhere, for example in your Documents folder, and double-click it.
2. The exe is not code-signed yet, so Windows SmartScreen may warn you. Choose **More info**, then **Run anyway**.
3. If Windows Firewall asks, allow RustDrop on **private** networks. Other devices can't reach it otherwise.

Received files go to `Downloads\RustDrop` on Windows and to `Download/RustDrop` on Android.

## Use it

1. **Pair once.** On Windows, open **Trusted devices** and choose **Show QR code**. On the phone, open **Settings**, tap **Scan QR code** and point the camera at the PC screen. You can also type the 64-character fingerprint shown on each device.
2. **Send.** Pick files or a folder, then tap the device. On Android you can also use **Share → RustDrop** from any app.
3. **Accept.** The receiver always sees who is sending what, and nothing is written until they accept.

The devices need to be on the same network. They can be on different subnets, for example the phone on Wi-Fi and the PC on a wired port behind the same router. After the first pairing, each device remembers the other's address and finds it directly.

## Features

- **Fast parallel transfers.** Files are cut into 1 MB pieces and sent over four connections at once.
- **Files can't arrive broken.** Every file is checked with SHA-256 after it arrives, before it is moved into place. Anything that doesn't match is deleted. Existing files are never overwritten; a copy gets a ` (1)` suffix.
- **Resume.** If the network drops, the receiver keeps what arrived. Send the same files again and the transfer continues where it stopped.
- **Cancel from either side,** with a notification on both devices.
- **Free-space check** before accepting, with a clear message if there isn't room.
- **Clipboard sync** (off by default). Text you copy on one device can be pasted on the other. Turn it on in **Settings → Clipboard** on each device.
- **Android:** a "dynamic island" style progress pill at the top of the app, and a progress notification with a Cancel button. On Android 16 this notification is a Live Update.
- **Low battery use.** Android stops looking for devices 30 seconds after you leave the app. Transfers keep running in the background.

## Security

- TLS 1.3 with a certificate on both sides. A device is accepted only if its certificate fingerprint is in your trusted list; names and IDs prove nothing.
- QR pairing uses a one-time 128-bit code that expires after 2 minutes and stops working after 3 wrong tries.
- Every transfer needs the receiver's approval, tied to that one transfer.
- File paths from the other device are checked: no `..`, absolute paths, drive letters or reserved names.
- Expired certificates are refused.
- Clipboard text is only shared with paired devices, never logged, and on Windows kept out of clipboard history.

Known gap: each device's private key is stored as a normal file in the app's data folder. Moving it into Windows DPAPI and the Android Keystore is planned.

The full rules are in [docs/SECURITY.md](docs/SECURITY.md).

## Build from source

All toolchains are portable and live in `Tools\`, which is git-ignored. Activate them in each new PowerShell session:

```powershell
cd Z:\RustDrop
. .\env.ps1
```

| Tool | Location |
|------|----------|
| Rust (Windows and Android targets), cargo-ndk, cargo-audit, cargo-outdated | `Tools\rust` |
| Git (MinGit) | `Tools\git` |
| Node.js 24 LTS and pnpm | `Tools\node`, `Tools\pnpm` |
| JDK 21 (Temurin) | `Tools\jdk` |
| Android SDK: platform 37, build-tools 37, NDK 30, CMake 3.31 | `Tools\android-sdk` |
| Gradle 9.8 | `Tools\gradle` |

These are system prerequisites and can't be moved into `Tools\`: Visual Studio 2022 Build Tools (MSVC and the Windows SDK), and the WebView2 runtime.

```powershell
# Rust core
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Windows app (in windows\)
pnpm install
pnpm tauri dev                 # run with hot reload
pnpm tauri build --no-bundle   # target\release\rustdrop-windows.exe

# Android app (in android\)
.\gradlew assembleDebug        # app\build\outputs\apk\debug\app-debug.apk
.\gradlew assembleRelease      # signed if Tools\signing\keystore.properties exists
```

The release APK is signed with the key in `Tools\signing\`, which is never committed. Keep a backup of that folder: without the same key, future versions can't be installed as updates.

## Project layout

| Path | What it holds |
|------|---------------|
| `rust-core\` | The shared core: identity, discovery (mDNS), TLS trust, the protocol and the transfer engine, plus the `rustdrop` CLI. |
| `windows\` | The Tauri 2 shell (`src-tauri\`) and the React UI (`src\`). |
| `android\` | The Compose app; the core is loaded through UniFFI. |
| `shared\design\` | Design tokens that both apps use. |
| `docs\` | Architecture, design and security specifications. |

## License

MIT. See [LICENSE](LICENSE).
