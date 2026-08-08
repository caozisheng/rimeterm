//! Three-column models.dev browser adapted from `reyamira/models` v0.14.0.

use std::any::Any;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use rimeterm_core::pane::{PaneCaps, PaneId, PaneProvider, PaneRenderCtx, RenderOutcome};
use rimeterm_models::format::{EM_DASH, format_context, format_cost_short};
use rimeterm_models::{Model, Provider};

use crate::models_model::{
    CatalogProjection, Filters, ModelEntry, ModelsRequest, ModelsResponse, ProviderCategory,
    ProviderListItem, Snapshot, SortKey, SortOrder, provider_category,
};
use crate::models_worker::ModelsWorker;

const PAGE_SIZE: usize = 10;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Focus {
    #[default]
    Providers,
    Models,
    Details,
}

impl Focus {
    fn left(self) -> Self {
        match self {
            Self::Providers => Self::Details,
            Self::Models => Self::Providers,
            Self::Details => Self::Models,
        }
    }

    fn right(self) -> Self {
        match self {
            Self::Providers => Self::Models,
            Self::Models => Self::Details,
            Self::Details => Self::Providers,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Providers => "providers",
            Self::Models => "models",
            Self::Details => "details",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum Modal {
    #[default]
    None,
    Search {
        input: String,
    },
}

pub struct ModelsPane {
    id: PaneId,
    title: String,
    worker: ModelsWorker,
    snapshot: Snapshot,
    requested_generation: u64,
    applied_generation: u64,
    sort_key: SortKey,
    sort_order: SortOrder,
    filters: Filters,
    category_filter: ProviderCategory,
    group_by_category: bool,
    search_query: String,
    selected_provider_id: Option<String>,
    selected_model_provider_id: Option<String>,
    selected_model_id: Option<String>,
    provider_cursor: usize,
    model_cursor: usize,
    focus: Focus,
    detail_scroll: u16,
    modal: Modal,
    fetching: bool,
    hint: Option<String>,
}

impl ModelsPane {
    pub fn new() -> Self {
        let worker = ModelsWorker::spawn();
        worker.send(ModelsRequest::Fetch { generation: 1 });
        Self {
            id: PaneId::next(),
            title: "models".to_owned(),
            worker,
            snapshot: Snapshot::empty(),
            requested_generation: 1,
            applied_generation: 0,
            sort_key: SortKey::Release,
            sort_order: SortOrder::Descending,
            filters: Filters::default(),
            category_filter: ProviderCategory::All,
            group_by_category: false,
            search_query: String::new(),
            selected_provider_id: None,
            selected_model_provider_id: None,
            selected_model_id: None,
            provider_cursor: 0,
            model_cursor: 0,
            focus: Focus::Providers,
            detail_scroll: 0,
            modal: Modal::None,
            fetching: true,
            hint: None,
        }
    }

    fn projection(&self) -> CatalogProjection {
        CatalogProjection::build(
            &self.snapshot,
            self.selected_provider_id.as_deref(),
            self.filters,
            self.category_filter,
            self.group_by_category,
            &self.search_query,
            self.sort_key,
            self.sort_order,
        )
    }

    fn request_fetch(&mut self) {
        self.requested_generation = self.requested_generation.saturating_add(1);
        self.worker.send(ModelsRequest::Fetch {
            generation: self.requested_generation,
        });
        self.fetching = true;
    }

    fn set_hint(&mut self, text: impl Into<String>) {
        self.hint = Some(text.into());
    }

    fn sync_selection(&mut self) {
        let projection = self.projection();
        if let Some(id) = self.selected_provider_id.as_deref() {
            if let Some(index) = projection.provider_items.iter().position(
                |item| matches!(item, ProviderListItem::Provider { id: current, .. } if current == id),
            ) {
                self.provider_cursor = index;
            } else {
                self.selected_provider_id = None;
                self.provider_cursor = 0;
            }
        } else {
            self.provider_cursor = 0;
        }
        let projection = self.projection();
        let selected_index = self
            .selected_model_provider_id
            .as_deref()
            .zip(self.selected_model_id.as_deref())
            .and_then(|(provider_id, model_id)| {
                projection.models.iter().position(|entry| {
                    entry.provider_id == provider_id && entry.model.id == model_id
                })
            });
        self.model_cursor = selected_index.unwrap_or(0);
        if let Some(entry) = projection.models.get(self.model_cursor) {
            self.selected_model_provider_id = Some(entry.provider_id.clone());
            self.selected_model_id = Some(entry.model.id.clone());
        } else {
            self.selected_model_provider_id = None;
            self.selected_model_id = None;
        }
        self.detail_scroll = 0;
    }

    fn choose_provider(&mut self, index: usize, projection: &CatalogProjection) {
        self.provider_cursor = projection.selectable_provider_index(index, true);
        self.selected_provider_id = match projection.provider_items.get(self.provider_cursor) {
            Some(ProviderListItem::Provider { id, .. }) => Some(id.clone()),
            _ => None,
        };
        self.model_cursor = 0;
        self.selected_model_provider_id = None;
        self.selected_model_id = None;
        self.sync_selection();
    }

    fn move_provider(&mut self, delta: isize) {
        let projection = self.projection();
        if projection.provider_items.is_empty() {
            return;
        }
        let max = projection.provider_items.len() - 1;
        let mut next = self.provider_cursor.saturating_add_signed(delta).min(max);
        next = projection.selectable_provider_index(next, delta >= 0);
        self.choose_provider(next, &projection);
    }

    fn move_model(&mut self, delta: isize) {
        let projection = self.projection();
        if projection.models.is_empty() {
            self.model_cursor = 0;
            self.selected_model_provider_id = None;
            self.selected_model_id = None;
            return;
        }
        self.model_cursor = self
            .model_cursor
            .saturating_add_signed(delta)
            .min(projection.models.len() - 1);
        let entry = &projection.models[self.model_cursor];
        self.selected_model_provider_id = Some(entry.provider_id.clone());
        self.selected_model_id = Some(entry.model.id.clone());
        self.detail_scroll = 0;
    }

    fn jump_start(&mut self) {
        match self.focus {
            Focus::Providers => self.move_provider(-(self.provider_cursor as isize)),
            Focus::Models => self.move_model(-(self.model_cursor as isize)),
            Focus::Details => self.detail_scroll = 0,
        }
    }

    fn jump_end(&mut self) {
        match self.focus {
            Focus::Providers => {
                let len = self.projection().provider_items.len();
                if len > 0 {
                    self.move_provider((len - 1) as isize);
                }
            }
            Focus::Models => {
                let len = self.projection().models.len();
                if len > 0 {
                    self.move_model((len - 1) as isize);
                }
            }
            Focus::Details => self.detail_scroll = u16::MAX,
        }
    }

    fn toggle_filter(&mut self, key: char) {
        match key {
            '1' => self.filters.reasoning = !self.filters.reasoning,
            '2' => self.filters.tools = !self.filters.tools,
            '3' => self.filters.open_weights = !self.filters.open_weights,
            '4' => self.filters.free = !self.filters.free,
            '5' => self.category_filter = self.category_filter.next(),
            '6' => self.group_by_category = !self.group_by_category,
            _ => return,
        }
        self.sync_selection();
    }

    fn current_model<'a>(&self, projection: &'a CatalogProjection) -> Option<&'a ModelEntry> {
        projection.models.get(self.model_cursor)
    }

    pub(crate) fn snapshot_state(&self) -> rimeterm_config::memory_state::PaneState {
        let mut values = std::collections::BTreeMap::from([
            ("sort".into(), self.sort_key.label().into()),
            (
                "order".into(),
                match self.sort_order {
                    SortOrder::Ascending => "ascending",
                    SortOrder::Descending => "descending",
                }
                .into(),
            ),
            ("focus".into(), self.focus.key().into()),
            ("reasoning".into(), self.filters.reasoning.to_string()),
            ("tools".into(), self.filters.tools.to_string()),
            ("open_weights".into(), self.filters.open_weights.to_string()),
            ("free".into(), self.filters.free.to_string()),
            ("category".into(), category_key(self.category_filter).into()),
            ("group".into(), self.group_by_category.to_string()),
        ]);
        if !self.search_query.is_empty() {
            values.insert("search".into(), self.search_query.clone());
        }
        if let Some(id) = &self.selected_provider_id {
            values.insert("provider".into(), id.clone());
        }
        if let Some(id) = &self.selected_model_provider_id {
            values.insert("model_provider".into(), id.clone());
        }
        if let Some(id) = &self.selected_model_id {
            values.insert("model".into(), id.clone());
        }
        rimeterm_config::memory_state::PaneState { values }
    }

    pub(crate) fn restore_state(&mut self, state: &rimeterm_config::memory_state::PaneState) {
        let value = |key| state.values.get(key).map(String::as_str);
        self.sort_key = match value("sort") {
            Some("name") => SortKey::Name,
            Some("cost") => SortKey::Cost,
            Some("ctx") => SortKey::Context,
            _ => SortKey::Release,
        };
        self.sort_order = match value("order") {
            Some("ascending") => SortOrder::Ascending,
            _ => SortOrder::Descending,
        };
        self.focus = match value("focus") {
            Some("models") => Focus::Models,
            Some("details") => Focus::Details,
            _ => Focus::Providers,
        };
        self.filters = Filters {
            reasoning: value("reasoning") == Some("true"),
            tools: value("tools") == Some("true"),
            open_weights: value("open_weights") == Some("true"),
            free: value("free") == Some("true"),
        };
        self.category_filter = parse_category(value("category"));
        self.group_by_category = value("group") == Some("true");
        self.search_query = value("search").unwrap_or_default().to_owned();
        self.selected_provider_id = value("provider").map(str::to_owned);
        self.selected_model_provider_id = value("model_provider").map(str::to_owned);
        self.selected_model_id = value("model").map(str::to_owned);
        self.modal = Modal::None;
        self.sync_selection();
    }
}

impl Default for ModelsPane {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneProvider for ModelsPane {
    fn id(&self) -> PaneId {
        self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn caps(&self) -> PaneCaps {
        PaneCaps {
            wants_raw_input: false,
            holds_foreground_work: false,
        }
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn render(
        &mut self,
        area: Rect,
        frame: &mut Frame<'_>,
        ctx: &PaneRenderCtx<'_>,
    ) -> RenderOutcome {
        let outer_style = if ctx.focused {
            Style::default().fg(ctx.focus_color)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let loading = if self.fetching { " · loading" } else { "" };
        let block = Block::default()
            .title(format!(
                " models · {} providers · {} models{} ",
                self.snapshot.provider_count, self.snapshot.model_count, loading
            ))
            .borders(Borders::ALL)
            .border_style(outer_style);
        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());
        if inner.width == 0 || inner.height == 0 {
            return RenderOutcome::default();
        }
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20),
                Constraint::Percentage(45),
                Constraint::Percentage(35),
            ])
            .split(vertical[0]);
        let projection = self.projection();
        render_providers(frame, columns[0], self, &projection);
        render_models(frame, columns[1], self, &projection);
        render_right(frame, columns[2], self, &projection);
        render_footer(frame, vertical[1], self);
        self.hint = None;
        RenderOutcome::default()
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        if let Modal::Search { input } = &mut self.modal {
            return match key.code {
                KeyCode::Char(c) => {
                    input.push(c);
                    true
                }
                KeyCode::Backspace => {
                    input.pop();
                    true
                }
                KeyCode::Enter => {
                    self.search_query = input.trim().to_owned();
                    self.modal = Modal::None;
                    self.sync_selection();
                    true
                }
                KeyCode::Esc => {
                    self.modal = Modal::None;
                    true
                }
                _ => true,
            };
        }
        match key.code {
            KeyCode::Char('h') | KeyCode::Left => self.focus = self.focus.left(),
            KeyCode::Char('l') | KeyCode::Right => self.focus = self.focus.right(),
            KeyCode::Char('j') | KeyCode::Down => match self.focus {
                Focus::Providers => self.move_provider(1),
                Focus::Models => self.move_model(1),
                Focus::Details => self.detail_scroll = self.detail_scroll.saturating_add(1),
            },
            KeyCode::Char('k') | KeyCode::Up => match self.focus {
                Focus::Providers => self.move_provider(-1),
                Focus::Models => self.move_model(-1),
                Focus::Details => self.detail_scroll = self.detail_scroll.saturating_sub(1),
            },
            KeyCode::PageDown => match self.focus {
                Focus::Providers => self.move_provider(PAGE_SIZE as isize),
                Focus::Models => self.move_model(PAGE_SIZE as isize),
                Focus::Details => {
                    self.detail_scroll = self.detail_scroll.saturating_add(PAGE_SIZE as u16)
                }
            },
            KeyCode::PageUp => match self.focus {
                Focus::Providers => self.move_provider(-(PAGE_SIZE as isize)),
                Focus::Models => self.move_model(-(PAGE_SIZE as isize)),
                Focus::Details => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(PAGE_SIZE as u16)
                }
            },
            KeyCode::Home | KeyCode::Char('g') => self.jump_start(),
            KeyCode::End | KeyCode::Char('G') => self.jump_end(),
            KeyCode::Char(c @ '1'..='6') => self.toggle_filter(c),
            KeyCode::Char('/') => {
                self.modal = Modal::Search {
                    input: self.search_query.clone(),
                }
            }
            KeyCode::Char('s') => {
                self.sort_key = self.sort_key.next();
                self.sync_selection();
            }
            KeyCode::Char('S') => {
                self.sort_order = self.sort_order.flip();
                self.sync_selection();
            }
            KeyCode::Char('r') | KeyCode::F(5) => {
                self.request_fetch();
                self.set_hint("refreshing models.dev");
            }
            _ => return false,
        }
        true
    }

