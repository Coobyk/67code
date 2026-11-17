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

| Feature                                           | Status | Notes                                                                                                                                                 |
| ------------------------------------------------- | ------ | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| Thread timeline + markdown                        | ✅     | markdown.rs incl. tables, code blocks                                                                                                                 |
| Streaming turn items (thinking, tool calls, text) | ✅     |                                                                                                                                                       |
| Working/elapsed indicator                         | ✅     | pulsing ● + ticking "Working for Ns"                                                                                                                  |
| Interactive approvals                             | ✅     | runtime-request.respond; auto-approve under full-access                                                                                               |
| Stop turn (composer Stop)                         | ✅     |                                                                                                                                                       |
| Queued dispatch / steer active run                | ✅     | `⏵ Queue ▾` picker on busy threads → `queue_after_active`/`steer_active`/`restart_active` with `targetRunId`                                          |
| Command output panel (terminal tool calls)        | ✅     | expandable detail rows + capped output                                                                                                                |
| Compaction display                                | ✅     | compaction_row divider with token counts                                                                                                              |
| Background work / delegated tasks                 | 🟡     | `subagent` rows render; child-thread approval routing unknown                                                                                         |
| Pending user-input panel (questions)              | ✅     | question option click-select + answers                                                                                                                |
| Attachments: file path + clipboard image          | ✅     | Attach field, paste-to-stage                                                                                                                          |
| Image thumbnails in composer strip                | ✅     | decoded RenderImage thumbnails + remove                                                                                                               |
| Drag & drop files onto composer                   | ✅     | ExternalPaths drop → staged attachments                                                                                                               |
| @mentions (files/threads/context chips)           | ✅     | `@` word-start popup: thread titles + debounced `projects.searchEntries` files                                                                        |
| Prompt history (up-arrow recall)                  | ✅     | per-project ClientConfig history                                                                                                                      |
| Message stash                                     | ✅     | ctrl-shift-s stash/restore, per-project ClientConfig                                                                                                  |
| Model effort picker                               | ✅     | effort chip → modelSelection.options                                                                                                                  |
| Usage-limit banner in composer                    | 🟡     | `server.getUsageSummary` banner (tokens/cost/sessions, warn-tinted heavy use, per-day dismiss); wire carries no limit % so "at limit" is approximated |
| Dictation/speech button                           | 🚫     | needs a transcription service (web uses cloud voice); no local engine                                                                                 |
| Citations on assistant messages                   | 🚫     | no provider emits citation items on the wire (contract citation is a user-authored review annotation)                                                 |
| Message feedback / copy                           | 🟡     | copy chips on messages/tool outputs; no thumbs/feedback actions                                                                                       |
| Expanded image viewer dialog                      | ✅     | click thumbnail → full overlay                                                                                                                        |
| Canvas/right-panel layout                         | ✅     | right panel Files/Terminals/PRs/Diff tabs at fixed 380px                                                                                              |

## Sidebar

| Feature                                          | Status | Notes                                                                       |
| ------------------------------------------------ | ------ | --------------------------------------------------------------------------- |
| Projects + threads list                          | ✅     |                                                                             |
| Search, rename, archive/unarchive, delete, pin   | ✅     |                                                                             |
| Status indicators (running dot, PR link, unread) | ✅     | running dot, ⧉ PR glyph, 😴 snoozed, unread markers                         |
| Drag-reorder threads                             | 🟡     | `thread.pin.reorder`/`thread.active.reorder` dispatched; no pointer-drag UX |
| Snooze + custom snooze                           | 🟡     | presets + overlay; no custom datetime picker                                |
| Undo-notice toasts on archive/delete             | ❌     | needs toast action buttons (push_toast callback support)                    |
| Draft threads block                              | ❌     | drafts are client-side in web                                               |
| Project icons/favicons                           | ❌     | ProjectFaviconPickerDialog; monogram exists?                                |
| Settings gear                                    | ✅     | footer gear + ctrl-,                                                        |
| Update pills / server version                    | 🚫     | Electron/desktop-update surface                                             |

