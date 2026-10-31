# One MCP connection for the installed Craft App Suite

`artcraft-launcher-mcp` is an opt-in, desktop-only gateway for Codex and other MCP clients.
Register it once. `apps_list` reuses ArtCraft Launcher's detection on every call, so an app
installed later can be discovered without editing the client's configuration or restarting it.
Listing apps does not open windows, download software or change client settings.

This first development increment implements live adapters for **FigCraft, DesignCraft and
VectorCraft**, plus explicit headless access to other detected Craft App CLIs. It is separate
from the launcher GUI and from a dialog that writes individual MCP entries for installed apps.

## Build and connect

```sh
cargo build --release -p artcraft-launcher-mcp
codex mcp add artcraft -- /absolute/path/target/release/artcraft-launcher-mcp
```

On Windows the executable ends in `.exe`. Other MCP clients use the same absolute executable
path as their stdio server command. Logs go to stderr; stdout contains only newline-delimited
JSON-RPC. No API key is needed for the gateway itself.

This executable is not yet included in release installers. Build it from the checkout and put
it in a stable folder before registering it. Restart the MCP client once after initial
registration; adding supported apps later does not change its five top-level tool schemas.

The launcher detects installed apps from its manifest and the operating system. The gateway
looks for `<app>-cli` beside the installed executable, in a tree's `bin` folder, then on `PATH`.
`--cli-dir /trusted/folder` adds an explicit search folder and can be repeated. Use it for
portable installations and macOS CLI archives separate from the `.app`. Only names from the
built-in Craft App catalog are considered; no arbitrary program command is accepted over MCP.
Flatpak CLIs are reached through the host when required. Missing CLIs are reported explicitly.
AppImage CLI routing and live launching of sandboxed installation formats are not implemented.

## Prompt workflow

1. Call `apps_list` to discover installed apps and their current capabilities.
2. Call `app_connect` with a catalog id, such as `figcraft`. The default mode is `live`.
3. Keep its `session` handle and call `app_tools` with a filter for the exact native schemas.
   At most 40 matches are returned; narrow a truncated search.
4. Inspect with `app_read`, edit with `app_call`, then inspect and render a preview. Native
   arguments are passed to the app, which validates its own tool schema.
5. Save a native project and any requested exports using that app's tools.

Example prompts: “Use FigCraft to inspect the selected frame” and “Use VectorCraft to create
an editable logo and show a preview”. List apps again after installing another tool through
ArtCraft Launcher.

`app_read` accepts only tools whose advertised `annotations.readOnlyHint` is explicitly `true`.
Other tools use `app_call`, conservatively annotated as potentially destructive and open-world:
it can dispatch saves, exports, scripts or app tools that contact outside services. Normal MCP
client permission policy applies; registration does not authorize every downstream action.
Native results are forwarded unchanged, including image blocks, structured results and
`isError`. Previews remain images rather than text containing base64.

## Live and headless sessions

Live mode opens a separate desktop instance on demand with a loopback control port and connects
the app's own MCP CLI. Existing windows stay open. Native executable and macOS `.app` launching
are implemented. To attach to an app already started with `--control PORT`, supply its
operator-selected localhost port:

```json
{"app":"figcraft","mode":"live","port":47800}
```

CLI initialization, tool discovery and a native document inspection must succeed before a live
connection is reported as successful. If document inspection fails (for example, DesignCraft's
welcome screen has no document), a successful native desktop UI inspection is required instead.
The connection result includes `initial_document_inspection_error`; inspect the error, then use
the app's native tools to create or open a document before editing. No document is created merely
by connecting.

| App | Live adapter in this increment |
| --- | --- |
| FigCraft | `mcp --attach 127.0.0.1:PORT` |
| DesignCraft | `mcp --connect 127.0.0.1:PORT` |
| VectorCraft | `mcp --connect 127.0.0.1:PORT` |
| Other catalog apps | Explicit `mode: "headless"`; live adapters need further implementation and release tests |

Headless mode starts the app's native `mcp` session without a window. It must be requested
explicitly, and the result labels it `headless`. It is never a fallback when live mode fails.
PdfCraft's documented MCP has no desktop-attach mode, so it belongs in this category even when
its desktop app is installed. Unsaved headless state belongs to its CLI process: save before
closing the gateway. Explicitly replacing a failed headless session starts a fresh session; it
does not recover its unsaved state. Closing the gateway terminates its owned CLI children,
but does not close desktop applications or save their projects.

Session handles select an app/endpoint, not an immutable document tab or window. The native CLI
acts on that app's active document. Inspect immediately before editing, especially after manual
changes or tab switches. Each gateway serves calls sequentially. Independent gateways can open
separate desktop instances; shared session ownership and document conflict detection are
future work.

A gateway owns at most 32 sessions. Native tool discovery is capped at 4096 tools, 128 pages
and 32 MiB across the catalog, in addition to the per-frame transport limit below.

## Protocol and failure handling

MCP `2025-06-18` over stdio is supported, with negotiation for `2025-03-26` and `2024-11-05`.
The gateway implements initialization, ping and tools. It does not advertise subscriptions,
background progress, server-initiated requests or cancellation in this increment.

Incoming and downstream JSON lines are capped at 32 MiB before the newline. Oversized or
unterminated frames close that transport. Child reads/writes have bounded queues and
60-second waits. GUI startup waits up to 20 seconds. Clients should allow more than 80 seconds
for a cold `app_connect`; a 120-second tool timeout is reasonable for testing.

A lost reply, malformed frame, mismatched id or transport timeout marks the connection
unhealthy. The operation may already have run. It is **never replayed**. Explicitly reconnect,
inspect, then decide whether anything needs retrying. Live reconnection keeps the original
endpoint. Native JSON-RPC argument rejections leave the connection healthy, so an agent can
correct an argument. Tool errors retain the native app's `isError` information.

Undo and batch atomicity are app-specific. The gateway does not enforce a uniform filesystem
sandbox; client permissions and supported native access controls still apply. Cross-app
transactions, automatic conversion, background sync and silent updates/restarts are not
implemented.

## Validation and next increments

Run the repository gate (`cargo xtask ci`). Targeted checks:

```sh
cargo test -p artcraft-launcher-mcp
cargo clippy -p artcraft-launcher-mcp --all-targets -- -D warnings
```

Unit tests cover bounded framing, malformed requests, initialization/notification handling,
response correlation, read/edit annotations and detecting a CLI added after the first scan.
Live checks should use scratch documents: connect, inspect, mutate, render, undo, save and
reopen. Install another app with the MCP client open, then discover and connect to it.

Next increments can add authenticated live adapters for other apps, document-level targeting,
shared session ownership, app-specific file roots, release packaging and an optional launcher
setup dialog. The standalone generative ArtCraft studio needs a separate automation/provider
audit; it is not assumed to follow these CLI conventions.
