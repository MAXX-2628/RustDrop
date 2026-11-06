RustDrop — DESIGN.md
Official UI/UX Design System & Interface Guidelines

Project: RustDrop
Design Version: 1.0
Application Version: 0.1.0
Platforms: Android and Windows
Design Philosophy: Simple. Elegant. Instant.
Theme: Premium minimalism
Primary Accent: Electric Blue

1. Design Vision

RustDrop is a modern peer-to-peer sharing application inspired by the simplicity of AirDrop.

The interface must feel:

Elegant.
Premium.
Minimal.
Modern.
Responsive.
Lightweight.
Intuitive.
Consistent across platforms.

The user should understand how to share something without reading instructions.

The primary experience must always be:

SEND → SELECT DEVICE → ACCEPT → DONE

The design must prioritize usability over decorative effects.

Core Design Principles
Simplicity before complexity.
Consistency across platforms.
Clear visual hierarchy.
Minimal navigation.
Fast interactions.
Subtle animations.
Accessible interfaces.
Native platform behavior.
No unnecessary dialogs.
No visual clutter.
2. Brand Identity
Application Name

RustDrop

Tagline

Simple. Elegant. Instant.

Brand Personality

RustDrop should look like a premium technology product.

Its personality should communicate:

Speed.
Reliability.
Security.
Simplicity.
Modern engineering.
Logo Direction

The logo should combine:

A minimal transfer symbol.
A subtle connection or movement concept.
A clean geometric shape.
Simple typography.

Avoid complicated gradients, excessive shadows and overly detailed symbols.

Logo Variants

Create:

Primary application logo.
Monochrome logo.
Dark-background logo.
Light-background logo.
Windows application icon.
Android launcher icon.
3. Visual Design Language
Primary Theme

RustDrop uses a dark-first design system.

Dark mode should be the default because it creates a clean, premium visual appearance.

Light mode must also be supported.

Dark Theme Colors
Token	Hex	Purpose
Background	#101319	Main background
Surface	#191F29	Cards
Surface Elevated	#222B38	Dialogs and panels
Primary	#2677F5	Main actions
Primary Hover	#438BFF	Hover state
Primary Pressed	#175BC5	Pressed state
Success	#36C98F	Completed
Warning	#F2B84B	Pending
Error	#F05F69	Failed
Text Primary	#F5F7FA	Main text
Text Secondary	#A3ADBB	Secondary text
Border	#2B3543	Subtle borders
Light Theme Colors
Token	Hex	Purpose
Background	#F7F9FC	Main background
Surface	#FFFFFF	Cards
Surface Elevated	#F0F3F8	Elevated panels
Primary	#1768E8	Main actions
Success	#16885C	Completed
Warning	#A86B08	Pending
Error	#D63D4B	Failed
Text Primary	#17202D	Main text
Text Secondary	#657184	Secondary text
Border	#E2E7EF	Borders
Color Usage Rules
Blue is reserved for primary actions and selection.
Green represents successful operations.
Amber represents pending operations.
Red represents errors and destructive actions.
Neutral colors should dominate the interface.
Avoid using multiple bright accent colors simultaneously.

Do not use gradients as the default background for ordinary cards or buttons.

Gradients may be used sparingly for branded artwork.

4. Typography

Typography must be clean, readable and consistent.

Recommended Fonts
Windows

Primary: Segoe UI Variable.

Fallback: Segoe UI.

Android

Primary: Roboto or Android system typography.

Shared Web Components

Primary: Inter.

Fallback:

css
font-family:
  Inter,
  "Segoe UI",
  Roboto,
  sans-serif;

Type Scale
Style	Size	Weight
Display	36px	Bold
Heading 1	28px	Semibold
Heading 2	22px	Semibold
Heading 3	18px	Semibold
Body Large	16px	Regular
Body	14px	Regular
Caption	12px	Regular
Small Label	11px	Medium

Use comfortable line heights.

Avoid unnecessarily small text.

Important actions must remain readable at all supported display sizes.

5. Spacing System

Use a consistent 4-point spacing system.

Token	Value
XS	4px
SM	8px
MD	12px
LG	16px
XL	20px
2XL	24px
3XL	32px
4XL	40px
5XL	48px
6XL	64px
Layout Rules
Maintain consistent screen margins.
Use sufficient space between unrelated elements.
Keep related elements visually grouped.
Avoid overly compressed layouts.
Avoid unnecessary large empty regions.
Adapt spacing to platform and display size.
6. Corner Radius and Elevation
Radius Tokens
Component	Radius
Small badges	6px
Inputs	10px
Buttons	12px
Cards	16px
Large panels	20px
Dialogs	20px
Device avatars	Circular
Elevation

Use subtle depth.

Main cards: minimal elevation.
Dialogs: moderate elevation.
Floating elements: subtle shadows.
Avoid excessive shadows.
Avoid making every element appear elevated.

