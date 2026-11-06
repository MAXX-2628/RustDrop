RustDrop — SECURITY.md
Official Security Architecture & Implementation Guidelines

Project: RustDrop
Security Version: 1.0
Application Version: 0.1.0
Platforms: Android and Windows
Core Language: Rust
Transport: QUIC / TLS 1.3
Cryptography: Modern, audited cryptographic libraries
Security Philosophy: Secure by Default. Private by Design.

1. Security Vision

RustDrop is a peer-to-peer file-sharing application.

Its security architecture must protect users against:

Unauthorized device connections.
Unauthorized file transfers.
Network interception.
Malicious transfer requests.
File manipulation.
Path traversal.
Malicious file names.
Clipboard data exposure.
Replay attacks.
Device impersonation.
Denial-of-service attempts.
Accidental data disclosure.

Security must be built into the Rust core rather than implemented independently by each platform.

Core Security Principles
Encrypt all transfer connections.
Authenticate communicating devices.
Require receiver approval for incoming transfers.
Never trust unknown devices automatically.
Never write files outside approved destinations.
Keep clipboard content private.
Store minimal personal information.
Avoid unnecessary cloud infrastructure.
Use established cryptographic libraries.
Fail securely when authentication or verification fails.
2. Security Architecture

All networking and transfer security must be handled by the shared Rust core.

text
                 RUSTDROP
                     |
              Platform UI
                     |
             Android / Windows
                     |
              Shared Rust Core
                     |
          +----------+----------+
          |                     |
       Identity              Security
          |                     |
          +----------+----------+
                     |
             Authenticated Peer
                     |
              Encrypted Session
                     |
             Transfer Authorization
                     |
               File Transfer
                     |
              Integrity Check
                     |
                Safe Storage

Security boundaries
Platform UI

Responsible for:

Showing security warnings.
Showing sender identity.
Requesting user approval.
Displaying trust status.
Managing user privacy settings.

The UI must never bypass security decisions.

Rust Core

Responsible for:

Device authentication.
Secure communication.
Transfer authorization.
File verification.
Session management.
Security event reporting.
Operating System

Responsible for:

Secure key storage.
File permissions.
Network permissions.
Platform sandboxing.
Background execution restrictions.
3. Cryptography Standards

RustDrop must use established cryptographic implementations.

Do not implement cryptographic algorithms manually.

Approved Components
Purpose	Recommended Technology
Transport encryption	TLS 1.3
Transport protocol	QUIC
Cryptographic library	rustls
Hashing	SHA-256
Random generation	Operating-system CSPRNG
Device IDs	UUID
Key storage	OS-protected storage
Serialization	Serde
Certificate verification	rustls verification with device trust policy
Cryptography Rules
Never use plaintext transport for production transfers.
Never disable certificate verification.
Never accept expired or invalid certificates.
Never hardcode private keys.
Never reuse temporary session secrets across sessions.
Never use MD5 or SHA-1 for security-sensitive integrity checks.
Never invent custom encryption protocols.
Keep cryptographic dependencies updated.
4. Device Identity

Every RustDrop installation needs a persistent device identity.

Identity Fields
text
DeviceIdentity
├── Device UUID
├── Device Name
├── Platform
├── Application Version
└── Public Key / Certificate Fingerprint


Device names are display information, not proof of identity.

The UUID alone must never authenticate a device.

Identity Requirements
Generate unique device identifiers.
Persist identifiers securely.
Protect private keys using platform-supported secure storage.
Rotate credentials when required.
Detect identity changes.
Never transmit private keys.
Never log private key material.
Private Key Storage
Windows

Use Windows-supported protected credential or key storage mechanisms.

Prefer non-exportable keys when supported.

Android

Use Android Keystore.

Prefer hardware-backed keys when available.

The application must continue to work securely on devices where hardware-backed key storage is unavailable, with clearly defined software-backed protection.

5. Device Pairing and Trust

RustDrop must distinguish between:

Unknown devices.
Trusted devices.
Temporarily connected devices.
Blocked devices.
First-Time Pairing

Recommended workflow:

Discover a nearby RustDrop device.
Display its name and platform.
Initiate pairing.
Exchange authenticated public-key information.
Display a verification code or QR pairing method.
Verify that both devices refer to the same pairing session.
Ask the user to approve pairing.
Store the trusted-device record.
Trust Record

Store:

text
TrustedDevice
├── Device UUID
├── Device Name
├── Platform
├── Public Key Fingerprint
├── Trust Created At
└── Last Connected At


A trusted device must be authenticated cryptographically.

Changing a device name must not silently change its identity.

Device Management

Users must be able to:

View trusted devices.
Rename local display labels.
Remove trusted devices.
Block devices.
Review last connection information.
Revoke trust.

Removing a trusted device must invalidate its authorization for future sessions.

6. Secure Connection Management

RustDrop should use QUIC with TLS 1.3 as its preferred transport.

TCP with TLS 1.3 may be used as a fallback.

Connection Requirements

Every connection must:

Authenticate the peer.
Validate the certificate.
Verify the trusted-device fingerprint.
Negotiate supported protocol versions.
Enforce connection timeouts.
Limit concurrent sessions.
Reject malformed messages.
Close invalid sessions safely.
Session Lifecycle
text
DISCOVERED
    |
    v
CONNECTING
    |
    v
AUTHENTICATING
    |
    v
TRUST_CHECK
    |
    v
CONNECTED
    |
    v
AUTHORIZED_TRANSFER
    |
    v
CLOSED


A device must not be considered trusted merely because it is discoverable on the same Wi-Fi network.

7. SEND and ACCEPT Authorization

This is one of RustDrop's most important security features.

Every incoming file transfer requires explicit receiver approval.

Incoming Request Information

Display:

Sender device name.
Sender platform.
Trusted or unknown status.
Number of files.
Total size.
Requested destination behavior.
Receiver Actions

ACCEPT

Authorizes the specific transfer request.

DECLINE

Rejects the transfer.

Authorization Rules
Never automatically accept requests from unknown devices.
Never automatically accept files solely because a device is trusted.
Require approval for each new transfer by default.
Bind approval to a specific transfer ID.
Expire unanswered requests.
Reject expired approvals.
Prevent reuse of old transfer authorization.
Permit the user to cancel an active transfer.

Trusted-device auto-accept may be considered as an explicitly opt-in future feature with carefully scoped rules.

8. Transfer Protocol Security

Every transfer must have a unique identifier and authenticated session.

Transfer Metadata
text
TransferRequest
├── Transfer ID
├── Sender Identity
├── File Count
├── Total Size
├── Relative File Paths
├── File Sizes
├── File Types
└── Integrity Metadata

Security Requirements
Enforce maximum message sizes.
Enforce configurable file-size limits.
Limit the number of files.
Validate all metadata.
Reject malformed paths.
Validate file sizes before writing.
Prevent transfer ID reuse.
Apply timeouts.
Limit concurrent transfers.
Use bounded memory buffers.
Integrity Verification

Calculate SHA-256 hashes for transferred files.

The receiver must verify the completed file before reporting success.

A failed verification must result in a failed transfer.

Never report a file as successfully received before verification and safe finalization.

9. File and Folder Safety

File transfers are a major security boundary.

RustDrop must prevent malicious filenames and directory traversal.

Path Security

Never directly trust paths received from remote devices.

Reject:

Absolute paths.
Parent-directory traversal.
Drive-letter paths.
UNC paths.
Null bytes.
Invalid path components.
Reserved platform names where applicable.
Unexpected symbolic links or special files.

Normalize and validate paths against the selected destination directory.

The receiver must never permit a remote sender to choose an arbitrary absolute destination.

Safe File Writing

Recommended process:

Select an approved destination.
Validate all relative paths.
Create files inside a controlled temporary directory.
Stream file data using bounded buffers.
Verify the completed file.
Finalize the file safely.
Report completion.

Do not overwrite existing files silently.

Offer:

Rename.
Skip.
Replace with explicit approval.
Folder Sharing

Folder transfers must preserve directory structure without permitting escape from the selected destination.

Apply limits for:

Maximum directory depth.
Maximum file count.
Maximum total size.
Maximum individual file size.

Do not follow symbolic links by default.

10. Clipboard Security

Clipboard synchronization is sensitive because clipboard data may contain:

Passwords.
Authentication codes.
Personal messages.
Private links.
Financial information.
Clipboard Rules
Clipboard sync must be opt-in.
Only text synchronization is enabled initially.
Sync only between trusted devices.
Encrypt clipboard messages.
Avoid permanent clipboard history.
Do not store clipboard contents in logs.
Provide a pause option.
Provide a clear sync-status indicator.
Avoid repeated synchronization loops.
Do not bypass operating-system clipboard privacy restrictions.
Android Restrictions

