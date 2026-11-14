//! Composer: input field, attachments, provider/model pickers, mode
//! selectors, slash commands. Owns `impl AppView` composer render methods and
//! `composer::State`.
//!
//! `render_composer` is called from `Render for AppView` as `&self`, so every
//! interactive element uses window-level callbacks (`&mut Window` + `&mut App`)
//! and reaches the view through `window.root::<AppView>()` instead of
//! `cx.listener`. Keyboard handling lives in an `App::intercept_keystrokes`
//! hook installed lazily on the first composer interaction — it runs before
//! keymap/action dispatch, which is what lets Enter apply a slash command
//! instead of submitting the raw text.

use gpui::{
    ClipboardEntry, Context, Entity, EntityInputHandler, Focusable, KeystrokeEvent, MouseButton,
    SharedString, Subscription, Window, div, prelude::*, px,
};

use crate::AppView;
use crate::text_field::{PromptSubmitted, Submit, TextField};
use crate::theme;

/// Composer-local UI state. Expand freely — this field lives on `AppView`.
pub struct State {
    /// Open picker, if any (model / interaction mode / runtime mode / attach menu).
    pub picker: Option<PickerKind>,
    /// Staged attachments to send with the next message.
    pub attachments: Vec<Attachment>,
    /// Mirror of the composer `TextField`'s content, synced after each keystroke
    /// (drives the slash-command menu, which `&self` rendering cannot read).
    pub draft: String,
    /// Slash-command menu visible (dismissed by Esc until the draft changes).
    pub slash_open: bool,
    /// Highlighted row in the slash menu (arrow keys cycle, Enter applies).
    pub slash_selection: usize,
    /// `default` | `plan` — read by the orchestrator for the `interactionMode`
    /// field of `thread.create` payloads.
    pub interaction_mode: String,
    /// Runtime mode for the `runtimeMode` field of `thread.create` payloads.
    /// One of `full-access`, `approval-required`, `auto`, `auto-accept-edits`.
    pub runtime_mode: String,
    /// Inline path input staged by "Attach file…" (no native file dialog).
    pub attach_field: Option<Entity<TextField>>,
    attach_subscription: Option<Subscription>,
    key_subscription: Option<Subscription>,
    hooks_installed: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            picker: None,
            attachments: Vec::new(),
            draft: String::new(),
            slash_open: false,
            slash_selection: 0,
            interaction_mode: "default".to_string(),
            runtime_mode: "full-access".to_string(),
            attach_field: None,
            attach_subscription: None,
            key_subscription: None,
            hooks_installed: false,
        }
    }
}

