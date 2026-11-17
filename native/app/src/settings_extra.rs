//! Extra settings sections: Keybindings, Source Control, Threads.
//!
//! - **Keybindings** edits the server's `keybindings.json` through
//!   `server.upsertKeybinding` / `server.removeKeybinding`
//!   (`{key, command, when?}` rules; the server validates the shortcut and
//!   when-expression). The list reads `server.getConfig →
//!   keybindings.{bindings,issues}` + `keybindingsConfigPath`. Unlike the
//!   web editor there is no command catalog, keystroke capture, search, or
//!   conflict highlighting — command is free text (accepts
//!   `script.<id>.run`), the key uses the `"mod+shift+k"` grammar, and
//!   `when` is optional raw text.
//! - **Source Control** shows the GitHub provider row from
//!   `sourceControl.appAccess` (this server always answers `signed-out`)
//!   plus a Sign-in button that surfaces the server's
//!   `sourceControl.signInStart` failure message verbatim. The writing
//!   settings (`sourceControlWritingStyle.mode` + `customInstructions`,
//!   `sourceControlWriterModelSelection`) are ServerSettings written via
//!   `server.updateSettings`. `server.discoverSourceControl` is not
//!   implemented server-side, so the web's VCS/CLI detection rows are a
//!   noted gap.
//! - **Threads** covers the web Threads/Diffs/Worktrees panels that are
//!   meaningful on a single-environment native client: diff defaults are
//!   client-persisted (`ClientConfig`, like the web's ClientSettings), the
//!   rest are ServerSettings via `server.updateSettings`. Browser, voice,
//!   access/connections, and licenses settings have no native analogue.

use std::cell::Cell;

use gpui::{
    Context, Div, Entity, EntityInputHandler, Focusable, MouseButton, SharedString, Subscription,
    Window, div, prelude::*, px,
};
use serde_json::{Value, json};

use crate::AppView;
use crate::composer::MODEL_CATALOG;
use crate::settings_misc::{
    self, AUTO_VALUE, OpenList, danger, edit_field, exit_failure_message, field_text_in,
    group_label, pack_model, setting_row, switch_control,
};
use crate::text_field::{PromptSubmitted, TextField};
use crate::theme;

/// `sourceControlWritingStyle.mode` options — labels/descriptions from
/// `MODE_OPTIONS` in apps/web/…/SourceControlWritingSettings.tsx.
const WRITING_MODES: &[(&str, &str, &str)] = &[
    (
        "repo_conventions",
        "Repository conventions",
        "Match each project's commit and pull request history.",
    ),
    (
        "conventional_commits",
        "Conventional Commits",
        "Conventional Commit prefixes, short PR titles and descriptions.",
    ),
    (
        "custom",
        "Custom instructions",
        "Your instructions shape commit and pull request text.",
    ),
];

/// `worktreeSubmodules` options — `WorktreeSubmodules` literals.
const SUBMODULE_MODES: &[(&str, &str)] = &[
    ("recursive", "Recursive"),
    ("top-level", "Top level only"),
    ("none", "None"),
];

/// `sidebarAutoSettleAfterDays` choices (value string → days; "off" → null).
const AUTO_SETTLE_DAYS: &[(&str, &str)] = &[
    ("off", "Off"),
    ("1", "1 day"),
    ("3", "3 days"),
    ("7", "7 days"),
    ("14", "14 days"),
    ("30", "30 days"),
];

/// `responseStreamingMode` options.
const STREAMING_MODES: &[(&str, &str, &str)] = &[
    (
        "turn",
        "After each turn",
        "Update the transcript when a turn finishes.",
    ),
    (
        "paragraph",
        "Live paragraphs",
        "Stream paragraphs as the agent writes them.",
    ),
];

/// State for the sections in this file. It hangs off
/// `settings_misc::State::extra` — the AppView field list lives in lib.rs.
#[derive(Default)]
pub(crate) struct ExtraState {
    /// TextField entities + subscriptions are created lazily on first
    /// section render (`cx.new` needs a context; the deferred-open pattern
    /// from settings_backend.rs).
    prepared: Cell<bool>,

    // ── Keybindings ─────────────────────────────────────────────────────
    kb_loaded: Cell<bool>,
    kb_loading: Cell<bool>,
    /// Raw rules from `server.getConfig → keybindings.bindings`
    /// (`{key, command, when?}`).
    kb_bindings: Vec<Value>,
    /// `keybindings.issues` from the same snapshot.
    kb_issues: Vec<Value>,
    /// `keybindingsConfigPath` for display.
    kb_path: Option<String>,
    kb_error: Option<String>,
    kb_add_open: bool,
    kb_saving: bool,
    kb_command_draft: String,
    kb_key_draft: String,
    kb_when_draft: String,
    kb_command_field: Option<Entity<TextField>>,
    kb_key_field: Option<Entity<TextField>>,
    kb_when_field: Option<Entity<TextField>>,
    kb_subs: Vec<Subscription>,