Dark mode should use surface contrast rather than heavy shadows.

7. Iconography

Use Lucide icons as the shared icon vocabulary where practical.

Recommended icons:

Action	Icon
Send	send
Receive	download
Device	monitor
Android	smartphone
Folder	folder
Files	files
Clipboard	clipboard
Settings	settings
History	history
Security	shield-check
Success	check-circle
Error	alert-circle
Search	search
Cancel	x
More	ellipsis
Icon Rules
Maintain consistent stroke weight.
Avoid mixing unrelated icon styles.
Use recognizable symbols.
Provide labels for important actions.
Do not use icons alone for ambiguous actions.
Decorative icons must not compete with primary actions.
8. Android UI Design
Platform

Kotlin + Jetpack Compose.

Use Android-native behavior and platform accessibility.

Navigation

Use three primary destinations:

Home.
Activity.
Settings.
Android Home Screen

The home screen must include:

Header
RustDrop logo.
Device name.
Connection status.
Settings shortcut.
Primary Actions

Two large actions:

SEND

Opens the Android document or media picker.

RECEIVE

Makes the device available for incoming transfer requests.

Nearby Devices

Display:

Device icon.
Device name.
Platform.
Availability.
Trust status.
Recent Activity

Show the latest transfers with:

File name.
Device name.
Transfer status.
Time.

Keep the main screen simple.

Android Responsive Behavior

Support:

Small phones.
Standard phones.
Large phones.
Tablets.
Portrait orientation.
Landscape orientation.

Use adaptive Compose layouts.

9. Windows UI Design
Platform

Tauri 2 + React + TypeScript.

Use native Windows conventions where appropriate.

Main Layout

Use a desktop-oriented interface.

Sidebar

Items:

Home.
Activity.
Trusted Devices.
Settings.
Main Content

Display:

Welcome message.
Local device status.
Nearby devices.
SEND action.
RECEIVE action.
Drag-and-drop area.
Activity Panel

Show:

Active transfers.
Completed transfers.
Failed transfers.
Cancelled transfers.
Windows Interaction

Support:

Mouse.
Keyboard.
Drag and drop.
Context menus.
System tray.
Notifications.
Window resizing.
High-DPI displays.

The application should remain usable at different window widths.

10. Core Component Library

Build reusable components.

Buttons

Required variants:

Primary.
Secondary.
Tertiary.
Destructive.
Icon button.

States:

Default.
Hover.
Focus.
Pressed.
Disabled.
Loading.

Primary actions must have clear labels.

Device Card

Properties:

Device name.
Device icon.
Platform.
Connection state.
Trusted status.
Last seen.

States:

Available.
Connecting.
Connected.
Unavailable.
Trusted.
Untrusted.
Transfer Card

Properties:

File name.
File type.
File size.
Device.
Transfer percentage.
Transfer speed.
Remaining time.
Status.

States:

Waiting.
Accepted.
Transferring.
Paused.
Verifying.
Completed.
Failed.
Cancelled.
Dialog

Use dialogs for important decisions.

Examples:

Incoming transfer.
Device trust request.
Permission request.
Destructive action confirmation.

Avoid unnecessary dialogs.

11. SEND and RECEIVE Experience

This is the most important interaction in RustDrop.

Sending Flow
User taps SEND.
File picker opens.
User selects files or folders.
Nearby devices appear.
User selects the receiver.
Receiver receives a request.
Receiver taps ACCEPT.
Transfer starts.
Progress appears.
Completion is displayed.
Receiving Flow
User enables receiving.
Device becomes discoverable.
Incoming request appears.
Sender identity is displayed.
File count and total size are displayed.
User chooses ACCEPT or DECLINE.
Transfer begins after acceptance.
Files are verified.
Completion notification appears.
Incoming Request Design

The request must clearly communicate:

Who is sending.
What is being sent.
How many files.
Total file size.
Whether the device is trusted.

Never automatically accept requests from unknown devices.

12. Transfer Progress Design

Transfer progress must be visually clear.

Display:

Percentage.
Transfer speed.
Total size.
Transferred size.
Estimated remaining time.
Cancel action.

Use smooth progress updates.

Do not update the UI excessively if the underlying transfer is producing high-frequency progress events.

Aggregate updates when necessary.

Completion State

Display:

Success icon.
Completed percentage.
File count.
Destination information.
Open file or folder action.

Use a subtle success animation.

13. Animation System

Animations should feel smooth and purposeful.

Recommended durations:

Animation	Duration
Button press	100ms
Hover	120ms
Card selection	160ms
Dialog	200ms
Device appearance	220ms
Transfer state	180ms
Completion	300ms
Animation Rules
Use ease-out for entrances.
Use ease-in for exits.
Keep interactions responsive.
Avoid unnecessary looping animations.
Avoid large animated backgrounds.
Avoid animations that delay user actions.
Respect reduced-motion settings.
Device Discovery Animation

