# Accessibility

Source: `standards/app-accessibility.md` in [storytold/craft-contrib](https://github.com/storytold/craft-contrib/blob/main/standards/app-accessibility.md); do not edit this copy directly. Propose changes to the shared statement in [`storytold/craft-contrib`](https://github.com/storytold/craft-contrib).

We build the Craft applications so everyone can create, edit, and share work—including people who use screen readers, braille displays, screen magnifiers, voice control, high contrast, large text, reduced motion, or keyboard-only navigation.

## Priorities and target standard

- **Desktop builds** aim to meet **WCAG 2.2 Level AA** as applied to non-web software by W3C's [WCAG2ICT](https://www.w3.org/TR/wcag2ict-22/) (matching EN 301 549 Chapter 11).
- **Web builds** (where published) aim to meet **[WCAG 2.2 Level AA](https://www.w3.org/TR/WCAG22/)** directly.
- Accessibility is treated as a core engineering requirement alongside stability ([RFC-0014](https://github.com/storytold/craft-contrib/blob/main/rfcs/0014-accessibility.md)): every UI change is expected to support keyboard operation, real focus management, assistive-technology semantics, sufficient colour contrast, and spoken feedback.

## How accessibility works and supported environments

The Craft applications render their own interface in Rust using `egui` and expose accessibility trees to the operating system or browser through **AccessKit**:

- **Windows:** UI Automation (tested with Accessibility Insights / Axe.Windows; intended for NVDA, JAWS, and Narrator)
- **macOS:** NSAccessibility (Accessibility Inspector; VoiceOver)
- **Linux and FreeBSD:** AT-SPI (Accerciser; Orca)
- **Web (WebAssembly):** Browser accessibility tree in modern evergreen browsers
- **Visual and motion settings:** Light and dark themes with at least 4.5:1 text contrast and 3:1 control and focus-indicator contrast, plus respect for system high-contrast, text-scaling, and reduced-motion preferences
- **Headless and command-line workflows:** Most Craft applications also ship a command-line and automation binary (`<app>-cli`) capable of inspecting and modifying documents from a terminal without driving the graphical interface; see [This application](#this-application) below

**Current verification status:** The Craft suite is in active early-alpha development. Automated widget-name checks (`egui_kittest`) run in tests, and platform accessibility checks are rolling out across repositories, but comprehensive manual screen-reader, braille, magnifier, and voice-control verification has not yet been completed across every surface.

## Known limitations and workarounds

These suite-wide barriers are actively tracked (shared framework work lives in [storytold/craft-libs#437](https://github.com/storytold/craft-libs/issues/437)):

- **Custom drawn surfaces (canvases, pages, grids, timelines, node graphs, and layer lists):** Some custom-painted views do not yet expose complete `AccessKit` nodes, caret bounds for magnifiers, or accurate editability states (for example, `AccessKit`'s Windows adapter reporting `Document` nodes as read-only, which can prevent typing echo in screen readers). *Workaround:* Use structured inspector panels where available, or run headless commands through `<app>-cli` when a task does not require interactive canvas drawing.
- **Focus in menus, toolbars, and dialogs:** Some popups, menus, or keyboard access-key overlays may leave focus in the underlying document or fail to restore focus when closed. *Workaround:* Press `Escape` to dismiss open popups, `Alt` or `F10` for menu/toolbar navigation, and `F6` to cycle major regions where supported.
- **Unannounced state changes:** Actions triggered away from focus—such as toggling formatting by keyboard shortcut or background export completion—do not yet consistently announce updates through a live region.
- **Drag-dependent interactions:** Certain timeline, node-wiring, or object-manipulation gestures still lack complete non-dragging keyboard equivalents outside freehand drawing.

See [This application](#this-application) below for issues specific to this repository.

## Reporting an accessibility barrier

If something gets in your way, please open an issue using the link in [This application](#this-application) below (or let us know in the [ArtCraft Discord](https://discord.gg/artcraft) if GitHub's issue form is itself a barrier).

- **No personal disclosure required:** You never need to disclose a disability or explain why you use assistive technology.
- **Screenshots and recordings are optional:** Plain-text descriptions are always enough.
- **Helpful context to include (when available):**
  - The task you were trying to complete, what happened, and what you expected
  - Application version (or commit) and operating system or browser
  - Input method, display settings, or assistive technology and version (for example, NVDA, JAWS, Narrator, VoiceOver, Orca, screen magnifier, high contrast, 200% text scaling, or keyboard-only)

## How we respond and prioritize

- **Triage and labels:** Maintainers and triagers apply the `accessibility` label ("Barrier affecting people with disabilities") and assign priority during triage.
- **Priority levels:** A barrier encountered with assistive technology or keyboard navigation is treated as a reproduced functional bug. Barriers that block a core task (opening, creating, editing, saving, reaching menus, or using dialogs) are triaged as `priority: high`; barriers that risk data or work loss are `priority: critical`.
- **Verification builds (`needs screen reader test`):** Pull requests that fix a reported assistive-technology barrier or implement custom accessibility-tree or focus logic receive the `needs screen reader test` label. While manual testing never blocks merging fixes that pass automated checks, the label remains until someone verifies a build with assistive technology and posts what they observed; if the barrier remains, we reopen the issue.

## Expectations for contributors and coding agents

Every contributor and coding agent changing user interface code is expected to follow the Craft suite's [Accessibility standard](https://github.com/storytold/craft-contrib/blob/main/standards/accessibility.md) and the repository's `AGENTS.md`:

1. Ensure every action works by keyboard in visual order with no focus trap (`Escape` closes popups and restores focus), and provide non-dragging alternatives for pointer gestures.
2. Move real `egui` focus (`request_focus`) rather than painting visual highlights alone, and expose accurate roles, translated names starting with visible labels, states, values, and platform-formatted shortcuts.
3. Maintain at least 4.5:1 text contrast and 3:1 control/focus contrast across light and dark themes, and never rely on colour alone.
4. Keep `egui_kittest` accessibility checks enabled, add keyboard and accessibility-tree assertions where warranted, and inspect the accessibility tree alongside any `ui.screenshot` check.
5. Include an honest **Accessibility** section in UI pull requests stating what automated tree checks covered and what still needs human assistive-technology testing—never claim "works with screen readers" from tree tests alone.

## Ownership and maintenance

This statement and our accessibility practices are maintained by the `craft-app-team` maintainers in [`storytold`](https://github.com/storytold). Changes to the shared statement go through [`storytold/craft-contrib`](https://github.com/storytold/craft-contrib); repository-specific details live below.

## This application

**ArtCraft Launcher** (`storytold/craft-launcher`) is the desktop and web launcher for installing, updating, and opening applications in the Craft App Suite.

- **Published application builds:** macOS, Windows, Linux, FreeBSD, and Web (WebAssembly)
- **Alternative download route:** Every application installed by ArtCraft Launcher can also be downloaded directly from its GitHub Releases page under [`storytold`](https://github.com/storytold)
- **Report an accessibility barrier:** [Open an issue in `storytold/craft-launcher`](https://github.com/storytold/craft-launcher/issues/new)
- **Tracked accessibility issues:** [Open issues labelled `accessibility` in `storytold/craft-launcher`](https://github.com/storytold/craft-launcher/issues?q=is%3Aissue+state%3Aopen+label%3Aaccessibility)
- **Pull requests awaiting assistive-technology verification:** [Pull requests labelled `needs screen reader test` in `storytold/craft-launcher`](https://github.com/storytold/craft-launcher/pulls?q=is%3Apr+label%3A%22needs+screen+reader+test%22)
- **Contributor and agent rules for this repository:** [`AGENTS.md`](AGENTS.md)
