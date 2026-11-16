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

| Feature                                           | Status | Notes                                                                                                                                 |
| ------------------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------- |
| Thread timeline + markdown                        | ✅     | markdown.rs incl. tables, code blocks                                                                                                 |
| Streaming turn items (thinking, tool calls, text) | ✅     |                                                                                                                                       |
| Working/elapsed indicator                         | ✅     | pulsing ● + ticking "Working for Ns"                                                                                                  |
| Interactive approvals                             | ✅     | runtime-request.respond; auto-approve under full-access                                                                               |
| Stop turn (composer Stop)                         | ✅     |                                                                                                                                       |
| Queued dispatch / steer active run                | ❌     | dispatchMode union: steer_active/restart_active/queue_after_active — needs timeline UI + server (server already accepts dispatchMode) |
| Command output panel (terminal tool calls)        | 🟡     | tool calls render, no expandable output panel                                                                                         |
| Compaction display                                | ❌     | `idleCompaction`, compaction items not rendered                                                                                       |
| Background work / delegated tasks                 | ❌     | child-thread approvals, subagent items                                                                                                |
| Pending user-input panel (questions)              | 🟡     | approvals only; `answers` questions not rendered                                                                                      |
| Attachments: file path + clipboard image          | ✅     | Attach field, paste-to-stage                                                                                                          |
| Image thumbnails in composer strip                | ❌     | strip lists names only                                                                                                                |
| Drag & drop files onto composer                   | ❌     | needs GPUI drop handler                                                                                                               |
| @mentions (files/threads/context chips)           | ❌     | needs `projects.searchContents`/listEntries RPC                                                                                       |
| Prompt history (up-arrow recall)                  | ❌     | client-side, easy                                                                                                                     |
| Message stash                                     | ❌     | client-side draft stash                                                                                                               |
| Model effort picker                               | ❌     | composer has model picker only; effort is a modelSelection field                                                                      |
| Usage-limit banner in composer                    | ❌     | needs `server.getUsageSummary`                                                                                                        |
| Dictation/speech button                           | 🚫     | needs a transcription service (web uses cloud voice); no local engine                                                                 |
| Citations on assistant messages                   | ❌     | citation chips/sources                                                                                                                |
| Message feedback / copy                           | ❌     | no per-message actions                                                                                                                |
| Expanded image viewer dialog                      | ❌     | inline images not decoded yet                                                                                                         |
| Canvas/right-panel layout                         | 🟡     | right panel renders Files/Terminals/PR panes; Terminals tab lists metadata + close/new; PR pane functional                            |

## Sidebar

| Feature                                          | Status | Notes                                                    |
| ------------------------------------------------ | ------ | -------------------------------------------------------- |
| Projects + threads list                          | ✅     |                                                          |
| Search, rename, archive/unarchive, delete, pin   | ✅     |                                                          |
| Status indicators (running dot, PR link, unread) | 🟡     | run-status dot only; no PR badges, unread markers        |
| Drag-reorder threads                             | ❌     | `thread.pin.reorder`/`thread.active.reorder` commands    |
| Snooze + custom snooze                           | ❌     | `thread.snooze`/`unsnooze` commands + CustomSnoozeDialog |
| Undo-notice toasts on archive/delete             | ❌     | SidebarThreadUndoNotice — timed undo toast               |
| Draft threads block                              | ❌     | drafts are client-side in web                            |
| Project icons/favicons                           | ❌     | ProjectFaviconPickerDialog; monogram exists?             |
| Settings gear                                    | ✅     | footer gear + ctrl-,                                     |
| Update pills / server version                    | 🚫     | Electron/desktop-update surface                          |

## Terminal drawer

| Feature                                   | Status | Notes                                                                                                                                                                                                                                                     |
| ----------------------------------------- | ------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Persistent per-thread terminals           | 🟡     | `terminal.open/attach/write/resize/clear/close` + `subscribeTerminalMetadata` wired; ~200KB scrollback, small VT emulator (SGR colors, cursor ops, erase, scroll regions, alt screen); no live resize tracking, no drag-resize, `terminal.observe` unused |
| Agent-owned terminal contexts in composer | ❌     | ComposerPendingTerminalContexts                                                                                                                                                                                                                           |

## Files / previews

| Feature                                       | Status | Notes                                                                                                   |
| --------------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------- |
| File explorer pane                            | ✅     | right-panel Files tab: lazy `projects.listEntries` tree, expand/collapse, explorer_reveal               |
| Text file preview                             | ✅     | `projects.readFile` bottom-split preview with byte size + truncation notice                             |
| Markdown preview                              | ✅     | readFile → markdown.rs render for .md paths                                                             |
| Image preview                                 | ✅     | `assets.createUrl` + bearer GET → image decode (image crate)                                            |
| File write/save                               | ❌     | `projects.writeFile` RPC exists; no editor UI                                                           |
| Project content search dialog                 | ✅     | ctrl-shift-f: `projects.searchContents`/`searchEntries`, case/word/regex toggles, Enter→explorer reveal |
| PDF/office/notebook/font/audio/video previews | 🚫     | GPUI has no document renderers; note: could open externally — degraded-by-design                        |
| Agent browser preview panes                   | 🚫     | `preview.*` sessions need an embedded webview; no webview in GPUI — open in system browser instead      |

## Source control & PRs