    // ── Source Control ──────────────────────────────────────────────────
    sc_loaded: Cell<bool>,
    sc_loading: Cell<bool>,
    /// `sourceControl.appAccess → status` ("signed-out" on this server).
    sc_status: Option<String>,
    /// Busy flag for the Sign-in round trip.
    sc_busy: bool,
    /// Status line under the provider row (sign-in outcome or load error).
    sc_note: Option<String>,
    sc_instructions_field: Option<Entity<TextField>>,
    sc_instructions_draft: String,
    /// True once the field was seeded from the settings mirror (don't
    /// clobber an in-progress edit on refresh).
    sc_instructions_seeded: bool,

    // ── Threads ─────────────────────────────────────────────────────────
    base_dir_field: Option<Entity<TextField>>,
    base_dir_draft: String,
    base_dir_seeded: bool,
    extra_subs: Vec<Subscription>,
}

impl ExtraState {
    fn reset_form(&mut self) {
        self.kb_add_open = false;
        self.kb_command_draft.clear();
        self.kb_key_draft.clear();
        self.kb_when_draft.clear();
    }
}

impl AppView {
    // ─── lazy loads ─────────────────────────────────────────────────────

    /// First-render `server.getConfig` for the keybindings mirror.
    /// (The misc mirror extracts `settings`; this one extracts
    /// `keybindings.*` — same payload, separate flag so sections stay
    /// independent.)
    fn maybe_load_keybindings(&self, cx: &mut Context<Self>) {
        let state = &self.misc_settings_state.extra;
        if state.kb_loaded.get() || state.kb_loading.get() {
            return;
        }
        match self.send_query("server.getConfig", json!({})) {
            Some(rx) => {
                state.kb_loading.set(true);
                cx.spawn(async move |this, cx| {
                    let reply = rx.recv().await;
                    let _ = this.update(cx, |this, cx| {
                        this.apply_keybindings_reply(reply);
                        cx.notify();
                    });
                })
                .detach();
            }
            None => state.kb_loaded.set(true),
        }
    }

    /// Fold a `server.getConfig` Exit into the keybindings mirror.
    fn apply_keybindings_reply(&mut self, reply: Result<Value, smol::channel::RecvError>) {
        let state = &mut self.misc_settings_state.extra;
        state.kb_loading.set(false);
        state.kb_loaded.set(true);
        match reply {
            Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                let value = &exit["value"];
                state.kb_bindings = value["keybindings"]["bindings"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                state.kb_issues = value["keybindings"]["issues"]
                    .as_array()
                    .or_else(|| value["issues"].as_array())
                    .cloned()
                    .unwrap_or_default();
                state.kb_path = value["keybindingsConfigPath"].as_str().map(str::to_string);
                state.kb_error = None;
            }
            Ok(exit) => {
                state.kb_error = Some(format!(
                    "Keybindings failed to load: {}",
                    exit_failure_message(&exit)
                ));
            }
            Err(_) => {
                state.kb_error = Some("Lost connection before the reply arrived.".into());
            }
        }
    }

    /// Refetch after a keybinding write — the upsert/remove replies carry
    /// resolved rules, not the raw `bindings` this list renders.
    fn reload_keybindings(&mut self, cx: &mut Context<Self>) {
        self.misc_settings_state.extra.kb_loaded.set(false);
        self.maybe_load_keybindings(cx);
    }