    fn on_mouse(&mut self, _event: MouseEvent, _outer_rect: Rect) -> bool {
        false
    }

    fn reload(&mut self) {
        self.request_fetch();
        self.set_hint("refreshing models.dev");
    }

    fn poll_background(&mut self) -> bool {
        let mut changed = false;
        for response in self.worker.drain() {
            let ModelsResponse::Fetch { generation, result } = response;
            if generation < self.applied_generation {
                continue;
            }
            self.applied_generation = generation;
            self.fetching = false;
            match result {
                Ok(snapshot) => {
                    self.snapshot = snapshot;
                    self.sync_selection();
                }
                Err(error) => self.snapshot.last_error = Some(error),
            }
            changed = true;
        }
        changed
    }
}

fn focus_border(focused: bool) -> Style {
    Style::default().fg(if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    })
}

fn category_color(category: ProviderCategory) -> Color {
    match category {
        ProviderCategory::All => Color::White,
        ProviderCategory::Origin => Color::Magenta,
        ProviderCategory::Cloud => Color::Blue,
        ProviderCategory::Inference => Color::Green,
        ProviderCategory::Gateway => Color::Yellow,
        ProviderCategory::Tool => Color::Cyan,
    }
}

fn render_providers(
    frame: &mut Frame<'_>,
    area: Rect,
    pane: &ModelsPane,
    projection: &CatalogProjection,
) {
    let block = Block::default()
        .title(" Providers ")
        .borders(Borders::ALL)
        .border_style(focus_border(pane.focus == Focus::Providers));
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);
    let cat_color = if pane.category_filter == ProviderCategory::All {
        Color::DarkGray
    } else {
        category_color(pane.category_filter)
    };
    Paragraph::new(Line::from(vec![
        Span::styled("[5]", Style::default().fg(cat_color)),
        Span::raw(format!(" {}  ", pane.category_filter.short_label())),
        Span::styled(
            "[6]",
            Style::default().fg(if pane.group_by_category {
                Color::Green
            } else {
                Color::DarkGray
            }),
        ),
        Span::raw(" Grp"),
    ]))
    .render(split[0], frame.buffer_mut());

    let viewport = split[1].height as usize;
    let (start, end) = window_around(
        pane.provider_cursor,
        projection.provider_items.len(),
        viewport,
    );
    let mut lines = Vec::with_capacity(end.saturating_sub(start));
    for (offset, item) in projection.provider_items[start..end].iter().enumerate() {
        let index = start + offset;
        let selected = index == pane.provider_cursor && pane.focus == Focus::Providers;
        let caret = if selected { "> " } else { "  " };
        let line = match item {
            ProviderListItem::All { count } => Line::from(vec![
                Span::styled(caret, Style::default().fg(Color::Cyan)),
                Span::styled(format!("All ({count})"), Style::default().fg(Color::Green)),
            ]),
            ProviderListItem::CategoryHeader(category) => Line::from(Span::styled(
                truncate(
                    &format!("── {} ─────────────────", category.label()),
                    inner.width as usize,
                ),
                Style::default()
                    .fg(category_color(*category))
                    .add_modifier(Modifier::BOLD),
            )),
            ProviderListItem::Provider { id, count } => {
                let category = provider_category(id);
                Line::from(vec![
                    Span::styled(caret, Style::default().fg(Color::Cyan)),
                    Span::styled(
                        format!("{} ", category.initial()),
                        Style::default().fg(category_color(category)),
                    ),
                    Span::raw(truncate(id, inner.width.saturating_sub(9) as usize)),
                    Span::styled(format!(" ({count})"), Style::default().fg(Color::Gray)),
                ])
            }
        };
        lines.push(line);
    }
    Paragraph::new(lines).render(split[1], frame.buffer_mut());
}

