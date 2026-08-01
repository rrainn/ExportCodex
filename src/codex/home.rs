//! Resolution of the local Codex data directory.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

/// Environment variable used by Codex to override its data directory.
pub const CODEX_HOME_ENV: &str = "CODEX_HOME";

/// Resolves the Codex home using CLI, environment, then platform-home precedence.
///
/// The path is not required to exist here because callers may want to report a
/// more specific error for a missing catalog or sessions directory.
///
/// # Errors
///
/// Returns an error when no explicit path, environment override, or platform
/// home directory is available.
pub fn resolve_codex_home(explicit: Option<&Path>) -> Result<PathBuf> {
	resolve_from(
		explicit,
		env::var_os(CODEX_HOME_ENV).map(PathBuf::from),
		dirs::home_dir(),
	)
}

/// Keeps precedence logic deterministic and independently testable.
fn resolve_from(
	explicit: Option<&Path>,
	environment: Option<PathBuf>,
	platform_home: Option<PathBuf>,
) -> Result<PathBuf> {
	if let Some(path) = explicit.filter(|path| !path.as_os_str().is_empty()) {
		return Ok(path.to_path_buf());
	}

	if let Some(path) = environment.filter(|path| !path.as_os_str().is_empty()) {
		return Ok(path);
	}

	platform_home
		.map(|path| path.join(".codex"))
		.ok_or_else(|| {
			anyhow!("could not determine the Codex home; pass --codex-home or set {CODEX_HOME_ENV}")
		})
}

#[cfg(test)]
mod tests {
	use std::path::{Path, PathBuf};

	use super::resolve_from;

	#[test]
	fn explicit_home_has_highest_precedence() {
		let resolved = resolve_from(
			Some(Path::new("/explicit")),
			Some(PathBuf::from("/environment")),
			Some(PathBuf::from("/home")),
		)
		.expect("an explicit path should resolve");

		assert_eq!(resolved, PathBuf::from("/explicit"));
	}

	#[test]
	fn environment_precedes_platform_home() {
		let resolved = resolve_from(
			None,
			Some(PathBuf::from("/environment")),
			Some(PathBuf::from("/home")),
		)
		.expect("the environment path should resolve");

		assert_eq!(resolved, PathBuf::from("/environment"));
	}

	#[test]
	fn platform_home_uses_standard_codex_directory() {
		let resolved = resolve_from(None, None, Some(PathBuf::from("/home")))
			.expect("the platform home should resolve");

		assert_eq!(resolved, PathBuf::from("/home/.codex"));
	}

	#[test]
	fn missing_sources_produce_an_actionable_error() {
		let error = resolve_from(None, None, None).expect_err("missing home sources should fail");

		assert!(error.to_string().contains("--codex-home"));
	}
}
