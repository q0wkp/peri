//! ratatui-kit StatusBar component.
//!
//! S9：完整双行布局——
//! - **Row 1**：权限模式 → cwd basename → provider/model → bg tasks
//!   全部从 SERVICE_SNAPSHOT atom 派生（S5 落地）；高亮计时器控制闪烁。
//!   CPU%/MEM/ctx 已迁移 composer footer 资源线（input_area.rs）。
//! - **Row 2**：状态相关的快捷键 hints（popup/mention/slash/默认 4 态切换）。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms;
use crate::kit::mouse_router;
use crate::kit::panel_registry::open_panel;
use crate::kit::popup_overlay::open_popup;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Alignment, Constraint, Direction, Flex, Rect},
        style::{Color, Modifier, Style, Stylize},
        text::{Line, Span},
        widgets::{Paragraph, Wrap},
    },
};
use std::time::Duration;

/// 状态栏第 1 行：权限模式 · cwd · provider/model · bg tasks
///（CPU%/MEM/ctx 已迁移 composer footer 资源线，见 input_area.rs）
#[component]
fn StatusBarRow1(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _lang = hooks.use_atom(&atoms::LANG_VERSION);
    let snap = hooks.use_atom(&atoms::SERVICE_SNAPSHOT);
    let model_hl = hooks.use_atom(&atoms::MODEL_HIGHLIGHT_UNTIL);
    let provider_hl = hooks.use_atom(&atoms::PROVIDER_HIGHLIGHT_UNTIL);
    let mode_hl = hooks.use_atom(&atoms::MODE_HIGHLIGHT_UNTIL);
    let bg_tasks = hooks.use_atom(&atoms::BG_TASKS);
    let preparing = hooks.use_atom(&atoms::SESSION_PREPARING);
    let goal_store = hooks.use_atom(&atoms::GOAL_SNAPSHOT);

    let snap = snap.read().clone();
    let goal = goal_store.read().clone();
    let now = peri_time::monotonic_now();
    // provider 不单独显示；provider 或 model 任一变化都让模型段闪烁提醒
    let model_highlighted = model_hl.read().as_ref().is_some_and(|t| *t > now)
        || provider_hl.read().as_ref().is_some_and(|t| *t > now);
    let mode_highlighted = mode_hl.read().as_ref().is_some_and(|t| *t > now);

    let mut spans: Vec<Span<'static>> = Vec::new();

    // 1. 权限模式
    let mode_label = permission_mode_display(&snap.permission_mode);
    if !mode_label.is_empty() {
        let color = permission_mode_color(&snap.permission_mode);
        let mut style = Style::default().fg(color);
        if mode_highlighted {
            style = style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
        }
        spans.push(Span::styled(format!(" {}", mode_label), style));
    }

    // 2. cwd basename
    spans.push(separator());
    spans.push(Span::styled(
        cwd_basename(&snap.cwd),
        Style::default().fg(statusbar().muted),
    ));

    // 3. alias model effort —— 档位名 + 模型名 + 推理力度
    // 由 model_segment_parts 做去重：alias 与模型名相同时只显示一次；
    // 模型名尾部已含 effort 后缀（如 "gpt-5.6-luna high"）时不重复追加。
    // model_start/model_end 记录模型段在 spans 中的索引范围，供鼠标点击区域计算。
    let model_parts = model_segment_parts(&snap.model_alias, &snap.model_name, &snap.effort);
    let mut model_start = None;
    let mut model_end = 0usize;
    if !model_parts.is_empty() {
        model_start = Some(spans.len());
        spans.push(separator());
        let theme = THEME_ATOM.state().read().semantic;
        // 末段是 effort 时拆分为独立 span，但样式与模型名一致（model_info 色，不另用 effort 色）
        let (head, effort_part) =
            if model_parts.len() >= 2 && model_parts.last().is_some_and(|p| p == &snap.effort) {
                let mut h = model_parts.clone();
                let e = h.pop().unwrap();
                (h, Some(e))
            } else {
                (model_parts, None)
            };
        let mut model_style = Style::default().fg(theme.model_info);
        if model_highlighted {
            model_style = model_style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
        }
        spans.push(Span::styled(head.join(" "), model_style));
        if let Some(e) = effort_part {
            spans.push(Span::styled(format!(" {e}"), model_style));
        }
        model_end = spans.len();
    }

    // 4. 当前 goal（简洁状态，点击打开详情）
    let mut goal_start = None;
    let mut goal_end = 0usize;
    if let Some(goal) = &goal {
        goal_start = Some(spans.len());
        spans.push(separator());
        let status = match goal.status {
            Some(peri_acp_types::goal::GoalStatus::Active) => i18n::tr("goal-status-active"),
            Some(peri_acp_types::goal::GoalStatus::Complete) => i18n::tr("goal-status-complete"),
            Some(peri_acp_types::goal::GoalStatus::Blocked) => i18n::tr("goal-status-blocked"),
            None => i18n::tr("ui-empty"),
        };
        spans.push(Span::styled(
            format!("Goal: {status} · ↻{}", goal.continuation_count),
            Style::default().fg(THEME_ATOM.state().read().semantic.accent),
        ));
        goal_end = spans.len();
    }

    // 5. 后台任务计数
    let bg = bg_tasks.read();
    let shell_c = bg.iter().filter(|t| t.kind == "shell").count();
    let agent_c = bg.iter().filter(|t| t.kind == "agent").count();
    let wf_c = bg.iter().filter(|t| t.kind == "workflow").count();
    if shell_c > 0 || agent_c > 0 || wf_c > 0 {
        spans.push(separator());
        let mut parts = vec![];
        if shell_c > 0 {
            parts.push(format!("{} shell", shell_c));
        }
        if agent_c > 0 {
            parts.push(format!("{} agent", agent_c));
        }
        if wf_c > 0 {
            parts.push(format!("{} workflow", wf_c));
        }
        spans.push(Span::styled(
            parts.join(" "),
            Style::default().fg(THEME_ATOM.state().read().semantic.loading),
        ));
    }

    // 6. 会话建立中
    //    这段窗口里输入既不在待发送队列、也没有发出请求，只有这里能说明正在做什么。
    if let Some(label) = preparing_label(*preparing.read()) {
        spans.push(separator());
        spans.push(Span::styled(
            label,
            Style::default().fg(THEME_ATOM.state().read().semantic.loading),
        ));
    }

    // 根据可用宽度动态决定是否需要折行（单行 → 双行）
    let total_width = Line::from(spans.clone()).width() as u16;
    let prev_size = hooks.use_previous_size();
    // 首帧 use_previous_size 返回 width=0，退守单行；后续帧宽度超过才启用双行折行
    let needs_wrap = prev_size.width > 0 && total_width > prev_size.width;
    let row_height: u16 = if needs_wrap { 2 } else { 1 };

    // ── 模型段点击区域：鼠标左键点击 alias/model 文本 → 打开快速切换弹窗 ──
    // AreaTracker 值拷贝模式（仿 input_area.rs）：每帧从 hook 取出 rect 副本，
    // 闭包按值捕获，避免 Arc 重建导致 handler 读到 None。
    let row1_area;
    {
        let tracker = hooks.use_hook(|| AreaTracker { rect: None });
        row1_area = tracker.rect;
    }
    // 模型段点击区域取自 Row1 的真实 Paragraph 渲染结果，折行后每行各有区域
    // `(line_idx, x_start, x_end)`，与屏幕显示逐格一致。
    let model_click: Vec<(u16, u16, u16)> =
        if let (Some(start), Some(area)) = (model_start, row1_area) {
            model_click_areas(&spans, area.width as usize, row_height, start, model_end)
        } else {
            Vec::new()
        };
    let goal_click: Vec<(u16, u16, u16)> =
        if let (Some(start), Some(area)) = (goal_start, row1_area) {
            model_click_areas(&spans, area.width as usize, row_height, start, goal_end)
        } else {
            Vec::new()
        };
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        if let Event::Mouse(mouse) = event {
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return EventResult::Ignored;
            }
            // 弹窗或面板激活时不响应——防止遮挡区域误触（如弹窗覆盖状态栏）
            if mouse_router::is_occluded() {
                return EventResult::Ignored;
            }
            if let Some(area) = row1_area
                && let Some((_, x_start, _)) = model_click.iter().copied().find(|&(li, xs, xe)| {
                    mouse.row == area.y.saturating_add(li)
                        && mouse.column >= area.x.saturating_add(xs)
                        && mouse.column < area.x.saturating_add(xe)
                })
            {
                // 锚点 = 模型段起点屏幕坐标——小弹窗定位在该点上方
                *atoms::MODEL_SWITCH_ANCHOR.state().write() =
                    Some((area.x.saturating_add(x_start), mouse.row));
                open_popup(atoms::PopupKind::ModelQuickSwitch);
                return EventResult::Consumed;
            }
            if let Some(area) = row1_area
                && goal_click.iter().copied().any(|(li, xs, xe)| {
                    mouse.row == area.y.saturating_add(li)
                        && mouse.column >= area.x.saturating_add(xs)
                        && mouse.column < area.x.saturating_add(xe)
                })
            {
                open_panel(PanelKind::Goal);
                return EventResult::Consumed;
            }
        }
        EventResult::Ignored
    });

    element!(
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Length(row_height),
        ) {
            Text(text: Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false }))
        }
    )
}