fn render_models(
    frame: &mut Frame<'_>,
    area: Rect,
    pane: &ModelsPane,
    projection: &CatalogProjection,
) {
    let provider_name = pane
        .selected_provider_id
        .as_deref()
        .and_then(|id| pane.snapshot.provider(id))
        .map_or("Models", |entry| entry.provider.name.as_str());
    let mut title = format!(" {provider_name} ({})", projection.models.len());
    if !pane.search_query.is_empty() {
        title.push_str(&format!(" [/{}]", pane.search_query));
    }
    let filters = pane.filters.labels();
    if !filters.is_empty() {
        title.push_str(&format!(" [{}]", filters.join(" ")));
    }
    title.push_str(&format!(
        " {}{} ",
        pane.sort_order.arrow(),
        pane.sort_key.label()
    ));
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(focus_border(pane.focus == Focus::Models));
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if projection.models.is_empty() {
        let message = if pane.fetching && pane.snapshot.provider_count == 0 {
            "fetching models.dev..."
        } else {
            "no matching models"
        };
        Paragraph::new(Span::styled(message, Style::default().fg(Color::DarkGray)))
            .render(inner, frame.buffer_mut());
        return;
    }
    let model_width = inner.width.saturating_sub(31).max(8) as usize;
    let header = Line::from(vec![
        Span::styled(
            "  RTFO ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<model_width$}", "Model"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " Input  Output Context",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let viewport = inner.height.saturating_sub(1) as usize;
    let (start, end) = window_around(pane.model_cursor, projection.models.len(), viewport);
    let mut lines = Vec::with_capacity(end.saturating_sub(start) + 1);
    lines.push(header);
    for (offset, entry) in projection.models[start..end].iter().enumerate() {
        let index = start + offset;
        let selected = index == pane.model_cursor;
        let caret = if selected && pane.focus == Focus::Models {
            "> "
        } else {
            "  "
        };
        let model_style = if selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(caret, Style::default().fg(Color::Cyan)),
            capability(entry.model.reasoning, "R", Color::Cyan, "·"),
            capability(entry.model.tool_call, "T", Color::Yellow, "·"),
            capability(entry.model.attachment, "F", Color::Magenta, "·"),
            capability(entry.model.open_weights, "O", Color::Green, "C"),
            Span::raw(" "),
            Span::styled(
                format!("{:<model_width$}", truncate(&entry.model.name, model_width)),
                model_style,
            ),
            Span::styled(
                format!(" {:>6}", format_cost_short(entry.model.input_cost())),
                cost_style(entry.model.input_cost()),
            ),
            Span::styled(
                format!(" {:>7}", format_cost_short(entry.model.output_cost())),
                cost_style(entry.model.output_cost()),
            ),
            Span::raw(format!(
                " {:>7}",
                format_context(entry.model.context_tokens())
            )),
        ]));
    }
    Paragraph::new(lines).render(inner, frame.buffer_mut());
}

