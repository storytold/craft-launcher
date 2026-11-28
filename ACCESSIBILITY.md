# Accessibility

Source: `standards/app-accessibility.md` in [storytold/craft-contrib](https://github.com/storytold/craft-contrib/blob/main/standards/app-accessibility.md); do not edit this copy directly. Propose changes to the shared statement in [`storytold/craft-contrib`](https://github.com/storytold/craft-contrib).

We want people who use screen readers, braille displays, screen magnifiers, voice control, high contrast, large text, reduced motion, or keyboard navigation to be able to get their work done in the Craft applications.

## Supported environments and how accessibility works

The Craft applications draw their own interface in Rust with `egui` and expose accessibility information through **AccessKit**:

- **Windows:** UI Automation (for tools such as NVDA, JAWS, and Narrator)
- **macOS:** NSAccessibility (VoiceOver)
- **Linux and FreeBSD:** AT-SPI (Orca)
- **Web (WebAssembly):** Browser accessibility tree in modern evergreen browsers
- **Display and motion:** Light and dark themes, along with ongoing work to honour system high-contrast, text-scaling, and reduced-motion settings
- **Headless and command-line workflows:** Most Craft applications also ship a command-line and automation binary (`<app>-cli`) that can inspect and edit documents from a terminal without using the graphical interface; see [This application](#this-application) below

**Current status:** The Craft suite is in early-alpha development. We use WCAG 2.2 and W3C's [WCAG2ICT](https://www.w3.org/TR/wcag2ict-22/) as practical reference points for user needs ([RFC-0014](https://github.com/storytold/craft-contrib/blob/main/rfcs/0014-accessibility.md)), while manual testing with assistive technologies across every tool and surface is still in progress.

## Known limitations and workarounds

Shared framework improvements are tracked in [storytold/craft-libs#437](https://github.com/storytold/craft-libs/issues/437). Current suite-wide gaps include:

- **Custom drawn surfaces (canvases, pages, grids, timelines, node graphs, and layer lists):** Some custom-painted views do not yet publish complete `AccessKit` nodes, caret bounds for magnifiers, or editable states (for example, `AccessKit`'s Windows adapter reporting `Document` nodes as read-only, which can prevent typing echo in screen readers). *Workaround:* Use structured inspector panels where available, or run headless commands through `<app>-cli` when a task does not need direct canvas interaction.
- **Focus in menus, toolbars, and dialogs:** Some popups, menus, or keyboard access-key overlays may leave focus in the underlying document or not yet restore focus cleanly when dismissed. *Workaround:* Press `Escape` to close open popups, `Alt` or `F10` for menu/toolbar navigation, and `F6` to move between major regions where supported.
- **Unannounced state changes:** Shortcuts or background tasks that change state away from focus do not yet consistently announce updates through a live region.
- **Pointer-heavy interactions:** Certain timeline, node-wiring, or object-placement gestures do not yet have non-dragging keyboard alternatives.

See [This application](#this-application) below for repository-specific links and details.

## Reporting an accessibility barrier

If you run into an accessibility barrier, please open an issue using the link in [This application](#this-application) below (or let us know in the [ArtCraft Discord](https://discord.gg/artcraft) if GitHub's issue form gets in the way).

- **No personal disclosure needed:** You never have to disclose a disability or explain why you use assistive technology.
- **Screenshots and recordings are optional:** Plain-text descriptions are always welcome and sufficient.
- **Helpful details to share (when convenient):**
  - What you were trying to do, what happened, and what you expected
  - Application version (or commit) and operating system or browser
  - Input method, display settings, or assistive technology and version (for example, NVDA, JAWS, Narrator, VoiceOver, Orca, screen magnifier, high contrast, text scaling, or keyboard-only)

## How we respond

- **Triage and labels:** Maintainers and triagers label barrier reports with `accessibility` and set priority during triage based on impact—barriers that block a core workflow are treated as high-priority bugs, and barriers that risk losing work are critical.
- **Assistive-technology verification (`needs screen reader test`):** Pull requests that fix a reported assistive-technology barrier or add custom accessibility-tree or focus behaviour are labelled `needs screen reader test` so someone using assistive technology can verify a build and share what they observed; if the barrier remains, we reopen the issue.

## Note for contributors and coding agents

`ACCESSIBILITY.md` is a user-facing overview, **not** an extra rule set or audit checklist for coding agents:

- **Follow `AGENTS.md` and [`standards/accessibility.md`](https://github.com/storytold/craft-contrib/blob/main/standards/accessibility.md) for engineering work,** and keep every pull request scoped strictly to its issue. Do not widen an unrelated change to audit or rework surrounding UI.
- **Design for the actual user need—do not overbuild or apply naive readings of standards.** Accessibility guidelines exist to support real workflows and give flexible ways to meet them. Understand what a criterion genuinely requires in context before changing UI: for example, a button whose label or icon is legible and whose focus or selection ring already has clear contrast does **not** need its resting fill or border restyled to hit a 3:1 ratio, and inherently spatial or freehand tools should not have artificial keyboard grids bolted onto them.
- **Preserve visual design and shared primitives.** Use shared theme tokens and shared widgets from `storytold/craft-libs` instead of inventing one-off styling, contrast overrides, focus shims, or live-region scaffolding inside an application.
- **Be candid in pull requests.** Automated accessibility-tree checks never prove a change works with a real screen reader; describe only what was actually verified.

## Ownership

Maintained by the `craft-app-team` maintainers in [`storytold`](https://github.com/storytold). Shared updates belong in [`storytold/craft-contrib`](https://github.com/storytold/craft-contrib); repository-specific facts follow below.

## This application

**ArtCraft Launcher** (`storytold/craft-launcher`) is the desktop and web launcher for installing, updating, and opening applications in the Craft App Suite.

- **Published application builds:** macOS, Windows, Linux, FreeBSD, and Web (WebAssembly)
- **Alternative download route:** Every application installed by ArtCraft Launcher can also be downloaded directly from its GitHub Releases page under [`storytold`](https://github.com/storytold)
- **Report an accessibility barrier:** [Open an issue in `storytold/craft-launcher`](https://github.com/storytold/craft-launcher/issues/new)
- **Tracked accessibility issues:** [Open issues labelled `accessibility` in `storytold/craft-launcher`](https://github.com/storytold/craft-launcher/issues?q=is%3Aissue+state%3Aopen+label%3Aaccessibility)
- **Pull requests awaiting assistive-technology verification:** [Pull requests labelled `needs screen reader test` in `storytold/craft-launcher`](https://github.com/storytold/craft-launcher/pulls?q=is%3Apr+label%3A%22needs+screen+reader+test%22)
- **Contributor and agent rules for this repository:** [`AGENTS.md`](AGENTS.md)
