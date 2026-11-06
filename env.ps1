# RustDrop portable development environment.
# Usage (from Z:\RustDrop):   . .\env.ps1
# Affects the current PowerShell session only; nothing is written to the registry or user environment.

$Root  = $PSScriptRoot
$Tools = Join-Path $Root 'Tools'

$env:RUSTUP_HOME          = "$Tools\rust\rustup"
$env:CARGO_HOME           = "$Tools\rust\cargo"
$env:JAVA_HOME            = "$Tools\jdk"
$env:ANDROID_HOME         = "$Tools\android-sdk"
$env:ANDROID_SDK_ROOT     = $env:ANDROID_HOME
$env:ANDROID_USER_HOME    = "$Tools\android-user-home"
$env:GRADLE_USER_HOME     = "$Tools\gradle-home"
$env:npm_config_cache     = "$Tools\npm-cache"
$env:npm_config_prefix    = "$Tools\npm-global"
$env:PNPM_HOME            = "$Tools\pnpm-home"
$env:npm_config_store_dir = "$Tools\pnpm-store"

# Newest installed NDK
$ndk = Get-ChildItem "$env:ANDROID_HOME\ndk" -Directory -ErrorAction SilentlyContinue | Sort-Object { [version]$_.Name } | Select-Object -Last 1
if ($ndk) { $env:ANDROID_NDK_HOME = $ndk.FullName; $env:ANDROID_NDK_ROOT = $ndk.FullName }

$cmake = Get-ChildItem "$env:ANDROID_HOME\cmake" -Directory -ErrorAction SilentlyContinue | Sort-Object { [version]$_.Name } | Select-Object -Last 1

$paths = @(
    "$Tools\rust\cargo\bin",
    "$Tools\git\cmd",
    "$Tools\node",
    "$Tools\npm-global",
    "$Tools\pnpm",
    "$Tools\pnpm-home",
    "$Tools\jdk\bin",
    "$Tools\gradle\bin",
    "$Tools\android-sdk\platform-tools",
    "$Tools\android-sdk\cmdline-tools\latest\bin"
)
if ($cmake) { $paths += "$($cmake.FullName)\bin" }

# Drop any other rustup/cargo from PATH so the portable toolchain always wins
$rest = $env:Path -split ';' | Where-Object { $_ -and $_ -notmatch '\\\.cargo\\bin\\?$' -and $paths -notcontains $_ }
$env:Path = ($paths + $rest) -join ';'

Write-Host "RustDrop environment active ($Tools)" -ForegroundColor Cyan
