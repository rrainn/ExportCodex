//! Shared theme values for screen, print, and PDF rendering.

use crate::domain::{Chat, MarginSize, PaperSize, RenderOptions, ThemeName};
use pulldown_cmark::{Event, Parser, TagEnd};
use url::Url;

/// Color values shared by the HTML and PDF renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Palette {
	pub background: &'static str,
	pub surface: &'static str,
	pub text: &'static str,
	pub muted: &'static str,
	pub border: &'static str,
	pub user_surface: &'static str,
	pub code_surface: &'static str,
	pub accent: &'static str,
}

/// Returns the built-in palette for a visual theme.
pub(crate) const fn palette(theme: ThemeName) -> Palette {
	match theme {
		ThemeName::Paper => Palette {
			background: "#F3EFE7",
			surface: "#FCFAF6",
			text: "#26231F",
			muted: "#716B63",
			border: "#D9D1C5",
			user_surface: "#F6F0E7",
			code_surface: "#F0ECE5",
			accent: "#2F5D8A",
		},
		ThemeName::Warm => Palette {
			background: "#F5ECDD",
			surface: "#FFF9EF",
			text: "#2B2922",
			muted: "#746E60",
			border: "#DCCDB7",
			user_surface: "#F5E9D7",
			code_surface: "#EFE4D4",
			accent: "#667451",
		},
		ThemeName::Mono => Palette {
			background: "#EFEFED",
			surface: "#FAFAF8",
			text: "#202224",
			muted: "#666A6D",
			border: "#CCCECF",
			user_surface: "#F1F2F2",
			code_surface: "#ECEDED",
			accent: "#35393C",
		},
	}
}

/// Uses a strict hex color so an accent cannot escape into CSS or TOML.
pub(crate) fn accent(options: &RenderOptions) -> &str {
	options
		.accent
		.as_deref()
		.filter(|value| is_hex_color(value))
		.unwrap_or_else(|| palette(options.theme).accent)
}

/// Keeps user-configured typography legible and rejects non-finite values.
pub(crate) fn font_size(options: &RenderOptions) -> f32 {
	if options.font_size.is_finite() {
		options.font_size.clamp(8.0, 20.0)
	} else {
		RenderOptions::default().font_size
	}
}

/// Returns the CSS page-size keyword used by print styles.
pub(crate) const fn paper_css(size: PaperSize) -> &'static str {
	match size {
		PaperSize::Letter => "Letter",
		PaperSize::A4 => "A4",
	}
}

/// Returns the PDF renderer's page-size spelling.
pub(crate) const fn paper_pdf(size: PaperSize) -> &'static str {
	match size {
		PaperSize::Letter => "Letter",
		PaperSize::A4 => "A4",
	}
}

/// Returns balanced page margins in millimeters.
pub(crate) const fn margin_mm(margins: MarginSize) -> f32 {
	match margins {
		MarginSize::Compact => 14.0,
		MarginSize::Normal => 19.0,
		MarginSize::Wide => 25.0,
	}
}

/// Resolves a single-line export title before falling back to Codex metadata.
pub(crate) fn title(chat: &Chat, options: &RenderOptions) -> String {
	if let Some(custom_title) = options.title.as_deref() {
		let custom_title = normalize_plain_title(custom_title);
		if !custom_title.is_empty() {
			return custom_title;
		}
	}

	clean_catalog_title(chat.thread.display_title())
}

/// Converts the Markdown-like catalog title into safe, readable plain text.
pub(crate) fn clean_catalog_title(value: &str) -> String {
	let mut extracted = String::new();
	for event in Parser::new(value) {
		match event {
			Event::Text(text) | Event::Code(text) => extracted.push_str(&text),
			Event::SoftBreak | Event::HardBreak => extracted.push(' '),
			// Catalog titles often contain the first prompt; HTML starts body content,
			// not a useful printable title.
			Event::Html(_) | Event::InlineHtml(_) if !extracted.trim().is_empty() => break,
			Event::End(TagEnd::Paragraph | TagEnd::Heading(_)) if !extracted.trim().is_empty() => {
				break;
			}
			_ => {}
		}
	}

	let extracted = normalize_plain_title(&extracted);
	if extracted.is_empty() {
		"Untitled chat".to_owned()
	} else {
		extracted
	}
}

fn normalize_plain_title(value: &str) -> String {
	value
		.chars()
		.map(|character| {
			if character.is_control() {
				' '
			} else {
				character
			}
		})
		.collect::<String>()
		.split_whitespace()
		.collect::<Vec<_>>()
		.join(" ")
}

/// Restricts clickable links to explicit, non-executable destinations.
pub(crate) fn is_safe_link_destination(value: &str) -> bool {
	if value.trim() != value || value.chars().any(char::is_control) {
		return false;
	}
	if value.is_empty() || value.starts_with('#') {
		return true;
	}
	Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https" | "mailto" | "codex"))
}

fn is_hex_color(value: &str) -> bool {
	let Some(hex) = value.strip_prefix('#') else {
		return false;
	};
	(matches!(hex.len(), 3 | 6) && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn invalid_accent_falls_back_to_the_theme() {
		let options = RenderOptions {
			accent: Some("red; } body { display: none".to_owned()),
			..RenderOptions::default()
		};

		assert_eq!(accent(&options), palette(ThemeName::Paper).accent);
	}

	#[test]
	fn valid_short_and_long_hex_accents_are_kept() {
		for value in ["#abc", "#A17C5B"] {
			let options = RenderOptions {
				accent: Some(value.to_owned()),
				..RenderOptions::default()
			};
			assert_eq!(accent(&options), value);
		}
	}

	#[test]
	fn documented_maximum_font_size_is_preserved() {
		let options = RenderOptions {
			font_size: 20.0,
			..RenderOptions::default()
		};

		assert_eq!(font_size(&options), 20.0);
	}

	#[test]
	fn safe_links_use_an_explicit_scheme_allowlist() {
		for value in [
			"https://example.com",
			"mailto:test@example.com",
			"codex://threads/123",
			"#answer",
		] {
			assert!(is_safe_link_destination(value));
		}
		for value in [
			"javascript:alert(1)",
			"data:text/html,boom",
			"file:///tmp/private",
			"relative.html",
		] {
			assert!(!is_safe_link_destination(value));
		}
	}

	#[test]
	fn catalog_markdown_becomes_a_plain_document_title() {
		let title =
			clean_catalog_title("Please export **this chat**. <script>not executable</script>");

		assert_eq!(title, "Please export this chat.");
	}
}
