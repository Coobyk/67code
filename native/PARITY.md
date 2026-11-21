# Native client ↔ web UI parity audit

Audit of `apps/web` user-facing surfaces against `native/app` + `native/server`.
Status: ✅ implemented · 🟡 degraded/partial · ❌ missing · 🚫 web-only (noted, not planned)

Last audited: 2026-11-15.

## Wire protocol budget

TS server exposes ~160 WS methods (`packages/contracts/src/rpc.ts` `WS_METHODS`).
The Rust server implements: subscribeShell, subscribeThread, subscribeServerConfig,
subscribeAuthAccess, dispatchCommand, getOlderSettledThreads, getThreadProjection,
getArchivedShellSnapshot, listAcpRegistrySessions, importAcpRegistrySession,
deleteAcpRegistrySession, logoutAcpRegistry, listAcpRegistryProviders, plus HTTP:
pairing links, websocket tickets, health. Every ❌ below that says "needs RPC"
requires a new `native/server` handler matching the TS wire shape.

## Chat surface

| Feature                                           | Status | Notes                                                                                                                                             |
| ------------------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| Thread timeline + markdown                        | ✅     | markdown.rs incl. tables, code blocks                                                                                                             |
| Streaming turn items (thinking, tool calls, text) | ✅     |                                                                                                                                                   |
| Working/elapsed indicator                         | ✅     | pulsing ● + ticking "Working for Ns"                                                                                                              |
| Interactive approvals                             | ✅     | runtime-request.respond; auto-approve under full-access                                                                                           |
| Stop turn (composer Stop)                         | ✅     |                                                                                                                                                   |
| Queued dispatch / steer active run                | ✅     | `⏵ Queue ▾` picker on busy threads → `queue_after_active`/`steer_active`/`restart_active` with `targetRunId`                                      |
| Command output panel (terminal tool calls)        | ✅     | expandable detail rows + capped output                                                                                                            |
| Compaction display                                | ✅     | compaction_row divider with token counts                                                                                                          |
| Background work / delegated tasks                 | ✅     | `subagent` rows render; child-thread approval requests route to the owning thread (item threadId) + "Approval needed in delegated task" jump chip |
| Pending user-input panel (questions)              | ✅     | question option click-select + answers                                                                                                            |
| Attachments: file path + clipboard image          | ✅     | Attach field, paste-to-stage                                                                                                                      |
| Image thumbnails in composer strip                | ✅     | decoded RenderImage thumbnails + remove                                                                                                           |
| Drag & drop files onto composer                   | ✅     | ExternalPaths drop → staged attachments                                                                                                           |
| @mentions (files/threads/context chips)           | ✅     | `@` word-start popup: thread titles + debounced `projects.searchEntries` files                                                                    |
| Prompt history (up-arrow recall)                  | ✅     | per-project ClientConfig history                                                                                                                  |
| Message stash                                     | ✅     | ctrl-shift-s stash/restore, per-project ClientConfig                                                                                              |
| Model effort picker                               | ✅     | effort chip → modelSelection.options                                                                                                              |
| Usage-limit banner in composer                    | ✅     | `server.getUsageSummary` banner + provider `usageLimits` windows (N% of limit + reset time) when the wire reports them; heavy-use fallback        |
| Dictation/speech button                           | ✅     | mic chip + mod+shift-d → pw-record/parecord/arecord PCM → `environment.transcribePcm` → insert; cloud speech keys in settings                     |
| Citations on assistant messages                   | ✅     | `t3-citation://` links parsed into AssistantCitation chips → jump to cited thread (no provider emits citation items — union checked)              |
| Message feedback / copy                           | ✅     | copy chips + 👍/👎 on assistant rows → `provider.uploadFeedback` (comment on thumbs-down)                                                         |
| Expanded image viewer dialog                      | ✅     | click thumbnail → full overlay                                                                                                                    |
| Canvas/right-panel layout                         | ✅     | right panel Files/Terminals/PRs/Diff tabs at 280px matching web                                                                                   |

## Sidebar