/// 状态栏第 2 行：状态相关的快捷键 hints + 复制提示
#[component]
fn StatusBarRow2(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _lang = hooks.use_atom(&atoms::LANG_VERSION);
    // I19-C：原代码读 POPUP_ACTIVE（dead atom，open/close_popup 从不同步）
    // 导致 popup hints 永远不显示。改读 POPUP_KIND.is_some()。
    let popup_kind = hooks.use_atom(&atoms::POPUP_KIND);
    let at_active = hooks.use_atom(&atoms::AT_MENTION_ACTIVE);
    let slash_active = hooks.use_atom(&atoms::SLASH_HINT_ACTIVE);
    let copy_until = hooks.use_atom(&atoms::COPY_MESSAGE_UNTIL);
    let copy_count = hooks.use_atom(&atoms::COPY_CHAR_COUNT);
    let quit_pending = hooks.use_atom(&atoms::QUIT_PENDING_SINCE);

    let is_popup = popup_kind.read().is_some();
    let is_at = *at_active.read();
    let is_slash = *slash_active.read();
    let now = peri_time::monotonic_now();

    // 复制提示优先于其他 hints。
    // [TRAP] 只读 atom 判断过期——禁止在 render body 中写 atom（render→write→render 自激）。
    // mark_copy_message 总是用新 Instant 覆盖 atom，旧 Some(until) 残留不影响下次显示。
    let copy_active = copy_until.read().is_some_and(|until| now < until);
    if copy_active {
        let char_count = *copy_count.read();
        let hint = i18n::tr_args(
            "statusbar-copied",
            &[("count".to_string(), FluentValue::from(char_count as u64))],
        );
        return element!(
            View(
                flex_direction: Direction::Horizontal,
                width: Constraint::Fill(1),
                height: Constraint::Length(1),
                justify_content: Flex::Center,
            ) {
                Text(text: Paragraph::new(
                    Line::from(hint).fg(statusbar().text)
                ).centered())
            }
        );
    }

    // Ctrl+C 退出待确认提示——在 hint 行显示，不挤占通知栏。
    let quit_active = quit_pending
        .read()
        .is_some_and(|t| now.duration_since(t) < Duration::from_secs(1));
    if quit_active {
        let hint = i18n::tr("statusbar-hint-quit-pending");
        return element!(
            View(
                flex_direction: Direction::Horizontal,
                width: Constraint::Fill(1),
                height: Constraint::Length(1),
                justify_content: Flex::End,
            ) {
                Text(text: Paragraph::new(
                    Line::from(hint).fg(statusbar().text)
                ).right_aligned())
            }
        );
    }

    let hints = if is_popup {
        Line::from(i18n::tr("statusbar-hint-popup")).fg(statusbar().muted)
    } else if is_at || is_slash {
        Line::from(i18n::tr("statusbar-hint-menu")).fg(statusbar().muted)
    } else {
        Line::from(i18n::tr("statusbar-hint-main")).fg(statusbar().muted)
    };

    element!(
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Length(1),
            justify_content: Flex::End,
        ) {
            Text(text: Paragraph::new(hints), alignment: Alignment::Right)
        }
    )
}