## Terminal drawer

| Feature                                   | Status | Notes                                                                                                                                                                                                |
| ----------------------------------------- | ------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Persistent per-thread terminals           | ✅     | `terminal.open/attach/write/resize/clear/close` + metadata subscribe; live grid resize (250ms debounce); VT emulator; ~200KB scrollback; no drag-resize; `terminal.observe` for other threads unused |
| Agent-owned terminal contexts in composer | ✅     | `ctx` chip stages ANSI-stripped terminal tails as `context.records` on `message.dispatch`, scoped to owning thread                                                                                   |

## Files / previews

| Feature                                       | Status | Notes                                                                                                   |
| --------------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------- |
| File explorer pane                            | ✅     | right-panel Files tab: lazy `projects.listEntries` tree, expand/collapse, explorer_reveal               |
| Text file preview                             | ✅     | `projects.readFile` bottom-split preview with byte size + truncation notice                             |
| Markdown preview                              | ✅     | readFile → markdown.rs render for .md paths                                                             |
| Image preview                                 | ✅     | `assets.createUrl` + bearer GET → image decode (image crate)                                            |
| File write/save                               | ✅     | inline editor w/ dirty indicator + Ctrl-S → `projects.writeFile`; truncated previews stay read-only     |
| Project content search dialog                 | ✅     | ctrl-shift-f: `projects.searchContents`/`searchEntries`, case/word/regex toggles, Enter→explorer reveal |
| PDF/office/notebook/font/audio/video previews | 🚫     | GPUI has no document renderers; note: could open externally — degraded-by-design                        |
| Agent browser preview panes                   | 🚫     | `preview.*` sessions need an embedded webview; no webview in GPUI — open in system browser instead      |

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

| Section                                                                                                               | Status | Notes                                                                                                                                                                                                                                                                                                                                                                                      |
| --------------------------------------------------------------------------------------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Connection / Environments / Appearance / Providers / ACP Sessions / Authorized Clients / Archived / Shortcuts / About | ✅     |                                                                                                                                                                                                                                                                                                                                                                                            |
| Notifications                                                                                                         | 🟡     | `notificationMode` select + in-app toast toggle persisted on `ClientConfig`; toasts gated; OS/sound backend pending                                                                                                                                                                                                                                                                        |
| Chat defaults (mode/model defaults)                                                                                   | ✅     | `defaultModelSelection`/`fallbackModelSelection`/`defaultRuntimeMode`/`defaultThreadEnvMode`/`defaultAutoPull`; composer honors defaults until user picks                                                                                                                                                                                                                                  |
| Keybindings editor                                                                                                    | 🟡     | list/add/remove rules via `server.getConfig`/`upsertKeybinding`/`removeKeybinding` + optional `when`; no command picker, keystroke capture, or conflict highlighting                                                                                                                                                                                                                       |
| Projects (icons, defaults, scripts, actions)                                                                          | 🟡     | add/rename/delete + `filesystem.browse` folder picker + per-project icon via `project.update{projectIcon}`; 🚫 scripts/actions not modeled on native client                                                                                                                                                                                                                                |
| Secrets                                                                                                               | 🟡     | pending `secret_request` items answer via `secrets.answerRequest` (save/decline); web's provider (1Password) management not ported — server resolves answers only                                                                                                                                                                                                                          |
| Scheduled tasks                                                                                                       | ✅     | `scheduledTasks.subscribe` stream + upsert/setEnabled/delete/runNow; compact schedule spec (interval/fixed_time/once/webhook — no cron type on the wire)                                                                                                                                                                                                                                   |
| Usage limits                                                                                                          | 🟡     | `server.getUsageSummary`/`refreshUsageRates` day-bucketed table + period chips; no pricing/cost (server reports unpriced)                                                                                                                                                                                                                                                                  |
| Source control                                                                                                        | 🟡     | GitHub `sourceControl.appAccess` status + sign-in attempt + writing style/model; 🚫 no `server.discoverSourceControl` (no VCS/provider discovery or install hints)                                                                                                                                                                                                                         |
| Integrations (MCP)                                                                                                    | 🟡     | `mcpApps.*`: full app-resolve (`t3McpApp` + toolName guard) and `updateModelContext` end-to-end (lineage + 16 KiB + `mcp_app_model_context` upsert); `callTool`/`toolInfo`/`readResource` resolve then faithfully `session-stopped` — Rust providers are one-shot turn subprocesses with no live MCP session to proxy; settings section lists apps in the loaded thread + `toolInfo` probe |
| Devices (mobile labs)                                                                                                 | 🚫     | `device.*` — EAS/simulator hosts; desktop app is the wrong host for this UX (noted)                                                                                                                                                                                                                                                                                                        |
| Diagnostics / storage / maintenance                                                                                   | ✅     | `server.getProcessDiagnostics`/`getHostResources`/`getDatabaseStorage` stat table + `runStorageCleanup`/`reclaimDatabaseSpace`                                                                                                                                                                                                                                                             |
| Threads / diffs / worktrees / browser / access / beta / labs / voice / licenses                                       | 🟡     | threads+diffs+worktrees rows done (client-persisted + updateSettings); browser/voice/licenses not applicable to native                                                                                                                                                                                                                                                                     |
| Scoped (env/local) settings axes                                                                                      | 🚫     | `server.getConfig` exposes merged settings only — no scope target/override wire model                                                                                                                                                                                                                                                                                                      |