| Feature                                          | Status | Notes                                                                                                                            |
| ------------------------------------------------ | ------ | -------------------------------------------------------------------------------------------------------------------------------- |
| Projects + threads list                          | ✅     |                                                                                                                                  |
| Search, rename, archive/unarchive, delete, pin   | ✅     |                                                                                                                                  |
| Status indicators (running dot, PR link, unread) | ✅     | running dot, ⧉ PR glyph, 😴 snoozed, unread markers                                                                              |
| Drag-reorder threads                             | ✅     | pointer drag on pinned rows with spread order keys → `thread.pin.reorder`                                                        |
| Snooze + custom snooze                           | ✅     | presets + custom "snooze until" field (relative durations / ISO parse) → `thread.snooze.until`                                   |
| Undo-notice toasts on archive/delete             | ✅     | toast action — Undo archive (`thread.unarchive`) / unsnooze; delete = plain notice (no un-delete RPC exists)                     |
| Draft threads block                              | ✅     | client-side drafts persisted on `ClientConfig.draft_threads`; sidebar block with open/delete/collapse                            |
| Project icons/favicons                           | ✅     | `faviconPath` images via `assets.createUrl`, `projectIcon` chips + picker, monogram fallback                                     |
| Settings gear                                    | ✅     | footer gear + ctrl-,                                                                                                             |
| Update pills / server version                    | ✅     | `environment.serverVersion` in Server group + "Check for updates" → `server.getNpmRelease`; sticky update toast on newer release |

## Terminal drawer

| Feature                                   | Status | Notes                                                                                                                                                                                                |
| ----------------------------------------- | ------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Persistent per-thread terminals           | ✅     | `terminal.open/attach/write/resize/clear/close` + metadata subscribe; live grid resize (250ms debounce); VT emulator; ~200KB scrollback; no drag-resize; `terminal.observe` for other threads unused |
| Agent-owned terminal contexts in composer | ✅     | `ctx` chip stages ANSI-stripped terminal tails as `context.records` on `message.dispatch`, scoped to owning thread                                                                                   |

## Files / previews

| Feature                                       | Status | Notes                                                                                                        |
| --------------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------------ |
| File explorer pane                            | ✅     | right-panel Files tab: lazy `projects.listEntries` tree, expand/collapse, explorer_reveal                    |
| Text file preview                             | ✅     | `projects.readFile` bottom-split preview with byte size + truncation notice                                  |
| Markdown preview                              | ✅     | readFile → markdown.rs render for .md paths                                                                  |
| Image preview                                 | ✅     | `assets.createUrl` + bearer GET → image decode (image crate)                                                 |
| File write/save                               | ✅     | inline editor w/ dirty indicator + Ctrl-S → `projects.writeFile`; truncated previews stay read-only          |
| Project content search dialog                 | ✅     | ctrl-shift-f: `projects.searchContents`/`searchEntries`, case/word/regex toggles, Enter→explorer reveal      |
| PDF/office/notebook/font/audio/video previews | ✅     | external-open card (icon/name/size/type) + "Open externally" via xdg-open; truncated text offers the same    |
| Agent browser preview panes                   | ✅     | Browser tab lists `preview.list` sessions + "Open in browser" via xdg-open (no embedded webview — by design) |

## Source control & PRs

| Feature                     | Status | Notes                                                                                                                                                             |
| --------------------------- | ------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Branch picker in composer   | ✅     | `⎇` chip → `vcs.listRefs` picker → `switchRef`/`createRef`; hidden on non-repo projects                                                                           |
| Worktree create/remove      | ✅     | PRs-pane Worktrees section: create + two-step remove via `vcs.createWorktree`/`removeWorktree` (verified end-to-end)                                              |
| VCS status chips            | ✅     | `subscribeVcsStatus` drives PRs-pane header + composer branch chip                                                                                                |
| Git actions (pull, init)    | ✅     | status-card Refresh/Pull + Init on non-repo; `git.runStackedAction` not surfaced (no client-side action)                                                          |
| Pull requests list/detail   | ✅     | `pullRequests.list` (+`subscribeRefreshes` invalidation) with expandable rows: `pullRequests.detail` checks/reviewers/mergeability + `pullRequests.linkedThreads` |
| PR diff/review sheets       | ✅     | `review.getDiffPreview` unified-patch pane (Diff tab); PR "Files changed" wired via `pullRequests.diffFileContents` (needs GitHub auth to exercise)               |
| Link PR to thread           | ✅     | `thread.pull-request.link`/`.unlink` from pinned row + detail; badge renders                                                                                      |
| Source-control sign-in flow | ✅     | `sourceControl.signInStart` surfaces server guidance (`gh auth login`) + copyable command — server cannot drive a device flow                                     |