fn capability(active: bool, yes: &'static str, color: Color, no: &'static str) -> Span<'static> {
    if active {
        Span::styled(yes, Style::default().fg(color))
    } else {
        Span::styled(no, Style::default().fg(Color::DarkGray))
    }
}

fn render_right(
    frame: &mut Frame<'_>,
    area: Rect,
    pane: &ModelsPane,
    projection: &CatalogProjection,
) {
    let selected = pane.current_model(projection);
    let provider = selected
        .and_then(|entry| pane.snapshot.provider(&entry.provider_id))
        .or_else(|| {
            pane.selected_provider_id
                .as_deref()
                .and_then(|id| pane.snapshot.provider(id))
        });
    let provider_height = provider.map_or(4, |entry| {
        let width = area.width.saturating_sub(2).max(1) as usize;
        let text = format!(
            "{}\nCategory: {}\nDocs: {}\nAPI: {}\nEnv: {}",
            entry.provider.name,
            provider_category(&entry.id).label(),
            entry.provider.doc.as_deref().unwrap_or(EM_DASH),
            entry.provider.api.as_deref().unwrap_or(EM_DASH),
            if entry.provider.env.is_empty() {
                EM_DASH.to_owned()
            } else {
                entry.provider.env.join(", ")
            },
        );
        text.lines()
            .map(|line| line.chars().count().div_ceil(width).max(1))
            .sum::<usize>()
            .saturating_add(2)
            .min(area.height.saturating_sub(3) as usize)
            .max(4) as u16
    });
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(provider_height), Constraint::Min(3)])
        .split(area);
    render_provider_card(frame, split[0], provider);
    render_model_detail(frame, split[1], pane, selected);
}

