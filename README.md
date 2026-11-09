# MaxxDrop

Send files, folders, links and clipboard content between your Android phone and your Windows PC over your own network. Nothing goes through the internet or a cloud account, and every transfer is encrypted end to end.

One shared Rust core does all the networking, security and file handling. The Windows app (Tauri + React) and the Android app (Kotlin + Jetpack Compose) are thin screens on top of it.

MaxxDrop was called RustDrop before version 0.2.

## Download

Get the latest files from the [Releases page](https://github.com/MAXX-2628/MaxxDrop/releases/latest):

| File | For | Notes |
|------|-----|-------|
| `MaxxDrop-<version>-android-universal.apk` | Android 10 or newer | One APK for every phone: 64-bit ARM, 32-bit ARM and x86_64. |
| `MaxxDrop-<version>-windows-x64.exe` | Windows 10 or 11 (64-bit) | Portable: no installer. Needs the Microsoft Edge WebView2 runtime, which Windows 10/11 already include. |
| `SHA256SUMS.txt` | | Checksums to confirm the files arrived intact. |

### Install on Android

1. Open the APK on the phone. Android asks you to allow installs from your browser or file manager the first time.
2. Allow notifications when MaxxDrop asks. They show transfer progress, incoming requests and links.
3. Coming from RustDrop? MaxxDrop installs as a new app next to it. Pair again, then uninstall RustDrop.

### Run on Windows

1. Put `MaxxDrop-<version>-windows-x64.exe` anywhere, for example in your Documents folder, and double-click it.
2. The exe is not code-signed yet, so Windows SmartScreen may warn you. Choose **More info**, then **Run anyway**.
3. If Windows Firewall asks, allow MaxxDrop on **private** networks. Other devices can't reach it otherwise.
4. Closing the window keeps MaxxDrop in the tray, so your phone can still send to the PC. Quit it from the tray icon.

Coming from RustDrop on Windows, your pairings move over automatically the first time MaxxDrop starts.

Received files go to `Downloads\MaxxDrop` on Windows (you can choose another folder in Settings) and to `Download/MaxxDrop` on Android.

## Use it

1. **Pair once.** On Windows, open **Trusted devices** and choose **Show QR code**. On the phone, open **Settings**, tap **Scan QR code** and point the camera at the PC screen. You can also type the 64-character fingerprint shown on each device.
2. **Send.** Pick files, a folder, or a link or text, then tap the device. With two or more paired devices nearby, **Send to all** sends to each of them.
   - On Android, use **Share → MaxxDrop** from any app. Paired devices also show at the top of the share sheet, and the **Send with MaxxDrop** Quick Settings tile opens the file picker.
   - On Windows, drag files onto a device, or turn on **Send with MaxxDrop** in Explorer's right-click menu (Settings → Windows).
3. **Accept.** The receiver sees who is sending what, and nothing is written until they accept. For a device you fully trust, turn on **Accept files without asking** under its name.

The devices need to be on the same network. They can be on different subnets, for example the phone on Wi-Fi and the PC on a wired port behind the same router. After the first pairing, each device remembers the other's address and finds it directly.

## Features

- **Fast parallel transfers.** Files are cut into 1 MB pieces and sent over four connections at once, with speed and time left shown while they move.
- **Files can't arrive broken.** Every file is checked with SHA-256 after it arrives, before it is moved into place. Anything that doesn't match is deleted. Existing files are never overwritten; a copy gets a ` (1)` suffix.
- **Pause and resume.** Pause a transfer you're sending and resume it later; it continues where it stopped, without asking the receiver again. If the network drops, sending the same files again also continues.
- **Cancel from either side,** with a notification on both devices.
- **Links and text.** Send a link from your phone and open it on the PC with one click (or the other way). Links are never opened by themselves.
- **History.** The last 200 transfers stay listed across restarts: who, what, when, and a button to show received files. Clear it any time.
- **Free-space check** before accepting, with a clear message if there isn't room.
- **Clipboard sync** (off by default). Text and images you copy on one device can be pasted on the other. Turn it on in **Settings → Clipboard** on each device.
- **Rename paired devices** so the lists show the names you want.
- **Windows:** tray icon, optional start at sign-in, Explorer right-click menu, and a choice of download folder.
- **Android:** a "dynamic island" style progress pill at the top of the app, and a progress notification with a Cancel button. On Android 16 this notification is a Live Update.
- **Low battery use.** Android stops looking for devices 30 seconds after you leave the app. Transfers keep running in the background.

## Security

- TLS 1.3 with a certificate on both sides. A device is accepted only if its certificate fingerprint is in your trusted list; names and IDs prove nothing.
- QR pairing uses a one-time 128-bit code that expires after 2 minutes and stops working after 3 wrong tries.
- Every transfer needs the receiver's approval, tied to that one transfer, unless you turned on **Accept files without asking** for that one paired device. A resumed transfer is accepted again only for the same files from the same device, and only until the app closes.
- File paths from the other device are checked: no `..`, absolute paths, drive letters or reserved names.
- Expired certificates are refused.
- Clipboard content is only shared with paired devices that turned sync on, never logged, and on Windows kept out of clipboard history. Images are limited to 32 MB of PNG.
- Received links are shown, never opened automatically; only `http` and `https` links get an Open button.

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
pnpm tauri build --no-bundle   # target\release\maxxdrop.exe

# Android app (in android\)
.\gradlew assembleDebug        # app\build\outputs\apk\debug\app-debug.apk
.\gradlew assembleRelease      # signed if Tools\signing\keystore.properties exists
```

The release APK is signed with the key in `Tools\signing\`, which is never committed. Keep a backup of that folder: without the same key, future versions can't be installed as updates.

## Project layout

| Path | What it holds |
|------|---------------|
| `rust-core\` | The shared core: identity, discovery (mDNS), TLS trust, the protocol, the transfer engine and history, plus the `rustdrop` CLI. |
| `windows\` | The Tauri 2 shell (`src-tauri\`: tray, autostart, Explorer menu, clipboard) and the React UI (`src\`). |
| `android\` | The Compose app; the core is loaded through UniFFI. |
| `shared\design\` | Design tokens that both apps use. |
| `docs\` | Architecture, design and security specifications. |

Code names (`rustdrop-core`, the `app.rustdrop` Kotlin package, the `RUSTDROP_HOME` variable) keep the old name; only what users see changed.

## License

MIT. See [LICENSE](LICENSE).

## Developer

MaxxDrop is built by **Devmaxx J4K**.
