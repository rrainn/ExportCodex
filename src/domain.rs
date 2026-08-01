//! Strong domain types shared by storage, rendering, and CLI layers.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A validated Codex technical thread identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadId(Uuid);

impl ThreadId {
	/// Returns the canonical UUID spelling used by Codex deeplinks.
	#[must_use]
	pub fn as_uuid(self) -> Uuid {
		self.0
	}

	/// Returns the first eight characters for compact filenames and displays.
	#[must_use]
	pub fn short(self) -> String {
		self.0.simple().to_string()[..8].to_owned()
	}
}

impl fmt::Display for ThreadId {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.0.fmt(formatter)
	}
}

impl FromStr for ThreadId {
	type Err = uuid::Error;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		Uuid::parse_str(value).map(Self)
	}
}

/// Metadata used to browse and resolve a locally stored Codex thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSummary {
	/// Stable technical identifier.
	pub id: ThreadId,
	/// User-facing task title.
	pub title: String,
	/// Short searchable preview from Codex's catalog.
	pub preview: String,
	/// Workspace active when the task was created.
	pub cwd: PathBuf,
	/// Rollout file containing the transcript.
	pub rollout_path: PathBuf,
	/// Creation time when the catalog provides it.
	pub created_at: Option<DateTime<Utc>>,
	/// Most recent activity time when the catalog provides it.
	pub updated_at: Option<DateTime<Utc>>,
	/// Whether Codex has archived this task.
	pub archived: bool,
	/// Whether this task belongs to a spawned agent rather than the user.
	pub is_agent: bool,
	/// Storage history representation reported by Codex.
	pub history_mode: String,
	/// Model recorded for the most recent turn, when available.
	pub model: Option<String>,
}

impl ThreadSummary {
	/// Returns a clean fallback title when an older catalog row has none.
	#[must_use]
	pub fn display_title(&self) -> &str {
		let title = self.title.trim();
		if title.is_empty() {
			"Untitled chat"
		} else {
			title
		}
	}
}

/// The visible author of one transcript entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
	/// A prompt written by the user.
	User,
	/// A response written by Codex.
	Assistant,
}

/// The presentation phase attached to a visible assistant message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessagePhase {
	/// A user-authored prompt.
	User,
	/// A progress update emitted while Codex is working.
	Commentary,
	/// The final answer for a turn.
	FinalAnswer,
	/// An older visible assistant message with no recorded phase.
	Legacy,
}

/// One user-visible message in chronological order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
	/// Visible author.
	pub role: MessageRole,
	/// UI phase used to style progress separately from final answers.
	pub phase: MessagePhase,
	/// Markdown-capable message body.
	pub text: String,
	/// Event timestamp, when present in the rollout.
	pub timestamp: Option<DateTime<Utc>>,
}

/// A complete normalized chat ready for any output renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chat {
	/// Catalog metadata for the selected task.
	pub thread: ThreadSummary,
	/// Safe, user-visible messages only.
	pub messages: Vec<ChatMessage>,
	/// Non-fatal compatibility or attachment warnings.
	pub warnings: Vec<String>,
}

/// Output artifact type requested by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
	/// Portable Markdown transcript.
	#[value(alias = "md")]
	Markdown,
	/// Self-contained, print-ready HTML document.
	Html,
	/// Selectable-text PDF document.
	Pdf,
	/// Produce Markdown, HTML, and PDF together.
	All,
}

impl OutputFormat {
	/// Returns the extension used for generated files.
	#[must_use]
	pub const fn extension(self) -> Option<&'static str> {
		match self {
			Self::Markdown => Some("md"),
			Self::Html => Some("html"),
			Self::Pdf => Some("pdf"),
			Self::All => None,
		}
	}

	/// Expands `all` into concrete artifact formats.
	#[must_use]
	pub fn concrete(self) -> Vec<Self> {
		match self {
			Self::All => vec![Self::Markdown, Self::Html, Self::Pdf],
			format => vec![format],
		}
	}
}

/// Built-in visual identity applied consistently across formats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
	/// Warm, restrained typography optimized for printing.
	#[default]
	Paper,
	/// Soft ivory and forest tones for long-form reading.
	Warm,
	/// Neutral grayscale with compact technical typography.
	Mono,
}

/// Standard page sizes supported by the PDF renderer and print CSS.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaperSize {
	/// North American letter paper.
	#[default]
	Letter,
	/// International A4 paper.
	A4,
}

/// Readability-focused margin presets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarginSize {
	/// More content per page while retaining print safety.
	Compact,
	/// Balanced default for screens and paper.
	#[default]
	Normal,
	/// Generous long-form reading margins.
	Wide,
}

/// Shared rendering preferences after config and CLI overrides are merged.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderOptions {
	/// Optional title used in place of the Codex task name.
	pub title: Option<String>,
	/// Visual theme.
	pub theme: ThemeName,
	/// Optional validated CSS color override.
	pub accent: Option<String>,
	/// PDF and print-CSS page size.
	pub paper_size: PaperSize,
	/// Page margin density.
	pub margins: MarginSize,
	/// Base body size in points.
	pub font_size: f32,
	/// Include event timestamps beside role labels.
	pub include_timestamps: bool,
	/// Include intermediate assistant progress messages.
	pub include_progress: bool,
	/// Include compact chat metadata on the cover.
	pub include_metadata: bool,
	/// Optional user-supplied CSS appended to self-contained HTML.
	pub custom_css: Option<String>,
}

impl Default for RenderOptions {
	fn default() -> Self {
		Self {
			title: None,
			theme: ThemeName::Paper,
			accent: None,
			paper_size: PaperSize::Letter,
			margins: MarginSize::Normal,
			font_size: 11.0,
			include_timestamps: false,
			include_progress: true,
			include_metadata: true,
			custom_css: None,
		}
	}
}