    /// First-render `sourceControl.appAccess` (provider sign-in status).
    fn maybe_load_source_control(&self, cx: &mut Context<Self>) {
        let state = &self.misc_settings_state.extra;
        if state.sc_loaded.get() || state.sc_loading.get() {
            return;
        }
        match self.send_query("sourceControl.appAccess", json!({})) {
            Some(rx) => {
                state.sc_loading.set(true);
                cx.spawn(async move |this, cx| {
                    let reply = rx.recv().await;
                    let _ = this.update(cx, |this, cx| {
                        let state = &mut this.misc_settings_state.extra;
                        state.sc_loading.set(false);
                        state.sc_loaded.set(true);
                        match reply {
                            Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                                state.sc_status = exit["value"]["status"]
                                    .as_str()
                                    .map(str::to_string)
                                    .or_else(|| Some("status unknown".into()));
                            }
                            Ok(exit) => {
                                state.sc_note = Some(exit_failure_message(&exit));
                            }
                            Err(_) => {
                                state.sc_note =
                                    Some("Lost connection before the reply arrived.".into());
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            None => state.sc_loaded.set(true),
        }
    }

    /// Lazily build the extra-section TextFields + subscriptions on first
    /// render of a section that needs them (deferred-open pattern — render
    /// is `&self`, so this queues one `this.update` that creates them).
    fn maybe_prepare_extra(&self, cx: &mut Context<Self>) {
        if self.misc_settings_state.extra.prepared.get() {
            return;
        }
        self.misc_settings_state.extra.prepared.set(true);
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |this, cx| {
                this.ensure_extra_fields(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Create the persistent extra-section fields (`server.getConfig`
    /// values are seeded in whenever they arrive or already exist).
    fn ensure_extra_fields(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.misc_settings_state.extra;
        if state.base_dir_field.is_none() {
            let field = cx.new(|cx| TextField::new("~/ (leave empty for home)", cx));
            state.extra_subs.push(cx.observe(&field, |this, field, cx| {
                this.misc_settings_state.extra.base_dir_draft =
                    settings_misc::field_text(&field, cx);
                cx.notify();
            }));
            state.extra_subs.push(cx.subscribe(
                &field,
                |this, _field, event: &PromptSubmitted, cx| {
                    let value = event.0.to_string();
                    this.update_server_settings(
                        json!({ "addProjectBaseDirectory": value.trim() }),
                        cx,
                    );
                },
            ));
            state.base_dir_field = Some(field);
        }
        if state.sc_instructions_field.is_none() {
            let field = cx.new(|cx| TextField::new("Instructions for commit / PR text", cx));
            state.extra_subs.push(cx.observe(&field, |this, field, cx| {
                this.misc_settings_state.extra.sc_instructions_draft =
                    settings_misc::field_text(&field, cx);
                cx.notify();
            }));
            state.extra_subs.push(cx.subscribe(
                &field,
                |this, _field, event: &PromptSubmitted, cx| {
                    let instructions = event.0.to_string();
                    this.save_sc_instructions(instructions, cx);
                },
            ));
            state.sc_instructions_field = Some(field);
        }
        self.seed_extra_fields(cx);
    }

    /// Push the loaded ServerSettings into the persistent fields once —
    /// either just after `ensure_extra_fields` or when `server.getConfig`
    /// lands (called from `maybe_load_misc_settings`' success arm too).
    pub(crate) fn seed_extra_fields(&mut self, cx: &mut Context<Self>) {
        let settings = self.misc_settings_state.settings.clone();
        let state = &mut self.misc_settings_state.extra;
        if !state.base_dir_seeded
            && let Some(field) = state.base_dir_field.clone()
        {
            let value = settings["addProjectBaseDirectory"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if !value.is_empty() {
                state.base_dir_draft = value.clone();
                edit_field(&field, cx, |field, window, cx| {
                    field.reset(window, cx);
                    field.replace_text_in_range(Some(0..0), &value, window, cx);
                });
            }
            state.base_dir_seeded = true;
        }
        if !state.sc_instructions_seeded
            && let Some(field) = state.sc_instructions_field.clone()
            && let Some(instructions) =
                settings["sourceControlWritingStyle"]["customInstructions"].as_str()
        {
            if !instructions.is_empty() {
                state.sc_instructions_draft = instructions.to_string();
                edit_field(&field, cx, |field, window, cx| {
                    field.reset(window, cx);
                    field.replace_text_in_range(Some(0..0), instructions, window, cx);
                });
            }
            state.sc_instructions_seeded = true;
        }
    }

    /// `sourceControlWritingStyle.customInstructions` — merged into the
    /// style object so `mode`/`followChangeRequestTemplates` survive.
    fn save_sc_instructions(&mut self, instructions: String, cx: &mut Context<Self>) {
        let mut style = self.misc_settings_state.settings["sourceControlWritingStyle"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        style.insert("mode".into(), json!("custom"));
        style.insert("customInstructions".into(), json!(instructions));
        self.update_server_settings(
            json!({ "sourceControlWritingStyle": Value::Object(style) }),
            cx,
        );
    }

    // ─── Keybindings ────────────────────────────────────────────────────

    /// "+ Add rule" — build the three fields once, clear drafts, focus the
    /// command input.
    fn open_kb_add_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        {
            let state = &mut self.misc_settings_state.extra;
            if state.kb_command_field.is_none() {
                let command_field = cx.new(|cx| {
                    TextField::new("Command (e.g. chat.new or script.my-script.run)", cx)
                });
                let key_field = cx.new(|cx| TextField::new("Shortcut (e.g. mod+shift+k)", cx));
                let when_field = cx.new(|cx| TextField::new("When expression (optional)", cx));
                state
                    .kb_subs
                    .push(cx.observe(&command_field, |this, field, cx| {
                        this.misc_settings_state.extra.kb_command_draft =
                            settings_misc::field_text(&field, cx);
                        cx.notify();
                    }));
                state
                    .kb_subs
                    .push(cx.observe(&key_field, |this, field, cx| {
                        this.misc_settings_state.extra.kb_key_draft =
                            settings_misc::field_text(&field, cx);
                        cx.notify();
                    }));
                state
                    .kb_subs
                    .push(cx.observe(&when_field, |this, field, cx| {
                        this.misc_settings_state.extra.kb_when_draft =
                            settings_misc::field_text(&field, cx);
                        cx.notify();
                    }));
                for field in [command_field.clone(), key_field.clone(), when_field.clone()] {
                    state.kb_subs.push(cx.subscribe(
                        &field,
                        |this, _field, _event: &PromptSubmitted, cx| {
                            this.save_keybinding(cx);
                        },
                    ));
                }
                state.kb_command_field = Some(command_field);
                state.kb_key_field = Some(key_field);
                state.kb_when_field = Some(when_field);
            }
            state.reset_form();
            state.kb_add_open = true;
            state.kb_error = None;
        }
        for field in [
            self.misc_settings_state.extra.kb_command_field.clone(),
            self.misc_settings_state.extra.kb_key_field.clone(),
            self.misc_settings_state.extra.kb_when_field.clone(),
        ]
        .into_iter()
        .flatten()
        {
            field.update(cx, |field, cx| field.reset(window, cx));
        }
        if let Some(field) = self.misc_settings_state.extra.kb_command_field.clone() {
            field.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// Save via `server.upsertKeybinding {key, command, when?}` — the
    /// server validates the shortcut grammar and when-expression; failures
    /// surface as `kb_error`.
    fn save_keybinding(&mut self, cx: &mut Context<Self>) {
        if self.misc_settings_state.extra.kb_saving {
            return;
        }
        let (command, key, when) = {
            let state = &self.misc_settings_state.extra;
            (
                state.kb_command_draft.trim().to_string(),
                state.kb_key_draft.trim().to_string(),
                state.kb_when_draft.trim().to_string(),
            )
        };
        if command.is_empty() || key.is_empty() {
            return;
        }
        let mut payload = json!({ "command": command, "key": key });
        if !when.is_empty() {
            payload["when"] = json!(when);
        }
        let Some(rx) = self.send_query("server.upsertKeybinding", payload) else {
            self.misc_settings_state.extra.kb_error = Some("Not connected to a server.".into());
            cx.notify();
            return;
        };
        let state = &mut self.misc_settings_state.extra;
        state.kb_saving = true;
        state.kb_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.misc_settings_state.extra;
                state.kb_saving = false;
                match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        state.kb_error = None;
                        state.reset_form();
                        this.reload_keybindings(cx);
                    }
                    Ok(exit) => {
                        state.kb_error = Some(exit_failure_message(&exit));
                    }
                    Err(_) => {
                        state.kb_error = Some("Lost connection before the reply arrived.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Read the three fields inside a click listener (has `window`), then
    /// save.
    fn save_keybinding_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (field, slot) in [
            self.misc_settings_state
                .extra
                .kb_command_field
                .clone()
                .map(|f| (f, "command")),
            self.misc_settings_state
                .extra
                .kb_key_field
                .clone()
                .map(|f| (f, "key")),
            self.misc_settings_state
                .extra
                .kb_when_field
                .clone()
                .map(|f| (f, "when")),
        ]
        .into_iter()
        .flatten()
        {
            let text = field_text_in(&field, window, cx);
            let state = &mut self.misc_settings_state.extra;
            match slot {
                "command" => state.kb_command_draft = text,
                "key" => state.kb_key_draft = text,
                _ => state.kb_when_draft = text,
            }
        }
        self.save_keybinding(cx);
    }

    /// Per-row delete: `server.removeKeybinding` with the rule's raw
    /// `{key, command, when?}` (the server matches on parsed identity).
    fn remove_keybinding(&mut self, rule: Value, cx: &mut Context<Self>) {
        let Some(rx) = self.send_query("server.removeKeybinding", rule) else {
            self.misc_settings_state.extra.kb_error = Some("Not connected to a server.".into());
            cx.notify();
            return;
        };
        self.misc_settings_state.extra.kb_error = None;
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.misc_settings_state.extra;
                match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        state.kb_error = None;
                        this.reload_keybindings(cx);
                    }
                    Ok(exit) => {
                        state.kb_error = Some(exit_failure_message(&exit));
                    }
                    Err(_) => {
                        state.kb_error = Some("Lost connection before the reply arrived.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn render_keybindings_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_keybindings(cx);
        let state = &self.misc_settings_state.extra;
        let connected = self.commands.is_some();
        let mut section = div().flex().flex_col();

        if !connected {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Connect to a server to edit keybindings."),
            );
            return section;
        }
        if state.kb_loading.get() && state.kb_bindings.is_empty() {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Loading keybindings…"),
            );
        }
        if let Some(path) = &state.kb_path {
            section = section.child(
                div()
                    .pb_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(format!("Editing {path} on the server.")),
            );
        }
        if let Some(error) = &state.kb_error {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(error.clone()),
            );
        }
        for issue in &state.kb_issues {
            let message = issue["message"].as_str().unwrap_or("Invalid rule");
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(format!("⚠ {message}")),
            );
        }

        section = section.child(group_label("Rules (action → keystroke)"));
        if state.kb_bindings.is_empty() && !state.kb_loading.get() {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No custom keybindings — the server defaults apply."),
            );
        }
        for (index, rule) in state.kb_bindings.iter().enumerate() {
            let command = rule["command"].as_str().unwrap_or("").to_string();
            let key = rule["key"].as_str().unwrap_or("").to_string();
            let when = rule["when"].as_str().map(str::to_string);
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .py_1p5()
                    .text_sm()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme::fg())
                            .child(command),
                    )
                    .when_some(when, |s, when| {
                        s.child(
                            div()
                                .flex_none()
                                .max_w(px(140.))
                                .truncate()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child(when),
                        )
                    })
                    .child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::canvas())
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child(key),
                    )
                    .child(
                        div()
                            .id(("kb-remove", index))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .bg(theme::hover())
                            .text_color(theme::fg_muted())
                            .hover(|s| s.bg(theme::selected()))
                            .child("Remove")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener({
                                    let rule = rule.clone();
                                    move |this, _ev, _window, cx| {
                                        this.remove_keybinding(rule.clone(), cx);
                                    }
                                }),
                            ),
                    ),
            );
        }

        if state.kb_add_open {
            let mut form = div().flex().flex_col().gap_2().pt_2();
            for (slot, field) in [
                ("Command", state.kb_command_field.clone()),
                ("Shortcut", state.kb_key_field.clone()),
                ("When (optional)", state.kb_when_field.clone()),
            ] {
                if let Some(field) = field {
                    form = form.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_xs().text_color(theme::fg_muted()).child(slot))
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(theme::border())
                                    .bg(theme::canvas())
                                    .text_sm()
                                    .text_color(theme::fg())
                                    .child(field),
                            ),
                    );
                }
            }
            let ok = !state.kb_command_draft.trim().is_empty()
                && !state.kb_key_draft.trim().is_empty()
                && !state.kb_saving;
            let mut save_btn = div()
                .id("kb-save")
                .px_3()
                .py_1p5()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .bg(theme::primary())
                .text_color(gpui::white())
                .child(if state.kb_saving {
                    "Saving…"
                } else {
                    "Save rule"
                });
            if !ok {
                save_btn = save_btn.opacity(0.4);
            }
            save_btn = save_btn.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| {
                    this.save_keybinding_click(window, cx);
                }),
            );
            form = form.child(
                div().flex().items_center().gap_2().child(save_btn).child(
                    div()
                        .id("kb-cancel")
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .bg(theme::hover())
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::selected()))
                        .child("Cancel")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _window, cx| {
                                this.misc_settings_state.extra.kb_add_open = false;
                                cx.notify();
                            }),
                        ),
                ),
            );
            section = section.child(group_label("Add rule")).child(form);
        } else {
            section = section.child(
                div().pt_3().flex().child(
                    div()
                        .id("kb-add-open")
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .bg(theme::hover())
                        .text_color(theme::fg())
                        .hover(|s| s.bg(theme::selected()))
                        .child("+ Add rule")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, window, cx| {
                                this.open_kb_add_form(window, cx);
                            }),
                        ),
                ),
            );
        }

        section = section.child(
            div()
                .pt_3()
                .text_xs()
                .text_color(theme::fg_muted())
                .child("The web editor adds a command picker, keystroke capture, conflict highlighting, and default-binding reset — text entry here covers the same wire rules."),
        );
        section
    }

    // ─── Source Control ─────────────────────────────────────────────────

    /// Sign-in click: `sourceControl.signInStart` — this server always
    /// fails with a readable "run `gh auth login` directly" error that is
    /// shown under the provider row.
    fn source_control_sign_in(&mut self, cx: &mut Context<Self>) {
        let Some(rx) =
            self.send_query("sourceControl.signInStart", json!({ "host": "github.com" }))
        else {
            self.misc_settings_state.extra.sc_note = Some("Not connected to a server.".into());
            cx.notify();
            return;
        };
        let state = &mut self.misc_settings_state.extra;
        state.sc_busy = true;
        state.sc_note = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let reply = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.misc_settings_state.extra;
                state.sc_busy = false;
                match reply {
                    Ok(exit) if exit["_tag"].as_str() == Some("Success") => {
                        state.sc_note = Some("Sign-in started.".into());
                        state.sc_loaded.set(false);
                        this.maybe_load_source_control(cx);
                    }
                    Ok(exit) => {
                        state.sc_note = Some(exit_failure_message(&exit));
                    }
                    Err(_) => {
                        state.sc_note = Some("Lost connection before the reply arrived.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn render_source_control_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_source_control(cx);
        self.maybe_load_misc_settings(cx);
        self.maybe_prepare_extra(cx);
        let state = &self.misc_settings_state.extra;
        let settings = &self.misc_settings_state.settings;
        let connected = self.commands.is_some();
        let mut section = div().flex().flex_col();

        if !connected {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Connect to a server to see source control providers."),
            );
            return section;
        }

        // ── Providers ───────────────────────────────────────────────────
        section = section.child(group_label("Providers"));
        let signed_in = state.sc_status.as_deref() == Some("signed-in")
            || state.sc_status.as_deref() == Some("signed_in");
        let status_label = if state.sc_loading.get() && state.sc_status.is_none() {
            "Checking…"
        } else {
            state.sc_status.as_deref().unwrap_or("status unknown")
        };
        let mut row = div()
            .flex()
            .items_center()
            .gap_3()
            .py_1p5()
            .text_sm()
            .child(
                div()
                    .w(px(8.))
                    .h(px(8.))
                    .flex_none()
                    .rounded_full()
                    .bg(if signed_in {
                        gpui::rgb(0x34d399)
                    } else {
                        theme::border()
                    }),
            )
            .child(
                div()
                    .w(px(140.))
                    .flex_none()
                    .text_color(theme::fg())
                    .child("GitHub"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(status_label.replace('-', " ")),
            );
        let mut sign_in_btn = div()
            .id("sc-sign-in")
            .px_3()
            .py_1p5()
            .rounded_md()
            .text_xs()
            .cursor_pointer()
            .bg(theme::hover())
            .text_color(theme::fg())
            .child(if state.sc_busy {
                "Signing in…"
            } else {
                "Sign in"
            });
        if state.sc_busy {
            sign_in_btn = sign_in_btn.opacity(0.4);
        }
        sign_in_btn = sign_in_btn.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _ev, _window, cx| {
                this.source_control_sign_in(cx);
            }),
        );
        row = row.child(sign_in_btn);
        section = section.child(row);
        if let Some(note) = &state.sc_note {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(note.clone()),
            );
        }
        section = section.child(
            div()
                .py_1p5()
                .text_xs()
                .text_color(theme::fg_muted())
                .child(
                    "CLI/host detection (git, gh, jj) needs server.discoverSourceControl, \
                     which this server does not implement — sign-in runs `gh auth login` \
                     on the server machine.",
                ),
        );

        // ── Writing (ServerSettings) ────────────────────────────────────
        if let Some(error) = &self.misc_settings_state.settings_error {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(error.clone()),
            );
        }
        section = section.child(group_label("Commit & pull request writing"));

        let style = &settings["sourceControlWritingStyle"];
        let mode = style["mode"].as_str().unwrap_or("repo_conventions");
        let mode_label = WRITING_MODES
            .iter()
            .find(|(value, _, _)| *value == mode)
            .map(|(_, label, _)| *label)
            .unwrap_or(mode);
        section = section.child(
            self.select_setting(
                "Writing style",
                "How generated commit messages and pull request text are shaped.",
                OpenList::ScWritingMode,
                mode_label.into(),
                mode,
                WRITING_MODES
                    .iter()
                    .map(|(value, label, detail)| {
                        (
                            value.to_string(),
                            label.to_string(),
                            Some(detail.to_string()),
                        )
                    })
                    .collect(),
                cx,
            ),
        );
        if mode == "custom" {
            if let Some(field) = state.sc_instructions_field.clone() {
                section = section.child(setting_row(
                    "Custom instructions",
                    "Enter saves. Applied to generated commit messages and PR text.",
                    div()
                        .w(px(220.))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::canvas())
                        .text_sm()
                        .text_color(theme::fg())
                        .child(field),
                ));
            }
        }

        // Writer model — same ModelSelection shape as Chat's defaults.
        let writer_selection = &settings["sourceControlWriterModelSelection"];
        let writer_value = if writer_selection.is_null() {
            AUTO_VALUE.to_string()
        } else {
            pack_model(
                writer_selection["instanceId"]
                    .as_str()
                    .or_else(|| writer_selection["provider"].as_str())
                    .unwrap_or_default(),
                writer_selection["model"].as_str().unwrap_or_default(),
            )
        };
        let writer_label: SharedString = if writer_selection.is_null() {
            "Automatic".into()
        } else {
            Self::selection_label(writer_selection).into()
        };
        let mut writer_options = vec![(
            AUTO_VALUE.to_string(),
            "Automatic".to_string(),
            Some("Follows the text generation model.".to_string()),
        )];
        for (instance, model, label) in MODEL_CATALOG {
            writer_options.push((
                pack_model(instance, model),
                (*label).to_string(),
                Some(format!("{instance}/{model}")),
            ));
        }
        section = section.child(self.select_setting(
            "Writing model",
            "Model used for generated source control text.",
            OpenList::ScWriterModel,
            writer_label,
            &writer_value,
            writer_options,
            cx,
        ));

        section
    }

    // ─── Threads (web Threads + Diffs + Worktrees panels) ───────────────

    pub(crate) fn render_threads_section(&self, cx: &mut Context<Self>) -> Div {
        self.maybe_load_misc_settings(cx);
        self.maybe_prepare_extra(cx);
        let settings = &self.misc_settings_state.settings;
        let connected = self.commands.is_some();
        let mut section = div().flex().flex_col();

        if let Some(error) = &self.misc_settings_state.settings_error {
            section = section.child(
                div()
                    .py_1p5()
                    .text_xs()
                    .text_color(danger())
                    .child(error.clone()),
            );
        }

        // ── Diffs (client-persisted — the web's ClientSettings) ─────────
        section = section.child(group_label("Diffs"));
        let layout = self.client_config.diff_layout.clone();
        let layout_label = if layout == "split" {
            "Split"
        } else {
            "Stacked"
        };
        section = section.child(self.select_setting(
            "Diff layout",
            "Show diffs stacked or side by side.",
            OpenList::DiffLayout,
            layout_label.into(),
            &layout,
            vec![
                ("stacked".to_string(), "Stacked".to_string(), None),
                ("split".to_string(), "Split".to_string(), None),
            ],
            cx,
        ));
        let ignore_ws = self.client_config.diff_ignore_whitespace;
        section = section.child(setting_row(
            "Hide whitespace changes",
            "Diffs ignore whitespace-only edits by default.",
            div()
                .id("threads-ignore-ws")
                .cursor_pointer()
                .child(switch_control(ignore_ws))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.client_config.diff_ignore_whitespace = !ignore_ws;
                        this.client_config.save();
                        cx.notify();
                    }),
                ),
        ));
        let collapsed = self.client_config.diff_files_collapsed;
        section = section.child(self.select_setting(
            "Default file state",
            "Start with files expanded or collapsed when opening diffs.",
            OpenList::DiffFiles,
            if collapsed { "Collapsed" } else { "Expanded" }.into(),
            if collapsed { "collapsed" } else { "expanded" },
            vec![
                ("expanded".to_string(), "Expanded".to_string(), None),
                ("collapsed".to_string(), "Collapsed".to_string(), None),
            ],
            cx,
        ));
        let proactive = self.client_config.proactive_panels;
        section = section.child(setting_row(
            "Proactive panels",
            "Open linked pull requests first; otherwise open Changes for edits to at least 3 files or 50 lines.",
            div()
                .id("threads-proactive")
                .cursor_pointer()
                .child(switch_control(proactive))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.client_config.proactive_panels = !proactive;
                        this.client_config.save();
                        cx.notify();
                    }),
                ),
        ));

        if connected {
            // ── New projects (ServerSettings) ────────────────────────────
            section = section.child(group_label("New projects"));
            let base_dir_field = self.misc_settings_state.extra.base_dir_field.clone();
            if let Some(field) = base_dir_field {
                section = section.child(setting_row(
                    "Add project starts in",
                    "Base folder for the project browser. Enter saves; empty means home.",
                    div()
                        .w(px(200.))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::canvas())
                        .text_sm()
                        .text_color(theme::fg())
                        .child(field),
                ));
            }
            let ghq = settings["ghqCloneLayout"].as_bool() == Some(true);
            section = section.child(setting_row(
                "Clone into host folders",
                "Clones go to github.com/owner/repo under the folder above, like ghq.",
                div()
                    .id("threads-ghq")
                    .cursor_pointer()
                    .child(switch_control(ghq))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.update_server_settings(json!({ "ghqCloneLayout": !ghq }), cx);
                        }),
                    ),
            ));

            // ── Worktrees (ServerSettings) ───────────────────────────────
            section = section.child(group_label("Worktrees"));
            let from_origin = settings["newWorktreesStartFromOrigin"].as_bool() == Some(true);
            section = section.child(setting_row(
                "Start new worktrees from remote",
                "Use the base branch's tracked remote branch instead of the local one.",
                div()
                    .id("threads-wt-origin")
                    .cursor_pointer()
                    .child(switch_control(from_origin))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.update_server_settings(
                                json!({ "newWorktreesStartFromOrigin": !from_origin }),
                                cx,
                            );
                        }),
                    ),
            ));
            let submodules = settings["worktreeSubmodules"].as_str();
            let submodules_value = submodules.unwrap_or("inherit");
            let submodules_label = match submodules {
                None => "Inherit",
                Some("recursive") => "Recursive",
                Some("top-level") => "Top level only",
                Some("none") => "None",
                Some(other) => other,
            };
            let mut submodule_options = vec![(
                "inherit".to_string(),
                "Inherit".to_string(),
                Some("Follow the project's t3.json.".to_string()),
            )];
            for (value, label) in SUBMODULE_MODES {
                submodule_options.push((value.to_string(), label.to_string(), None));
            }
            section = section.child(self.select_setting(
                "Submodules",
                "How new worktrees check out git submodules.",
                OpenList::WorktreeSubmodules,
                submodules_label.into(),
                submodules_value,
                submodule_options,
                cx,
            ));

            // ── Usage limits (ServerSettings) ────────────────────────────
            section = section.child(group_label("Usage limits"));
            let auto_resume = settings["autoResumeLimitedThreads"].as_bool() == Some(true);
            section = section.child(setting_row(
                "Resume after usage limits",
                "Automatically resume threads when their usage limits reset.",
                div()
                    .id("threads-auto-resume")
                    .cursor_pointer()
                    .child(switch_control(auto_resume))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.update_server_settings(
                                json!({ "autoResumeLimitedThreads": !auto_resume }),
                                cx,
                            );
                        }),
                    ),
            ));
            let snooze_limited = settings["snoozeLimitedThreads"].as_bool() == Some(true);
            section = section.child(setting_row(
                "Snooze until limits reset",
                "Snooze threads until their usage limits reset. Enable auto-resume to continue the agent after the reset.",
                div()
                    .id("threads-snooze-limited")
                    .cursor_pointer()
                    .child(switch_control(snooze_limited))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.update_server_settings(
                                json!({ "snoozeLimitedThreads": !snooze_limited }),
                                cx,
                            );
                        }),
                    ),
            ));

            // ── Auto-settle (ServerSettings) ─────────────────────────────
            section = section.child(group_label("Auto-settle"));
            let settle_merge = settings["sidebarAutoSettleOnMerge"].as_bool() == Some(true);
            section = section.child(setting_row(
                "Settle merged threads",
                "Settle a thread when its pull request merges. Closed pull requests still settle automatically.",
                div()
                    .id("threads-settle-merge")
                    .cursor_pointer()
                    .child(switch_control(settle_merge))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.update_server_settings(
                                json!({ "sidebarAutoSettleOnMerge": !settle_merge }),
                                cx,
                            );
                        }),
                    ),
            ));
            let settle_days = settings["sidebarAutoSettleAfterDays"].as_i64();
            let days_value = settle_days
                .map(|d| d.to_string())
                .unwrap_or_else(|| "off".into());
            let days_label = AUTO_SETTLE_DAYS
                .iter()
                .find(|(v, _)| *v == days_value)
                .map(|(_, label)| *label)
                .unwrap_or("Off");
            section = section.child(
                self.select_setting(
                    "Settle inactive threads",
                    "Sidebar threads with no activity for this long settle automatically.",
                    OpenList::AutoSettleDays,
                    days_label.into(),
                    &days_value,
                    AUTO_SETTLE_DAYS
                        .iter()
                        .map(|(value, label)| (value.to_string(), label.to_string(), None))
                        .collect(),
                    cx,
                ),
            );
            if settle_days.is_some() {
                let keep_prs =
                    settings["sidebarAutoSettleKeepPullRequests"].as_bool() == Some(true);
                section = section.child(setting_row(
                    "Keep threads with a PR",
                    "Inactivity does not settle threads with a pull request. Merge and close still do.",
                    div()
                        .id("threads-keep-prs")
                        .cursor_pointer()
                        .child(switch_control(keep_prs))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                this.update_server_settings(
                                    json!({ "sidebarAutoSettleKeepPullRequests": !keep_prs }),
                                    cx,
                                );
                            }),
                        ),
                ));
            }

            // ── Text generation (ServerSettings) ─────────────────────────
            section = section.child(group_label("Text generation"));
            let textgen_selection = &settings["textGenerationModelSelection"];
            let textgen_value = if textgen_selection.is_null() {
                AUTO_VALUE.to_string()
            } else {
                pack_model(
                    textgen_selection["instanceId"]
                        .as_str()
                        .or_else(|| textgen_selection["provider"].as_str())
                        .unwrap_or_default(),
                    textgen_selection["model"].as_str().unwrap_or_default(),
                )
            };
            let textgen_label: SharedString = if textgen_selection.is_null() {
                "Automatic".into()
            } else {
                Self::selection_label(textgen_selection).into()
            };
            let mut textgen_options = vec![(
                AUTO_VALUE.to_string(),
                "Automatic".to_string(),
                Some("The provider's default text model.".to_string()),
            )];
            for (instance, model, label) in MODEL_CATALOG {
                textgen_options.push((
                    pack_model(instance, model),
                    (*label).to_string(),
                    Some(format!("{instance}/{model}")),
                ));
            }
            section = section.child(self.select_setting(
                "Text generation model",
                "For thread titles and other generated text.",
                OpenList::TextGenModel,
                textgen_label,
                &textgen_value,
                textgen_options,
                cx,
            ));
            let streaming = settings["responseStreamingMode"].as_str().unwrap_or("turn");
            let streaming_label = STREAMING_MODES
                .iter()
                .find(|(v, _, _)| *v == streaming)
                .map(|(_, label, _)| *label)
                .unwrap_or("After each turn");
            section = section.child(
                self.select_setting(
                    "Response streaming",
                    "How agent replies appear while a turn is running.",
                    OpenList::ResponseStreaming,
                    streaming_label.into(),
                    streaming,
                    STREAMING_MODES
                        .iter()
                        .map(|(value, label, detail)| {
                            (
                                value.to_string(),
                                label.to_string(),
                                Some(detail.to_string()),
                            )
                        })
                        .collect(),
                    cx,
                ),
            );
        }

        section = section.child(div().pt_3().text_xs().text_color(theme::fg_muted()).child(
            "The web also has Browser, Voice, Access/Connections, and Licenses settings — \
                     none apply to the native client (no embedded browser, no speech engine, \
                     single environment).",
        ));
        section
    }
}