fn render_provider_card(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: Option<&crate::models_model::ProviderEntry>,
) {
    let block = Block::default()
        .title(" Provider ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    let lines = match entry {
        Some(entry) => provider_lines(&entry.id, &entry.provider),
        None => vec![Line::from(Span::styled(
            "No provider selected",
            Style::default().fg(Color::DarkGray),
        ))],
    };
    Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .render(inner, frame.buffer_mut());
}

fn provider_lines(id: &str, provider: &Provider) -> Vec<Line<'static>> {
    let category = provider_category(id);
    vec![
        Line::from(Span::styled(
            provider.name.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        label_value("Category: ", category.label(), category_color(category)),
        label_value(
            "Docs: ",
            provider.doc.as_deref().unwrap_or(EM_DASH),
            Color::White,
        ),
        label_value(
            "API:  ",
            provider.api.as_deref().unwrap_or(EM_DASH),
            Color::White,
        ),
        label_value(
            "Env:  ",
            &if provider.env.is_empty() {
                EM_DASH.to_owned()
            } else {
                provider.env.join(", ")
            },
            Color::White,
        ),
    ]
}

fn render_model_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    pane: &ModelsPane,
    entry: Option<&ModelEntry>,
) {
    let block = Block::default()
        .title(" Details ")
        .borders(Borders::ALL)
        .border_style(focus_border(pane.focus == Focus::Details));
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    let lines = entry.map_or_else(
        || {
            vec![Line::from(Span::styled(
                "No model selected",
                Style::default().fg(Color::DarkGray),
            ))]
        },
        |entry| model_detail_lines(&entry.model, inner.width),
    );
    let max_scroll = lines.len().saturating_sub(inner.height as usize) as u16;
    let scroll = pane.detail_scroll.min(max_scroll);
    Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .scroll((scroll, 0))
        .render(inner, frame.buffer_mut());
}

