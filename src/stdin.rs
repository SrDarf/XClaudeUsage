use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;

/// Read all of stdin, giving up after `timeout` if EOF never arrives. The
/// legacy JS armed a `setTimeout(process.exit, ...)` for the same reason: a
/// pipe whose writer never closes (or a user testing the subcommand in a bare
/// terminal) must not leave the process blocked forever — statusLine commands
/// have no harness-side timeout. Returns `None` on timeout; the reader thread
/// is left blocked and dies with the process.
pub fn read_to_string_timeout(timeout: Duration) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).ok();
        let _ = tx.send(s);
    });
    rx.recv_timeout(timeout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_quickly_when_stdin_is_closed() {
        // Under `cargo test` stdin is closed, so the read hits EOF immediately
        // and must come back as Some (almost certainly an empty string).
        let r = read_to_string_timeout(Duration::from_secs(5));
        assert!(r.is_some());
    }
}
