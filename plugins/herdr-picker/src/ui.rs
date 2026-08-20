use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Paragraph, Tabs};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::model::{App, LoadState, PreviewState, Tab};

const STACKED_AT: u16 = 96;

pub fn draw(frame: &mut Frame, app: &App) {
    let [body, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    let (left, right) = panels(body);
    draw_left(frame, app, left);
    draw_preview(frame, app, right);
    frame.render_widget(Paragraph::new(footer_text(app, footer.width)).dim(), footer);
}

fn panels(area: Rect) -> (Rect, Rect) {
    if area.width < STACKED_AT {
        let [left, right] =
            Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area);
        (left, right)
    } else {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)])
                .areas(area);
        (left, right)
    }
}

pub fn preview_dimensions(area: Rect) -> (u16, u16) {
    let [body, _] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    let (_, preview) = panels(body);
    let inner = Block::bordered().inner(preview);
    (inner.width.max(1), inner.height.max(1))
}

fn draw_left(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().dim());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [tabs, search, list] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(1),
    ])
    .areas(inner);
    let titles = Tab::ALL.into_iter().map(|tab| Line::from(tab.label()));
    frame.render_widget(
        Tabs::new(titles)
            .select(app.tab.index())
            .highlight_style(Style::new().cyan().bold())
            .divider("  "),
        tabs,
    );
    let mut search_line = Line::from(vec![
        Span::styled("❯ ", Style::new().cyan().bold()),
        Span::raw(app.state().query.as_str()),
    ]);
    if search.width >= 24 {
        search_line.push_span(Span::styled(
            format!("  [{}]", app.state().sort.label()),
            Style::new().dim(),
        ));
    }
    frame.render_widget(Paragraph::new(search_line), search);
    if let Some(position) = search_cursor(search, app.state().query.as_str()) {
        frame.set_cursor_position(position);
    }
    match &app.state().source {
        LoadState::Loading => centered(frame, list, "Loading…"),
        LoadState::Error(error) => centered(frame, list, &format!("Error: {error}")),
        LoadState::Ready(items) if items.is_empty() => centered(frame, list, "No items"),
        LoadState::Ready(_) if app.filtered().is_empty() => centered(frame, list, "No matches"),
        LoadState::Ready(_) => {
            let filtered = app.filtered();
            let selected = app
                .selected()
                .and_then(|selected| filtered.iter().position(|item| item.id == selected.id));
            let rows = filtered.iter().map(|item| {
                ListItem::new(vec![
                    Line::from(item.label.as_str()),
                    Line::from(item.detail.as_str()).dim(),
                ])
            });
            let mut state = ListState::default().with_selected(selected);
            frame.render_stateful_widget(
                List::new(rows)
                    .highlight_style(
                        Style::new()
                            .bg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("› "),
                list,
                &mut state,
            );
        }
    }
}

fn footer_text(app: &App, width: u16) -> String {
    if let Some(warning) = &app.warning {
        return format!(" ! {warning}");
    }
    let prefix = if app.pending_prefix {
        "prefix…  "
    } else {
        ""
    };
    let hints = if width >= 72 {
        "enter focus  ^p up  ^o sort  ←/→ tabs  ↑/↓ move  esc close"
    } else if width >= 45 {
        "^p up  ^o sort  ←/→ tabs  esc close"
    } else if width >= 28 {
        "^p up  ^o sort  esc close"
    } else {
        "^p  ^o  esc"
    };
    format!(" {prefix}{hints}")
}

fn search_cursor(area: Rect, query: &str) -> Option<Position> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let query_width = UnicodeWidthStr::width(query);
    let offset = 2usize.saturating_add(query_width);
    let maximum = usize::from(area.width.saturating_sub(1));
    Some(Position::new(
        area.x
            .saturating_add(u16::try_from(offset.min(maximum)).unwrap_or(u16::MAX)),
        area.y,
    ))
}

fn draw_preview(frame: &mut Frame, app: &App, area: Rect) {
    let title = app.selected().map_or_else(
        || " Preview ".to_string(),
        |item| format!(" {} ", item.label),
    );
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().dim())
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    match &app.preview {
        PreviewState::Empty => centered(frame, inner, "Nothing to preview"),
        PreviewState::Loading => centered(frame, inner, "Loading preview…"),
        PreviewState::Error(error) => centered(frame, inner, &format!("Preview error: {error}")),
        PreviewState::Ready(contents) if app.tab == Tab::Zoxide => {
            frame.render_widget(Paragraph::new(contents.as_str()), inner);
        }
        PreviewState::Ready(contents) => frame.render_widget(
            parse_ansi(contents, (inner.width.max(1), inner.height.max(1))),
            inner,
        ),
    }
}

