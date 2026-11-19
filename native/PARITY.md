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
| Canvas/right-panel layout                         | ✅     | right panel Files/Terminals/PRs/Diff tabs at fixed 380px                                                                                          |

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

Deliberate divergences:

- Right panel width kept at 380px vs web's 280px — the native panel
  hosts the diff viewer and PR cards in a single column where 280px
  cramps the dual line-number gutters.
- Client decorations are enabled only where the compositor supports
  them (Wayland/GNOME etc.); bare X11 falls back to server
  decorations, so under Xvfb the window keeps the system frame. The
  in-app topbar drag area and min/max/close controls render only in
  client mode.
- Timestamps in timeline meta rows render UTC (no timezone database
  in the native binary).
- Iconography uses DejaVu-covered glyphs (✎ ⚠ ▸ ✓ …) as stand-ins
  for lucide SVG icons.
- Toast viewport sits above the composer (`bottom 140px`) rather than
  flush bottom-right to avoid covering the send button.