## Settings (web has ~30 routes; native has 16 sections)

| Section                                                                                                               | Status | Notes                                                                                                                                                                                  |
| --------------------------------------------------------------------------------------------------------------------- | ------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Connection / Environments / Appearance / Providers / ACP Sessions / Authorized Clients / Archived / Shortcuts / About | ✅     |                                                                                                                                                                                        |
| Notifications                                                                                                         | ✅     | `notificationMode` (off/notifications/sound/both) + in-app toast toggle persisted; settle/approval alerts fan out to `notify-send` + sound player (best-effort, no-ops headless)       |
| Chat defaults (mode/model defaults)                                                                                   | ✅     | `defaultModelSelection`/`fallbackModelSelection`/`defaultRuntimeMode`/`defaultThreadEnvMode`/`defaultAutoPull`; composer honors defaults until user picks                              |
| Keybindings editor                                                                                                    | ✅     | keystroke capture + command picker + conflict highlighting + remove; reset-to-defaults not exposed                                                                                     |
| Projects (icons, defaults, scripts, actions)                                                                          | ✅     | add/rename/delete + browse picker + icon picker + scripts list/edit + Run→PTY (`defaultProjectScripts`)                                                                                |
| Secrets                                                                                                               | ✅     | `secret_request` answering via `secrets.answerRequest`; providers row shows server-managed state (only contract RPC)                                                                   |
| Scheduled tasks                                                                                                       | ✅     | `scheduledTasks.subscribe` stream + upsert/setEnabled/delete/runNow; compact schedule spec (interval/fixed_time/once/webhook — no cron type on the wire)                               |
| Usage limits                                                                                                          | ✅     | getUsageSummary table + `costUsd`/`costSource`/`unpricedRecords`/`pricing` + provider usageLimit windows                                                                               |
| Source control                                                                                                        | ✅     | `server.discoverSourceControl` (real CLI probes + auth + install hints) + GitHub device-flow sign-in                                                                                   |
| Integrations (MCP)                                                                                                    | ✅     | persistent stdio MCP sessions (codex/claude/.mcp.json/settings configs) — `callTool`/`toolInfo`/`readResource` proxy live MCP connections; `updateModelContext` + Integrations listing |
| Devices (mobile labs)                                                                                                 | ✅     | `device.list` rows render real devices or the honest unavailable reason (no mobile dev host)                                                                                           |
| Diagnostics / storage / maintenance                                                                                   | ✅     | `server.getProcessDiagnostics`/`getHostResources`/`getDatabaseStorage` stat table + `runStorageCleanup`/`reclaimDatabaseSpace`                                                         |
| Threads / diffs / worktrees / browser / access / beta / labs / voice / licenses                                       | ✅     | threads+diffs+worktrees+agent-access rows all wired; voice=dictation; licenses=static text                                                                                             |
| Scoped (env/local) settings axes                                                                                      | ✅     | Global/Project selector → `projectSettingsOverrides` patch with override/inherited chips (all `PROJECT_SCOPED_SERVER_SETTING_KEYS`)                                                    |

## Other routes / features

