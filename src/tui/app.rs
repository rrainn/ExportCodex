//! State reducer and terminal rendering for the interactive task browser.

use std::io::{self, Stdout};

use anyhow::{Context, Result};
use chrono::Local;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
	EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, symbols};

use crate::domain::ThreadSummary;

/// Initial filters chosen by browser command flags.
#[derive(Debug, Clone, Copy, Default)]
pub struct BrowserOptions {
	/// Open directly on the archived view.
	pub archived: bool,
	/// Include spawned agent tasks immediately.
	pub include_agents: bool,
}

/// Opens a full-screen browser and returns the chosen thread.
pub fn select_thread(
	threads: Vec<ThreadSummary>,
	options: BrowserOptions,
) -> Result<Option<ThreadSummary>> {
	let mut terminal = TerminalSession::start()?;
	let mut app = BrowserApp::new(threads, options);

	loop {
		terminal
			.terminal
			.draw(|frame| draw(frame, &mut app))
			.context("could not draw the Codex task browser")?;

		if let Event::Key(key) = event::read().context("could not read terminal input")? {
			match app.handle_key(key) {
				BrowserAction::Continue => {}
				BrowserAction::Cancel => return Ok(None),
				BrowserAction::Select => return Ok(app.selected_thread().cloned()),
			}
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveView {
	Active,
	Archived,
	All,
}

impl ArchiveView {
	fn next(self) -> Self {
		match self {
			Self::Active => Self::Archived,
			Self::Archived => Self::All,
			Self::All => Self::Active,
		}
	}

	const fn label(self) -> &'static str {
		match self {
			Self::Active => "Active",
			Self::Archived => "Archived",
			Self::All => "All",
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserAction {
	Continue,
	Cancel,
	Select,
}

#[derive(Debug)]
struct BrowserApp {
	threads: Vec<ThreadSummary>,
	archive_view: ArchiveView,
	include_agents: bool,
	query: String,
	searching: bool,
	selected: usize,
	list_state: ListState,
}

impl BrowserApp {
	fn new(threads: Vec<ThreadSummary>, options: BrowserOptions) -> Self {
		let archive_view = if options.archived {
			ArchiveView::Archived
		} else {
			ArchiveView::Active
		};
		let mut state = Self {
			threads,
			archive_view,
			include_agents: options.include_agents,
			query: String::new(),
			searching: false,
			selected: 0,
			list_state: ListState::default(),
		};
		state.sync_selection();
		state
	}

	fn visible_indices(&self) -> Vec<usize> {
		let query = self.query.to_lowercase();
		self.threads
			.iter()
			.enumerate()
			.filter(|(_, thread)| {
				let archive_matches = match self.archive_view {
					ArchiveView::Active => !thread.archived,
					ArchiveView::Archived => thread.archived,
					ArchiveView::All => true,
				};
				let agent_matches = self.include_agents || !thread.is_agent;
				archive_matches && agent_matches && thread_matches(thread, &query)
			})
			.map(|(index, _)| index)
			.collect()
	}

	fn selected_thread(&self) -> Option<&ThreadSummary> {
		let indices = self.visible_indices();
		indices
			.get(self.selected)
			.and_then(|index| self.threads.get(*index))
	}

	fn sync_selection(&mut self) {
		let count = self.visible_indices().len();
		self.selected = self.selected.min(count.saturating_sub(1));
		self.list_state.select((count > 0).then_some(self.selected));
	}

	fn move_selection(&mut self, delta: isize) {
		let count = self.visible_indices().len();
		if count == 0 {
			return;
		}
		self.selected = self.selected.saturating_add_signed(delta).min(count - 1);
		self.sync_selection();
	}

	fn handle_key(&mut self, key: KeyEvent) -> BrowserAction {
		if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
			return BrowserAction::Cancel;
		}

		if self.searching {
			match key.code {
				KeyCode::Esc | KeyCode::Enter => self.searching = false,
				KeyCode::Backspace => {
					self.query.pop();
					self.selected = 0;
				}
				KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
					self.query.push(character);
					self.selected = 0;
				}
				_ => {}
			}
			self.sync_selection();
			return BrowserAction::Continue;
		}

		match key.code {
			KeyCode::Char('q') | KeyCode::Esc => BrowserAction::Cancel,
			KeyCode::Char('/') => {
				self.searching = true;
				BrowserAction::Continue
			}
			KeyCode::Char('a') => {
				self.archive_view = self.archive_view.next();
				self.selected = 0;
				self.sync_selection();
				BrowserAction::Continue
			}
			KeyCode::Char('g') => {
				self.include_agents = !self.include_agents;
				self.selected = 0;
				self.sync_selection();
				BrowserAction::Continue
			}
			KeyCode::Down | KeyCode::Char('j') => {
				self.move_selection(1);
				BrowserAction::Continue
			}
			KeyCode::Up | KeyCode::Char('k') => {
				self.move_selection(-1);
				BrowserAction::Continue
			}
			KeyCode::PageDown => {
				self.move_selection(10);
				BrowserAction::Continue
			}
			KeyCode::PageUp => {
				self.move_selection(-10);
				BrowserAction::Continue
			}
			KeyCode::Enter if self.selected_thread().is_some() => BrowserAction::Select,
			_ => BrowserAction::Continue,
		}
	}
}

fn thread_matches(thread: &ThreadSummary, query: &str) -> bool {
	if query.is_empty() {
		return true;
	}
	[
		thread.display_title().to_owned(),
		thread.preview.clone(),
		thread.cwd.display().to_string(),
		thread.id.to_string(),
	]
	.into_iter()
	.any(|candidate| candidate.to_lowercase().contains(query))
}

fn draw(frame: &mut Frame<'_>, app: &mut BrowserApp) {
	let outer = Layout::default()
		.direction(Direction::Vertical)
		.constraints([
			Constraint::Length(3),
			Constraint::Min(8),
			Constraint::Length(2),
		])
		.split(frame.area());
	draw_search(frame, outer[0], app);

	let body = Layout::default()
		.direction(Direction::Horizontal)
		.constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
		.split(outer[1]);
	draw_list(frame, body[0], app);
	draw_preview(frame, body[1], app);
	draw_footer(frame, outer[2], app);
}

fn draw_search(frame: &mut Frame<'_>, area: Rect, app: &BrowserApp) {
	let prompt = if app.query.is_empty() && !app.searching {
		"Press / to search titles, previews, workspaces, or IDs"
	} else {
		&app.query
	};
	let border = if app.searching {
		Color::Cyan
	} else {
		Color::DarkGray
	};
	let search = Paragraph::new(prompt)
		.style(Style::default().fg(if app.query.is_empty() {
			Color::DarkGray
		} else {
			Color::White
		}))
		.block(
			Block::default()
				.borders(Borders::ALL)
				.border_style(Style::default().fg(border))
				.title(" Search "),
		);
	frame.render_widget(search, area);
}

fn draw_list(frame: &mut Frame<'_>, area: Rect, app: &mut BrowserApp) {
	let indices = app.visible_indices();
	let items = indices
		.iter()
		.filter_map(|index| app.threads.get(*index))
		.map(|thread| {
			let date = thread
				.updated_at
				.map(|value| value.with_timezone(&Local).format("%b %d").to_string())
				.unwrap_or_else(|| "Unknown date".to_owned());
			let workspace = thread
				.cwd
				.file_name()
				.and_then(|name| name.to_str())
				.unwrap_or("No workspace");
			let agent = if thread.is_agent { " · agent" } else { "" };
			ListItem::new(vec![
				Line::from(Span::styled(
					thread.display_title(),
					Style::default()
						.fg(Color::White)
						.add_modifier(Modifier::BOLD),
				)),
				Line::from(Span::styled(
					format!("{date} · {workspace}{agent}"),
					Style::default().fg(Color::DarkGray),
				)),
			])
		})
		.collect::<Vec<_>>();

	let title = format!(
		" {} · {} tasks{} ",
		app.archive_view.label(),
		items.len(),
		if app.include_agents {
			" · agents shown"
		} else {
			""
		}
	);
	let list = List::new(items)
		.block(
			Block::default()
				.borders(Borders::ALL)
				.border_set(symbols::border::ROUNDED)
				.title(title),
		)
		.highlight_style(Style::default().bg(Color::Rgb(35, 45, 58)).fg(Color::White))
		.highlight_symbol("▌ ");
	frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_preview(frame: &mut Frame<'_>, area: Rect, app: &BrowserApp) {
	let Some(thread) = app.selected_thread() else {
		let empty = Paragraph::new("No tasks match these filters.\n\nPress a for Active / Archived / All, g for agent tasks, or / to change the search.")
			.style(Style::default().fg(Color::DarkGray))
			.wrap(Wrap { trim: false })
			.block(Block::default().borders(Borders::ALL).title(" Preview "));
		frame.render_widget(empty, area);
		return;
	};

	let state = if thread.archived {
		"Archived"
	} else {
		"Active"
	};
	let model = thread.model.as_deref().unwrap_or("Not recorded");
	let text = vec![
		Line::from(Span::styled(
			thread.display_title(),
			Style::default()
				.fg(Color::White)
				.add_modifier(Modifier::BOLD),
		)),
		Line::from(""),
		Line::from(Span::styled(
			&thread.preview,
			Style::default().fg(Color::Gray),
		)),
		Line::from(""),
		Line::from(vec![
			Span::styled("State  ", Style::default().fg(Color::DarkGray)),
			Span::raw(state),
		]),
		Line::from(vec![
			Span::styled("Model  ", Style::default().fg(Color::DarkGray)),
			Span::raw(model),
		]),
		Line::from(vec![
			Span::styled("ID     ", Style::default().fg(Color::DarkGray)),
			Span::raw(thread.id.to_string()),
		]),
		Line::from(vec![
			Span::styled("Folder ", Style::default().fg(Color::DarkGray)),
			Span::raw(thread.cwd.display().to_string()),
		]),
	];
	let preview = Paragraph::new(text).wrap(Wrap { trim: false }).block(
		Block::default()
			.borders(Borders::ALL)
			.border_set(symbols::border::ROUNDED)
			.title(" Preview "),
	);
	frame.render_widget(preview, area);
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, app: &BrowserApp) {
	let help = if app.searching {
		"Type to filter · Enter/Esc finish search"
	} else {
		"↑↓/jk move · Enter export · / search · a archive view · g agents · q quit"
	};
	frame.render_widget(
		Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
		area,
	);
}

struct TerminalSession {
	terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
	fn start() -> Result<Self> {
		enable_raw_mode().context("could not enable terminal raw mode")?;
		let mut stdout = io::stdout();
		if let Err(error) = execute!(stdout, EnterAlternateScreen) {
			let _ = disable_raw_mode();
			return Err(error).context("could not open the alternate terminal screen");
		}
		let backend = CrosstermBackend::new(stdout);
		let terminal = Terminal::new(backend).context("could not initialize terminal UI")?;
		Ok(Self { terminal })
	}
}

impl Drop for TerminalSession {
	fn drop(&mut self) {
		let _ = disable_raw_mode();
		let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
		let _ = self.terminal.show_cursor();
	}
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;
	use std::str::FromStr;

	use crate::domain::ThreadId;

	use super::*;

	fn thread(id: &str, title: &str, archived: bool, is_agent: bool) -> ThreadSummary {
		ThreadSummary {
			id: ThreadId::from_str(id).expect("fixture ID should parse"),
			title: title.to_owned(),
			preview: format!("Preview for {title}"),
			cwd: PathBuf::from("/workspace/export"),
			rollout_path: PathBuf::from("rollout.jsonl"),
			created_at: None,
			updated_at: None,
			archived,
			is_agent,
			history_mode: "legacy".to_owned(),
			model: None,
		}
	}

	fn app() -> BrowserApp {
		BrowserApp::new(
			vec![
				thread(
					"019f874c-1e53-7d12-97db-fac3249ce327",
					"Active chat",
					false,
					false,
				),
				thread(
					"019f874c-1e53-7d12-97db-fac3249ce328",
					"Old chat",
					true,
					false,
				),
				thread(
					"019f874c-1e53-7d12-97db-fac3249ce329",
					"Agent task",
					false,
					true,
				),
			],
			BrowserOptions::default(),
		)
	}

	#[test]
	fn clean_initial_view_hides_archived_and_agents() {
		let app = app();
		let visible = app.visible_indices();
		assert_eq!(visible, vec![0]);
	}

	#[test]
	fn toggles_make_hidden_tasks_intentionally_discoverable() {
		let mut app = app();
		app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
		assert_eq!(app.visible_indices(), vec![1]);
		app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
		app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
		assert_eq!(app.visible_indices(), vec![0, 1, 2]);
	}

	#[test]
	fn search_matches_id_and_preview_without_losing_selection_safety() {
		let mut app = app();
		app.archive_view = ArchiveView::All;
		app.query = "old chat".to_owned();
		app.sync_selection();
		assert_eq!(
			app.selected_thread().map(|item| item.title.as_str()),
			Some("Old chat")
		);
	}
}
