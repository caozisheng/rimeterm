use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget,
};

use crate::app::{App, Mode, View};
use crate::theme::Theme;
use crate::ui::{header, keep_cursor_visible, task_row};

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let theme = *app.theme();
    super::fill_bg(frame, area, Style::default().bg(theme.bg));

    let [header_area, _sp, body_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(area);

    header::render(
        frame,
        header_area,
        &theme,
        header::HeaderProps {
            title: Some("archive.txt"),
            count: app.archive().len(),
            sort: "file-order",
            filter: None,
        },
    );

    let visible = app.visible_indices();
    let cursor_active = app.mode != Mode::Help && app.mode != Mode::Settings;
    if visible.is_empty() {
        let para = Paragraph::new(vec![Line::from(Span::styled(
            "   no archived tasks".to_string(),
            Style::default().fg(theme.dim),
        ))])
        .style(Style::default().bg(theme.bg).fg(theme.fg));
        frame.render_widget(para, body_area);
        app.set_text_layout(body_area, vec!["   no archived tasks".into()], None);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_line: Option<usize> = None;
    for (i, &abs) in visible.iter().enumerate() {
        let task = &app.archive().tasks()[abs];
        let opts = task_row::RowOpts {
            idx_label: i,
            cursor: i == app.cursor && cursor_active,
            multi_mode: false,
            multi_checked: false,
            selected: false,
            show_line_num: app.prefs.layout.line_num,
            match_term: None,
            today: app.today(),
            hidden_keys: &app.prefs.hidden_keys,
        };
        if i == app.cursor {
            cursor_line = Some(lines.len());
        }
        lines.push(task_row::build_line(task, opts, &theme));
    }

    let scroll_cell = &app.view_scroll[View::Archive.idx()];
    let scroll = keep_cursor_visible(
        scroll_cell.get(),
        cursor_line,
        body_area.height,
        lines.len(),
    );
    scroll_cell.set(scroll);
    let scrollable = lines.len() > usize::from(body_area.height);
    let text_area = if scrollable {
        Rect::new(
            body_area.x,
            body_area.y,
            body_area.width.saturating_sub(1),
            body_area.height,
        )
    } else {
        body_area
    };
    let scrollbar = scrollable.then(|| {
        Rect::new(
            body_area.right().saturating_sub(1),
            body_area.y,
            1,
            body_area.height,
        )
    });
    let lines: Vec<Line<'static>> = lines
        .into_iter()
        .map(|line| {
            Line::from(
                line.spans
                    .into_iter()
                    .map(|span| Span::styled(span.content.into_owned(), span.style))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let plain_lines: Vec<String> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    app.set_text_layout(text_area, plain_lines, scrollbar);
    let line_count = lines.len();
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().bg(theme.bg).fg(theme.fg))
            .scroll((scroll, 0)),
        text_area,
    );
    render_selection(frame, text_area, scroll, app, &theme);
    if let Some(scrollbar_area) = scrollbar {
        let mut state = ScrollbarState::new(line_count)
            .position(usize::from(scroll))
            .viewport_content_length(usize::from(body_area.height));
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .render(scrollbar_area, frame.buffer_mut(), &mut state);
    }
}

fn render_selection(frame: &mut Frame, area: Rect, scroll: u16, app: &App, theme: &Theme) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let point = crate::app::TextPoint {
                line: usize::from(y.saturating_sub(area.y)) + usize::from(scroll),
                column: usize::from(x.saturating_sub(area.x)),
            };
            if app.text_selection_contains(point)
                && let Some(cell) = frame.buffer_mut().cell_mut((x, y))
            {
                cell.set_style(Style::default().bg(theme.selection).fg(theme.bg));
            }
        }
    }
}
