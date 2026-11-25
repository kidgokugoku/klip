# Klip

Raycast-style cross-platform clipboard manager (macOS + Windows) with local SQLite FTS5 search, CLI access, and end-to-end encrypted (E2EE) Cloudflare R2 sync.

## Architecture

- `crates/klip-core`: Storage (`SQLite` + `FTS5`), image blob/thumbnail management, `Argon2id` + `XChaCha20-Poly1305` E2EE envelope, Cloudflare R2 / S3 sync via `rusty-s3`, privacy marker filtering, and read-only Maccy importer.
- `crates/klip-cli`: Terminal CLI (`klip list`, `klip search`, `klip copy`, `klip import-maccy`, `klip sync`).
- `apps/desktop`: Tauri v2 + SolidJS Split-Pane floating panel (`NSPanel` on macOS for fullscreen overlay + non-activating focus; `GetForegroundWindow` + `SendInput` auto-paste on Windows).

## Default Shortcuts

- **macOS global toggle**: `Control+Space`
- **Windows global toggle**: `Alt+\`` (`Alt+Backquote`)
- **In-panel**:
  - `Enter`: Paste to previously active window (configurable to copy-only in Settings)
  - `Cmd/Ctrl+Enter`: Copy without pasting (or paste when `paste_on_enter` is off)
  - `Cmd/Ctrl+Shift+P`: Pin / Unpin
  - `Cmd/Ctrl+Backspace`: Delete item
  - `Cmd/Ctrl+K`: Action menu
  - `Cmd/Ctrl+,`: Settings (paste behavior, retention by count/days/both, hotkey, ignored apps, R2 sync)
  - `Esc`: Close popover or hide panel

## Commands

```bash
# Install CLI to ~/.local/bin/klip
just install-cli

# Read-only import from Maccy
klip import-maccy

# Configure paste & retention limits (count | days | both | unlimited)
klip config --paste-on-enter true --retention both --max-items 10000 --max-days 90

# Configure Cloudflare R2 E2EE sync (prompts for R2 Secret + E2EE Passphrase)
klip sync setup \
  --endpoint https://<account-id>.r2.cloudflarestorage.com \
  --bucket clip-sync \
  --access-key-id <ACCESS_KEY_ID>

# Run desktop app in dev/local mode
just dev-desktop

# Build desktop app bundle
just build-desktop

# Cross-check Windows target from macOS
just check-win
```
