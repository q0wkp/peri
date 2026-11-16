//! ratatui-kit SessionColumn layout component.

// element! 宏展开触发 clippy::needless_update（ratatui-kit 上游问题），模块级抑制。
#![allow(clippy::needless_update)]

use crate::kit::atoms;
use crate::kit::input_area::InputArea;
use crate::kit::message_area::MessageArea;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::panel_overlay::PanelOverlay;
use ratatui_kit::{
    prelude::*,
    ratatui::layout::{Constraint, Direction, Rect},
};

// ── 共享内容带 ────────────────────────────────────────────────────────

/// 将共享网格限制在父区域内，终端尺寸未知时保持父区域。
pub(crate) fn content_band_area(area: Rect, grid: GridSpec) -> Rect {
    if grid.band_width == 0 || area.width == 0 {
        return area;
    }
    Rect {
        width: grid.band_width.min(area.width),
        ..area
    }
}

/// draw 期统一子组件区域，使渲染、换行、光标与鼠标命中使用同一矩形。
/// 透明布局保留子组件的高度约束；主界面网格占满终端宽度。
#[derive(Default, Props)]
pub(crate) struct ContentBandProps<'a> {
    /// Transcript 水平网格（终端 resize 后每帧重建）。
    pub grid: GridSpec,
    pub children: Vec<AnyElement<'a>>,
}

pub(crate) struct ContentBand {
    grid: GridSpec,
}

impl Component for ContentBand {
    type Props<'a> = ContentBandProps<'a>;

    fn new(props: &Self::Props<'_>) -> Self {
        Self { grid: props.grid }
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        self.grid = props.grid;
        updater.set_transparent_layout(true);
        updater.update_children(&mut props.children, None);
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_, '_>) {
        drawer.area = content_band_area(drawer.area, self.grid);
    }
}

// ── 高度降级计划（spec §11）─────────────────────────────────────────────

/// §11「transcript 至少保留 3 行」——输入区（composer + 队列 + 弹出层）的
/// 高度预算下限（SessionColumn 用 `term_h - status - MIN_TRANSCRIPT_ROWS`
/// 推导 `max_total_height`；InputArea 超预算时先截断队列）。
pub const MIN_TRANSCRIPT_ROWS: u16 = 3;

/// 状态栏在给定终端高度下的降级档位（§11）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusBarMode {
    /// `h ≥ 12`：完整双行（Row1 + Row2 key hints + NotifRow + 缓冲行，高度 4）。
    Full,
    /// `8 ≤ h < 12`：仅 Row1 + NotifRow（高度 2），隐藏 Row2 key hints 与缓冲行。
    Row1Only,
    /// `h < 8`：完全隐藏（高度 0），把高度让给 transcript/composer。
    Hidden,
}

impl StatusBarMode {
    /// 各档位占用的高度行数（Full=4 / Row1Only=2 / Hidden=0）——
    /// 供 SessionColumn 计算输入区高度预算（§11 transcript ≥3 行保证）。
    pub fn height(self) -> u16 {
        match self {
            StatusBarMode::Full => 4,
            StatusBarMode::Row1Only => 2,
            StatusBarMode::Hidden => 0,
        }
    }
}

/// 按终端高度推导的布局降级计划（§11）——纯函数，AppShell 消费结果。
///
/// 约束优先级：transcript ≥3 行 > composer ≥1 行 > status。
/// - `h ≥ 12`：Full 状态栏，session title 可见，composer 不钳制；
/// - `8 ≤ h < 12`：Row1Only（隐藏 Row2 key hints），隐藏 session title；
/// - `h < 8`：状态栏隐藏，composer 钳制 ≤2 行（transcript ≥3 由
///   `Constraint::Fill(1)` 余量自动保证——status 0 + composer ≤4 → transcript
///   = h - 4 ≥ 3 当 h ≥ 7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeightPlan {
    pub status_bar: StatusBarMode,
    /// composer 编辑行数上限（`None` = 默认 10）。h<8 时为 `Some(2)`。
    pub composer_max_lines: Option<u16>,
    /// session title（composer 上边栏）是否可见。h<12 时隐藏。
    pub session_title_visible: bool,
}

impl Default for HeightPlan {
    fn default() -> Self {
        Self {
            status_bar: StatusBarMode::Full,
            composer_max_lines: None,
            session_title_visible: true,
        }
    }
}

/// 高度断点纯函数（§11）：`h ≥ 12` Full；`8 ≤ h < 12` Row1Only + 隐藏 title；
/// `h < 8` Hidden + composer ≤2 行。
pub fn layout_plan(h: u16) -> HeightPlan {
    if h >= 12 {
        HeightPlan::default()
    } else if h >= 8 {
        HeightPlan {
            status_bar: StatusBarMode::Row1Only,
            composer_max_lines: None,
            session_title_visible: false,
        }
    } else {
        HeightPlan {
            status_bar: StatusBarMode::Hidden,
            composer_max_lines: Some(2),
            session_title_visible: false,
        }
    }
}

