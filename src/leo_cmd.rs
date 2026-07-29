use anyhow::{Context, Result, bail};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run a `leo` subcommand, optionally inside a specific project directory.
///
/// Inherits stdout/stderr so the user sees Leo's real output directly.
/// Returns a descriptive error if `leo` is not found or the subcommand fails.
pub fn run_leo(subcommand: &str, dir: Option<&Path>) -> Result<()> {
    run_leo_with(subcommand, &[], dir)
}

/// Run a `leo` subcommand with extra flags, optionally in a project directory.
pub fn run_leo_with(subcommand: &str, extra_flags: &[String], dir: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new("leo");
    cmd.arg(subcommand);
    for flag in extra_flags {
        cmd.arg(flag);
    }

    if let Some(path) = dir {
        cmd.current_dir(path);
    }

    let status = cmd.status().with_context(|| {
        format!(
            "Failed to execute 'leo {}'. \
             Make sure leo is installed and on your PATH.\n\
             Install it with: cargo binstall leo-lang",
            subcommand
        )
    })?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        bail!("'leo {}' failed with exit code {}", subcommand, code);
    }

    Ok(())
}

/// Check whether `leo` is available on PATH.
pub fn leo_is_installed() -> bool {
    Command::new("leo")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check whether `snarkos` is available on PATH.
pub fn snarkos_is_installed() -> bool {
    Command::new("snarkos")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check whether `leo-fmt` (the leo fmt plugin) is available on PATH.
pub fn leo_fmt_is_installed() -> bool {
    Command::new("leo-fmt")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run a `leo` subcommand, capturing stdout and stderr for error analysis.
///
/// Prints leo's raw stdout and stderr to the terminal (so the user sees
/// the real output live), then returns a (result, captured_stderr) tuple.
/// The captured stderr is always returned (empty string on success) so the
/// caller can pass it directly to error translation without anyohw chaining.
///
/// This is designed for `run` and `execute` where leo errors have known
/// patterns that AleoFlow can translate into friendlier summaries.
/// Other commands (build, test, deploy) should use the simpler `run_leo_with`
/// which inherits stdio directly.
pub fn run_leo_captured(
    subcommand: &str,
    extra_flags: &[String],
    dir: Option<&Path>,
) -> (Result<()>, String) {
    let mut cmd = Command::new("leo");
    cmd.arg(subcommand);
    for flag in extra_flags {
        cmd.arg(flag);
    }

    if let Some(path) = dir {
        cmd.current_dir(path);
    }

    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            return (
                Err(anyhow::Error::from(e).context(format!(
                    "Failed to execute 'leo {}'. \
                     Make sure leo is installed and on your PATH.\n\
                     Install it with: cargo binstall leo-lang",
                    subcommand
                ))),
                String::new(),
            );
        }
    };

    // Print leo's raw output to the terminal (stdout first, then stderr)
    if !output.stdout.is_empty() {
        let _ = io::stdout().write_all(&output.stdout);
    }
    if !output.stderr.is_empty() {
        let _ = io::stderr().write_all(&output.stderr);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        (
            Err(anyhow::anyhow!(
                "'leo {}' failed with exit code {}",
                subcommand, code
            )),
            stderr,
        )
    } else {
        (Ok(()), stderr)
    }
}

/// Build the `--json-output[=<PATH>]` flags based on the user's setting.
/// Returns an empty vec if the flag was not passed.
pub fn json_output_flag(json_output: &Option<Option<PathBuf>>) -> Vec<String> {
    match json_output {
        Some(None) => vec!["--json-output".to_string()],
        Some(Some(path)) => {
            vec![format!("--json-output={}", path.to_string_lossy())]
        }
        None => vec![],
    }
}