fn model_detail_lines(model: &Model, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            model.name.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            model.id.clone(),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(vec![
            Span::styled("Family: ", Style::default().fg(Color::Gray)),
            Span::raw(model.family.clone().unwrap_or_else(|| EM_DASH.into())),
            Span::raw("  "),
            Span::styled("Status: ", Style::default().fg(Color::Gray)),
            Span::styled(
                model.status.clone().unwrap_or_else(|| "active".into()),
                Style::default().fg(if model.status.as_deref() == Some("deprecated") {
                    Color::Red
                } else {
                    Color::Green
                }),
            ),
        ]),
    ];
    if let Some(description) = model.description.as_deref().filter(|text| !text.is_empty()) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            description.to_owned(),
            Style::default().fg(Color::Gray),
        )));
    }
    lines.push(section_header(width, "Capabilities"));
    lines.push(two_pair(
        "Reasoning",
        yes_no(model.reasoning),
        "Tools",
        yes_no(model.tool_call),
    ));
    lines.push(two_pair(
        "Source",
        if model.open_weights { "Open" } else { "Closed" },
        "Files",
        yes_no(model.attachment),
    ));
    lines.push(two_pair(
        "Temp",
        yes_no(model.temperature),
        "Structured",
        optional_yes_no(model.structured_output),
    ));
    for option in &model.reasoning_options {
        let label = option.r#type.as_deref().unwrap_or("reasoning");
        let value = if !option.values.is_empty() {
            option
                .values
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        } else if option.min.is_some() || option.max.is_some() {
            format!(
                "{}–{}",
                option
                    .min
                    .map_or_else(|| EM_DASH.into(), |v| format_context(Some(v as u64))),
                option
                    .max
                    .map_or_else(|| EM_DASH.into(), |v| format_context(Some(v as u64)))
            )
        } else {
            "Yes".into()
        };
        lines.push(label_value(
            &format!("{}: ", title_case(label)),
            &value,
            Color::LightGreen,
        ));
    }
    lines.push(section_header(width, "Pricing"));
    if let Some(cost) = &model.cost {
        lines.push(two_pair(
            "Input",
            &price(cost.input),
            "Output",
            &price(cost.output),
        ));
        lines.push(two_pair(
            "Cache Read",
            &price(cost.cache_read),
            "Cache Write",
            &price(cost.cache_write),
        ));
        if cost.reasoning.is_some() {
            lines.push(label_value(
                "Thinking: ",
                &price(cost.reasoning),
                Color::White,
            ));
        }
        if cost.input_audio.is_some() || cost.output_audio.is_some() {
            lines.push(two_pair(
                "Audio In",
                &price(cost.input_audio),
                "Audio Out",
                &price(cost.output_audio),
            ));
        }
        for tier in &cost.tiers {
            let label = tier.tier.as_ref().and_then(|spec| spec.size).map_or_else(
                || "Tier".into(),
                |size| format!("Over {}", format_context(Some(size))),
            );
            lines.push(label_value(
                &format!("{label}: "),
                &format!("{} / {}", price(tier.input), price(tier.output)),
                Color::White,
            ));
        }
    } else {
        lines.push(Line::from(EM_DASH));
    }
    lines.push(section_header(width, "Limits"));
    let limits = model.limit.as_ref();
    lines.push(Line::from(format!(
        "Context: {}   Input: {}   Output: {}",
        format_context(limits.and_then(|limit| limit.context)),
        format_context(limits.and_then(|limit| limit.input)),
        format_context(limits.and_then(|limit| limit.output)),
    )));
    lines.push(section_header(width, "Modalities"));
    if let Some(modalities) = &model.modalities {
        lines.push(label_value(
            "Input:  ",
            &list_or_dash(&modalities.input),
            Color::White,
        ));
        lines.push(label_value(
            "Output: ",
            &list_or_dash(&modalities.output),
            Color::White,
        ));
    } else {
        lines.push(Line::from("Input: text   Output: text"));
    }
    lines.push(section_header(width, "Dates"));
    lines.push(two_pair(
        "Released",
        model.release_date.as_deref().unwrap_or(EM_DASH),
        "Knowledge",
        model.knowledge.as_deref().unwrap_or(EM_DASH),
    ));
    if model.last_updated.is_some() {
        lines.push(label_value(
            "Updated: ",
            model.last_updated.as_deref().unwrap_or(EM_DASH),
            Color::White,
        ));
    }
    lines
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, pane: &ModelsPane) {
    let (text, color) = match &pane.modal {
        Modal::Search { input } => (format!("/ {input}_"), Color::Cyan),
        Modal::None => {
            if let Some(hint) = &pane.hint {
                (hint.clone(), Color::Yellow)
            } else if let Some(error) = &pane.snapshot.last_error {
                (
                    format!("error: {error} · press r refresh to retry"),
                    Color::Red,
                )
            } else {
                (
                    "h/l focus  j/k nav  / search  s/S sort  1-6 filter  r refresh".into(),
                    Color::DarkGray,
                )
            }
        }
    };
    Paragraph::new(Line::styled(text, Style::default().fg(color))).render(area, frame.buffer_mut());
}

