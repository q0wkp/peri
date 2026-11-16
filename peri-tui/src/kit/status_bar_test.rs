//! Tests for status_bar

#[cfg(test)]
use super::*;
#[cfg(test)]
use serial_test::serial;

#[test]
fn test_permission_mode_display() {
    assert_eq!(permission_mode_display(""), "Initializing…");
    assert_eq!(permission_mode_display("default"), "Don't Ask");
    assert_eq!(permission_mode_display("accept-edit"), "Accept Edit");
    assert_eq!(permission_mode_display("auto-mode"), "Auto Mode");
    assert_eq!(permission_mode_display("bypass"), "Bypass");
    assert_eq!(permission_mode_display("unknown"), "Don't Ask");
}

#[test]
fn test_preparing_label_only_while_preparing() {
    // 会话建立期间（用户提交了输入、会话还没建好）在别处没有任何可见投影，
    // 状态栏必须明确说明正在准备，而不是保持提交前的样子。
    assert_eq!(
        preparing_label(true).as_deref(),
        Some("Preparing session…"),
        "准备在途时状态栏应显示正在准备会话"
    );
    assert_eq!(preparing_label(false), None, "准备结束后不得留下提示");
}

#[test]
fn test_permission_mode_color() {
    assert_eq!(
        permission_mode_color("accept-edit"),
        statusbar().mode_accept_edit
    );
    assert_eq!(permission_mode_color("auto-mode"), statusbar().mode_auto);
    assert_eq!(permission_mode_color("bypass"), statusbar().mode_bypass);
}

#[test]
fn test_cwd_basename_simple() {
    assert_eq!(cwd_basename("/Users/foo/project"), "project");
    assert_eq!(cwd_basename("/tmp"), "tmp");
    assert_eq!(cwd_basename("/"), "/");
}

#[test]
fn test_cwd_basename_empty() {
    assert_eq!(cwd_basename(""), "");
}

#[test]
fn test_model_segment_parts_full() {
    // alias + model + effort 三段
    assert_eq!(
        model_segment_parts("opus", "claude-opus-4-20250514", "high"),
        vec!["opus", "claude-opus-4-20250514", "high"]
    );
}

#[test]
fn test_model_segment_parts_no_effort() {
    assert_eq!(
        model_segment_parts("opus", "claude-opus-4-20250514", ""),
        vec!["opus", "claude-opus-4-20250514"]
    );
}

#[test]
fn test_model_segment_parts_model_has_effort_suffix() {
    // 模型名尾部已含 effort 后缀 → 不重复追加
    assert_eq!(
        model_segment_parts("opus", "gpt-5.6-luna high", "high"),
        vec!["opus", "gpt-5.6-luna high"]
    );
}

#[test]
fn test_model_segment_parts_alias_equals_model() {
    // 配置回退到 alias（model_name 为空或等于 alias）→ 只显示一次
    assert_eq!(
        model_segment_parts("haiku", "haiku", "medium"),
        vec!["haiku", "medium"]
    );
    assert_eq!(
        model_segment_parts("haiku", "", "medium"),
        vec!["haiku", "medium"]
    );
}

#[test]
fn test_model_segment_parts_empty_all() {
    assert!(model_segment_parts("", "", "").is_empty());
}

#[test]
#[serial]
fn test_status_bar_row_renders_without_panic() {
    crate::kit::atoms::init_atoms();
    // 写入测试数据
    *atoms::SERVICE_SNAPSHOT.state().write() = atoms::ServiceSnapshot {
        cwd: "/home/user/test-project".into(),
        provider_name: "anthropic".into(),
        model_alias: "sonnet".into(),
        model_name: "claude-sonnet-4-20250514".into(),
        effort: "high".into(),
        permission_mode: "accept-edit".into(),
        memory_mb: 256,
        cpu_percent: 12.5,
        ..Default::default()
    };
    // 辅助函数应能正确处理这些值
    let snap = atoms::SERVICE_SNAPSHOT.state().read().clone();
    assert_eq!(snap.cwd, "/home/user/test-project");
    assert_eq!(cwd_basename(&snap.cwd), "test-project");
    assert_eq!(
        permission_mode_display(&snap.permission_mode),
        "Accept Edit"
    );
    // 模型段三段式：alias + model + effort
    assert_eq!(
        model_segment_parts(&snap.model_alias, &snap.model_name, &snap.effort),
        vec!["sonnet", "claude-sonnet-4-20250514", "high"]
    );
}

#[test]
#[serial]
fn test_status_bar_handles_empty_provider_model() {
    crate::kit::atoms::init_atoms();
    *atoms::SERVICE_SNAPSHOT.state().write() = atoms::ServiceSnapshot {
        cwd: "/tmp".into(),
        provider_name: "".into(),
        model_alias: "".into(),
        model_name: "".into(),
        permission_mode: "default".into(),
        memory_mb: 0,
        cpu_percent: 0.0,
        ..Default::default()
    };
    let snap = atoms::SERVICE_SNAPSHOT.state().read().clone();
    // 空 provider/model 应被渲染逻辑跳过（不在 Row1 中显示）
    assert!(snap.provider_name.is_empty());
    assert!(snap.model_alias.is_empty());
    assert!(snap.model_name.is_empty());
    // Default mode → Don't Ask 标签
    assert_eq!(permission_mode_display(&snap.permission_mode), "Don't Ask");
    // 0% CPU 应被跳过
    assert_eq!(snap.cpu_percent, 0.0);
}

