# claude-tray

<img src="docs/hero.png" width="900"
  alt="claude-tray: a red tray icon badged 2, beside the session menu listing one waiting, one busy and two idle Claude Code sessions">

Tray-resident status for every running Claude Code session on this machine. Tells you at a glance
— without focusing the terminal — how many sessions are running and whether any of them is blocked
waiting on you.

Display only. It never writes to Claude Code's files and never sends it anything.

```
tray icon:  ( 2 )   red disc   -> 2 sessions waiting on you
            ( 1 )   blue disc  -> 1 session working, none blocked
            ( 4 )   gray ring  -> 4 sessions, all idle
```

Click the icon for the session list. Each row carries a color chip of its own, so a session stays
recognizable as its status changes and the list re-sorts around it — the chip says *which* session,
the glyph and label say what it is doing. A session keeps its color across a resume, and two
sessions running in the same repo still get different colors.

Rows are sorted attention-first (waiting → busy → idle), then oldest-first within a status, so the
session that has been stuck longest is always at the top — as below, where a permission prompt pulls
`api-gateway-f6` to the top of the list.

<img src="docs/demo.gif" width="900"
  alt="The tray icon changing from a gray ring to blue to red as sessions start working and then block on a permission prompt, while the menu re-sorts the blocked session to the top">

## Where the data comes from

Claude Code maintains a live registry at `%USERPROFILE%\.claude\sessions\<pid>.json` (or
`%CLAUDE_CONFIG_DIR%\sessions`), rewriting a session's file on every status change:

```json
{"pid":10900,"sessionId":"45f2d40d-…","cwd":"C:\\Users\\you\\repos\\api-gateway",
 "name":"api-gateway-f6","kind":"interactive","status":"waiting",
 "waitingFor":"permission prompt","startedAt":1786632182305,"statusUpdatedAt":1786633564687}
```

- `status` is one of `busy`, `shell`, `idle`, `waiting`.
- `waitingFor` (only while `waiting`) is one of `permission prompt`, `input needed`, `dialog open`,
  `sandbox request`, `worker request`.
- `kind` is one of `interactive`, `bg`, `daemon`, `daemon-worker`; daemon kinds are not listed.

The registry is polled once per second. A session whose JSON is caught mid-write falls back to the
last good copy for that tick, so rows don't flicker.

Claude Code deletes a session's file on a clean exit, but a killed session leaves the file behind,
so each pid is verified to be a running `claude.exe` before it is listed. That check also guards
against pid reuse. If a pid exists but its image path can't be read, the session is still shown —
a stale row beats a missing one.

## Build and run

```powershell
cargo build --release
.\target\release\claude-tray.exe
```

No console window appears; look for the icon in the tray. Exit from the tray menu.

### Start it with Windows

Drop a shortcut to the release exe in the startup folder:

```powershell
$exe = "$PWD\target\release\claude-tray.exe"
$lnk = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Startup\claude-tray.lnk"
$s = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)
$s.TargetPath = $exe
$s.Save()
```

## Limitations

- Clicking a session row does nothing. Windows Terminal has no API to activate a specific tab, so
  "click to jump to that session" can't be done properly; the rows are informational.
- The menu is a snapshot from the moment it opened — Windows can't repaint an open menu, so elapsed
  times freeze until you close and reopen it.
- Menu rows use the system menu font, which is proportional, so columns are separated with `—` and
  `·` rather than aligned with padding.
- Windows caps tray tooltips at 128 characters; the hover text is a summary, not the full list.
- The color is a chip rather than colored row text: Windows will not tint menu text without
  owner-drawing the whole menu.
- Chips are a fixed 16x16 bitmap and do not scale with display DPI, so they look small above 100%.
- There are twelve colors. A thirteenth concurrent session reuses one.