/// 通知行：固定高度 1，位于状态栏第 3 行（原视觉缓冲空行位置）。
/// 平时渲染空行，有通知时显示消息文本。高度恒定 → 通知出现/消失不引起
/// 任何行高变化，无行抖动；不再作为 StatusBar 顶部的动态插入行。
#[component]
fn NotifRow(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 渲染前检查过期——不写 atom，过期自动忽略。
    // 下次事件处理器写 NOTIFICATION 会用新值覆盖旧 Some。
    let notif_store = hooks.use_atom(&atoms::NOTIFICATION);
    let show_notif = notif_store
        .read()
        .as_ref()
        .is_some_and(|n| peri_time::monotonic_now() < n.until);
    let notif_text = if show_notif {
        notif_store
            .read()
            .as_ref()
            .map(|n| n.message.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };

    let statusbar_tokens = statusbar();
    let text = if show_notif && !notif_text.is_empty() {
        Line::from(Span::styled(
            notif_text,
            Style::default()
                .fg(statusbar_tokens.text)
                .add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from("")
    };

    element!(
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Length(1),
        ) {
            Text(text: Paragraph::new(text))
        }
    )
}

#[derive(Default, Props)]
pub struct StatusBarProps {
    /// §11 高度降级：`h < 12` 时隐藏 Row2 key hints（Row1Only，高度 2）。
    pub hide_hints: bool,
    /// §11 高度降级：`h < 8` 时完全隐藏（高度 0，高度让给 transcript/composer）。
    pub hidden: bool,
}

#[component]
pub fn StatusBar(props: &StatusBarProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _lang = hooks.use_atom(&atoms::LANG_VERSION);

    // [§3.1] 内容带由 AppShell 的 `ContentBand` 包装在 draw 期统一区域；Row1 的
    // 折行与点击列都以内容区域为准（`use_previous_size` / AreaTracker 在其后）。

    // §11：h<8 完全隐藏；h<12 仅 Row1 + NotifRow（无 key hints / 缓冲行）。
    // Row2 是独立组件，条件渲染不违反 hook 顺序（计划 Slice 1c 裁决）。
    let height = if props.hidden {
        0
    } else if props.hide_hints {
        2
    } else {
        4
    };

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(height),
        ) {
            StatusBarRow1()
            if !props.hidden && !props.hide_hints {
                element!(StatusBarRow2())
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0)))
            }
            if props.hidden {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0)))
            } else {
                element!(NotifRow())
            }
            if !props.hidden && !props.hide_hints {
                // 第 4 行留空（视觉缓冲，Row1 折行为双行时自动压缩此区域）
                element!(Text(text: Paragraph::new(Line::from(""))))
            } else {
                element!(View(height: Constraint::Length(0), width: Constraint::Length(0)))
            }
        }
    )
}

