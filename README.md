# Tachi Noti

<img src="assets/tachi.png" width="160" align="right" alt="Tachi, a Border Collie in a butler's tailcoat">

Native macOS notifications for [Claude Code](https://code.claude.com) hooks, delivered by **Tachi** — a Border Collie butler who watches your sessions so you don't have to. A single fast Rust binary, zero runtime dependencies.

## Meet Tachi

Tachi is a sharp-eyed Border Collie in a tailcoat and bow tie. Herding is in his blood; these days the flock is your Claude Code sessions. He keeps quiet while you're at the terminal — a good butler never interrupts — but the moment you wander off and something needs you, he pads over with a discreet announcement: the task is done, or Claude is waiting on your word. One session, one notification; he tidies up after himself.

His portrait rides along inside the binary, so every notification arrives with his face on it.

## What he announces

- **Task complete** (`Stop`) — repo name, `repo @ branch` subtitle, Claude's last reply (truncated), elapsed time, Glass sound.
- **Needs your input** (`Notification`: permission prompt / idle) — the prompt message, Basso sound, so you can tell "done" from "waiting" by ear.
- **Focus suppression** — no notification when the terminal/IDE hosting the session is already frontmost. (A butler doesn't announce guests you're already talking to.)
- **Click-to-focus** — clicking the notification activates the right app (requires `terminal-notifier`). For VS Code-family apps (VS Code, Cursor, Antigravity, Windsurf, …) it focuses the exact window that has the session's project folder open, even with multiple windows of the same app.

## Install

```sh
cargo install --path .
tachi-noti install       # adds hooks to ~/.claude/settings.json (backs up first)
tachi-noti test          # fire a sample notification
```

Restart your Claude Code session to pick up the hooks. `tachi-noti install --scope project` writes to `./.claude/settings.json` instead. `tachi-noti uninstall` removes only tachi-noti's entries.

### Notification backend

tachi-noti auto-detects the best available backend:

1. **terminal-notifier** (`brew install terminal-notifier`) — grouping (one notification per session, replaced in place), click-to-focus, and Tachi's portrait on the notification.
2. **osascript** — built into macOS, always works. Notifications are attributed to "Script Editor"; on first use macOS asks for notification permission (System Settings → Notifications → Script Editor). No custom image on this backend.

## Configuration

Optional, at `~/.config/tachi-noti/config.toml`. Defaults shown:

```toml
focus_suppression = true   # skip notification when the session's app is frontmost
min_duration_secs = 0      # skip Stop notifications for turns shorter than this
max_body_len = 120         # truncate notification body to this many chars
# backend = "osascript"    # force a backend instead of auto-detect
# icon = "/path/to.png"    # notification image; unset = Tachi's portrait, "" = none

[sounds]
stop = "Glass"             # "" = silent; names from /System/Library/Sounds
attention = "Basso"
```

## Commands

| Command | What it does |
|---|---|
| `tachi-noti hook` | Reads a hook event JSON from stdin and notifies. Used by the hooks; always exits 0 so it can never block Claude Code. |
| `tachi-noti install [--scope user\|project]` | Safely merges hook entries into settings.json: timestamped backup, append-only (existing hooks untouched), idempotent, atomic write. Aborts rather than touch a file it can't parse. |
| `tachi-noti uninstall [--scope ...]` | Removes only tachi-noti's hook entries. |
| `tachi-noti test` | Sends a sample notification and prints the active backend. |
| `tachi-noti doctor` | Prints backend, bundle-id detection, config, install status per scope. |

## How it works

The installed hooks are:

- `UserPromptSubmit` — records a start timestamp (`~/Library/Caches/tachi-noti/sessions/<session_id>`) for duration tracking; no notification.
- `Stop` — reads the last assistant message from the transcript JSONL (tail-reads the last 256 KB, skips tool-use-only and subagent lines), computes elapsed time, notifies.
- `Notification` (matcher `permission_prompt|idle_prompt`) — relays the message.

Implementation notes:

- The session's host app is identified by the `__CFBundleIdentifier` env var (falls back to a `TERM_PROGRAM` map), used both for click-to-focus and for frontmost comparison via `lsappinfo`.
- Notification text is passed strictly via argv (AppleScript `on run argv` / discrete `Command` args) — message content can never be injected into a shell or script.
- Git branch is read directly from `.git/HEAD` (handles worktrees and detached HEAD) — no `git` subprocess.
- Tachi's portrait is embedded with `include_bytes!` and materialized to `~/Library/Caches/tachi-noti/tachi-head.png` on first use; it's passed as both `-appIcon` and `-contentImage` since recent macOS ignores the former.
- Debugging: `TACHI_NOTI_DEBUG=1` logs hook errors to `~/Library/Caches/tachi-noti/debug.log`; otherwise the hook path is silent by design.