| Feature                                                                         | Status | Notes                                                                                                                                               |
| ------------------------------------------------------------------------------- | ------ | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| Usage page (`_chat.usage`)                                                      | ✅     | ctrl+u / palette "Open usage view" — full center-pane usage table                                                                                   |
| Pull-requests page                                                              | ✅     | palette "Open pull requests view" — full center-pane PR list/detail                                                                                 |
| Onboarding / welcome / first-run gate                                           | ✅     | welcome.rs — first-run gate + project pick + provider select                                                                                        |
| Pairing surfaces (pair/connect/approve-agent)                                   | ✅     | "Connect to server…" → Environments (URL+token+reconnect); approvals render via runtime-request.respond                                             |
| Guest share links                                                               | ✅     | Share chip → `guestShare.create` → copy URL + open externally                                                                                       |
| Hosted prompt links                                                             | ✅     | "Copy prompt link" builds the web `buildNewThreadLink` URL (C67_HOSTED_APP_URL override)                                                            |
| 67 Connect / cloud sign-in                                                      | ✅     | `cloud.getRelayClientStatus` + `installRelayClient` stream (verified installing cloudflared); auth URL absent on wire — Environments covers connect |
| Triage (`server.prepareTriage`, triageTarget)                                   | ✅     | palette "Triage an issue…" → prepareTriage → scratch project → thread.create + prompt prefilled                                                     |
| Search-everything (projects.searchContents in palette)                          | ✅     | palette fires debounced `searchEntries`+`searchContents`; Enter reveals in Files tab                                                                |
| Keybinding coverage parity                                                      | ✅     | mod+1..9 thread jump, mod+t, ctrl+u, ctrl+[/], plus prior set; global handlers moved to element-level (were silently dead)                          |
| Toast notifications                                                             | ✅     |                                                                                                                                                     |
| Reconnect w/ backoff + resubscribe                                              | ✅     |                                                                                                                                                     |
| Multi-window                                                                    | ✅     | palette/context "Open thread in new window" — second AppView + own socket                                                                           |
| Dictation/voice                                                                 | ✅     | above                                                                                                                                               |
| Bedtime reminder, update-everything, GitHub-star prompt, legacy-migration toast | ✅     | bedtimeReminder.subscribe stream; getNpmRelease update toast; star prompt (once/session); projectSettingsFolded migration notice                    |

## Visual parity (web design system → GPUI)

Design tokens were extracted from `apps/web/src/index.css` (resolved
oklch→hex) and codified in `native/app/src/theme.rs`: Inter bundled
(OFL, registered via `add_fonts`), mono `FONT_MONO`, text sizes
10/11/12/14/16/18, radii 4/6/8/12/16/20, scrollbar 6px, topbar 52px,
chat lane 736px, canvas #000 / card #111 / border white-8% / primary
#346bf1 / user-bubble #1e1e1e + full sidebar/diff/popover/overlay
token sets, light-theme counterparts.

Surfaces restyled to web metrics (class→px): sidebar (256px, folder
headers, status dots, draft cards, MenuPopup), timeline + markdown
(rounded-2xl user bubbles, work-log rows, approval/plan cards), composer
(card + inset highlight shadows, chips, pickers, image attachments),
right panel/explorer/diff/terminal drawer, settings modal, cmdk
palette + search dialogs, toasts, welcome, window chrome
(WindowDecorations::Client + min/max/close overlay + drag topbar,
400×360 min, 1100×780 default).

Refinement pass 2: all icon surfaces now render bundled lucide SVGs
(lucide-static v1.77.0, ISC — assets inlined in `icons.rs` via GPUI's
`svg().data()` element; each call site sets `text_color` because GPUI
does not cascade parent text color into `Svg::paint`). Timestamps
render in local time via `chrono::Local` with the web's day-aware
format. Toasts sit at the web-computed offset (top 84px, right 32px).

Pixel-diff pass 3 (live web reference at 1280×800): sidebar brand
aurora, project monogram/favicon chips with relative timestamps,
thread status glyphs + day labels + branch lines + hover Settle pill,
collapsible Settled shelf, topbar breadcrumb + panel icon cluster,
single-card composer (paperclip, provider model chip, overflow, mic,
32px send circle, provider-unavailable state), timeline turn-fold
rows, checkpoint changed-files card, Pierre-dark code blocks with
wrap/copy, centered empty state, right panel resized to the web's
280px with tab scroll controls, palette row fixes, explorer search.

