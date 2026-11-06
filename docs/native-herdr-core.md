# Native Herdr core

The `herdr-core` terminal backend connects to the native core's generation-1
JSON-RPC API. It is separate from the `herdr` backend for the Herdr TUI socket.
The supported baseline is herdr-core 0.1.18. Other protocol generations are
refused during the handshake.

## Configure a connection

For a local Unix core, add a backend with its existing socket:

```sh
muqun-gateway backend add herdr-core --id native --label 'Native Herdr' \
  --socket-path /run/user/1000/herdr-core/default.sock
muqun-gateway backend default native
```

Use your core's actual endpoint. The default follows `HC_SOCKET`, then the
standard runtime/state location for the default native session. For a named
session or a custom core configuration, pass `--socket-path` explicitly.
The Windows core uses a named pipe. A Gateway in WSL cannot open that pipe
as a Unix socket; use a stdio bridge.

A configured core can expose its connected machines through the same backend.
The Gateway does not add machines, copy SSH credentials or start Pi sessions.

For a binary-transparent stdio connection, set `core_command` on that session
in `config.json`. Its entries are an executable and its arguments, passed
without an added shell. For example, a Linux Gateway in WSL can use the Windows
core's installed CLI:

```json
{
  "id": "native",
  "label": "Native Herdr",
  "backend": "herdr-core",
  "socket_path": "",
  "core_command": [
    "/mnt/c/Users/YourName/AppData/Local/Programs/herdr-core/hc.exe",
    "connect"
  ]
}
```

The command belongs to the local configuration, never to an HTTP request.
It must write only the core protocol to stdout. Gateway owns and closes the
bridge child; it does not own the terminals behind it. `hc connect` follows
native CLI policy and can start its core when absent. Use a direct socket or
named pipe when the connection must never activate a core. Gateway's separate
`backend autostart` option is not supported for this backend.

Restart Gateway after editing its sessions. Keep the existing Gateway identity,
pairing files and device records; adding this backend does not require pairing
again. On upgrades, preserve the existing transport-encryption policy. Changing
old token-only devices to `required` would require pairing them again.

## Terminal behavior

Projects and tabs use the core's per-machine `herdr-app` layout documents and
terminal membership labels. Layout documents are read-only. Projects from
other machines include the machine name in their label. Terminal addresses
become opaque URL-safe IDs at the Gateway boundary.

Browsing a project does not change another Herdr screen's selection. Reads
request only the current viewport. Native history reads can scroll a full-screen
program, so native paging and absolute ranges are not advertised. Gateway's
captured viewport history remains separate; native addresses alone do not
prove a program incarnation for cross-restart history adoption.

`send-text` with the default paste mode honors the program's bracketed-paste
mode and does not press Enter. Embedded paste terminators are removed. Keyboard
mode sends the text bytes without paste markers. `send-keys` uses the core's
mode-aware key encoder; unsupported names are refused before any key is sent.
The core's human-input quiet rule also applies to named-key RPC calls.

The App's agent composer uses the core's guarded `terminal.send`: paste and
one submission key, with the native human-input quiet rule and blocked-program
refusal. Gateway never forces a blocked program or adds its legacy Enter retry.
An ambiguous response is reported without replaying the prompt.

ANSI output uses a read-only observer snapshot and preserves screen colors and
styles as SGR text. Plain output remains available. Observer reads request no
history and send no input, focus or resize frames.

Input uses the terminal's existing grid. It sends no resize or focus frames.
A lost connection or an ambiguous acknowledgement fails the request without
replaying the input. Reconnecting re-reads the terminal and layout state.

## Capabilities and limits

Connected native backends declare `native_herdr_core_v1`,
`native_agent_prompt_v1` and `native_ansi_viewport_v1` in their session
capabilities. The existing HTTP/SSE endpoints and legacy `herdr` compatibility
envelope remain available for Apps that accept a Gateway-declared backend.
The native generation is reported in `backend.protocol`.

This backend supports existing terminal discovery, status, viewport output,
text input, guarded prompts to existing agents, named keys and topology events.
It does not create, close, rename, split or focus desktop projects or terminals.
Agent startup, worktrees and collaboration are unsupported.

Core paths belong to that core's filesystem namespace. They are not supplied
as Gateway-host working directories or checkout roots. Native panes therefore
do not enable Gateway-host file inspection, Git operations or local agent
session adoption. Remote filesystem access needs its own transport contract.

## Source installations

Back up the installed binary, configuration and complete state directory before
replacing an existing Gateway. The state backup must include the database and
its encryption key when present. Restore matching state and configuration when
rolling back across a database migration.

Build and install from the reviewed fork revision. The official installer and
the built-in explicit updater target upstream releases; replacing this fork
with an upstream binary can remove native-core support. No release or preview
tag is needed for a local source installation.
