//! Offline selectable-text PDF rendering through `markdown2pdf`.

use std::error::Error;
use std::fmt;
use std::io;

use markdown2pdf::MdpError;
use markdown2pdf::config::{ConfigSource, load_config_strict};

use crate::domain::{Chat, RenderOptions, ThemeName};

use super::markdown;
use super::theme::{accent, font_size, margin_mm, palette, paper_pdf, title};

/// A failure while preparing or generating a PDF document.
#[derive(Debug)]
pub enum PdfRenderError {
	/// The temporary empty asset root could not be created.
	AssetSandbox(io::Error),
	/// The embedded renderer configuration was rejected.
	Configuration(String),
	/// Markdown parsing or PDF generation failed.
	Rendering(MdpError),
}

impl fmt::Display for PdfRenderError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::AssetSandbox(error) => {
				write!(
					formatter,
					"could not create an isolated PDF asset directory: {error}"
				)
			}
			Self::Configuration(error) => {
				write!(formatter, "the built-in PDF theme is invalid: {error}")
			}
			Self::Rendering(error) => error.fmt(formatter),
		}
	}
}

impl Error for PdfRenderError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::AssetSandbox(error) => Some(error),
			Self::Rendering(error) => Some(error),
			Self::Configuration(_) => None,
		}
	}
}

/// Renders a chat to a PDF byte buffer without network access.
///
/// # Errors
///
/// Returns [`PdfRenderError`] when the isolated asset directory, theme
/// configuration, Markdown parser, font loader, or PDF generator fails.
pub fn render(chat: &Chat, options: &RenderOptions) -> Result<Vec<u8>, PdfRenderError> {
	// An empty root prevents untrusted message Markdown from embedding host files.
	let asset_root = tempfile::tempdir().map_err(PdfRenderError::AssetSandbox)?;
	let config = configuration(chat, options, asset_root.path());
	let style = load_config_strict(ConfigSource::Embedded(&config), None)
		.map_err(|error| PdfRenderError::Configuration(error.to_string()))?;
	let markdown = super::safety::pdf_document(&markdown::render(chat, options));

	markdown2pdf::parse_into_bytes_with_style(markdown, style, None)
		.map_err(PdfRenderError::Rendering)
}

fn configuration(chat: &Chat, options: &RenderOptions, asset_root: &std::path::Path) -> String {
	let colors = palette(options.theme);
	let accent = accent(options);
	let body_size = font_size(options);
	let margin = margin_mm(options.margins);
	let page_size = paper_pdf(options.paper_size);
	let document_title = title(chat, options);
	let title = toml_string(&document_title);
	let image_root = toml_string(&asset_root.to_string_lossy());
	let link_underline = matches!(options.theme, ThemeName::Mono);

	format!(
		r##"theme = "minimal"

[page]
size = "{page_size}"
orientation = "portrait"
margins = {{ top = {margin:.1}, right = {margin:.1}, bottom = {margin:.1}, left = {margin:.1} }}
columns = 1

[defaults]
font_size_pt = {body_size:.2}
line_height = 1.52
text_color = "{text}"

[paragraph]
margin_after_pt = 6.0

[headings.h1]
font_size_pt = 23.0
font_weight = "bold"
text_color = "{text}"
line_height = 1.12
text_align = "left"
margin_before_pt = 4.0
margin_after_pt = 10.0

[headings.h2]
font_size_pt = 10.0
font_weight = "bold"
text_color = "{accent}"
margin_before_pt = 14.0
margin_after_pt = 5.0

[headings.h3]
font_size_pt = 13.0
font_weight = "bold"
text_color = "{text}"
margin_before_pt = 9.0
margin_after_pt = 4.0

[code_block]
font_size_pt = 8.7
line_height = 1.42
background_color = "{code_surface}"
text_color = "{text}"
padding = {{ top = 7.0, right = 9.0, bottom = 7.0, left = 9.0 }}
margin_before_pt = 6.0
margin_after_pt = 7.0

[code_block.border.all]
width_pt = 0.5
color = "{border}"
style = "solid"

[code_inline]
background_color = "{code_surface}"
text_color = "{text}"

[blockquote]
font_style = "normal"
text_color = "{muted}"
background_color = "{surface}"
padding = {{ top = 3.0, right = 5.0, bottom = 3.0, left = 9.0 }}
margin_before_pt = 5.0
margin_after_pt = 6.0

[blockquote.border.left]
width_pt = 2.0
color = "{accent}"
style = "solid"

[table]
row_gap_pt = 2.0
cell_padding = {{ top = 3.5, right = 4.5, bottom = 3.5, left = 4.5 }}
margin_before_pt = 7.0
margin_after_pt = 7.0
alternating_row_background = "{surface}"

[table.header]
font_weight = "bold"
background_color = "{code_surface}"

[table.border.all]
width_pt = 0.5
color = "{border}"
style = "solid"

[link]
text_color = "{accent}"
underline = {link_underline}

[horizontal_rule]
color = "{border}"
thickness_pt = 0.5
style = "solid"
width_pct = 100.0
margin_before_pt = 8.0
margin_after_pt = 8.0

[metadata]
title = {title}
subject = "Exported Codex chat"
keywords = ["Codex", "chat", "export"]
creator = "ExportCodex"

[header]
left = "{{title}}"
show_on_first_page = false
gap_pt = 12.0

[header.style]
font_size_pt = 7.5
text_color = "{muted}"

[footer]
left = "Exported from Codex"
right = "{{page}} / {{total_pages}}"
show_on_first_page = true
gap_pt = 12.0

[footer.style]
font_size_pt = 7.5
text_color = "{muted}"

[toc]
enabled = false
max_depth = 2

[security]
image_root = {image_root}
allow_absolute_image_paths = false
allow_remote_images = false
"##,
		text = colors.text,
		muted = colors.muted,
		surface = colors.surface,
		code_surface = colors.code_surface,
		border = colors.border,
	)
}