Pixel-diff pass 4 (live web + computed styles, dark + light, three
widths): sidebar rebuilt to the web's default flat card mode
(chip/project/time line, title line, branch meta, hover snooze+Settle,
pin indicator), real prerendered `assets/aurora-{dark,light}.png`
backdrop, `Svg::group_hover` icon brightening, GPUI native tooltips,
`orchestration.getTurnDiff`/`getFullThreadDiff` + `Open diff`,
bounded-snapshot + `GET /api/orchestration/threads/:id/history`
pagination with the "Load earlier turns" control, scratch singleton
groups, palette measured rows (32/48px), composer picker footer +
Shift+Enter, light syntax palette, hover-reveal timestamps, right
panel auto-collapse at 980px, full light theme on every surface.

Remaining divergences (all evidence-based):

- The brand aurora is the web's `DevBlueprintArt` SVG prerendered to
  `assets/aurora-{dark,light}.png` (including the
  `sidebar-stage-backdrop` mask + `--stage-fade` overlay) — GPUI has
  no SVG patterns/gradient transforms, so the baked image is the
  primitive. The nightly variant and stage-label gating are not
  ported: the dev blueprint renders unconditionally.
- "Load earlier turns" is implemented: `orchestration.subscribeThread`
  honors `acceptBoundedSnapshot` (newest 10-user-turn window,
  `hasMoreHistory` + opaque `historyCursor` on the snapshot) and
  `GET /api/orchestration/threads/:id/history?cursor=…` serves older
  pages (20-user-turn window) from the same visible-item set — the
  `environmentHttp.ts` `threadHistoryPage` contract. Deferred-work
  cursors (`inclusive`), `throughEntryId`, `view`, `deferWork`, and
  `compactIds` are not ported; the client always asks for the default
  conversation page.
- Turn diffs resolve checkpoint git refs against the scope's worktree
  (`orchestration.getTurnDiff`/`getFullThreadDiff` are implemented);
  threads whose captured worktree no longer exists still show the
  inline error.
- Palette active row uses `bg-accent` like the web's `aria-selected`
  style, not the `data-highlighted` hover tint.
- Scratch "No project" threads render one singleton group per thread
  like the web's per-family groups, with per-thread collapse state
  (`Project.expanded` is shared across the scratch workspace, so a
  `scratch_collapsed` set keys off the thread id instead).
- Under bare X11/Xvfb (no WM) the window renders borderless — GPUI's
  X11 backend sets Motif hints for server decorations and no WM draws
  them; the in-app topbar drag area and min/max/close controls render
  only when `window_decorations()` reports `Client` (Wayland/GNOME).
- Icon hover-brighten is applied via `Svg::group_hover` to all icon
  buttons; decorative (non-button) row glyphs keep resting ink.
- Tooltips use GPUI's built-in `tooltip` (500ms, cursor-anchored,
  window-clamped) styled like the web's `TooltipPopup` — cursor-anchored
  rather than element-anchored with an arrow.
- Sidebar "Searching thread messages…" pending state is unreachable —
  native search is synchronous local filtering.
- Sidebar cards: line-2 right glyph is the status icon (web renders a
  provider badge — no provider artwork bundled); "No project" uses a
  solid `message-square` (no dashed variant bundled); unpin keeps the
  Pin glyph brightened (no `pin-off` asset); snooze applies the first
  preset instantly instead of opening the preset menu; pinned
  drag-reorder is not implemented; "Working Nm" live-status line is
  not rendered (no live-status thread in the wire model).
- Composer send uses a simplified 2-stop gradient vs the web's
  stage-backdrop art; the footer does not collapse to an ellipsis at
  narrow widths.
- A few semantic colors (destructive reds, warning ambers) in
  settings/archived/notifications are literal hex rather than theme
  tokens — visually identical in both themes, just not token-driven.