impl State {
    /// Take the staged attachments and serialize them for the `"attachments"`
    /// array of a `message.dispatch` command. Emits the contract
    /// `ChatAttachment` shape (`type`/`id`/`name`/`mimeType`/`sizeBytes`);
    /// `path` and `dataUrl` are extra fields the Rust provider can consume.
    /// The orchestrator should call this while building the dispatch payload
    /// so the composer clears on send.
    pub fn drain_attachments(&mut self) -> Vec<serde_json::Value> {
        std::mem::take(&mut self.attachments)
            .into_iter()
            .map(|attachment| match attachment.kind {
                AttachmentKind::File { path } => {
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    serde_json::json!({
                        "type": "file",
                        "id": uuid::Uuid::new_v4().to_string().replace('-', ""),
                        "name": attachment.name,
                        "mimeType": "application/octet-stream",
                        "sizeBytes": size.max(1),
                        "path": path,
                    })
                }
                AttachmentKind::Image { bytes, mime } => serde_json::json!({
                    "type": "image",
                    "id": uuid::Uuid::new_v4().to_string().replace('-', ""),
                    "name": attachment.name,
                    "mimeType": &mime,
                    "sizeBytes": bytes.len(),
                    "dataUrl": format!("data:{};base64,{}", mime, base64_encode(&bytes)),
                }),
                AttachmentKind::Text { content } => serde_json::json!({
                    "type": "file",
                    "id": uuid::Uuid::new_v4().to_string().replace('-', ""),
                    "name": attachment.name,
                    "mimeType": "text/plain",
                    "sizeBytes": content.len().max(1),
                    "text": content,
                }),
            })
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Model,
    InteractionMode,
    RuntimeMode,
    /// The `+` menu (attach file / paste image).
    Attach,
}

/// A staged composer attachment (image bytes, file path, or pasted text).
pub struct Attachment {
    pub name: String,
    pub kind: AttachmentKind,
}

pub enum AttachmentKind {
    Image { bytes: Vec<u8>, mime: String },
    File { path: String },
    Text { content: String },
}

/// Static provider/model catalog for the model picker (the web client resolves
/// this from the server's provider list; the native client keeps it fixed).
/// Each row is `(instanceId, model, label)`.
const MODEL_CATALOG: &[(&str, &str, &str)] = &[
    ("devReplay", "showcase", "Replay · showcase"),
    ("devin", "swe-2-high", "Devin · SWE-2 High"),
    ("codex", "gpt-6-astra", "Codex · GPT-6 Astra"),
    ("codex", "gpt-5.3-codex", "Codex · GPT-5.3 Codex"),
    ("codex", "o4-mini", "Codex · o4-mini"),
    ("claudeAgent", "claude-opus-5-5", "Claude · Opus 5.5"),
    ("claudeAgent", "claude-sonnet-4-6", "Claude · Sonnet 4.6"),
    ("claudeAgent", "claude-haiku-5-5", "Claude · Haiku 5.5"),
    ("opencode", "openai/gpt-5", "OpenCode · GPT-5"),
    ("cursor", "composer-2", "Cursor · Composer 2"),
];

const INTERACTION_MODES: &[(&str, &str)] = &[
    ("default", "Default"),
    ("plan", "Plan"),
];

const RUNTIME_MODES: &[(&str, &str)] = &[
    ("full-access", "Full access"),
    ("auto", "Auto"),
    ("auto-accept-edits", "Auto-accept edits"),
    ("approval-required", "Supervised"),
];

struct SlashCommand {
    name: &'static str,
    description: &'static str,
}

const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "new",
        description: "Start a new thread",
    },
    SlashCommand {
        name: "plan",
        description: "Switch to plan mode",
    },
    SlashCommand {
        name: "model",
        description: "Choose a model",
    },
    SlashCommand {
        name: "compact",
        description: "Compact this thread",
    },
    SlashCommand {
        name: "stop",
        description: "Stop the running turn",
    },
];

/// Commands whose name matches the text typed after the leading `/`.
fn slash_commands_for(draft: &str) -> Vec<&'static SlashCommand> {
    let Some(rest) = draft.strip_prefix('/') else {
        return Vec::new();
    };
    if rest.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    SLASH_COMMANDS
        .iter()
        .filter(|command| command.name.starts_with(rest))
        .collect()
}

fn app_view(window: &mut Window) -> Option<Entity<AppView>> {
    window.root::<AppView>().flatten()
}

/// Install the app-wide keystroke hook once, from the first composer
/// interaction (key or click). The interceptor runs before action dispatch so
/// menu keys can be consumed instead of reaching the text field.
fn ensure_composer_hooks(window: &mut Window, cx: &mut gpui::App) {
    let Some(view) = app_view(window) else {
        return;
    };
    view.update(cx, |this, cx| {
        if this.composer_state.hooks_installed {
            return;
        }
        this.composer_state.hooks_installed = true;
        let subscription = cx.intercept_keystrokes(|event, window, cx| {
            on_composer_keystroke(event, window, cx);
        });
        this.composer_state.key_subscription = Some(subscription);
    });
}

