//! Message timeline: turn item rendering — tool calls, diffs, approvals,
//! user/assistant styling. Owns `impl AppView` timeline render methods and
//! `timeline::State`.
//!
//! Field names come from `OrchestrationV2TurnItem` in
//! `packages/contracts/src/orchestrationV2.ts` (the complete payload arrives
//! on `TurnItemView::raw`); `item.status` is one of `OrchestrationV2TurnItemStatus`
//! (idle, pending, running, waiting, completed, failed, cancelled, interrupted).

use std::collections::HashSet;

use gpui::{AnyElement, Rgba, div, prelude::*, px, rgb, rgba};
use serde_json::{Value, json};

use crate::model::{ConnectionStatus, Role};
use crate::net::TurnItemView;
use crate::{AppView, theme};

/// Timeline-local UI state. Expand freely — this field lives on `AppView`.
#[derive(Default)]
pub struct State {
    /// Turn item ids whose expandable detail (tool output, diff) is open.
    pub expanded: HashSet<String>,
    /// Approval `requestId`s already answered from this client; keeps the
    /// buttons disabled until the server echoes the resolved status back.
    pub responded: HashSet<String>,
}

fn success() -> Rgba {
    rgb(0x4cc38a)
}

fn danger() -> Rgba {
    rgb(0xff5a5a)
}

fn danger_bg() -> Rgba {
    rgba(0xff5a5a14)
}

fn danger_border() -> Rgba {
    rgba(0xff5a5a55)
}

fn warning() -> Rgba {
    rgb(0xe8b45a)
}

fn warning_border() -> Rgba {
    rgba(0xe8b45a55)
}

/// The item is still producing content (streaming or actively running).
/// `waiting` means it is blocked on the user, not that it is working.
fn is_live(item: &TurnItemView) -> bool {
    item.streaming || matches!(item.status.as_str(), "pending" | "running")
}

fn raw_str<'a>(item: &'a TurnItemView, key: &str) -> Option<&'a str> {
    item.raw
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn status_color(item: &TurnItemView) -> Rgba {
    if item.status == "failed" {
        danger()
    } else if item.status == "waiting" {
        warning()
    } else if is_live(item) {
        theme::primary()
    } else if item.status == "completed" {
        success()
    } else {
        theme::fg_muted()
    }
}

fn status_dot(color: Rgba) -> gpui::Div {
    div()
        .w(px(6.))
        .h(px(6.))
        .rounded_full()
        .bg(color)
        .flex_shrink_0()
}

fn chevron(expanded: bool) -> gpui::Div {
    div()
        .text_xs()
        .text_color(theme::fg_muted())
        .flex_shrink_0()
        .child(if expanded { "▾" } else { "▸" })
}

fn streaming_dot() -> gpui::Div {
    div()
        .text_sm()
        .text_color(theme::primary())
        .flex_shrink_0()
        .child("●")
}

/// `render_timeline` takes `&self`, so click handlers reach the view through
/// the window root instead of `cx.listener`.
fn update_view(
    window: &mut gpui::Window,
    cx: &mut gpui::App,
    f: impl FnOnce(&mut AppView, &mut gpui::Context<AppView>),
) {
    if let Some(Some(view)) = window.root::<AppView>() {
        view.update(cx, f);
    }
}

fn toggle_expanded(
    item_id: String,
) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) {
    move |_event, window, cx| {
        let item_id = item_id.clone();
        update_view(window, cx, move |this, cx| {
            if !this.timeline_state.expanded.remove(&item_id) {
                this.timeline_state.expanded.insert(item_id);
            }
            cx.notify();
        });
    }
}

/// Pretty-print a JSON detail value; strings pass through verbatim.
fn pretty_json(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        other => serde_json::to_string_pretty(other).ok(),
    }
}

