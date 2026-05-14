use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "xclaudeusage",
    version,
    about = "Claude Code token usage statusline and recorder",
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Render the Claude Code statusline. Reads hook payload JSON from stdin.
    Statusline,

    /// Record token usage from a Stop / SubagentStop / SubagentStart / PostToolUse hook.
    /// Reads hook payload JSON from stdin.
    Record,

    /// Interactive installer. Writes hook entries into ~/.claude/settings.json
    /// and (optionally) the Turso cloud config.
    Install,

    /// Print diagnostic information about the local install.
    #[command(hide = true)]
    Doctor,

    /// Remove XClaudeUsage entries from ~/.claude/settings.json (keeps DB and cloud config).
    #[command(hide = true)]
    Uninstall,
}

impl Cli {
    pub fn run(self) -> i32 {
        match self.command {
            // Hooks must NEVER propagate errors fatally — a broken hook cannot
            // break the user's Claude Code session. Swallow + log.
            Command::Statusline => {
                if let Err(e) = crate::statusline::run() {
                    crate::log::warn(&format!("statusline: {e:#}"));
                }
                0
            }
            Command::Record => {
                if let Err(e) = crate::record::run() {
                    crate::log::warn(&format!("record: {e:#}"));
                }
                0
            }
            // Installer + diagnostics: errors are user-facing, exit non-zero.
            Command::Install => match crate::install::run() {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("[install] ABORT: {e:#}");
                    1
                }
            },
            Command::Doctor => match crate::install::doctor() {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("doctor: {e:#}");
                    1
                }
            },
            Command::Uninstall => match crate::install::uninstall() {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("[uninstall] {e:#}");
                    1
                }
            },
        }
    }
}