fn label_value(label: &str, value: &str, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(label.to_owned(), Style::default().fg(Color::Gray)),
        Span::styled(value.to_owned(), Style::default().fg(color)),
    ])
}

fn two_pair(left_label: &str, left: &str, right_label: &str, right: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{left_label}: "), Style::default().fg(Color::Gray)),
        Span::raw(format!("{left:<10}")),
        Span::styled(format!("{right_label}: "), Style::default().fg(Color::Gray)),
        Span::raw(right.to_owned()),
    ])
}

fn section_header(width: u16, title: &str) -> Line<'static> {
    let prefix = format!("── {title} ");
    let fill = "─".repeat((width as usize).saturating_sub(prefix.chars().count()));
    Line::from(Span::styled(
        format!("\n{prefix}{fill}"),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    ))
}

fn yes_no(value: bool) -> &'static str {
    if value { "Yes" } else { "No" }
}

fn optional_yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "Yes",
        Some(false) => "No",
        None => EM_DASH,
    }
}

fn price(value: Option<f64>) -> String {
    value.map_or_else(
        || EM_DASH.into(),
        |value| format!("{}/M", format_cost_short(Some(value))),
    )
}

fn list_or_dash(values: &[String]) -> String {
    if values.is_empty() {
        EM_DASH.into()
    } else {
        values.join(", ")
    }
}

fn title_case(value: &str) -> String {
    value
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn cost_style(cost: Option<f64>) -> Style {
    match cost {
        Some(0.0) => Style::default().fg(Color::Green),
        Some(value) if value < 10.0 => Style::default().fg(Color::White),
        Some(_) => Style::default().fg(Color::Yellow),
        None => Style::default().fg(Color::DarkGray),
    }
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let mut output: String = value.chars().take(width - 1).collect();
    output.push('…');
    output
}

fn window_around(cursor: usize, total: usize, viewport: usize) -> (usize, usize) {
    if total == 0 || viewport == 0 {
        return (0, 0);
    }
    if total <= viewport {
        return (0, total);
    }
    let end = (cursor.saturating_sub(viewport / 2) + viewport).min(total);
    (end.saturating_sub(viewport), end)
}

fn category_key(category: ProviderCategory) -> &'static str {
    match category {
        ProviderCategory::All => "all",
        ProviderCategory::Origin => "origin",
        ProviderCategory::Cloud => "cloud",
        ProviderCategory::Inference => "inference",
        ProviderCategory::Gateway => "gateway",
        ProviderCategory::Tool => "tool",
    }
}

