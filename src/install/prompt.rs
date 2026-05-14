// Interactive prompts that survive a piped invocation (`curl | sh`).
//
// `~/.cargo/bin/xclaudeusage install` is intended to be `exec`'d by the shell
// installer right after the binary is dropped on disk. The shell pipeline's
// stdin is the script body, so we cannot use `io::stdin()` directly — open
// the controlling TTY explicitly. Falls back to stdin if a TTY is already
// attached (e.g. when the user runs `xclaudeusage install` themselves).

use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};

use anyhow::Result;

pub struct Tty {
    reader: Box<dyn BufRead>,
    writer: Box<dyn Write>,
}

impl Tty {
    pub fn open() -> Result<Self> {
        if let Some(tty) = open_controlling_tty() {
            return Ok(tty);
        }
        if io::stdin().is_terminal() && io::stdout().is_terminal() {
            return Ok(Self {
                reader: Box::new(BufReader::new(io::stdin())),
                writer: Box::new(io::stdout()),
            });
        }
        anyhow::bail!(
            "this installer is interactive and needs a terminal. \
             If you piped the installer from curl, download install.sh first and run it directly."
        );
    }

    pub fn ask(&mut self, question: &str) -> Result<String> {
        self.writer.write_all(question.as_bytes())?;
        self.writer.flush()?;
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        // Strip trailing CR/LF.
        while matches!(line.chars().last(), Some('\n' | '\r')) {
            line.pop();
        }
        Ok(line)
    }

    pub fn writeln(&mut self, msg: &str) -> Result<()> {
        self.writer.write_all(msg.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

#[cfg(unix)]
fn open_controlling_tty() -> Option<Tty> {
    let read = OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?;
    let write = OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?;
    Some(Tty {
        reader: Box::new(BufReader::new(read)),
        writer: Box::new(write),
    })
}

#[cfg(windows)]
fn open_controlling_tty() -> Option<Tty> {
    let read = OpenOptions::new().read(true).write(true).open(r"\\.\CON").ok()?;
    let write = OpenOptions::new().read(true).write(true).open(r"\\.\CON").ok()?;
    Some(Tty {
        reader: Box::new(BufReader::new(read)),
        writer: Box::new(write),
    })
}

#[cfg(not(any(unix, windows)))]
fn open_controlling_tty() -> Option<Tty> {
    None
}

// Helper kept for symmetry with the JS installer's `mask` of secrets.
pub fn mask_token(token: &str) -> String {
    if token.len() <= 12 {
        return "***".to_string();
    }
    let prefix: String = token.chars().take(8).collect();
    let suffix: String = token
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{prefix}…{suffix} ({} chars)", token.len())
}

// Silence unused-warning when neither Read nor BufRead callers are present.
#[allow(dead_code)]
fn _force_read_import(_r: &mut dyn Read) {}
