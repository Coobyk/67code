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

| Feature                                           | Status | Notes                                                                                                      |
| ------------------------------------------------- | ------ | ---------------------------------------------------------------------------------------------------------- |
| Thread timeline + markdown                        | ✅     | markdown.rs incl. tables, code blocks                                                                      |
| Streaming turn items (thinking, tool calls, text) | ✅     |                                                                                                            |
| Working/elapsed indicator                         | ✅     | pulsing ● + ticking "Working for Ns"                                                                       |
| Interactive approvals                             | ✅     | runtime-request.respond; auto-approve under full-access                                                    |
| Stop turn (composer Stop)                         | ✅     |                                                                                                            |
| Queued dispatch / steer active run                | 🟡     | `queue_after_active`/`start_immediately` sent on busy threads; `steer_active`/`restart_active` not offered |
| Command output panel (terminal tool calls)        | ✅     | expandable detail rows + capped output                                                                     |
| Compaction display                                | ✅     | compaction_row divider with token counts                                                                   |
| Background work / delegated tasks                 | 🟡     | `subagent` rows render; child-thread approval routing unknown                                              |
| Pending user-input panel (questions)              | ✅     | question option click-select + answers                                                                     |
| Attachments: file path + clipboard image          | ✅     | Attach field, paste-to-stage                                                                               |
| Image thumbnails in composer strip                | ✅     | decoded RenderImage thumbnails + remove                                                                    |
| Drag & drop files onto composer                   | ✅     | ExternalPaths drop → staged attachments                                                                    |
| @mentions (files/threads/context chips)           | ❌     | popup needed; `projects.searchEntries`/`searchContents` + thread list available                            |
| Prompt history (up-arrow recall)                  | ✅     | per-project ClientConfig history                                                                           |
| Message stash                                     | ✅     | ctrl-shift-s stash/restore, per-project ClientConfig                                                       |
| Model effort picker                               | ✅     | effort chip → modelSelection.options                                                                       |
| Usage-limit banner in composer                    | ❌     | `server.getUsageSummary` implemented; banner UI needed                                                     |
| Dictation/speech button                           | 🚫     | needs a transcription service (web uses cloud voice); no local engine                                      |
| Citations on assistant messages                   | ❌     | check wire items; no provider currently emits citation items                                               |
| Message feedback / copy                           | 🟡     | copy chips on messages/tool outputs; no thumbs/feedback actions                                            |
| Expanded image viewer dialog                      | ✅     | click thumbnail → full overlay                                                                             |
| Canvas/right-panel layout                         | 🟡     | right panel renders Files/Terminals/PR panes; Terminals tab lists metadata + close/new; PR pane functional |

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

## Settings (web has ~30 routes; native has 16 sections)

| Section                                                                                                               | Status | Notes                                                                                                                                                             |
| --------------------------------------------------------------------------------------------------------------------- | ------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Connection / Environments / Appearance / Providers / ACP Sessions / Authorized Clients / Archived / Shortcuts / About | ✅     |                                                                                                                                                                   |
| Notifications                                                                                                         | 🟡     | `notificationMode` select + in-app toast toggle persisted on `ClientConfig`; toasts gated; OS/sound backend pending                                               |
| Chat defaults (mode/model defaults)                                                                                   | ✅     | `defaultModelSelection`/`fallbackModelSelection`/`defaultRuntimeMode`/`defaultThreadEnvMode`/`defaultAutoPull`; composer honors defaults until user picks         |
| Keybindings editor                                                                                                    | ❌     | `server.upsertKeybinding`/`removeKeybinding`                                                                                                                      |
| Projects (icons, defaults, scripts, actions)                                                                          | 🟡     | add/rename/delete via `projects.mutate` (+ text path input; `filesystem.browse` picker gap); no favicon picker, scripts, or actions                               |
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
| Usage page (`_chat.usage`)                                                      | 🟡     | Usage Limits settings section covers the data; standalone route not ported                                                                     |
| Pull-requests page                                                              | 🟡     | right-panel Pull requests tab covers list/detail/linked threads; no dedicated page or diff sheets                                              |
| Onboarding / welcome / first-run gate                                           | ✅     | welcome.rs — first-run gate + project pick + provider select                                                                                   |
| Pairing surfaces (pair/connect/approve-agent)                                   | 🟡     | native pairs via config token already; approve-agent flow web-hosted                                                                           |
| Guest share links                                                               | 🚫     | `guestShare.create` produces a web URL for recipients — server RPC exists but the view is a hosted web page; native could generate + copy link |
| Hosted prompt links                                                             | 🚫     | same — URL artifact for browsers                                                                                                               |
| 67 Connect / cloud sign-in                                                      | 🚫     | Clerk-hosted auth + relay infra (`cloud.*`) — requires the hosted T3 Connect service; native client connects by endpoint+token                 |
| Triage (`server.prepareTriage`, triageTarget)                                   | ❌     | one RPC + composer entry point                                                                                                                 |
| Search-everything (projects.searchContents in palette)                          | 🟡     | ctrl-shift-f content/file dialog done; palette-mode search integration pending                                                                 |
| Keybinding coverage parity                                                      | 🟡     | core set exists; web has dozens (see KeybindingsSettings)                                                                                      |
| Toast notifications                                                             | ✅     |                                                                                                                                                |
| Reconnect w/ backoff + resubscribe                                              | ✅     |                                                                                                                                                |
| Multi-window                                                                    | 🚫     | GPUI single window; not planned                                                                                                                |
| Dictation/voice                                                                 | 🚫     | above                                                                                                                                          |
| Bedtime reminder, update-everything, GitHub-star prompt, legacy-migration toast | 🚫     | niche/desktop-update surfaces                                                                                                                  |
