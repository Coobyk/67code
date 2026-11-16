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
| Canvas/right-panel layout                         | ❌     | right panel tabs (files/PR/terminal) — major shell work                                                                               |

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

| Feature                                   | Status | Notes                                                                                                                                                  |
| ----------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Persistent per-thread terminals           | ❌     | needs `terminal.open/attach/write/resize/close/clear/observe` + `subscribeTerminalMetadata` — server PTY (portable-pty) + xterm-grade renderer in GPUI |
| Agent-owned terminal contexts in composer | ❌     | ComposerPendingTerminalContexts                                                                                                                        |

## Files / previews

| Feature                                       | Status | Notes                                                                                              |
| --------------------------------------------- | ------ | -------------------------------------------------------------------------------------------------- |
| File explorer pane                            | ❌     | needs `projects.listEntries`/`listIgnoredFiles`/`filesystem.browse` RPCs                           |
| Text file preview                             | ❌     | needs `projects.readFile` (+ `filesystem.getMetadata`)                                             |
| Markdown preview                              | ❌     | renderer exists (markdown.rs); needs readFile                                                      |
| Image preview                                 | ❌     | needs readFile/binary + image decode (image crate)                                                 |
| File write/save                               | ❌     | `projects.writeFile`                                                                               |
| Project content search dialog                 | ❌     | `projects.searchContents`                                                                          |
| PDF/office/notebook/font/audio/video previews | 🚫     | GPUI has no document renderers; note: could open externally — degraded-by-design                   |
| Agent browser preview panes                   | 🚫     | `preview.*` sessions need an embedded webview; no webview in GPUI — open in system browser instead |

## Source control & PRs

| Feature                     | Status | Notes                                                                            |
| --------------------------- | ------ | -------------------------------------------------------------------------------- |
| Branch picker in composer   | ❌     | needs `vcs.listRefs`/`switchRef`/`createRef` + `subscribeVcsBranch`              |
| Worktree create/remove      | ❌     | `vcs.createWorktree`/`removeWorktree` + settings.worktrees                       |
| VCS status chips            | ❌     | `subscribeVcsStatus`                                                             |
| Git actions (pull, init)    | ❌     | `vcs.pull`/`vcs.init`, `git.runStackedAction`                                    |
| Pull requests list/detail   | ❌     | ~25 `pullRequests.*` RPCs — GitHub-backed; feasible via `gh` CLI proxy on server |
| PR diff/review sheets       | ❌     | `review.getDiff*` + `pullRequests.diffFileContents`; large                       |
| Link PR to thread           | ❌     | `git.resolvePullRequest`, `pullRequests.linkedThreads`                           |
| Source-control sign-in flow | 🟡     | could shell out to `gh auth`; OAuth device flow exists in TS server              |

## Settings (web has ~30 routes; native has 9 sections)

| Section                                                                                                               | Status | Notes                                                                               |
| --------------------------------------------------------------------------------------------------------------------- | ------ | ----------------------------------------------------------------------------------- |
| Connection / Environments / Appearance / Providers / ACP Sessions / Authorized Clients / Archived / Shortcuts / About | ✅     |                                                                                     |
| Notifications                                                                                                         | ❌     | NotificationSettings (per-event toggles) — client-persisted                         |
| Chat defaults (mode/model defaults)                                                                                   | ❌     | settings.chat                                                                       |
| Keybindings editor                                                                                                    | ❌     | `server.upsertKeybinding`/`removeKeybinding`                                        |
| Projects (icons, defaults, scripts, actions)                                                                          | ❌     | `projects.mutate`, projectScripts, favicon picker                                   |
| Secrets                                                                                                               | ❌     | `secrets.*` + SecretsSettings providers                                             |
| Scheduled tasks                                                                                                       | ❌     | `scheduledTasks.*` (8 methods)                                                      |
| Usage limits                                                                                                          | ❌     | `server.getUsageSummary`/`refreshUsageRates` + usage page                           |
| Source control                                                                                                        | ❌     | `sourceControl.*`, `server.discoverSourceControl`                                   |
| Integrations (MCP)                                                                                                    | ❌     | `mcpApps.*` — server MCP bridge, large                                              |
| Devices (mobile labs)                                                                                                 | 🚫     | `device.*` — EAS/simulator hosts; desktop app is the wrong host for this UX (noted) |
| Diagnostics / storage / maintenance                                                                                   | ❌     | `server.get*Diagnostics`/`getHostResources`/`getDatabaseStorage`/cleanup            |
| Threads / diffs / worktrees / browser / access / beta / labs / voice / licenses                                       | ❌     | mostly client-persisted toggles or small sections                                   |
| Scoped (env/local) settings axes                                                                                      | ❌     | settingsScope machinery — large                                                     |

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
