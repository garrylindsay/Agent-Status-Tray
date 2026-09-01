# Agent-Status-Tray

<img src="docs/hero.png" width="900"
  alt="A red tray icon badged 2, beside the session menu listing one waiting, one busy and two idle agent sessions">

Tray-resident status for every agent session on this machine — **Claude Code** sessions and
**Cursor** cloud agents in one list. Tells you at a glance, without focusing anything, how many are
running and whether any of them is blocked waiting on you.

Display only. It reads each tool's own files, never writes to them, and never sends them anything.
Cursor's store is opened read-only.

> Forked from [joshvito/claude-tray](https://github.com/joshvito/claude-tray), which is the Claude
> Code tray this is built on. This fork adds Cursor as a second source, desktop alerts, a settings
> panel, click-to-jump and system theming. Improvements to the Claude Code side go back upstream.

## Where sessions come from

A Claude Code session leaves the registry the moment its process exits, but the conversation does
not go anywhere: it stays in the app's list, still unread, still wanting a reply. Those are listed
from the desktop app's own records, within a recent window, so a chat that is waiting on you does
not vanish from the tray just because its process has ended.

| Tool | Source | What it can say |
| --- | --- | --- |
| Claude Code, running | the session registry, the transcript, and the desktop app's own record | waiting, busy, finished-unread, finished |
| Claude Code, ended | the desktop app's records alone | finished-unread, finished |
| Cursor cloud agents | `state.vscdb`, key `cloudAgentRepository.agents.*` | running, finished-unread, finished, failed |
| Cursor local chats | `conversation-search.db`, `composerData:<id>`, and the local-agent project map | finished |

Rows also carry a **repository mark** where the tool records one: a green arrow for an open pull
request, purple for a merged one, and a purple fork for a branch that has no pull request yet. An
open pull request wins over a merged one, because it is the one still wanting something.

Only Claude Code rows can have this. Its records carry `prs` — with a `state` of `OPEN`, `MERGED`
or `CLOSED` — and `writtenBranches`. Cursor's cloud-agent records carry no branch and no pull
request at all: it stores branch-to-PR links separately under `branchMetadata.prUrl`, keyed by
branch, with a URL but no state, and nothing ties an agent to the branch it used. Marking those
rows would mean asking GitHub, which this program does not do.

A tray menu item gets a single icon, so the status dot and the repository mark share one 16px
bitmap there — the dot a size down, the mark beside it. Menu rows also keep the state in words
(`· PR open`), which the alert does not need because it has room to draw the mark full size.

Cursor's **cloud agents** carry a real status and an unread flag, so every row shows genuine state.

Its **local chats** take three sources to assemble, because no one of them is enough:

- `conversation-search.db` lists them with a timestamp, but its titles lag — the newest chat is
  usually in it with an empty title.
- `composerData:<id>` in the key/value store holds the real title and how the chat ended
  (`completed`, `aborted`, or `none`). None of those is live: Cursor never flushes a running
  composer to disk, so a local chat only ever reads as finished.
- `glass.localAgentProjects` with `glass.localAgentProjectMembership` maps each chat to a folder,
  which is what names its row — the same grouping Cursor's own sidebar uses.

They are limited to a recent window, a week by default, because there are hundreds of them and none
of them reports a live state. Only the handful inside the window have their composer record read,
so the large conversation blobs are touched a few at a time rather than in their hundreds.

Chats with no title are left out. They are scratch composers that were opened and never used —
Cursor shows nothing for them either, and they have no timestamp, so they would otherwise appear as
a run of identical rows dated to the epoch.

The Cursor status numbers are its own `aiserver.v1.BackgroundComposerStatus`, read out of the app's
bundle rather than guessed:

```
1 RUNNING    2 FINISHED    3 ERROR    4 CREATING    5 EXPIRED
```

```
tray icon:  ( 2 )   red disc   -> 2 sessions waiting on you
            ( 1 )   blue disc  -> 1 session working, none blocked
            ( 4 )   gray ring  -> 4 sessions, all idle

repo mark:  green  arrow   -> pull request open
            purple arrow   -> pull request merged
            purple fork    -> branch written, no pull request

row dots:   amber  filled  -> waiting on you
            red    filled  -> failed
            grey   filled  -> working
            blue   filled  -> finished, not looked at
            grey   hollow  -> finished
```

Right-click the icon for the session list, and a row in it to jump to that session. Rows are
sorted attention-first (waiting → busy → idle), then
oldest-first within a status, so the session that has been stuck longest is always at the top — as
below, where a permission prompt pulls `api-gateway-f6` to the top of the list.

<img src="docs/demo.gif" width="900"
  alt="The tray icon changing from a gray ring to blue to red as sessions start working and then block on a permission prompt, while the menu re-sorts the blocked session to the top">

## Desktop alerts

A tray icon only helps while you are looking at it. For the times you are not, the tray can
raise an Outlook-style alert in the corner of the screen while any session sits in a status you
asked to be told about, and repeat it on a schedule until you deal with it.

```
┌──────────────────────────────────────────────────────────────────┐
│ 2 sessions are waiting on you                                    │
│ ● api-gateway-f6 · Rate limiting rollout — WAITING 4m · permission prompt
│ ● claude-tray-97 · Claude-tray repo setup — WAITING 38s · input needed
└──────────────────────────────────────────────────────────────────┘
```

The card sizes itself to its longest row, so a conversation title is never cut short, within a
minimum and a maximum and never wider than the work area.

Rows carry the conversation's own title beside the registry name. The registry name is derived
from the folder (`claude-tray-97`), so it says *where* a session is but not what it is about, and
two sessions in the same folder differ only by a suffix. The title is read from Claude Code's
transcript for that session — `%USERPROFILE%\.claude\projects\<encoded cwd>\<sessionId>.jsonl` —
preferring a title you set (`custom-title`) over a generated one (`ai-title`). Transcripts run to
megabytes and grow constantly, so only the last 256KB is searched, and only when the file has
changed since it was last read.

The alert repeats on the interval for as long as a session stays in a watched status. A session
that *newly* enters one interrupts that schedule and alerts immediately, so a fresh permission
prompt never waits out the remainder of a repeat interval. When nothing matches any more, the
schedule re-arms, so the next one alerts at once.

Click a row in the alert to jump to that session — it raises the window hosting it and dismisses
the alert. Rows highlight under the pointer and show a hand cursor, and hovering holds the alert
open so it cannot expire on the way to a click. Clicking anywhere else just dismisses. Session
rows in the tray menu do the same thing. See the limitations below for what "jump to" can and
cannot mean.

The alert never takes focus (`WS_EX_NOACTIVATE`), so it cannot swallow a keystroke meant for the
terminal you are typing in. It is a plain Win32 window rather than a WinRT toast on purpose: a
toast needs a registered AppUserModelID and a Start-menu shortcut, and Focus Assist and the
per-app notification switches can silently suppress it — the wrong behaviour for something whose
whole job is to be seen.

## Settings

Everything is configured from **Settings…** in the tray menu, which opens a panel that stays put
while you work through it. Changes apply and are written to disk the moment you click them, so
nothing is lost on a reboot. Tick boxes toggle; the `‹ value ›` rows step through their choices —
click the row (or `›`) for the next value, `‹` for the previous. The panel closes when the pointer
leaves it, or on Esc.

Settings deliberately are not menu items: a Win32 menu closes on every click, so changing three
things meant reopening the menu three times.

### Every setting

Grouped as the panel groups them. Each writes to disk the moment you click it.

#### Show desktop alerts

**On.** The master switch. Off keeps the tray icon, the menu and the session list working, and
stops any alert appearing. Everything below still applies to what the menu shows.

#### Alert me about

Which states raise an alert. **Waiting on you** only, by default. Tick as many as you like; the dot
beside each is the one you will see on the row, so a state can be matched by eye.

| State | Dot | Means |
| --- | --- | --- |
| Waiting on you | amber, filled | Claude Code says it is blocked on a permission prompt or a question |
| Failed | red, filled | a Cursor cloud agent ended in error |
| Busy | grey, filled | working |
| Running a shell command | grey, filled | working, in a shell |
| Finished, not looked at | blue, filled | something happened since you last opened it |
| Finished | grey, hollow | ended, and you have seen it |
| Unknown / not reported | grey, hollow | the tool said nothing about this one |

**Finished, not looked at** is the useful one to add if you leave sessions running and come back to
them. **Finished** and **Unknown** will alert about nearly everything, so they are mostly for
seeing what the tray can see at all.

#### Session list

**Sort rows by** — *Attention first.*
- *Attention first* — what needs you at the top, and within a state whatever has been stuck
  longest. Failures and prompts outrank work in progress, which outranks anything finished.
- *Most recent* / *Oldest* — ignore state entirely and go purely by last activity.
- *Going cold first* — whatever is closest to losing its cached context, with everything already
  cold after it. Pairs with the countdown below.

The order also decides which rows survive the two row limits, so it is not only cosmetic.

**Context cache window** — *1 hour.* How long a Claude Code session's context is assumed to stay
cached, which drives the `cold in 37m` countdown and the *Going cold first* order. **This is your
figure, not one Claude Code publishes** — see [Going cold](#going-cold). Shorten it if you find
sessions going cold sooner than the countdown suggests; set it to **off** if the guess is not worth
having.

**Menu shows at most** — *20 rows.* The rest collapse into a `+N more` line. The ceiling is 50: a
menu longer than the screen cannot be used.

**Alert shows at most** — *4 rows.* Same idea for the alert, which also never grows past the work
area whatever this says. Four keeps an alert glanceable; raise it if you would rather see the whole
picture each time.

**Claude past chats** — *Last day.* Claude Code drops a session from its registry the moment the
process exits, but the conversation stays in the app, still unread and still wanting a reply. This
lists those, going back the chosen number of days. *Running only* lists nothing but live processes.
Raise it if you work across many conversations and come back to them; lower it if old chats crowd
out the live ones.

**Cursor local chats** — *Last week.* Cursor's ordinary (non-cloud) chats, going back the chosen
number of days. *Cloud agents only* leaves them out. These never report a live state, so they
always read as finished — the window is what keeps hundreds of them from burying the agents that do
say something.

#### Timing and sound

**Repeat alert** — *Every minute.* How often an alert comes back while a session is still in a state
you asked about. *Only once* alerts and then stays quiet until the set of sessions changes. A newly
matching session always interrupts the schedule, so a fresh prompt never waits out the remainder of
an interval.

**Alert sound** — *Notification.* Played with each alert, and again when you pick one here so you
can hear it. These are Windows event sounds, so they follow whatever sound scheme is set. *No sound*
makes alerts silent.

**Alert stays for** — *8 seconds.* How long an alert stays before it dismisses itself. *Until
clicked* leaves it up. Hovering it holds it open regardless, so it cannot expire on the way to a
click.

**Check sessions every** — *1 second.* How often each tool's files are re-read. Lower is more
responsive; higher costs less. Little of the work actually repeats at this rate — transcripts and
records are only re-read when they change, and Cursor's store no more than every few seconds — so
1 second is cheap.

**Test alert now** shows a sample alert, so the look, the sound and the position can be checked
without waiting for a session to do anything.

### The file

Settings live in `%APPDATA%\claude-tray\config.json`, written with defaults on first run so there is
something to hand-edit. A leading byte-order mark is ignored on load — Notepad and PowerShell's
`Set-Content -Encoding utf8` both write one, and JSON has no place for it, so without that every
setting in the file would silently revert to its default. Values outside sensible bounds are clamped
rather than honoured.

| Key | Setting |
| --- | --- |
| `notificationsEnabled` | Show desktop alerts |
| `notifyStatuses` | Alert me about |
| `sort` | Sort rows by |
| `cacheWindowMins` | Context cache window |
| `maxListRows` | Menu shows at most |
| `maxAlertRows` | Alert shows at most |
| `claudePastDays` | Claude past chats |
| `cursorLocalDays` | Cursor local chats |
| `repeatSecs` | Repeat alert |
| `sound` | Alert sound |
| `popupSecs` | Alert stays for |
| `pollMs` | Check sessions every |

The tray menu follows the system theme too. A Win32 menu is drawn by the shell and always uses the
light scheme unless the process opts in through two uxtheme entry points that exist only as
ordinals — `SetPreferredAppMode` (135) and `FlushMenuThemes` (136), the same pair Chromium and
Electron use. Both are resolved defensively; if either is missing the menu just stays light. The
mode is set before the first menu is built and re-applied when the system theme changes.

Both owner-drawn windows paint in the system colours: `GetSysColor` for the light scheme, the shell's dark
surface colours when apps are set to dark (`GetSysColor` predates dark mode and keeps returning
the light scheme, so there is nothing to read for that), and the Windows accent colour for the
panel's stripe, ticks and values. The theme is re-read each time a window is shown, so switching
it while the app runs is picked up without a restart.

To see either without going through the tray:

```powershell
.\target\release\agent-status-tray.exe --demo-alert
.\target\release\agent-status-tray.exe --demo-settings
```

## Going cold

Claude Code rows carry a countdown — `cold in 37m`, then just `cold`. Picking a session back up
after its context has gone means the whole conversation is sent again. The transcripts on the
machine this was built against show exactly that, for this very repository's session:

```
2026-08-31 22:47   cache_write 610,119   cache_read 42,082   gap   41 min
2026-09-01 17:02   cache_write 663,059   cache_read 42,082   gap 1077 min
2026-09-01 18:12   cache_write 706,952   cache_read 42,082   gap   58 min
```

Warm, a turn costs `input_tokens: 2` with two hundred thousand served from cache. Cold, it rewrites
six to seven hundred thousand while reading back only the same forty-odd thousand.

**The window is your figure, not one Claude Code publishes.** Nothing on disk says when a session's
prompt cache expires, and the CLI has no idle-timeout or session-lifetime option at all — a session
lives until its process ends, with no timer to count down against. So the countdown runs against
the **Context cache window** you set, and the row says "cold in" rather than anything more certain.
A forty-minute gap was already enough above, so an hour may be optimistic.

The countdown is coloured by how much is left, so a glance is enough:

```
> 30 min   green
> 10 min   amber
>  5 min   red
<= 5 min   red, flashing
cold       the row's own colour; nothing left to save
```

The flash alternates with the row's own colour rather than blanking, so nothing on the row moves as
it blinks, and its timer runs only while a row is actually in its last minutes.

Only the alert is coloured. The tray menu is a native Win32 menu whose item text is drawn by the
shell in one colour, so the countdown is there in words but not in colour. And only Claude Code
rows carry it at all: Cursor's cloud agents run on Cursor's own machines and its local chats are
finished, so neither has a conversation of yours sitting in a cache.

Launching the program while it is already running does nothing: it takes a named mutex and exits if
one is already held, because two tray icons for the same thing can only be sorted out from Task
Manager. The `--demo-` modes are not covered by it, so they still run alongside the real one.

## What the desktop app knows

The session registry says a session exists; it does not say what has happened to it, and on builds
that never write `status` it says nothing at all. The Claude desktop app keeps its own record per
session, and keeps it current:

```
%APPDATA%\Claude\claude-code-sessions\<workspace>\<project>\local_<uuid>.json
```

```json
{"sessionId":"local_193507c2-…","cliSessionId":"d476abc9-…","title":"DevQA email sending debug",
 "lastFocusedAt":1788209553150,"lastActivityAt":1788209618351,"isArchived":false}
```

`lastActivityAt` against `lastFocusedAt` is exactly what the app's blue dot means: something
happened in that session since you last looked at it. Where the registry reports no status, that
gives two of the app's states honestly — **finished, not looked at** (blue, filled) and
**finished** (hollow) — and the row's elapsed time becomes time since last activity rather than
the session's age. A status the registry does report always wins over this.

`sessionId` here is also the id the app's deep links expect. It is **not** `local_` plus the CLI
session id: the two are different uuids, related only through `cliSessionId`, so a link built from
the CLI id passes the app's format check and then matches nothing in its store.

Records are re-read only when their file changes.

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

The registry is polled once per second by default, adjustable under Settings → Check sessions
every. A session whose JSON is caught mid-write falls back to the last good copy for that tick, so
rows don't flicker.

Claude Code deletes a session's file on a clean exit, but a killed session leaves the file behind,
so each pid is verified to be a running `claude.exe` before it is listed. That check also guards
against pid reuse. If a pid exists but its image path can't be read, the session is still shown —
a stale row beats a missing one.

## Build and run

```powershell
cargo build --release
.\target\release\agent-status-tray.exe
```

The only non-obvious dependency is `rusqlite`, bundled, because Cursor's stores are SQLite. It
builds from source, so a C compiler is needed — the MSVC toolchain Rust already requires on Windows
is enough.

No console window appears; look for the icon in the tray. Exit from the tray menu.

### Rebuilding while it runs

Windows locks a running exe against writes, so `cargo build` cannot relink while the tray is
running — it has to be stopped first. Stopping it is forced on you, starting it again is not,
which is how a rebuild quietly leaves you with no tray at all. `rebuild.ps1` does both halves:

```powershell
.\rebuild.ps1
```

It stops any running copy, waits for the handle to actually close — the link step needs the file
released, not just the kill delivered — builds, and starts the result. Trailing arguments are
passed through to `cargo`, and `-NoStart` builds without relaunching, for when you are about to
start it yourself under a debugger.

A failed build still gets you a tray: the exe on disk is then the previous build, and an old tray
is worth more than none, a failed build being the moment you least want to lose sight of your
sessions. The script exits with cargo's own exit code, so it still fails honestly in a chain.

### Start it with Windows

Drop a shortcut to the release exe in the startup folder:

```powershell
$exe = "$PWD\target\release\agent-status-tray.exe"
$lnk = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Startup\agent-status-tray.lnk"
$s = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)
$s.TargetPath = $exe
$s.Save()
```

## Limitations

- Some Claude Code builds do not write a `status` field to the session file at all (observed on
  2.1.247, which advertises a `notify_idle` peer feature and appears to publish status over its
  messaging pipe instead). Where that is the case every session reads as `Unknown`, the icon stays
  a gray ring with a live count, and the waiting/busy colours never appear. Alerts still work —
  tick **Unknown / not reported** under Settings → Alert me about.
- Cursor's **local** chats never show as unread here, even when Cursor's own sidebar shows one blue.
  The sidebar reads `isUnread: r.hasUnreadMessages`, and that field is on the `composerData` record
  — but it is never written as `true`: across 483 records on this machine, 357 hold `false` and 126
  do not carry it at all, including chats the sidebar was showing blue at the time. Cursor keeps
  the live value in memory and persists only the read state, so a local chat reads as finished
  here. Cursor's **cloud** agents are unaffected: their `isUnread` is persisted, and blue works.
- The blue "finished, not looked at" dot cannot be narrowed to Claude's amber "waiting on you".
  The app decides that with `!isArchived && pendingToolPermissions.length > 0`, and
  `pendingToolPermissions` is held in memory: it appears in none of the session records on disk.
  A session with a permission prompt open therefore shows here as unread, which is true but less
  specific than what the app shows. Narrowing it would mean the messaging pipe again.
- Clicking a session row raises the window *hosting* that session, not the session itself. A
  session is a console process with no window of its own, so the row walks up the process tree to
  the first real top-level window — the Claude desktop app, Windows Terminal, a console host.
  Where a host runs several sessions in one window (the desktop app runs them all under a single
  window), every one of those rows raises that same window and lands on whichever tab was last
  open. Selecting the tab would mean driving Claude Code over the private pipe named in
  `messagingSocketPath`, which is undocumented and which this program deliberately does not touch.
  Clicking a desktop-hosted session *does* also fire the deep link that would select the exact
  session — `claude://code/continue?session=local_<sessionId>&source=desktop_action`, where
  `<sessionId>` is the one in the registry file and `local_` is the prefix the app's own session
  store uses. The app accepts the URL (a malformed id logs `code entry link invalid ?session`
  instead) but currently answers `claudeURLHandler: code entry deep link gated off`: the handler
  sits behind a server-side GrowthBook flag. Until that flag is enabled the link costs one no-op
  launch and the window raise is what you see; once it is enabled, clicks land on the right session
  with no change here. It is only sent for sessions whose `entrypoint` is `claude-desktop`, since
  firing `claude://` for a terminal-hosted session would raise the wrong application.
- The menu is a snapshot from the moment it opened — Windows can't repaint an open menu, so elapsed
  times freeze until you close and reopen it.
- Menu rows use the system menu font, which is proportional, so columns are separated with `—` and
  `·` rather than aligned with padding.
- Windows caps tray tooltips at 128 characters; the hover text is a summary, not the full list.