fn parse_category(value: Option<&str>) -> ProviderCategory {
    match value {
        Some("origin") => ProviderCategory::Origin,
        Some("cloud") => ProviderCategory::Cloud,
        Some("inference") => ProviderCategory::Inference,
        Some("gateway") => ProviderCategory::Gateway,
        Some("tool") => ProviderCategory::Tool,
        _ => ProviderCategory::All,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn keymap_cycles_focus_filters_and_sort() {
        let mut pane = ModelsPane::new();
        assert!(pane.on_key(key(KeyCode::Char('l'))));
        assert_eq!(pane.focus, Focus::Models);
        assert!(pane.on_key(key(KeyCode::Char('1'))));
        assert!(pane.filters.reasoning);
        let previous = pane.sort_key;
        assert!(pane.on_key(key(KeyCode::Char('s'))));
        assert_ne!(pane.sort_key, previous);
        assert!(pane.on_key(key(KeyCode::Char('S'))));
        assert_eq!(pane.sort_order, SortOrder::Ascending);
    }

    #[test]
    fn search_commits_only_on_enter() {
        let mut pane = ModelsPane::new();
        pane.on_key(key(KeyCode::Char('/')));
        pane.on_key(key(KeyCode::Char('g')));
        pane.on_key(key(KeyCode::Char('p')));
        assert!(pane.search_query.is_empty());
        pane.on_key(key(KeyCode::Enter));
        assert_eq!(pane.search_query, "gp");
    }

    #[test]
    fn stable_state_round_trips_browsing_preferences() {
        let mut pane = ModelsPane::new();
        pane.focus = Focus::Details;
        pane.filters.reasoning = true;
        pane.category_filter = ProviderCategory::Cloud;
        pane.group_by_category = true;
        pane.search_query = "claude".into();
        pane.sort_key = SortKey::Context;
        pane.sort_order = SortOrder::Ascending;
        let state = pane.snapshot_state();
        let mut restored = ModelsPane::new();
        restored.restore_state(&state);
        assert_eq!(restored.focus, Focus::Details);
        assert!(restored.filters.reasoning);
        assert_eq!(restored.category_filter, ProviderCategory::Cloud);
        assert!(restored.group_by_category);
        assert_eq!(restored.search_query, "claude");
        assert_eq!(restored.sort_key, SortKey::Context);
        assert_eq!(restored.sort_order, SortOrder::Ascending);
    }

    #[test]
    fn fetch_error_keeps_last_snapshot_and_hints_refresh() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let json = r#"{
          "openai":{"id":"openai","name":"OpenAI","models":{
            "gpt":{"id":"gpt","name":"GPT","cost":{"input":2,"output":8}}
          }}
        }"#;
        let mut pane = ModelsPane::new();
        pane.snapshot = Snapshot::from_providers(&serde_json::from_str(json).unwrap());
        pane.sync_selection();
        let (worker, response_tx) = ModelsWorker::test_channels();
        pane.worker = worker;
        pane.requested_generation = 2;
        pane.applied_generation = 1;
        pane.fetching = true;
        response_tx
            .send(ModelsResponse::Fetch {
                generation: 2,
                result: Err("timed out reaching models.dev".into()),
            })
            .unwrap();

        assert!(pane.poll_background());
        assert_eq!(pane.snapshot.model_count, 1);

        let mut terminal = Terminal::new(TestBackend::new(160, 44)).unwrap();
        terminal
            .draw(|frame| {
                pane.render(
                    frame.area(),
                    frame,
                    &PaneRenderCtx {
                        focused: true,
                        title_override: None,
                        focus_color: Color::Cyan,
                    },
                );
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("timed out reaching models.dev"), "{text}");
        assert!(text.contains("r refresh"), "{text}");
        assert!(text.contains("GPT"), "{text}");
    }

    #[test]
    fn narrow_helpers_never_underflow() {
        assert_eq!(window_around(0, 0, 0), (0, 0));
        assert_eq!(truncate("abc", 0), "");
        let lines = model_detail_lines(
            &serde_json::from_str::<Model>(r#"{"id":"m","name":"M"}"#).unwrap(),
            0,
        );
        assert!(!lines.is_empty());
    }

    #[test]
    fn render_exposes_three_columns_and_core_hints() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let json = r#"{
          "openai":{"id":"openai","name":"OpenAI","doc":"https://docs.example","api":"https://api.example","env":["OPENAI_API_KEY"],"models":{
            "gpt":{"id":"gpt","name":"GPT","tool_call":true,"attachment":true,"description":"A capable model","cost":{"input":2,"output":8},"limit":{"context":128000}}
          }}
        }"#;
        let mut pane = ModelsPane::new();
        pane.snapshot = Snapshot::from_providers(&serde_json::from_str(json).unwrap());
        pane.sync_selection();
        let backend = TestBackend::new(160, 44);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                pane.render(
                    frame.area(),
                    frame,
                    &PaneRenderCtx {
                        focused: true,
                        title_override: None,
                        focus_color: Color::Cyan,
                    },
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content()
            .chunks(buffer.area.width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Providers"), "{text}");
        assert!(text.contains("RTFO"), "{text}");
        assert!(text.contains("Provider"), "{text}");
        assert!(text.contains("Capabilities"), "{text}");
        assert!(text.contains("h/l focus"), "{text}");
    }
}