## Other routes / features

| Feature                                                                         | Status | Notes                                                                                                                                          |
| ------------------------------------------------------------------------------- | ------ | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Usage page (`_chat.usage`)                                                      | 🟡     | Usage Limits settings section covers the data; standalone route not ported                                                                     |
| Pull-requests page                                                              | 🟡     | right-panel Pull requests tab covers list/detail/linked threads + diff pane; no dedicated page                                                 |
| Onboarding / welcome / first-run gate                                           | ✅     | welcome.rs — first-run gate + project pick + provider select                                                                                   |
| Pairing surfaces (pair/connect/approve-agent)                                   | 🟡     | native pairs via config token already; approve-agent flow web-hosted                                                                           |
| Guest share links                                                               | 🚫     | `guestShare.create` produces a web URL for recipients — server RPC exists but the view is a hosted web page; native could generate + copy link |
| Hosted prompt links                                                             | 🚫     | same — URL artifact for browsers                                                                                                               |
| 67 Connect / cloud sign-in                                                      | 🚫     | Clerk-hosted auth + relay infra (`cloud.*`) — requires the hosted T3 Connect service; native client connects by endpoint+token                 |
| Triage (`server.prepareTriage`, triageTarget)                                   | ✅     | palette "Triage an issue…" → prepareTriage → scratch project → thread.create + prompt prefilled                                                |
| Search-everything (projects.searchContents in palette)                          | ✅     | palette fires debounced `searchEntries`+`searchContents`; Enter reveals in Files tab                                                           |
| Keybinding coverage parity                                                      | 🟡     | ctrl-j/alt-b/ctrl-p/ctrl-w/ctrl-{,}/shift-p/shift-m/shift-o/shift-n added; web mod+{1..9} thread-jump + mod+t still missing                    |
| Toast notifications                                                             | ✅     |                                                                                                                                                |
| Reconnect w/ backoff + resubscribe                                              | ✅     |                                                                                                                                                |
| Multi-window                                                                    | 🚫     | GPUI single window; not planned                                                                                                                |
| Dictation/voice                                                                 | 🚫     | above                                                                                                                                          |
| Bedtime reminder, update-everything, GitHub-star prompt, legacy-migration toast | 🚫     | niche/desktop-update surfaces                                                                                                                  |