Android restricts background clipboard access.

RustDrop must respect these restrictions and should not promise unrestricted background clipboard monitoring.

Clipboard synchronization should operate only when platform rules and user consent permit it.

Windows Restrictions

Clipboard monitoring should be controlled by the user.

Provide an option to disable synchronization immediately.

11. Network Discovery Security

Discovery is not authentication.

A nearby device can advertise a false device name.

Discovery Requirements
Treat discovery announcements as untrusted input.
Validate message size.
Validate message structure.
Never authorize transfers from discovery metadata.
Authenticate the device after connection.
Rate-limit discovery processing.
Ignore malformed announcements.
Avoid unnecessary device metadata exposure.

Discovery service:

text
_rustdrop._tcp.local


The discovery protocol should expose only the minimum information needed to locate a device.

12. Denial-of-Service Protection

RustDrop must remain stable when receiving malformed or excessive network traffic.

Protection Requirements
Maximum concurrent connections.
Connection timeouts.
Message size limits.
Request rate limits.
Transfer count limits.
Bounded channels.
Bounded buffers.
Idle session expiration.
Resource cleanup.
Graceful connection shutdown.

Do not allow a remote peer to allocate unlimited memory or create unlimited transfer tasks.

13. Android Security

RustDrop must respect Android's application sandbox.

Requirements
Use Android Keystore for cryptographic keys.
Use Storage Access Framework for user-selected files.
Request minimum necessary permissions.
Avoid broad storage access.
Use foreground services for ongoing transfers where required.
Show notifications for ongoing transfers.
Respect background restrictions.
Protect sensitive local application data.
Do not expose internal files through unsafe exported components.
Android Manifest

Review:

Internet permissions.
Nearby Wi-Fi permissions where applicable.
Notification permissions.
Foreground service declarations.
Service export settings.
Intent filters.
Backup configuration.

Never request permissions without a corresponding feature.

14. Windows Security

RustDrop must respect Windows security mechanisms.

Requirements
Use protected key storage.
Store application data in appropriate user directories.
Restrict incoming network access to required ports.
Prefer private-network firewall rules.
Validate file destinations.
Avoid unnecessary administrator privileges.
Use signed release builds.
Protect update mechanisms.
Avoid arbitrary command execution.
Avoid loading untrusted plugins.
Startup Security

Automatic startup must be user-controlled.

Do not use hidden persistence mechanisms.

Provide a clear startup setting and an uninstall process that removes the application's startup registration.

15. Secrets and Configuration

Never commit secrets to source control.

Never store:

Private keys.
Signing passwords.
Release credentials.
API tokens.
Sensitive pairing secrets.

Use:

Windows protected storage.
Android Keystore.
Environment variables for build-only secrets.
Secure CI/CD secret storage.

Do not put secrets in:

Source code.
README files.
Logs.
Screenshots.
Git commits.
16. Logging and Privacy

RustDrop should collect minimal diagnostic information.

Allowed Logs
Application startup.
Connection state changes.
Transfer state changes.
Error codes.
Non-sensitive performance metrics.
Prohibited Logs
Private keys.
Authentication secrets.
Clipboard contents.
Full file contents.
Sensitive file paths.
Unnecessary personal information.

Provide a user-controlled diagnostic logging mode.

Sensitive information must be redacted.

17. Remote Network Sharing

The first release should prioritize local-network transfers.

Remote-network sharing should be a later optional feature.

If implemented, it must include:

Authenticated device identities.
Encrypted connections.
Strong transfer authorization.
Secure relay authentication if relays are used.
Abuse prevention.
Connection expiration.
Privacy-conscious metadata handling.

Never expose an unauthenticated file-transfer listener directly to the public internet.

18. Secure Error Handling

Security failures must fail closed.

Examples:

Condition	Required behavior
Invalid certificate	Reject connection
Unknown device	Require pairing
Invalid fingerprint	Reject connection
Expired request	Reject transfer
Invalid path	Reject file
Hash mismatch	Mark transfer failed
Invalid message	Close or reject session
Storage failure	Stop transfer safely
Authentication timeout	Close session
Insufficient storage	Reject before writing

Do not silently bypass security checks to improve convenience.

19. Dependency Security

Keep Rust dependencies updated.

