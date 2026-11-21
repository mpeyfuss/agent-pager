# agent-pager

Native macOS notifications for coding agents (Claude Code, Codex, OpenCode), each with its own icon. Clicking a notification brings up the editor window (or terminal) the agent is running in.

No daemon and no third-party code. Each agent's hook runs `agent-pager hook <agent>`, which normalizes the event and launches a tiny per-agent `.app` via LaunchServices to post it.

## How it works

- `install` builds `~/Applications/AgentPager/{Claude,Codex,OpenCode}Pager.app`. Each bundle has its own bundle ID (`local.agent-pager.<agent>`) and icon and is ad-hoc signed. All three contain a copy of this binary.
- macOS only allows notifications from a process it sees as that bundle, which is why the hook starts the app with `open -n -g … --args post <event>` rather than running it as a child process.
- On click, macOS relaunches the app with no args. It reads the stored cwd and focus app, then runs `open -b <bundle> <git root>`.
- The focus app is detected from the agent's environment (`__CFBundleIdentifier` or `TERM_PROGRAM`). It defaults to VS Code.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/mpeyfuss/agent-pager/main/install.sh | sh
```

This downloads the universal binary to `~/.local/bin/agent-pager` (after checking its checksum) and runs `agent-pager setup`, which:

1. builds the three pager apps (click **Allow** on each notification prompt), then
2. adds hooks to every agent config it finds, backing up each file first. Running it again is safe.

| Agent | What setup changes | Events |
|---|---|---|
| Claude Code | `hooks` in `~/.claude*/settings.json` (also `$CLAUDE_CONFIG_DIR`, `--claude-dir DIR`) | Notification (permission / input), Stop, StopFailure |
| Codex | a marked block at the end of `~/.codex/config.toml` | Stop, PermissionRequest (no error hook exists) |
| OpenCode | `~/.config/opencode/plugins/agent-pager.ts` | execution succeeded/failed, permission.asked, form.created |

After setup, **run `/hooks` once inside Codex** to trust its hooks. Then try `agent-pager test claude approval`.

To undo everything: `agent-pager teardown`. To upgrade: re-run the install command.

From source instead: `cargo build --release && ./target/release/agent-pager setup`.
`--assets DIR` overrides the built-in icons (`DIR/<agent>.png` or `.icns`, 1024×1024).

## Releasing

Bump `version` in `Cargo.toml` (and run `cargo build` so `Cargo.lock` follows), then push to `main`.
If no `v<version>` release exists yet, GitHub Actions builds the universal binary, tags the
commit and publishes the release. The workflow can also be run by hand from the Actions tab.

The repo must be public for `install.sh` to download releases without auth.

To avoid duplicate alerts, turn off each agent's built-in notifications: Codex `[tui] notifications = false`, OpenCode `"attention.notifications": false` in `cli.json`.

## Troubleshooting

- Log: `~/Library/Logs/agent-pager.log`. Hooks never write to stdout; Codex would read that as an approval decision.
- Raw payload logging: `touch ~/.config/agent-pager/debug` (delete the file to turn it off).
- Stale icons after reinstalling: `killall NotificationCenter usernoted`.
- Per-agent sound, banner style and muting: System Settings → Notifications → Claude Code / Codex / OpenCode.
- Clicking can bring up the right VS Code window but can't select the specific terminal tab.