// ── 辅助函数 ─────────────────────────────────────────────────────────────

/// 用 Ratatui 的真实 Paragraph 渲染求目标 span 点击区域，保留换行、裁剪和宽字符语义。
fn model_click_areas(
    spans: &[Span<'static>],
    area_w: usize,
    row_height: u16,
    model_start: usize,
    model_end: usize,
) -> Vec<(u16, u16, u16)> {
    if area_w == 0 || spans.is_empty() || model_start >= model_end || row_height == 0 {
        return Vec::new();
    }
    let width = area_w.min(usize::from(u16::MAX)) as u16;
    let area = Rect::new(0, 0, width, row_height);
    let marker = Color::Rgb(1, 2, 3);
    let marked = spans
        .iter()
        .enumerate()
        .map(|(index, span)| {
            let style = if (model_start..model_end).contains(&index) {
                span.style.bg(marker)
            } else {
                span.style
            };
            Span::styled(span.content.clone(), style)
        })
        .collect::<Vec<_>>();
    let mut buffer = ratatui_kit::ratatui::buffer::Buffer::empty(area);
    ratatui_kit::ratatui::widgets::Widget::render(
        Paragraph::new(Line::from(marked)).wrap(Wrap { trim: false }),
        area,
        &mut buffer,
    );

    let mut areas = Vec::new();
    for y in 0..row_height {
        let mut x = 0;
        while x < width {
            if buffer[(x, y)].style().bg != Some(marker) {
                x += 1;
                continue;
            }
            let start = x;
            while x < width && buffer[(x, y)].style().bg == Some(marker) {
                x += 1;
            }
            areas.push((y, start, x));
        }
    }
    areas
}

/// 追踪 Row1 组件区域，供鼠标点击→打开 alias 快速切换弹窗使用。
/// 仿 input_area.rs AreaTracker：rect 是值类型（Copy），每帧 pre_component_draw
/// 更新后在 handler 注册前取出副本传给闭包。
struct AreaTracker {
    rect: Option<Rect>,
}

impl Hook for AreaTracker {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.rect = Some(drawer.area);
    }
}