Run:

bash
cargo audit


Check dependency updates:

bash
cargo outdated


Run formatting:

bash
cargo fmt --all --check


Run linting:

bash
cargo clippy --workspace --all-targets -- -D warnings


Run tests:

bash
cargo test --workspace


Review dependency licenses and supply-chain changes before release.

Pin and review release dependencies through the lockfile.

20. Security Testing

Security testing must cover both platforms.

Authentication Tests
Unknown device rejected or prompted for pairing.
Invalid certificates rejected.
Changed device fingerprint detected.
Revoked device rejected.
Expired sessions rejected.
Replay of expired authorization rejected.
Transfer Tests
Unauthorized transfers rejected.
Declined transfers stopped.
Cancelled transfers cleaned up.
Malformed metadata rejected.
Oversized messages rejected.
Hash mismatch detected.
Interrupted transfers recover safely.
File Safety Tests
Parent traversal rejected.
Absolute paths rejected.
Duplicate names handled safely.
Symlinks handled safely.
Existing files protected from accidental overwrite.
Insufficient storage handled.
Large file handling tested.
Clipboard Tests
Clipboard sync disabled by default.
Untrusted devices cannot sync clipboard.
Clipboard content excluded from logs.
Duplicate synchronization prevented.
Android privacy restrictions respected.
Platform Tests
Android Keystore integration tested.
Windows protected storage tested.
Firewall rules reviewed.
Permissions reviewed.
Startup behavior reviewed.
Uninstallation tested.
21. Security Incident Handling

Provide a documented process for handling security vulnerabilities.

Required Actions
Reproduce and assess the report.
Determine affected application versions.
Identify affected components.
Develop a security fix.
Test the fix.
Release a patched version.
Notify affected users when appropriate.

Provide a private vulnerability reporting contact before public release.

Do not publish exploit details before a fix is available when doing so would put users at risk.

22. Release Security Checklist

Before publishing RustDrop:

Core
TLS 1.3 enabled.
Device authentication enabled.
Certificate validation enabled.
Trusted-device management implemented.
Transfer approval implemented.
Replay protections tested.
File integrity verification implemented.
Secure file writing implemented.
Resource limits enforced.
Android
Keystore integration.
Permission review.
Storage sandbox review.
Background service review.
Release signing configured.
Windows
Protected key storage.
Firewall review.
Least-privilege operation.
Installer signing.
Startup behavior review.
Privacy
Clipboard sync opt-in.
Sensitive logging disabled.
No unnecessary data collection.
Privacy policy published.
Final
Security tests pass.
Dependency audit reviewed.
Release build reviewed.
Known vulnerabilities resolved or appropriately disclosed.
Security documentation updated.
23. Security Development Roadmap
Phase 1 — Secure Foundation
Device identity.
Key generation.
Secure key storage.
TLS 1.3.
Device authentication.
Certificate fingerprint verification.
Phase 2 — Secure Transfer
Explicit transfer authorization.
Safe file handling.
SHA-256 verification.
Resource limits.
Secure cancellation.
Phase 3 — Privacy
Clipboard consent.
Sensitive logging protection.
Trusted-device management.
Privacy settings.
Phase 4 — Hardening
Fuzz testing.
Malformed protocol testing.
Dependency auditing.
Platform security testing.
Security review.
Phase 5 — Release
Signed installers.
Signed Android packages.
Vulnerability reporting process.
Security documentation.
Release verification.
24. Mandatory Security Rules

These rules must never be violated:

No plaintext production file transfers.
No unauthenticated trusted-device connections.
No automatic acceptance of unknown transfers.
No arbitrary remote file writes.
No unverified received files reported as complete.
No clipboard synchronization without user consent.
No hardcoded cryptographic secrets.
No disabled certificate verification.
No unlimited network resource allocation.
No unnecessary administrator privileges.
No hidden automatic startup behavior.
No security bypass for convenience.
25. Final Security Standard

RustDrop must provide secure, private and reliable peer-to-peer communication.

Its security model must be based on:

Encrypted communication.
Authenticated devices.
Explicit receiver consent.
Trusted-device management.
Safe file handling.
Verified transfers.
Privacy-conscious clipboard synchronization.
Platform-native security.
Minimal data collection.

RustDrop Security Philosophy

Secure by Default.
Private by Design.
Simple by Experience.

Security is a release requirement, not an optional feature.