fn on_composer_keystroke(event: &KeystrokeEvent, window: &mut Window, cx: &mut gpui::App) {
    // The text field applies this keystroke later in dispatch; re-read the
    // draft on the next frame so the mirror (and slash menu) stays current.
    window.on_next_frame(|window, cx| {
        let Some(view) = app_view(window) else {
            return;
        };
        view.update(cx, |this, cx| this.sync_composer_draft(window, cx));
    });

    let Some(view) = app_view(window) else {
        return;
    };
    let (slash_open, picker_open, attach_open) = {
        let state = &view.read(cx).composer_state;
        (
            state.slash_open,
            state.picker.is_some(),
            state.attach_field.is_some(),
        )
    };
    if !slash_open && !picker_open && !attach_open {
        return;
    }

    match event.keystroke.key.as_str() {
        "escape" => {
            view.update(cx, |this, cx| {
                this.composer_state.slash_open = false;
                this.composer_state.picker = None;
                this.composer_state.attach_field = None;
                this.composer_state.attach_subscription = None;
                cx.notify();
            });
            cx.stop_propagation();
        }
        "enter" if slash_open => {
            let applied = view.update(cx, |this, cx| {
                let commands = slash_commands_for(&this.composer_state.draft);
                let index = this
                    .composer_state
                    .slash_selection
                    .min(commands.len().saturating_sub(1));
                match commands.get(index) {
                    Some(command) => {
                        this.apply_slash_command(command.name, window, cx);
                        true
                    }
                    None => false,
                }
            });
            if applied {
                cx.stop_propagation();
            }
        }
        key @ ("up" | "down") if slash_open => {
            view.update(cx, |this, cx| {
                let count = slash_commands_for(&this.composer_state.draft).len();
                if count > 0 {
                    let selection = this.composer_state.slash_selection;
                    this.composer_state.slash_selection = if key == "down" {
                        (selection + 1) % count
                    } else {
                        selection.checked_sub(1).unwrap_or(count - 1)
                    };
                    cx.notify();
                }
            });
            cx.stop_propagation();
        }
        _ => {}
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(TABLE[(word >> 18) as usize & 0x3f] as char);
        out.push(TABLE[(word >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(word >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[word as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

fn interaction_mode_label(mode: &str) -> &'static str {
    INTERACTION_MODES
        .iter()
        .find(|(value, _)| *value == mode)
        .map(|(_, label)| *label)
        .unwrap_or("Default")
}

fn runtime_mode_label(mode: &str) -> &'static str {
    RUNTIME_MODES
        .iter()
        .find(|(value, _)| *value == mode)
        .map(|(_, label)| *label)
        .unwrap_or("Full access")
}

/// Shared styling for the floating menus above the composer.
fn menu_panel() -> gpui::Div {
    div()
        .absolute()
        .bottom_full()
        .left_0()
        .right_0()
        .mb_2()
        .flex()
        .flex_col()
        .rounded_lg()
        .bg(theme::raised())
        .border_1()
        .border_color(theme::border())
        .py_1()
        .max_h(px(280.))
        .overflow_y_hidden()
        .occlude()
}

fn menu_row(
    id: impl Into<gpui::ElementId>,
    title: impl Into<SharedString>,
    detail: Option<SharedString>,
    active: bool,
) -> gpui::Stateful<gpui::Div> {
    let mut row = div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1p5()
        .text_xs()
        .cursor(gpui::CursorStyle::PointingHand)
        .when(active, |s| {
            s.bg(theme::selected()).text_color(theme::fg())
        })
        .when(!active, |s| {
            s.text_color(theme::fg()).hover(|s| s.bg(theme::hover()))
        })
        .child(
            div()
                .min_w(px(120.))
                .text_color(theme::fg())
                .child(title.into()),
        );
    if let Some(detail) = detail {
        row = row.child(
            div()
                .flex_1()
                .truncate()
                .text_color(theme::fg_muted())
                .child(detail),
        );
    }
    row
}

/// Small footer control button (pill) that opens a picker.
fn footer_chip(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    kind: PickerKind,
    open: Option<PickerKind>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .text_xs()
        .cursor(gpui::CursorStyle::PointingHand)
        .when(open == Some(kind), |s| {
            s.bg(theme::selected()).text_color(theme::fg())
        })
        .when(open != Some(kind), |s| {
            s.text_color(theme::fg_muted()).hover(|s| s.bg(theme::hover()))
        })
        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
            ensure_composer_hooks(window, cx);
            let Some(view) = app_view(window) else {
                return;
            };
            view.update(cx, |this, cx| {
                this.composer_state.picker = if this.composer_state.picker == Some(kind) {
                    None
                } else {
                    Some(kind)
                };
                cx.notify();
            });
        })
        .child(label.into())
        .child(div().text_color(theme::fg_muted()).child("▾"))
}

impl AppView {
    /// Submit the composer draft exactly like pressing Enter in the input —
    /// routes the `Submit` action to the composer's focusable node, which emits
    /// `PromptSubmitted` through the existing subscription in `lib.rs`.
    pub fn send_composer(&self, window: &mut Window, cx: &mut gpui::App) {
        self.composer
            .read(cx)
            .focus_handle(cx)
            .dispatch_action(&Submit, window, cx);
    }

    /// Re-read the composer text field and refresh the mirrored draft plus
    /// slash-menu visibility. Only notifies when the draft actually changed.
    fn sync_composer_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.update(cx, |field, cx| {
            let len = field.text_length_utf16(window, cx).unwrap_or(0);
            field
                .text_for_range(0..len, &mut None, window, cx)
                .unwrap_or_default()
        });
        let state = &mut self.composer_state;
        if state.draft != text {
            state.draft = text;
            state.slash_open = state.draft.starts_with('/');
            state.slash_selection = 0;
            cx.notify();
        }
    }

    fn apply_slash_command(
        &mut self,
        name: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer
            .update(cx, |field, cx| field.reset(window, cx));
        self.composer_state.draft.clear();
        self.composer_state.slash_open = false;
        self.composer_state.slash_selection = 0;
        match name {
            // `/new`: the next message creates a fresh thread, like the web UI's
            // draft route. Local-only — nothing is dispatched.
            "new" => {
                self.composer_state.attachments.clear();
                self.state.selected = None;
            }
            "plan" => {
                self.composer_state.interaction_mode = "plan".to_string();
            }
            "model" => {
                self.composer_state.picker = Some(PickerKind::Model);
            }
            // The server treats a bare "/compact" user message as a compaction
            // request (IDLE_COMPACTION_TEXT in the contracts).
            "compact" => {
                if let Some(thread) = self.state.selected_thread() {
                    self.dispatch(serde_json::json!({
                        "type": "message.dispatch",
                        "commandId": uuid::Uuid::new_v4().to_string(),
                        "threadId": thread.id.to_string(),
                        "messageId": uuid::Uuid::new_v4().to_string(),
                        "text": "/compact",
                        "attachments": [],
                        "dispatchMode": { "type": "start_immediately" },
                        "createdBy": "user",
                        "creationSource": "web",
                    }));
                }
            }
            "stop" => {
                if let Some(thread) = self.state.selected_thread() {
                    self.dispatch(serde_json::json!({
                        "type": "thread.stop",
                        "commandId": uuid::Uuid::new_v4().to_string(),
                        "threadId": thread.id.to_string(),
                    }));
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// Stage a clipboard item as attachments: images become image attachments,
    /// external paths become file attachments (matches the web client's paste
    /// behavior). Text clipboard content is left for the input's own paste.
    fn stage_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        for entry in &item.entries {
            match entry {
                ClipboardEntry::Image(image) => {
                    self.composer_state.attachments.push(Attachment {
                        name: format!("pasted-image.{}", image.format.extension()),
                        kind: AttachmentKind::Image {
                            bytes: image.bytes.clone(),
                            mime: image.format.mime_type().to_string(),
                        },
                    });
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    for path in paths.paths() {
                        let display = path.display().to_string();
                        let name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| display.clone());
                        self.composer_state.attachments.push(Attachment {
                            name,
                            kind: AttachmentKind::File { path: display },
                        });
                    }
                }
                ClipboardEntry::String(_) => {}
            }
        }
        cx.notify();
    }

    /// Open the inline path input staged by "Attach file…". Its `PromptSubmitted`
    /// (Enter) becomes a file attachment instead of a message.
    fn open_attach_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let field = cx.new(|cx| TextField::new("File path…", cx));
        let subscription = window.subscribe(&field, cx, |_field, event: &PromptSubmitted, window, cx| {
            let Some(view) = app_view(window) else {
                return;
            };
            view.update(cx, |this, cx| {
                let path = event.0.trim().to_string();
                if !path.is_empty() {
                    let name = path
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(path.as_str())
                        .to_string();
                    this.composer_state.attachments.push(Attachment {
                        name,
                        kind: AttachmentKind::File { path },
                    });
                }
                this.composer_state.attach_field = None;
                this.composer_state.attach_subscription = None;
                cx.notify();
            });
        });
        field.read(cx).focus_handle(cx).focus(window, cx);
        self.composer_state.attach_field = Some(field);
        self.composer_state.attach_subscription = Some(subscription);
    }

    fn render_slash_menu(&self) -> impl IntoElement {
        let commands = slash_commands_for(&self.composer_state.draft);
        let selection = self.composer_state.slash_selection;
        let mut panel = menu_panel();
        if commands.is_empty() {
            panel = panel.child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No matching command"),
            );
        } else {
            for (index, command) in commands.iter().enumerate() {
                let name = command.name;
                panel = panel.child(
                    menu_row(
                        ("slash", index),
                        format!("/{name}"),
                        Some(command.description.into()),
                        index == selection,
                    )
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.apply_slash_command(name, window, cx)
                        });
                    }),
                );
            }
        }
        panel
    }

    fn render_picker(&self, kind: PickerKind) -> impl IntoElement {
        let mut panel = menu_panel();
        match kind {
            PickerKind::Model => {
                for (index, (instance, model, label)) in MODEL_CATALOG.iter().enumerate() {
                    let active = self.provider == *instance && self.model == *model;
                    panel = panel.child(
                        menu_row(
                            ("model", index),
                            *label,
                            Some(format!("{instance}/{model}").into()),
                            active,
                        )
                        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| {
                                this.provider = (*instance).to_string();
                                this.model = (*model).to_string();
                                this.composer_state.picker = None;
                                cx.notify();
                            });
                        }),
                    );
                }
            }
            PickerKind::InteractionMode => {
                for (index, (value, label)) in INTERACTION_MODES.iter().enumerate() {
                    let active = self.composer_state.interaction_mode == *value;
                    panel = panel.child(
                        menu_row(("interaction", index), *label, None, active)
                            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    this.composer_state.interaction_mode =
                                        (*value).to_string();
                                    this.composer_state.picker = None;
                                    cx.notify();
                                });
                            }),
                    );
                }
            }
            PickerKind::RuntimeMode => {
                for (index, (value, label)) in RUNTIME_MODES.iter().enumerate() {
                    let active = self.composer_state.runtime_mode == *value;
                    panel = panel.child(
                        menu_row(("runtime", index), *label, None, active)
                            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    this.composer_state.runtime_mode = (*value).to_string();
                                    this.composer_state.picker = None;
                                    cx.notify();
                                });
                            }),
                    );
                }
            }
            PickerKind::Attach => {
                panel = panel.child(
                    menu_row(
                        "attach-file",
                        "Attach file…",
                        Some("Type or paste a path".into()),
                        false,
                    )
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.composer_state.picker = None;
                            this.open_attach_field(window, cx);
                            cx.notify();
                        });
                    }),
                );
                panel = panel.child(
                    menu_row(
                        "attach-image",
                        "Paste image",
                        Some("From the clipboard".into()),
                        false,
                    )
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        let Some(view) = app_view(window) else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.composer_state.picker = None;
                            this.stage_clipboard(cx);
                        });
                    }),
                );
            }
        }
        panel
    }

    fn render_attachment_strip(&self) -> impl IntoElement {
        let mut strip = div().flex().flex_wrap().gap_1().pb_2();
        for (index, attachment) in self.composer_state.attachments.iter().enumerate() {
            strip = strip.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl_2()
                    .pr_1()
                    .py_0p5()
                    .rounded_md()
                    .bg(theme::user_bubble())
                    .border_1()
                    .border_color(theme::border())
                    .text_xs()
                    .child(attachment.name.clone())
                    .child(
                        div()
                            .id(("attach-remove", index))
                            .px_1()
                            .rounded_sm()
                            .cursor(gpui::CursorStyle::PointingHand)
                            .text_color(theme::fg_muted())
                            .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    if index < this.composer_state.attachments.len() {
                                        this.composer_state.attachments.remove(index);
                                    }
                                    cx.notify();
                                });
                            })
                            .child("×"),
                    ),
            );
        }
        strip
    }

    pub fn render_composer(&self) -> impl IntoElement {
        let state = &self.composer_state;
        let composer = self.composer.clone();

        let mut root = div()
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .relative()
            .key_context("Composer")
            .on_key_down(|_event, window, cx| {
                ensure_composer_hooks(window, cx);
            })
            .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                ensure_composer_hooks(window, cx);
            });

        // Floating menus above the composer.
        if state.slash_open {
            root = root.child(self.render_slash_menu());
        }
        if let Some(kind) = state.picker {
            root = root.child(self.render_picker(kind));
        }

        let mut card = div()
            .flex()
            .flex_col()
            .rounded_lg()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border())
            .px_3()
            .py_2();

        if !state.attachments.is_empty() {
            card = card.child(self.render_attachment_strip());
        }
        if let Some(field) = &state.attach_field {
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_2()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("Path:")
                    .child(div().flex_1().text_color(theme::fg()).child(field.clone()))
                    .child("↵ to attach · esc to cancel"),
            );
        }

        card = card.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("composer-attach")
                        .px_1()
                        .rounded_md()
                        .cursor(gpui::CursorStyle::PointingHand)
                        .text_sm()
                        .text_color(theme::fg_muted())
                        .hover(|s| s.bg(theme::hover()).text_color(theme::fg()))
                        .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                            ensure_composer_hooks(window, cx);
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| {
                                this.composer_state.picker =
                                    if this.composer_state.picker == Some(PickerKind::Attach) {
                                        None
                                    } else {
                                        Some(PickerKind::Attach)
                                    };
                                cx.notify();
                            });
                        })
                        .child("+"),
                )
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .child(composer.clone()),
                )
                .child(
                    div()
                        .id("send")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(theme::primary())
                        .cursor(gpui::CursorStyle::PointingHand)
                        .text_xs()
                        .text_color(gpui::white())
                        .hover(|s| s.bg(gpui::rgb(0x6d8cff)))
                        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                            ensure_composer_hooks(window, cx);
                            let Some(view) = app_view(window) else {
                                return;
                            };
                            view.update(cx, |this, cx| {
                                this.send_composer(window, cx);
                            });
                            // `PromptSubmitted` clears the field — re-sync the
                            // draft mirror now since no keystroke fired.
                            window.on_next_frame(|window, cx| {
                                let Some(view) = app_view(window) else {
                                    return;
                                };
                                view.update(cx, |this, cx| {
                                    this.sync_composer_draft(window, cx)
                                });
                            });
                        })
                        .child("Send"),
                ),
        );

        root = root.child(card);

        // Footer control row: model pill + mode chips + empty-thread hint.
        let mut footer = div()
            .flex()
            .items_center()
            .gap_2()
            .px_1()
            .child(footer_chip(
                "composer-model",
                format!("{}/{}", self.provider, self.model),
                PickerKind::Model,
                state.picker,
            ))
            .child(footer_chip(
                "composer-interaction",
                interaction_mode_label(&state.interaction_mode),
                PickerKind::InteractionMode,
                state.picker,
            ))
            .child(footer_chip(
                "composer-runtime",
                runtime_mode_label(&state.runtime_mode),
                PickerKind::RuntimeMode,
                state.picker,
            ))
            .child(div().flex_1());

        if self.state.selected.is_none() {
            footer = footer.child(
                div()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child("No thread selected — this will start a new thread"),
            );
        }

        root.child(footer)
    }
}