| Feature                     | Status | Notes                                                                                                                                                             |
| --------------------------- | ------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Branch picker in composer   | 🟡     | `vcs.listRefs`/`switchRef`/`createRef` picker + inline new-branch input live in the Pull Requests pane (vcs.rs); composer hookup pending                          |
| Worktree create/remove      | ❌     | `vcs.createWorktree`/`removeWorktree` + settings.worktrees                                                                                                        |
| VCS status chips            | 🟡     | `subscribeVcsStatus` snapshot + local/remote diffs drive the PRs-pane header (ref, upstream, ↑/↓, working-tree changes); no composer chips                        |
| Git actions (pull, init)    | ❌     | `vcs.pull`/`vcs.init`, `git.runStackedAction`                                                                                                                     |
| Pull requests list/detail   | ✅     | `pullRequests.list` (+`subscribeRefreshes` invalidation) with expandable rows: `pullRequests.detail` checks/reviewers/mergeability + `pullRequests.linkedThreads` |
| PR diff/review sheets       | ❌     | `review.getDiff*` + `pullRequests.diffFileContents`; large                                                                                                        |
| Link PR to thread           | 🟡     | thread's `linked_pull_request` badge pinned atop the PRs pane + `pullRequests.linkedThreads` rows in detail; no link/unlink action                                |
| Source-control sign-in flow | 🟡     | could shell out to `gh auth`; OAuth device flow exists in TS server                                                                                               |

## Settings (web has ~30 routes; native has 9 sections)

| Section                                                                                                               | Status | Notes                                                                                                                                                             |
| --------------------------------------------------------------------------------------------------------------------- | ------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Connection / Environments / Appearance / Providers / ACP Sessions / Authorized Clients / Archived / Shortcuts / About | ✅     |                                                                                                                                                                   |
| Notifications                                                                                                         | ❌     | NotificationSettings (per-event toggles) — client-persisted                                                                                                       |
| Chat defaults (mode/model defaults)                                                                                   | ❌     | settings.chat                                                                                                                                                     |
| Keybindings editor                                                                                                    | ❌     | `server.upsertKeybinding`/`removeKeybinding`                                                                                                                      |
| Projects (icons, defaults, scripts, actions)                                                                          | ❌     | `projects.mutate`, projectScripts, favicon picker                                                                                                                 |
| Secrets                                                                                                               | 🟡     | pending `secret_request` items answer via `secrets.answerRequest` (save/decline); web's provider (1Password) management not ported — server resolves answers only |
| Scheduled tasks                                                                                                       | ✅     | `scheduledTasks.subscribe` stream + upsert/setEnabled/delete/runNow; compact schedule spec (interval/fixed_time/once/webhook — no cron type on the wire)          |
| Usage limits                                                                                                          | 🟡     | `server.getUsageSummary`/`refreshUsageRates` day-bucketed table + period chips; no pricing/cost (server reports unpriced)                                         |
| Source control                                                                                                        | ❌     | `sourceControl.*`, `server.discoverSourceControl`                                                                                                                 |
| Integrations (MCP)                                                                                                    | ❌     | `mcpApps.*` — server MCP bridge, large                                                                                                                            |
| Devices (mobile labs)                                                                                                 | 🚫     | `device.*` — EAS/simulator hosts; desktop app is the wrong host for this UX (noted)                                                                               |
| Diagnostics / storage / maintenance                                                                                   | ✅     | `server.getProcessDiagnostics`/`getHostResources`/`getDatabaseStorage` stat table + `runStorageCleanup`/`reclaimDatabaseSpace`                                    |
| Threads / diffs / worktrees / browser / access / beta / labs / voice / licenses                                       | ❌     | mostly client-persisted toggles or small sections                                                                                                                 |
| Scoped (env/local) settings axes                                                                                      | ❌     | settingsScope machinery — large                                                                                                                                   |

## Other routes / features

| Feature                                                                         | Status | Notes                                                                                                                                          |
| ------------------------------------------------------------------------------- | ------ | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Usage page (`_chat.usage`)                                                      | ❌     | needs `server.getUsageSummary`                                                                                                                 |
| Pull-requests page                                                              | ❌     | needs pullRequests.*                                                                                                                           |
| Onboarding / welcome / first-run gate                                           | ❌     | client-only, easy                                                                                                                              |
| Pairing surfaces (pair/connect/approve-agent)                                   | 🟡     | native pairs via config token already; approve-agent flow web-hosted                                                                           |
| Guest share links                                                               | 🚫     | `guestShare.create` produces a web URL for recipients — server RPC exists but the view is a hosted web page; native could generate + copy link |
| Hosted prompt links                                                             | 🚫     | same — URL artifact for browsers                                                                                                               |
| 67 Connect / cloud sign-in                                                      | 🚫     | Clerk-hosted auth + relay infra (`cloud.*`) — requires the hosted T3 Connect service; native client connects by endpoint+token                 |
| Triage (`server.prepareTriage`, triageTarget)                                   | ❌     | one RPC + composer entry point                                                                                                                 |
| Search-everything (projects.searchContents in palette)                          | ❌     | needs RPC                                                                                                                                      |
| Keybinding coverage parity                                                      | 🟡     | core set exists; web has dozens (see KeybindingsSettings)                                                                                      |
| Toast notifications                                                             | ✅     |                                                                                                                                                |
| Reconnect w/ backoff + resubscribe                                              | ✅     |                                                                                                                                                |
| Multi-window                                                                    | 🚫     | GPUI single window; not planned                                                                                                                |
| Dictation/voice                                                                 | 🚫     | above                                                                                                                                          |
| Bedtime reminder, update-everything, GitHub-star prompt, legacy-migration toast | 🚫     | niche/desktop-update surfaces                                                                                                                  |