fn parse_ansi(ansi: &str, (columns, rows): (u16, u16)) -> Text<'static> {
    let mut parser = vt100::Parser::new(rows, columns, 0);
    parser.process(ansi.as_bytes());
    let screen = parser.screen();
    Text::from(
        (0..rows)
            .map(|row| {
                let mut spans = Vec::new();
                let mut contents = String::new();
                let mut current = Style::default();
                for column in 0..columns {
                    let Some(cell) = screen.cell(row, column) else {
                        continue;
                    };
                    if cell.is_wide_continuation() {
                        continue;
                    }
                    let style = cell_style(cell);
                    if style != current && !contents.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut contents), current));
                    }
                    current = style;
                    if cell.has_contents() {
                        contents.push_str(&cell.contents());
                    } else {
                        contents.push(' ');
                    }
                }
                if !contents.is_empty() {
                    spans.push(Span::styled(contents, current));
                }
                Line::from(spans)
            })
            .collect::<Vec<_>>(),
    )
}

fn cell_style(cell: &vt100::Cell) -> Style {
    let color = |color| match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(index) => Some(Color::Indexed(index)),
        vt100::Color::Rgb(red, green, blue) => Some(Color::Rgb(red, green, blue)),
    };
    let mut style = Style::default();
    if let Some(foreground) = color(cell.fgcolor()) {
        style = style.fg(foreground);
    }
    if let Some(background) = color(cell.bgcolor()) {
        style = style.bg(background);
    }
    let mut modifiers = Modifier::empty();
    modifiers.set(Modifier::BOLD, cell.bold());
    modifiers.set(Modifier::ITALIC, cell.italic());
    modifiers.set(Modifier::UNDERLINED, cell.underline());
    modifiers.set(Modifier::REVERSED, cell.inverse());
    style.add_modifier(modifiers)
}

fn centered(frame: &mut Frame, area: Rect, message: &str) {
    let mut lines = vec![Line::raw(""); usize::from(area.height / 3)];
    lines.push(Line::raw(message.to_string()));
    frame.render_widget(Paragraph::new(lines).centered(), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Item, Target};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn app() -> App {
        let mut app = App::new(Tab::Workspaces);
        app.set_items(
            Tab::Workspaces,
            vec![Item {
                id: "w1".into(),
                label: "dotfiles".into(),
                detail: "/src/dotfiles".into(),
                search: "dotfiles".into(),
                preview_pane: Some("p1".into()),
                match_paths: Vec::new(),
                target: Target::Workspace { id: "w1".into() },
            }],
        );
        app
    }

    fn render(width: u16, height: u16) -> String {
        render_app(&app(), width, height)
    }

    fn render_app(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let frame = terminal.draw(|frame| draw(frame, app)).unwrap();
        frame
            .buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn wide_and_narrow_layouts_render_tabs_search_and_items() {
        for width in [140, 70] {
            let screen = render(width, 30);
            assert!(screen.contains("Workspaces"));
            assert!(screen.contains("dotfiles"));
            assert!(screen.contains("Nothing to preview"));
            assert!(screen.contains("[Recent]"));
            assert!(screen.contains("^p up"));
            assert!(screen.contains("^o sort"));
        }
    }

    #[test]
    fn layout_switches_to_stacked_when_narrow() {
        let (wide_left, wide_right) = panels(Rect::new(0, 0, 140, 30));
        let (narrow_left, narrow_right) = panels(Rect::new(0, 0, 70, 30));
        assert_eq!(wide_left.y, wide_right.y);
        assert!(narrow_right.y > narrow_left.y);
    }

    #[test]
    fn tiny_narrow_layouts_handle_long_wide_unicode_queries() {
        let mut app = app();
        app.state_mut().query = "界界界界界界界界界界-long-query".into();
        for (width, height) in [(30, 3), (40, 5), (50, 8)] {
            render_app(&app, width, height);
            let footer = footer_text(&app, width);
            assert!(footer.contains("^p"));
            assert!(footer.contains("^o"));
        }
        assert_eq!(
            search_cursor(Rect::new(2, 4, 10, 1), "界界界界界"),
            Some(Position::new(11, 4))
        );
    }

    #[test]
    fn narrow_footer_compacts_and_history_warning_is_visible() {
        assert_eq!(footer_text(&app(), 20), " ^p  ^o  esc");
        let mut app = app();
        app.warning = Some("history not saved".into());
        assert!(footer_text(&app, 40).contains("history not saved"));
        assert!(render_app(&app, 40, 8).contains("history not saved"));
    }
}