/// Some providers wrap command output as `{"stdout","stderr","interrupted"}`;
/// join the streams like the web client does.
fn command_output_text(output: &str) -> String {
    let trimmed = output.trim_start();
    if !trimmed.starts_with("{\"stdout\"") {
        return output.to_string();
    }
    let Ok(parsed) = serde_json::from_str::<Value>(trimmed) else {
        return output.to_string();
    };
    [parsed["stdout"].as_str(), parsed["stderr"].as_str()]
        .into_iter()
        .flatten()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Tool `output` is either a plain string or a provider content-block array
/// (`[{type: "content", content: {type: "text", text: "…"}}]`).
fn tool_output_text(output: &Value) -> Option<String> {
    match output {
        Value::Array(parts) => {
            let texts: Vec<&str> = parts
                .iter()
                .filter_map(|part| {
                    part["content"]["text"]
                        .as_str()
                        .or_else(|| part["text"].as_str())
                })
                .collect();
            if texts.is_empty() {
                pretty_json(output)
            } else {
                Some(texts.join("\n\n"))
            }
        }
        other => pretty_json(other),
    }
}

/// One-line input summary for a tool call without a `title`.
fn input_summary(input: &Value) -> String {
    let flat = match input {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    let flat = flat.replace('\n', " ");
    flat.chars().take(80).collect()
}

/// `+`/`-` counts from a unified diff when `additions`/`deletions` are absent.
fn diff_stats(diff: &str) -> (usize, usize) {
    let mut additions = 0;
    let mut deletions = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        } else if line.starts_with('+') {
            additions += 1;
        } else if line.starts_with('-') {
            deletions += 1;
        }
    }
    (additions, deletions)
}

fn diff_line_color(line: &str) -> Rgba {
    if line.starts_with("+++") || line.starts_with("---") {
        theme::fg_muted()
    } else if line.starts_with('+') {
        success()
    } else if line.starts_with('-') {
        danger()
    } else if line.starts_with("@@") {
        theme::primary()
    } else {
        theme::fg_muted()
    }
}

/// Scrollable detail area capped at roughly 16 lines; callers set the font
/// (mono for command/tool output and diffs, regular for prose). The explicit
/// x-scroller matters: nowrap diff lines would otherwise propagate their
/// intrinsic width through the visible-x ancestor chain and widen the whole
/// timeline column, pushing every header's trailing chip/chevron offscreen.
fn detail_scroll(namespace: &'static str, index: usize) -> gpui::Stateful<gpui::Div> {
    div()
        .id((namespace, index))
        .w_full()
        .overflow_y_scroll()
        .overflow_x_scroll()
        .max_h(px(288.))
        .bg(theme::canvas())
        .border_t_1()
        .border_color(theme::border())
        .px_3()
        .py_2()
        .text_xs()
        .line_height(px(16.))
}

fn request_kind_label(kind: &str) -> &'static str {
    match kind {
        "command" => "Command approval",
        "file-change" => "File change approval",
        "file-read" => "File read approval",
        "permission" => "Permission approval",
        "mcp-elicitation" => "App access approval",
        _ => "Approval request",
    }
}