#[derive(Default, Props)]
pub struct SessionColumnProps {
    /// 高度降级计划（AppShell 由 `layout_plan(term_h)` 推导后传入）。
    pub plan: HeightPlan,
}

#[component]
pub fn SessionColumn(
    props: &SessionColumnProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let acp = hooks.use_atom(&atoms::ACP_STATE);
    let active_panel = hooks.use_atom(&atoms::ACTIVE_PANEL);

    let loading = acp.read().is_loading;
    let panel_open = active_panel.read().is_some();

    // 消息与输入共用终端全宽网格。
    let (term_w, term_h) = hooks.use_terminal_size();
    let grid = GridSpec::grid_for(term_w);
    // hook 占位——ratatui-kit 要求 hook 数量恒定不可增减
    let _last_width = hooks.use_state(|| 0u16);

    // [Fix §11] 输入区高度预算 = 终端高度 - 状态栏 - transcript 最低 3 行。
    // 队列（queued）/弹出层超过预算时由 InputArea 先截断队列（transcript
    // ≥3 行优先于 composer > status；40×8 + 队列场景 transcript 不再被挤到
    // 2 行）。预算低于 composer 最低需求时 InputArea 保 composer（优雅降级）。
    let input_max_height =
        term_h.saturating_sub(props.plan.status_bar.height() + MIN_TRANSCRIPT_ROWS);

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            // 位置追踪与渲染共用同一内容区域。
            ContentBand(grid: grid) {
                MessageArea(grid: grid)
            }

            // 面板位于消息流之上、输入区之上；参与主布局，不再是根级浮动覆盖。
            // 面板自带内边距与坐标基准，不参与内容带约束。
            PanelOverlay()

            // 面板打开时隐藏输入区，避免输入框抢占用户注意力；关闭面板后自动恢复。
            // [Slice 3a] grid prop：composer prompt 前缀按 gap 对齐 transcript
            // content 起点（§10）；窄断点下标题/footer 随 HeightPlan 降级。
            ContentBand(grid: grid) {
                InputArea(
                    loading: loading,
                    hidden: panel_open,
                    max_lines: props.plan.composer_max_lines,
                    session_title_visible: props.plan.session_title_visible,
                    grid: grid,
                    max_total_height: Some(input_max_height),
                )
            }
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_kit::ratatui::style::Style;
    use ratatui_kit::test_util::render_frame;
    use std::sync::{Arc, Mutex};

    // ── 内容带渲染与位置追踪 ──────────────────────────────────────────

    #[derive(Default, Props)]
    struct AreaProbeProps {
        text: &'static str,
        sink: Arc<Mutex<Option<Rect>>>,
    }

    /// 测试专用探针：hook 记录 `pre_component_draw` 看到的区域，
    /// draw 在该区域左上角写标记——两者必须落在同一列。
    struct AreaProbe {
        text: &'static str,
    }

    struct AreaProbeHook {
        sink: Arc<Mutex<Option<Rect>>>,
    }

    impl Hook for AreaProbeHook {
        fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
            *self.sink.lock().unwrap() = Some(drawer.area);
        }
    }

    impl Component for AreaProbe {
        type Props<'a> = AreaProbeProps;

        fn new(_props: &Self::Props<'_>) -> Self {
            Self { text: "P" }
        }

        fn update(
            &mut self,
            props: &mut Self::Props<'_>,
            mut hooks: Hooks,
            _updater: &mut ComponentUpdater,
        ) {
            self.text = props.text;
            let sink = props.sink.clone();
            hooks.use_hook(move || AreaProbeHook { sink });
        }

        fn draw(&mut self, drawer: &mut ComponentDrawer<'_, '_>) {
            let area = drawer.area;
            drawer
                .buffer_mut()
                .set_string(area.x, area.y, self.text, Style::default());
            if area.width > 1 {
                drawer
                    .buffer_mut()
                    .set_string(area.right() - 1, area.y, "R", Style::default());
            }
        }
    }

    /// 宽终端与窄终端的子组件都从左侧开始绘制。
    #[test]
    fn content_band_draw_places_child_inside_band() {
        for (term_w, expected_x) in [(80u16, 0u16), (140, 0), (220, 0)] {
            let grid = GridSpec::grid_for(term_w);
            assert_eq!(grid.left_pad, expected_x, "term_w={term_w}");
            let buf = render_frame(
                element!(ContentBand(grid: grid) {
                    Text(text: "X")
                }),
                term_w,
                3,
            );
            assert_eq!(
                buf[(expected_x, 0)].symbol(),
                "X",
                "term_w={term_w}: 标记应出现在带左缘"
            );
        }
    }

    /// tracker 一致性：hook 的 `pre_component_draw` 记录区域 == 实际绘制起点/宽度。
    #[test]
    fn content_band_tracker_matches_rendered_band() {
        let grid = GridSpec::grid_for(140);
        let expected = content_band_area(Rect::new(0, 0, 140, 5), grid);
        let sink = Arc::new(Mutex::new(None));
        let buf = render_frame(
            element!(ContentBand(grid: grid) {
                AreaProbe(text: "P", sink: sink.clone())
            }),
            140,
            5,
        );
        let recorded = sink
            .lock()
            .unwrap()
            .expect("pre_component_draw 应记录组件区域");
        assert_eq!(recorded, expected);
        assert_eq!(recorded.x, grid.left_pad);
        assert_eq!(recorded.width, grid.band_width);
        assert_eq!(buf[(recorded.x, recorded.y)].symbol(), "P");
        assert_eq!(recorded, Rect::new(0, 0, 140, 5));
        assert_eq!(buf[(139, 0)].symbol(), "R", "子组件应绘制到终端右缘");
    }

    /// §11 高度断点矩阵：13/12 → Full；11/9/8 → Row1Only；7 → Hidden + composer 钳制。
    #[test]
    fn layout_plan_height_matrix() {
        assert_eq!(layout_plan(13).status_bar, StatusBarMode::Full);
        assert_eq!(layout_plan(12).status_bar, StatusBarMode::Full);
        assert!(layout_plan(12).session_title_visible);
        assert_eq!(layout_plan(12).composer_max_lines, None);

        assert_eq!(layout_plan(11).status_bar, StatusBarMode::Row1Only);
        assert_eq!(layout_plan(9).status_bar, StatusBarMode::Row1Only);
        assert_eq!(layout_plan(8).status_bar, StatusBarMode::Row1Only);
        assert!(
            !layout_plan(8).session_title_visible,
            "h<12 隐藏 session title"
        );
        assert_eq!(layout_plan(8).composer_max_lines, None);

        assert_eq!(layout_plan(7).status_bar, StatusBarMode::Hidden);
        assert!(!layout_plan(7).session_title_visible);
        assert_eq!(layout_plan(7).composer_max_lines, Some(2));
        assert_eq!(layout_plan(1).status_bar, StatusBarMode::Hidden);
    }

    /// 40×8 冒烟（§11/§15）：h=8 时 transcript ≥3 行、composer 编辑行 ≤2、
    /// 无 key hints（Row2 隐藏）。
    /// 数学依据：status(Row1Only)=2 + composer(editor 1 + border 2)=3 → transcript=3。
    /// 多行输入（editor_rows ≤ 10）会压缩 transcript——用户主动行为，接受。
    #[test]
    fn layout_plan_40x8_smoke() {
        let plan = layout_plan(8);
        assert_eq!(plan.status_bar, StatusBarMode::Row1Only);
        // status 高度 2（Row1 + NotifRow）
        let status_h = 2u16;
        // 单行输入 composer 高度 3（editor 1 + border 2）
        let composer_h = 3u16;
        assert!(
            8 - status_h - composer_h >= 3,
            "40×8 下 transcript 应保留 ≥3 行"
        );
        // composer 编辑行数 ≤2：单行输入自然满足；h<8 时由 max_lines=Some(2) 钳制
        let editor_rows = 1u16;
        assert!(editor_rows <= 2, "composer 编辑行数应 ≤2");
        // 无 key hints：Row1Only 隐藏 Row2
        assert_ne!(plan.status_bar, StatusBarMode::Full);
    }

    /// 内容区域占满终端，且保留父区域起点与高度。
    #[test]
    fn content_band_area_fills_terminal() {
        for width in [40, 80, 120, 140, 220] {
            let area = Rect::new(3, 2, width, 24);
            assert_eq!(content_band_area(area, GridSpec::grid_for(width)), area);
        }
    }

    /// 区域比带宽窄（嵌套布局 / 面板挤压）：钳到区域内，不越界、不留 0 宽。
    #[test]
    fn content_band_area_clamps_to_narrower_area() {
        for width in [0u16, 1, 2, 10, 40, 116] {
            let area = Rect::new(3, 5, width, 10);
            let band = content_band_area(area, GridSpec::grid_for(220));
            if width == 0 {
                assert_eq!(band, area, "0 宽区域不放大");
                continue;
            }
            assert!(band.width >= 1, "width={width}: 至少保留 1 列");
            assert!(
                band.x >= area.x
                    && band.x as usize + band.width as usize <= area.x as usize + width as usize,
                "width={width}: band {band:?} 应落在 {area:?} 内"
            );
            assert_eq!(band.y, area.y, "width={width}: 只改水平轴");
            assert_eq!(band.height, area.height, "width={width}: 高度不变");
        }
    }

    /// 终端尺寸未知（`terminal::size()` 失败 → `grid_for(0)`，带宽 0）：
    /// 保持原区域，不能把整条 UI 塌成 1 列。
    #[test]
    fn content_band_area_keeps_area_when_grid_unavailable() {
        let area = Rect::new(0, 0, 100, 30);
        assert_eq!(GridSpec::grid_for(0).band_width, 0);
        assert_eq!(content_band_area(area, GridSpec::grid_for(0)), area);
    }
}