New devices should appear with a subtle fade and slide.

Do not repeatedly animate devices that are already visible.

Transfer Animation

Progress indicators should update smoothly without distracting effects.

14. Empty, Loading and Error States

Every screen must have a proper state.

Empty Nearby Devices

Message:

"No devices nearby"

Description:

"Make sure both devices are connected to the same network and RustDrop is running."

Action:

"Refresh"

Searching

Message:

"Looking for nearby devices..."

Use a subtle discovery indicator.

Connection Error

Message:

"Couldn't connect"

Description:

"Check your network and try again."

Action:

"Retry"

Transfer Failed

Message:

"Transfer failed"

Display a short, understandable reason when available.

Action:

"Try again"

No Transfer History

Message:

"Nothing shared yet"

Description:

"Your recent transfers will appear here."

15. Dark and Light Mode

Support:

Dark.
Light.
System default.

Theme changes must update:

Backgrounds.
Text.
Icons.
Cards.
Borders.
Dialogs.
Buttons.
Notifications where customizable.

Never hardcode theme colors inside individual components.

All colors must come from shared design tokens.

16. Accessibility

Accessibility is mandatory.

Requirements:

Minimum 4.5:1 contrast for normal text.
Accessible names for icon-only buttons.
Screen-reader support.
Keyboard navigation on Windows.
Visible keyboard focus.
Large touch targets on Android.
Dynamic text scaling.
Reduced-motion support.
Clear error descriptions.
Status indicators that do not rely on color alone.

Important actions must remain accessible without a mouse.

17. Responsive Design
Android

Use adaptive layouts and platform-native navigation.

Windows

Suggested layout behavior:

Window Width	Layout
Below 640px	Compact layout
640–900px	Narrow desktop layout
900–1200px	Standard desktop
Above 1200px	Spacious desktop

These are design breakpoints, not fixed platform restrictions.

The interface must not overflow or clip when resized.

18. Design Token Architecture

Store shared design tokens centrally.

text
shared/
└── design/
    ├── colors.json
    ├── typography.json
    ├── spacing.json
    ├── radius.json
    ├── elevation.json
    └── animation.json


Android and Windows should consume equivalent token values.

Platform-specific adaptations are allowed when necessary for accessibility and native behavior.

Example:

json
{
  "colors": {
    "primary": "#2677F5",
    "background": "#101319",
    "surface": "#191F29",
    "success": "#36C98F",
    "error": "#F05F69"
  },
  "spacing": {
    "xs": 4,
    "sm": 8,
    "md": 12,
    "lg": 16,
    "xl": 20,
    "xxl": 24
  },
  "radius": {
    "small": 6,
    "medium": 12,
    "large": 16,
    "extraLarge": 20
  }
}

19. UI Performance Requirements

The interface must remain lightweight.

Requirements:

No blocking file operations on the UI thread.
No excessive recomposition.
No unnecessary React rerenders.
Efficient device-list updates.
Efficient transfer-progress updates.
Lazy rendering of long activity lists.
No oversized images.
Avoid excessive blur and backdrop filters.
No heavy animations.
Proper lifecycle cleanup.

Rust handles networking and transfer work.

The UI should only display application state and issue user commands.

20. Platform Consistency Rules

Android and Windows must share:

Brand identity.
Color palette.
Typography principles.
Icon vocabulary.
Transfer status terminology.
Component semantics.
Interaction principles.
Error messages.
Design token values.

However, do not force identical layouts.

Android should feel native to Android.

Windows should feel native to Windows.

The visual identity should remain recognizable on both.

21. UI/UX Quality Checklist

Before every release, verify:

Consistent color palette.
Consistent typography.
Consistent icon style.
Correct spacing.
Responsive layout.
Dark mode.
Light mode.
SEND workflow.
RECEIVE workflow.
Incoming request.
Transfer progress.
Transfer completion.
Transfer failure.
Empty states.
Loading states.
Error states.
Accessibility.
Keyboard navigation.
Screen reader support.
Reduced motion.
Android touch usability.
Windows resizing.
No UI blocking.
No visual inconsistencies.
22. Final Design Standard

RustDrop must look and feel like a polished, premium application.

The design must never look like:

An unfinished developer dashboard.
A generic admin template.
A cluttered file manager.
A web page inside a desktop window.
An application with inconsistent components.

The design should feel like a carefully crafted native product.

Final Design Identity

RustDrop

Simple. Elegant. Instant.

Premium dark-first appearance.
Electric blue accent.
Minimal interface.
Spacious layouts.
Smooth transitions.
Native Android and Windows experience.
Clear SEND and ACCEPT actions.
Consistent design language.
Lightweight implementation.

UI/UX quality is a release requirement, not a future enhancement.