fn toml_string(value: &str) -> String {
	toml::Value::String(value.to_owned()).to_string()
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;
	use std::str::FromStr;

	use pulldown_cmark::{Event, Parser, Tag};

	use crate::domain::{
		ChatMessage, MarginSize, MessagePhase, MessageRole, PaperSize, ThreadId, ThreadSummary,
	};

	use super::*;

	fn sample_chat() -> Chat {
		Chat {
			thread: ThreadSummary {
				id: ThreadId::from_str("019f874c-1e53-7d12-97db-fac3249ce327").unwrap(),
				title: "Selectable transcript".to_owned(),
				preview: String::new(),
				cwd: PathBuf::from("/tmp/ExportCodex"),
				rollout_path: PathBuf::from("session.jsonl"),
				created_at: None,
				updated_at: None,
				archived: false,
				is_agent: false,
				history_mode: "legacy".to_owned(),
				model: None,
			},
			messages: vec![ChatMessage {
				role: MessageRole::Assistant,
				phase: MessagePhase::FinalAnswer,
				text: "Copy this text from the generated PDF.".to_owned(),
				timestamp: None,
			}],
			warnings: Vec::new(),
		}
	}

	#[test]
	fn configuration_honors_page_margins_and_blocks_external_images() {
		let options = RenderOptions {
			paper_size: PaperSize::A4,
			margins: MarginSize::Wide,
			..RenderOptions::default()
		};
		let config = configuration(&sample_chat(), &options, std::path::Path::new("/tmp/empty"));

		assert!(config.contains("size = \"A4\""));
		assert!(config.contains("top = 25.0"));
		assert!(config.contains("allow_remote_images = false"));
		assert!(config.contains("allow_absolute_image_paths = false"));
		assert!(config.contains("right = \"{page} / {total_pages}\""));
	}

	#[test]
	fn generated_document_has_a_pdf_header_and_title_metadata() {
		let bytes = render(&sample_chat(), &RenderOptions::default()).expect("render PDF");
		let document = lopdf::Document::load_mem(&bytes).expect("load generated PDF");
		let pages = document.get_pages().keys().copied().collect::<Vec<_>>();
		let text = document
			.extract_text(&pages)
			.expect("extract selectable PDF text");

		assert!(bytes.starts_with(b"%PDF-"));
		assert!(!pages.is_empty());
		assert!(text.contains("Selectable transcript"));
		assert!(text.contains("Workspace: ExportCodex"));
		assert!(text.contains("Copy this text from the generated PDF."));
	}

	#[test]
	fn error_type_is_compatible_with_anyhow() {
		fn assert_error<T: Error + Send + Sync>() {}
		assert_error::<PdfRenderError>();
	}

	#[test]
	fn pdf_input_keeps_safe_links_and_neutralizes_unsafe_uris() {
		let input = "[safe](https://example.com) [bad](javascript:alert(1)) <script>hidden-looking text</script>";
		let sanitized = super::super::safety::pdf_document(input);
		let destinations = Parser::new(&sanitized)
			.filter_map(|event| match event {
				Event::Start(Tag::Link { dest_url, .. }) => Some(dest_url.into_string()),
				_ => None,
			})
			.collect::<Vec<_>>();

		assert_eq!(destinations, ["https://example.com"]);
		assert!(sanitized.contains("&lt;script&gt;hidden-looking text&lt;/script&gt;"));
	}

	#[test]
	fn authored_math_remains_literal_selectable_text() {
		let mut chat = sample_chat();
		chat.messages[0].text = "Inline $a^2+b^2=c^2$ and display math:\n\n$$E=mc^2$$".to_owned();
		let bytes = render(&chat, &RenderOptions::default()).expect("render PDF");
		let document = lopdf::Document::load_mem(&bytes).expect("load generated PDF");
		let pages = document.get_pages().keys().copied().collect::<Vec<_>>();
		let text = document
			.extract_text(&pages)
			.expect("extract selectable PDF text");

		assert!(text.contains("$a^2+b^2=c^2$"));
		assert!(text.contains("$$E=mc^2$$"));
	}
}
