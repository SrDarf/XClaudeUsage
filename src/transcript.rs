use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct AssistantEvent {
    pub model: String,
    pub uuid: Option<String>,
    pub usage: Usage,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Usage {
    pub input: i64,
    pub output: i64,
    pub cache_creation: i64,
    pub cache_read: i64,
}

#[derive(Debug, Default)]
pub struct ReadResult {
    pub text: String,
    pub new_offset: u64,
}

#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    ty: Option<String>,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    message: Option<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Deserialize, Default)]
struct RawUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    cache_creation_input_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: i64,
}

/// Read the JSONL transcript from `offset` to EOF, returning all complete lines.
/// If the file shrank below `offset`, reset to 0 (handles transcript recreation).
/// Returns `None` only when the path cannot be stat'd at all.
pub fn read_new(path: &Path, offset: u64) -> Result<Option<ReadResult>> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    let size = meta.len();

    let mut offset = if offset > size { 0 } else { offset };
    if offset == size {
        return Ok(Some(ReadResult {
            text: String::new(),
            new_offset: offset,
        }));
    }

    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::with_capacity((size - offset) as usize);
    f.take(size - offset).read_to_end(&mut buf)?;

    let text = String::from_utf8_lossy(&buf).into_owned();
    let Some(last_nl) = text.rfind('\n') else {
        return Ok(Some(ReadResult {
            text: String::new(),
            new_offset: offset,
        }));
    };

    let process = text[..last_nl].to_string();
    offset += (last_nl as u64) + 1;
    Ok(Some(ReadResult {
        text: process,
        new_offset: offset,
    }))
}

pub fn parse_assistant_events(text: &str, fallback_model: Option<&str>) -> Vec<AssistantEvent> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(raw) = serde_json::from_str::<RawLine>(line) else {
            continue;
        };
        if raw.ty.as_deref() != Some("assistant") {
            continue;
        }
        let Some(message) = raw.message else { continue };
        let Some(usage) = message.usage else { continue };
        let model = message
            .model
            .or_else(|| fallback_model.map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown".to_string());
        let uuid = raw.uuid.or(message.id);
        out.push(AssistantEvent {
            model,
            uuid,
            usage: Usage {
                input: usage.input_tokens,
                output: usage.output_tokens,
                cache_creation: usage.cache_creation_input_tokens,
                cache_read: usage.cache_read_input_tokens,
            },
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};

    const ASSISTANT_LINE: &str = r#"{"type":"assistant","uuid":"u-1","message":{"id":"m-1","model":"claude-sonnet-4-6","usage":{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":5,"cache_read_input_tokens":3}}}"#;

    fn tmp_file(tag: &str, contents: &[u8]) -> std::path::PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "xcu-transcript-{}-{}-{}.jsonl",
            tag,
            std::process::id(),
            n
        ));
        let mut f = File::create(&p).unwrap();
        f.write_all(contents).unwrap();
        p
    }

    #[test]
    fn parses_assistant_usage_with_model_and_uuid() {
        let events = parse_assistant_events(ASSISTANT_LINE, None);
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.model, "claude-sonnet-4-6");
        assert_eq!(e.uuid.as_deref(), Some("u-1"));
        assert_eq!(e.usage.input, 10);
        assert_eq!(e.usage.output, 20);
        assert_eq!(e.usage.cache_creation, 5);
        assert_eq!(e.usage.cache_read, 3);
    }

    #[test]
    fn skips_non_assistant_blank_and_malformed_lines() {
        let text = format!(
            "{}\n{}\n   \n{}\n{}",
            r#"{"type":"user","message":{"role":"user"}}"#,
            "not json at all",
            r#"{"type":"assistant","message":{"model":"m"}}"#, // no usage -> skipped
            ASSISTANT_LINE,
        );
        let events = parse_assistant_events(&text, None);
        assert_eq!(
            events.len(),
            1,
            "only the well-formed assistant+usage line counts"
        );
        assert_eq!(events[0].usage.output, 20);
    }

    #[test]
    fn uses_fallback_model_and_message_id_uuid() {
        // model absent on message -> fallback; uuid absent on raw line -> message.id
        let line = r#"{"type":"assistant","message":{"id":"m-9","usage":{"output_tokens":7}}}"#;
        let events = parse_assistant_events(line, Some("fallback-model"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model, "fallback-model");
        assert_eq!(events[0].uuid.as_deref(), Some("m-9"));
        assert_eq!(events[0].usage.output, 7);
    }

    #[test]
    fn model_is_unknown_without_model_or_fallback() {
        let line = r#"{"type":"assistant","message":{"usage":{"output_tokens":1}}}"#;
        let events = parse_assistant_events(line, None);
        assert_eq!(events[0].model, "unknown");
    }

    #[test]
    fn read_new_returns_complete_lines_and_advances_offset() {
        let body = "line-a\nline-b\n";
        let p = tmp_file("complete", body.as_bytes());
        let r = read_new(&p, 0).unwrap().unwrap();
        assert_eq!(r.text, "line-a\nline-b");
        assert_eq!(r.new_offset, body.len() as u64);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn read_new_buffers_partial_trailing_line() {
        // No trailing newline on the last (partial) line: it must NOT be consumed.
        let body = "done\npartial-without-newline";
        let p = tmp_file("partial", body.as_bytes());
        let r = read_new(&p, 0).unwrap().unwrap();
        assert_eq!(r.text, "done");
        assert_eq!(r.new_offset, "done\n".len() as u64);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn read_new_no_newline_yields_empty_without_advancing() {
        let p = tmp_file("nonl", b"no-newline-yet");
        let r = read_new(&p, 0).unwrap().unwrap();
        assert_eq!(r.text, "");
        assert_eq!(r.new_offset, 0);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn read_new_offset_equals_size_is_noop() {
        let body = "x\n";
        let p = tmp_file("eq", body.as_bytes());
        let r = read_new(&p, body.len() as u64).unwrap().unwrap();
        assert_eq!(r.text, "");
        assert_eq!(r.new_offset, body.len() as u64);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn read_new_resets_when_file_shrinks_below_offset() {
        // Transcript was recreated/truncated: an offset past EOF must reset to 0.
        let body = "fresh\n";
        let p = tmp_file("shrink", body.as_bytes());
        let r = read_new(&p, 9999).unwrap().unwrap();
        assert_eq!(r.text, "fresh");
        assert_eq!(r.new_offset, body.len() as u64);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn read_new_missing_file_returns_none() {
        let p = std::env::temp_dir().join("xcu-transcript-absent-zzz.jsonl");
        std::fs::remove_file(&p).ok();
        assert!(read_new(&p, 0).unwrap().is_none());
    }
}