impl AppView {
    pub fn render_timeline(&self) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.render_timeline_inner())
    }

    fn render_timeline_inner(&self) -> impl IntoElement {
        let mut list = div()
            .id("messages")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .p_4()
            .gap_3();

        if self.state.connection != ConnectionStatus::Mock {
            if self.thread_items.is_empty() {
                return list.child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme::fg_muted())
                        .text_sm()
                        .child("No messages yet — say something to the agent."),
                );
            }
            for (i, item) in self.thread_items.iter().enumerate() {
                list = list.child(
                    div()
                        .flex()
                        .flex_col()
                        .max_w_full()
                        .child(self.render_turn_item(i, item)),
                );
            }
            return list;
        }

        if self.state.messages.is_empty() {
            return list.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme::fg_muted())
                    .text_sm()
                    .child("No messages yet — say something to the agent."),
            );
        }

        for (i, message) in self.state.messages.iter().enumerate() {
            let bubble = div()
                .px_3()
                .py_2()
                .rounded_lg()
                .text_sm()
                .max_w(px(560.))
                .child(message.text.clone());
            let row = match message.role {
                Role::User => div().flex().justify_end().child(
                    bubble.bg(theme::user_bubble()).text_color(theme::fg()),
                ),
                Role::Agent => div().flex().justify_start().child(
                    bubble
                        .bg(theme::raised())
                        .border_1()
                        .border_color(theme::border())
                        .text_color(theme::fg()),
                ),
            };
            list = list.child(div().id(("msg", i)).child(row));
        }
        list
    }

    fn render_turn_item(&self, index: usize, item: &TurnItemView) -> AnyElement {
        match item.kind.as_str() {
            "user_message" => self.user_message_row(item),
            "assistant_message" => self.assistant_message_row(item),
            "reasoning" => self.reasoning_row(index, item),
            "command_execution" => self.command_row(index, item),
            "dynamic_tool" => self.tool_row(index, item),
            "file_change" => self.file_change_row(index, item),
            "approval_request" => self.approval_row(index, item),
            "todo_list" => self.todo_list_row(item),
            "system_notice" | "notification" => self.notice_row(item),
            "error" => self.error_row(item),
            "proposed_plan" => self.proposed_plan_row(item),
            "subagent" => self.subagent_row(index, item),
            "web_search" | "file_search" => self.search_row(index, item),
            _ => self.fallback_row(item),
        }
    }

    fn user_message_row(&self, item: &TurnItemView) -> AnyElement {
        let text = raw_str(item, "text")
            .map(str::to_owned)
            .or_else(|| item.body.clone())
            .unwrap_or_default();
        let mut bubble = div()
            .px_3()
            .py_2()
            .rounded_lg()
            .text_sm()
            .max_w(px(560.))
            .bg(theme::user_bubble())
            .text_color(theme::fg())
            .child(text);
        let attachment_count = item.raw["attachments"].as_array().map_or(0, Vec::len);
        if attachment_count > 0 {
            bubble = bubble.child(
                div()
                    .pt_1()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(format!(
                        "{attachment_count} attachment{}",
                        if attachment_count == 1 { "" } else { "s" }
                    )),
            );
        }
        div()
            .flex()
            .justify_end()
            .child(bubble)
            .into_any_element()
    }

    fn assistant_message_row(&self, item: &TurnItemView) -> AnyElement {
        let text = raw_str(item, "text")
            .or(item.body.as_deref())
            .unwrap_or_default();
        let mut col = div()
            .flex()
            .flex_col()
            .gap_1()
            .max_w(px(760.))
            .when(item.streaming, |d| d.opacity(0.85))
            .children(crate::markdown::render_markdown(text));
        if item.streaming {
            col = col.child(streaming_dot());
        }
        col.into_any_element()
    }

    fn reasoning_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        // Reasoning stays collapsed once settled; while it streams it is shown.
        let expanded = item.streaming || self.timeline_state.expanded.contains(&item.id);
        let text = raw_str(item, "text")
            .or(item.body.as_deref())
            .unwrap_or_default()
            .to_string();
        let mut col = div().flex().flex_col().child(
            div()
                .id(("head", index))
                .flex()
                .items_center()
                .gap_1p5()
                .py_1()
                .cursor_pointer()
                .opacity(0.8)
                .hover(|s| s.opacity(1.))
                .on_click(toggle_expanded(item.id.clone()))
                .child(chevron(expanded))
                .child(
                    div()
                        .text_xs()
                        .italic()
                        .text_color(theme::fg_muted())
                        .child("Thinking"),
                )
                .when(item.streaming, |d| {
                    d.child(div().text_xs().text_color(theme::primary()).child("…"))
                }),
        );
        if expanded && !text.is_empty() {
            col = col.child(
                div()
                    .pl_4()
                    .pb_1()
                    .text_sm()
                    .italic()
                    .text_color(theme::fg_muted())
                    .child(text),
            );
        }
        col.into_any_element()
    }

    fn command_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let input = raw_str(item, "input").unwrap_or("command");
        let output = raw_str(item, "output")
            .map(command_output_text)
            .filter(|o| !o.trim().is_empty());
        let exit_code = item.raw["exitCode"].as_i64();
        let failed = item.status == "failed"
            || exit_code.is_some_and(|code| code != 0)
            || item.raw["outputIndicatesFailure"].as_bool() == Some(true);
        let expanded = self.timeline_state.expanded.contains(&item.id);
        let title = raw_str(item, "title");

        let mut header = div()
            .id(("head", index))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .on_click(toggle_expanded(item.id.clone()))
            .child(status_dot(status_color(item)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family("monospace")
                    .text_xs()
                    .truncate()
                    .text_color(theme::fg())
                    .child(format!("$ {input}")),
            );
        // Provider labels like "exec" / "Ran command" trail the real command.
        if let Some(title) = title {
            header = header.child(
                div()
                    .max_w(px(200.))
                    .text_xs()
                    .truncate()
                    .text_color(theme::fg_muted())
                    .flex_shrink_0()
                    .child(title.to_string()),
            );
        }
        if is_live(item) && exit_code.is_none() {
            header = header.child(
                div()
                    .text_xs()
                    .text_color(theme::primary())
                    .flex_shrink_0()
                    .child("…"),
            );
        }
        if let Some(code) = exit_code {
            header = header.child(
                div()
                    .px_1p5()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .flex_shrink_0()
                    .when(code == 0, |d| d.text_color(success()))
                    .when(code != 0, |d| d.bg(danger_bg()).text_color(danger()))
                    .child(format!("exit {code}")),
            );
        }
        header = header.child(chevron(expanded));

        let mut card = div()
            .rounded_lg()
            .border_1()
            .border_color(if failed {
                danger_border()
            } else {
                theme::border()
            })
            .bg(theme::raised())
            .overflow_hidden()
            .child(header);

        if expanded {
            let mut detail = detail_scroll("cmd-out", index).font_family("monospace");
            if input != "command" {
                detail = detail.child(
                    div()
                        .pb_1()
                        .text_color(theme::fg_muted())
                        .child(format!("$ {input}")),
                );
            }
            detail = match output {
                Some(output) => detail.child(div().text_color(theme::fg()).child(output)),
                None if item.raw["outputOmitted"].as_bool() == Some(true) => detail.child(
                    div().italic().text_color(theme::fg_muted()).child(
                        "Output withheld on the wire — fetch it with orchestration.getTurnItem.",
                    ),
                ),
                None => detail.child(
                    div()
                        .italic()
                        .text_color(theme::fg_muted())
                        .child("No output."),
                ),
            };
            card = card.child(detail);
        }
        card.into_any_element()
    }

    fn tool_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let name = raw_str(item, "toolName").unwrap_or("tool");
        let summary = raw_str(item, "title")
            .map(str::to_owned)
            .unwrap_or_else(|| input_summary(&item.raw["input"]));
        let expanded = self.timeline_state.expanded.contains(&item.id);

        let header = div()
            .id(("head", index))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .on_click(toggle_expanded(item.id.clone()))
            .child(status_dot(status_color(item)))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::fg())
                    .flex_shrink_0()
                    .child(name.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .truncate()
                    .text_color(theme::fg_muted())
                    .child(summary),
            )
            .when(is_live(item), |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme::primary())
                        .flex_shrink_0()
                        .child("…"),
                )
            })
            .child(chevron(expanded));

        let mut card = div()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .overflow_hidden()
            .child(header);

        if expanded {
            let mut detail = detail_scroll("tool-out", index).font_family("monospace");
            let mut shown = false;
            if let Some(output) = tool_output_text(&item.raw["output"]) {
                shown = true;
                detail = detail.child(div().text_color(theme::fg()).child(output));
            } else if item.raw["outputOmitted"].as_bool() == Some(true) {
                shown = true;
                detail = detail.child(
                    div().italic().text_color(theme::fg_muted()).child(
                        "Output withheld on the wire — fetch it with orchestration.getTurnItem.",
                    ),
                );
            }
            if let Some(input) = pretty_json(&item.raw["input"]) {
                shown = true;
                detail = detail.child(
                    div()
                        .pt_2()
                        .text_color(theme::fg_muted())
                        .child(format!("input\n{input}")),
                );
            }
            if !shown {
                detail = detail.child(
                    div()
                        .italic()
                        .text_color(theme::fg_muted())
                        .child("No detail."),
                );
            }
            card = card.child(detail);
        }
        card.into_any_element()
    }

    fn file_change_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let file_name = raw_str(item, "fileName").unwrap_or("file");
        let diff = raw_str(item, "diffStr").unwrap_or_default();
        let (additions, deletions) = match (
            item.raw["additions"].as_u64(),
            item.raw["deletions"].as_u64(),
        ) {
            (Some(adds), Some(dels)) => (adds as usize, dels as usize),
            (Some(adds), None) => (adds as usize, 0),
            (None, Some(dels)) => (0, dels as usize),
            (None, None) => diff_stats(diff),
        };
        let expanded = self.timeline_state.expanded.contains(&item.id);

        let mut stats = div().flex().items_center().gap_1().flex_shrink_0();
        if additions > 0 {
            stats = stats.child(
                div()
                    .text_xs()
                    .text_color(success())
                    .child(format!("+{additions}")),
            );
        }
        if deletions > 0 {
            stats = stats.child(
                div()
                    .text_xs()
                    .text_color(danger())
                    .child(format!("-{deletions}")),
            );
        }

        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .cursor_pointer()
            .child(status_dot(status_color(item)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family("monospace")
                    .text_xs()
                    .whitespace_nowrap()
                    .overflow_x_hidden()
                    .text_ellipsis_middle()
                    .text_color(theme::fg())
                    .child(file_name.to_string()),
            )
            .child(stats)
            .child(chevron(expanded));

        let mut card = div()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .child(header);

        if expanded {
            let mut detail = detail_scroll("diff", index).font_family("monospace");
            if diff.is_empty() {
                detail = detail.child(
                    div()
                        .italic()
                        .text_color(theme::fg_muted())
                        .child("No diff recorded."),
                );
            } else {
                for line in diff.lines() {
                    detail = detail.child(
                        div()
                            .whitespace_nowrap()
                            .text_color(diff_line_color(line))
                            .child(line.to_string()),
                    );
                }
            }
            card = card.child(detail);
        }
        card.into_any_element()
    }

    fn approval_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let request_id = raw_str(item, "requestId").unwrap_or_default().to_string();
        let kind = raw_str(item, "requestKind").unwrap_or("request");
        let prompt = raw_str(item, "prompt").map(str::to_owned);
        let reason = raw_str(item, "reason").map(str::to_owned);
        let app_name = raw_str(item, "appName").map(str::to_owned);
        let responded = self.timeline_state.responded.contains(&request_id);
        let pending = matches!(item.status.as_str(), "waiting" | "pending" | "running")
            && !responded;

        let options: Vec<(String, String, bool)> = item.raw["options"]
            .as_array()
            .map(|options| {
                options
                    .iter()
                    .filter_map(|option| {
                        let decision = option["decision"].as_str()?.to_string();
                        let label = option["label"]
                            .as_str()
                            .unwrap_or(&decision)
                            .to_string();
                        Some((decision, label, option["warning"].is_string()))
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|options| !options.is_empty())
            .unwrap_or_else(|| {
                vec![
                    ("accept".to_string(), "Accept".to_string(), false),
                    ("decline".to_string(), "Decline".to_string(), false),
                ]
            });

        let mut card = div()
            .rounded_lg()
            .border_1()
            .border_color(if pending {
                warning_border()
            } else {
                theme::border()
            })
            .bg(theme::raised())
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(status_dot(status_color(item)))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::fg())
                            .child(request_kind_label(kind)),
                    )
                    .when(pending, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(warning())
                                .child("waiting for approval"),
                        )
                    }),
            );

        if let Some(app_name) = app_name {
            card = card.child(
                div()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(app_name),
            );
        }
        if let Some(prompt) = prompt {
            card = card.child(
                div()
                    .font_family("monospace")
                    .text_xs()
                    .line_clamp(8)
                    .text_color(theme::fg())
                    .child(prompt),
            );
        }
        if let Some(reason) = reason {
            card = card.child(
                div()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(reason),
            );
        }

        if pending {
            let mut buttons = div().flex().flex_wrap().items_center().gap_2();
            for (option_index, (decision, label, has_warning)) in options.iter().enumerate() {
                let declining = matches!(decision.as_str(), "decline" | "cancel");
                let payload = json!({
                    "type": "runtime-request.respond",
                    "commandId": uuid::Uuid::new_v4().to_string(),
                    "threadId": item.raw["threadId"],
                    "requestId": request_id.clone(),
                    "decision": decision,
                });
                let request_id = request_id.clone();
                buttons = buttons.child(
                    div()
                        .id(("approve", index * 64 + option_index))
                        .px_2p5()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .when(declining, |d| {
                            d.border_1()
                                .border_color(theme::border())
                                .text_color(theme::fg())
                                .hover(|s| s.bg(theme::hover()))
                        })
                        .when(!declining && !*has_warning, |d| {
                            d.bg(theme::primary())
                                .text_color(gpui::white())
                                .hover(|s| s.opacity(0.9))
                        })
                        .when(*has_warning, |d| {
                            d.bg(rgb(0x7a2e2e))
                                .text_color(gpui::white())
                                .hover(|s| s.bg(rgb(0x9a3a3a)))
                        })
                        .on_click(move |_event, window, cx| {
                            let payload = payload.clone();
                            let request_id = request_id.clone();
                            update_view(window, cx, move |this, cx| {
                                this.timeline_state.responded.insert(request_id);
                                this.dispatch(payload);
                                cx.notify();
                            });
                        })
                        .child(label.clone()),
                );
            }
            card = card.child(buttons);
        } else {
            // Resolved: prefer the option label matching a decision echoed on
            // the item, else map the settled status to an outcome word.
            let outcome = match raw_str(item, "decision") {
                Some(decision) => options
                    .iter()
                    .find(|(d, _, _)| d.as_str() == decision)
                    .map(|(_, label, _)| label.clone())
                    .unwrap_or_else(|| decision.to_string()),
                None => match item.status.as_str() {
                    "completed" => "Approved".to_string(),
                    "cancelled" | "interrupted" => "Declined".to_string(),
                    "failed" => "Failed".to_string(),
                    _ if responded => "Response sent — waiting for the provider…".to_string(),
                    _ => "Pending".to_string(),
                },
            };
            let outcome_color = match item.status.as_str() {
                "completed" => success(),
                "cancelled" | "interrupted" | "failed" => theme::fg_muted(),
                _ => warning(),
            };
            card = card.child(div().text_xs().text_color(outcome_color).child(outcome));
        }
        card.into_any_element()
    }

    fn todo_list_row(&self, item: &TurnItemView) -> AnyElement {
        let mut card = div()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1();
        if let Some(explanation) = raw_str(item, "explanation") {
            card = card.child(
                div()
                    .pb_1()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(explanation.to_string()),
            );
        }
        let steps: &[Value] = item.raw["steps"].as_array().map_or(&[], Vec::as_slice);
        if steps.is_empty() {
            card = card.child(
                div()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .child(item.body.clone().unwrap_or_else(|| "Checklist".to_string())),
            );
        }
        for step in steps {
            // Contract: pending | running | completed; accept provider
            // spellings like in_progress/done too.
            let status = step["status"].as_str().unwrap_or("pending");
            let (glyph, color, done) = match status {
                "completed" | "done" => ("✓", success(), true),
                "running" | "in_progress" | "in-progress" => ("◐", theme::primary(), false),
                _ => ("○", theme::fg_muted(), false),
            };
            let text = step["text"]
                .as_str()
                .or_else(|| step["content"].as_str())
                .or_else(|| step["title"].as_str())
                .unwrap_or("step");
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().text_color(color).child(glyph))
                    .child(
                        div()
                            .text_sm()
                            .when(done, |d| d.text_color(theme::fg_muted()))
                            .when(!done, |d| d.text_color(theme::fg()))
                            .child(text.to_string()),
                    ),
            );
        }
        card.into_any_element()
    }

    fn notice_row(&self, item: &TurnItemView) -> AnyElement {
        let failed = item.status == "failed" || item.raw["outcome"].as_str() == Some("failed");
        let text = raw_str(item, "summary")
            .or_else(|| raw_str(item, "message"))
            .or_else(|| raw_str(item, "text"))
            .map(str::to_owned)
            .or_else(|| item.body.clone())
            .unwrap_or_else(|| item.kind.clone());
        div()
            .flex()
            .justify_center()
            .child(
                div()
                    .px_3()
                    .py_1()
                    .rounded_full()
                    .bg(theme::raised())
                    .border_1()
                    .border_color(theme::border())
                    .text_xs()
                    .when(failed, |d| d.text_color(danger()))
                    .when(!failed, |d| d.text_color(theme::fg_muted()))
                    .child(text),
            )
            .into_any_element()
    }

    fn error_row(&self, item: &TurnItemView) -> AnyElement {
        let failure = &item.raw["failure"];
        let title = raw_str(item, "title")
            .map(str::to_owned)
            .or_else(|| failure["class"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "Error".to_string());
        let message = failure["message"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| item.body.clone())
            .unwrap_or_else(|| title.clone());
        let code = failure["code"].as_str().map(str::to_owned);
        div()
            .rounded_lg()
            .border_1()
            .border_color(danger_border())
            .bg(danger_bg())
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(danger())
                            .child(title),
                    )
                    .when_some(code, |d, code| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme::fg_muted())
                                .child(code),
                        )
                    }),
            )
            .child(div().text_xs().text_color(theme::fg()).child(message))
            .into_any_element()
    }

    fn proposed_plan_row(&self, item: &TurnItemView) -> AnyElement {
        let markdown = raw_str(item, "markdown")
            .or(item.body.as_deref())
            .unwrap_or_default();
        let actionable = self
            .state
            .selected_thread()
            .is_some_and(|thread| thread.has_actionable_proposed_plan);
        let mut card = div()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(status_dot(status_color(item)))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::fg())
                            .child("Proposed plan"),
                    )
                    .when(item.streaming, |d| d.child(streaming_dot())),
            )
            .children(crate::markdown::render_markdown(markdown));
        if actionable {
            card = card.child(
                div()
                    .pt_1()
                    .mt_1()
                    .border_t_1()
                    .border_color(theme::border())
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(warning())
                            .child("Plan awaiting your decision"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::fg_muted())
                            .child("— accept or request edits in the composer."),
                    ),
            );
        }
        card.into_any_element()
    }

    fn subagent_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let title = raw_str(item, "title").unwrap_or("Subagent");
        let expanded = self.timeline_state.expanded.contains(&item.id);
        let status_label = match item.status.as_str() {
            "pending" => "queued",
            "running" => "running",
            "waiting" => "waiting",
            "completed" => "done",
            "failed" => "failed",
            "cancelled" | "interrupted" => "stopped",
            _ => "",
        };

        let header = div()
            .id(("head", index))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .on_click(toggle_expanded(item.id.clone()))
            .child(status_dot(status_color(item)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .truncate()
                    .text_color(theme::fg())
                    .child(format!("subagent · {title}")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .flex_shrink_0()
                    .child(status_label.to_string()),
            )
            .child(chevron(expanded));

        let mut card = div()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .overflow_hidden()
            .child(header);

        if expanded {
            let body = raw_str(item, "result")
                .or_else(|| raw_str(item, "progress"))
                .or_else(|| raw_str(item, "prompt"))
                .unwrap_or("No detail.")
                .to_string();
            card = card.child(
                detail_scroll("subagent", index)
                    .child(div().text_color(theme::fg()).child(body)),
            );
        }
        card.into_any_element()
    }

    fn search_row(&self, index: usize, item: &TurnItemView) -> AnyElement {
        let expanded = self.timeline_state.expanded.contains(&item.id);
        let (label, query) = if item.kind == "web_search" {
            let patterns = item.raw["patterns"]
                .as_array()
                .map(|patterns| {
                    patterns
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            ("Web search", patterns)
        } else {
            (
                "File search",
                raw_str(item, "pattern").map(str::to_owned).unwrap_or_default(),
            )
        };

        let header = div()
            .id(("head", index))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .on_click(toggle_expanded(item.id.clone()))
            .child(status_dot(status_color(item)))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::fg())
                    .flex_shrink_0()
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .truncate()
                    .text_color(theme::fg_muted())
                    .child(query),
            )
            .child(chevron(expanded));

        let mut card = div()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .overflow_hidden()
            .child(header);

        if expanded {
            let mut detail = detail_scroll("search", index);
            let results: &[Value] = item.raw["results"].as_array().map_or(&[], Vec::as_slice);
            if results.is_empty() {
                detail = detail.child(
                    div()
                        .italic()
                        .text_color(theme::fg_muted())
                        .child("No results."),
                );
            }
            for result in results.iter().take(20) {
                let name = result["url"]
                    .as_str()
                    .or_else(|| result["fileName"].as_str())
                    .unwrap_or("result");
                let snippet = result["snippet"]
                    .as_str()
                    .or_else(|| result["preview"].as_str())
                    .map(|s| s.lines().next().unwrap_or_default().to_string());
                let mut row = div()
                    .pb_1()
                    .flex()
                    .flex_col()
                    .child(div().text_color(theme::fg()).child(name.to_string()));
                if let Some(snippet) = snippet {
                    row = row.child(
                        div()
                            .line_clamp(2)
                            .text_color(theme::fg_muted())
                            .child(snippet),
                    );
                }
                detail = detail.child(row);
            }
            if results.len() > 20 {
                detail = detail.child(
                    div()
                        .text_color(theme::fg_muted())
                        .child(format!("…{} more", results.len() - 20)),
                );
            }
            card = card.child(detail);
        }
        card.into_any_element()
    }

    /// Compact muted one-liner for lifecycle rows and unknown kinds — matches
    /// the web's system-divider treatment.
    fn fallback_row(&self, item: &TurnItemView) -> AnyElement {
        let text = match item.kind.as_str() {
            "checkpoint" => {
                let files = item.raw["files"].as_array().map_or(0, Vec::len);
                if files > 0 {
                    format!("Checkpoint · {files} files")
                } else {
                    "Checkpoint".to_string()
                }
            }
            "run_interrupt_request" => raw_str(item, "message")
                .map(|m| format!("Interrupting run — {m}"))
                .unwrap_or_else(|| "Interrupting run…".to_string()),
            "run_interrupt_result" => raw_str(item, "message")
                .map(|m| format!("Run interrupted — {m}"))
                .unwrap_or_else(|| "Run interrupted".to_string()),
            "compaction" => raw_str(item, "summary")
                .map(str::to_owned)
                .unwrap_or_else(|| "Context compacted".to_string()),
            "user_input_request" => item.raw["questions"]
                .as_array()
                .map(|questions| {
                    let text = questions
                        .iter()
                        .filter_map(|q| {
                            q["question"].as_str().or_else(|| q["header"].as_str())
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!("Agent asked: {text}")
                })
                .unwrap_or_else(|| "Agent requested input".to_string()),
            "handoff" => raw_str(item, "summary")
                .map(str::to_owned)
                .unwrap_or_else(|| "Handoff".to_string()),
            "fork" => "Forked conversation".to_string(),
            "thread_created" => raw_str(item, "title")
                .map(|t| format!("Created thread · {t}"))
                .unwrap_or_else(|| "Created thread".to_string()),
            "link_request" => raw_str(item, "reason")
                .map(|r| format!("Link request — {r}"))
                .unwrap_or_else(|| "Link request".to_string()),
            "secret_request" => raw_str(item, "label")
                .map(|l| format!("Secret request — {l}"))
                .unwrap_or_else(|| "Secret request".to_string()),
            _ => item
                .body
                .clone()
                .or_else(|| item.title.clone())
                .map(|text| format!("{}  {text}", item.kind))
                .unwrap_or_else(|| item.kind.clone()),
        };
        let mut row = div()
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .py_1()
            .when(item.streaming, |d| d.opacity(0.75))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::fg_muted())
                    .truncate()
                    .max_w(px(720.))
                    .child(text),
            );
        if is_live(item) {
            row = row.child(div().text_xs().text_color(theme::primary()).child("…"));
        }
        row.into_any_element()
    }
}