fn statusbar() -> peri_theme::component::StatusBarTokens {
    THEME_ATOM.state().read().component.statusbar
}

fn separator() -> Span<'static> {
    Span::styled(" · ", Style::default().fg(statusbar().muted))
}

/// 把 atom 中的 permission_mode 字符串映射为显示标签。
fn permission_mode_display(mode: &str) -> String {
    match mode {
        "" => i18n::tr("statusbar-initializing"),
        "accept-edit" => i18n::tr("statusbar-permission-accept-edit"),
        "auto-mode" => i18n::tr("statusbar-permission-auto"),
        "bypass" => i18n::tr("statusbar-permission-bypass"),
        _ => i18n::tr("statusbar-permission-dont-ask"),
    }
}

/// 准备阶段的状态栏文案：准备在途时有明确状态，结束后回到常规内容。
///
/// 只做「事实 → 文案」映射；位置与样式由 `StatusBarRow1` 决定。
fn preparing_label(preparing: bool) -> Option<String> {
    preparing.then(|| i18n::tr("statusbar-preparing"))
}

fn permission_mode_color(mode: &str) -> ratatui::style::Color {
    match mode {
        "accept-edit" => statusbar().mode_accept_edit,
        "auto-mode" => statusbar().mode_auto,
        "bypass" => statusbar().mode_bypass,
        _ => statusbar().text,
    }
}

/// 从 cwd 路径取 basename（最后一节）。空或异常返回原串。
fn cwd_basename(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| cwd.to_string())
}

/// 状态栏模型段三段式 `alias model effort` 的组成部分。
///
/// 去重规则：
/// - alias 与模型名相同时（配置回退到 alias）只显示一次；
/// - 模型名尾部已含 effort 后缀（如 `gpt-5.6-luna high`）时不重复追加，
///   避免出现 `high high`。
fn model_segment_parts(alias: &str, model_name: &str, effort: &str) -> Vec<String> {
    let model = if !model_name.is_empty() {
        model_name
    } else {
        alias
    };
    let mut parts = Vec::new();
    if !alias.is_empty() && alias != model {
        parts.push(alias.to_string());
    }
    if !model.is_empty() {
        parts.push(model.to_string());
    }
    if !effort.is_empty()
        && !model.is_empty()
        && !model.to_lowercase().ends_with(&format!(" {effort}"))
    {
        parts.push(effort.to_string());
    }
    parts
}

#[cfg(test)]
#[path = "status_bar_test.rs"]
mod tests;
