# RustDrop

Simple. Elegant. Instant. — peer-to-peer file sharing between Android and Windows, built on a shared Rust core.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/DESIGN.md](docs/DESIGN.md) and [docs/SECURITY.md](docs/SECURITY.md).

## Development environment

All toolchains are portable and live in `Tools\` (git-ignored). Activate them in every new PowerShell session:

```powershell
cd Z:\RustDrop
. .\env.ps1
```

| Tool | Location |
|------|----------|
| Rust (rustup/cargo, Windows + Android targets), cargo-ndk, cargo-audit, cargo-outdated | `Tools\rust` |
| Git (MinGit) | `Tools\git` |
| Node.js 24 LTS | `Tools\node` |
| pnpm | `Tools\pnpm` |
| JDK 21 (Temurin) | `Tools\jdk` |
| Android SDK: platform-tools, android-37.0, build-tools 37.0.0, NDK 30, CMake 3.31.6 | `Tools\android-sdk` |

System prerequisites (not relocatable): Visual Studio 2022 Build Tools (MSVC + Windows SDK) and the WebView2 runtime.

## Commands

```powershell
cargo check --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p rustdrop-core
cargo ndk -t arm64-v8a build -p rustdrop-core --lib   # Android .so
cargo audit
```