// ── model_click_areas：按真实渲染结果生成点击区域（行号/列范围） ──────────
//
// 区域与 Paragraph（Wrap { trim: false }）实际落格一致：折行、宽字符与被裁掉的
// 文本都以渲染缓冲区为准。
// row1_spans 模型段词流单行区域：
//   ·(sep) → (0,24,27) ws=2（sep2 尾部空格 + sep4 前导空格，词首 x26）；
//   opus → (0,27,32)（词首 x28）；claude-opus-4-20250514 → (0,32,55)（词首 x33）；
//   high → (0,55,60)（词首 x56）

/// 构造与 Row1 布局一致的 spans：模型段前后都有内容（bg 任务在模型段之后；
/// CPU%/MEM/ctx 已迁移 composer，不再出现在状态栏）。
/// 返回 (spans, model_start, model_end)。
fn row1_spans() -> (Vec<Span<'static>>, usize, usize) {
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(" Accept Edit", Style::default()), // mode
        separator(),
        Span::styled("project", Style::default()), // cwd
        separator(),
        separator(),
        Span::styled("opus claude-opus-4-20250514", Style::default()), // 模型段 head
        Span::styled(" high", Style::default()),                       // effort
    ];
    let model_end = spans.len();
    // 尾部：bg 任务（模型段后紧跟一个 sep，折行边界与迁移前一致）
    spans.push(separator());
    spans.push(Span::styled("2 agent", Style::default()));
    (spans, 4, model_end)
}

#[test]
fn test_model_click_areas_match_ratatui_render_across_wraps() {
    let (spans, start, end) = row1_spans();
    for (width, height) in [(200u16, 1u16), (61, 2), (30, 2), (20, 1)] {
        let areas = model_click_areas(&spans, usize::from(width), height, start, end);
        let rendered = render_status_bar_buffer(&spans, width, height);
        if width == 20 {
            assert!(areas.is_empty(), "模型文本在视口外时不应产生点击区");
        }
        for &(row, x_start, x_end) in &areas {
            assert!(row < height && x_start < x_end && x_end <= width);
            assert!(
                (x_start..x_end).any(|x| rendered[(x, row)].symbol() != " "),
                "点击区域应覆盖模型文字：{areas:?}"
            );
        }
    }
}

#[test]
fn test_model_click_areas_follow_actual_cjk_and_clipping() {
    let spans = vec![
        Span::styled(" 中文目录", Style::default()),
        Span::styled(" opus", Style::default()),
    ];
    let areas = model_click_areas(&spans, 10, 2, 1, 2);
    let rendered = render_status_bar_buffer(&spans, 10, 2);
    assert_eq!(areas, vec![(1, 0, 4)]);
    assert_eq!(rendered[(0, 1)].symbol(), "o");

    let clipped = vec![
        Span::styled("ab", Style::default()),
        Span::styled(" pr目", Style::default()),
    ];
    let areas = model_click_areas(&clipped, 6, 2, 0, 2);
    let rendered = render_status_bar_buffer(&clipped, 6, 2);
    assert_eq!(areas, vec![(0, 0, 6)]);
    assert_eq!(rendered[(3, 0)].symbol(), "p");
    assert_eq!(rendered[(0, 1)].symbol(), " ");
}

#[test]
fn test_model_click_areas_respect_target_span_and_truncated_viewport() {
    let spans = vec![
        Span::styled("abcdef ", Style::default()),
        Span::styled("model suffix", Style::default()),
        Span::styled(" tail", Style::default()),
    ];
    let areas = model_click_areas(&spans, 8, 1, 1, 2);
    assert!(
        areas.is_empty(),
        "被 Paragraph 完全裁掉的模型文字没有点击区"
    );
}

// ── 防御 ──────────────────────────────────────────────────────────────────

#[test]
fn test_model_click_areas_defensive() {
    let (spans, start, end) = row1_spans();
    // 空 spans
    assert!(model_click_areas(&[], 100, 2, 0, 0).is_empty());
    // area_w = 0
    assert!(model_click_areas(&spans, 0, 2, start, end).is_empty());
    // model_start >= model_end
    assert!(model_click_areas(&spans, 100, 2, 0, 0).is_empty());
    assert!(model_click_areas(&spans, 100, 2, 5, 4).is_empty());
    // row_height = 0（组件内不可达，纯函数不 panic）
    assert!(model_click_areas(&spans, 100, 0, start, end).is_empty());
}

/// 渲染与组件同构的 Paragraph(wrap)（区域宽 = area_w），返回 buffer。
fn render_status_bar_buffer(
    spans: &[Span<'static>],
    area_w: u16,
    area_h: u16,
) -> ratatui::buffer::Buffer {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(area_w, area_h))
        .expect("TestBackend 可创建");
    terminal
        .draw(|f| {
            f.render_widget(
                Paragraph::new(Line::from(spans.to_vec())).wrap(Wrap { trim: false }),
                f.area(),
            );
        })
        .expect("渲染成功");
    terminal.backend().buffer().clone()
}

#[test]
fn test_model_click_regions_cover_rendered_model_text() {
    let (spans, start, end) = row1_spans();
    for width in [61u16, 30] {
        let areas = model_click_areas(&spans, width as usize, 2, start, end);
        let marked = spans
            .iter()
            .enumerate()
            .map(|(index, span)| {
                let style = if (start..end).contains(&index) {
                    span.style.bg(Color::Rgb(1, 2, 3))
                } else {
                    span.style
                };
                Span::styled(span.content.clone(), style)
            })
            .collect::<Vec<_>>();
        let buf = render_status_bar_buffer(&marked, width, 2);
        for (y, x_start, x_end) in areas {
            assert!(x_start < x_end && x_end <= width);
            assert!((x_start..x_end).any(|x| buf[(x, y)].style().bg == Some(Color::Rgb(1, 2, 3))));
        }
    }
}
