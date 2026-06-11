use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

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

// `Option<i64>` (not plain i64 with serde(default)) so an explicit JSON
// `null` in one field doesn't fail the whole line and drop its valid tokens —
// the legacy JS tolerated null via `|| 0`.
#[derive(Deserialize, Default)]
struct RawUsage {
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<i64>,
    #[serde(default)]
    cache_read_input_tokens: Option<i64>,
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

    // Find the last newline at the byte level (newline is ASCII, so this is
    // safe regardless of UTF-8 boundaries), keep only the complete lines, and
    // decode once — avoids decoding the whole tail and then re-copying a slice.
    let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') else {
        return Ok(Some(ReadResult {
            text: String::new(),
            new_offset: offset,
        }));
    };
    buf.truncate(last_nl);
    offset += (last_nl as u64) + 1;
    Ok(Some(ReadResult {
        text: String::from_utf8_lossy(&buf).into_owned(),
        new_offset: offset,
    }))
}

/// Discover the subagent/workflow transcripts that belong to a main session
/// transcript. For `.../projects/<proj>/<session>.jsonl` they live under
/// `.../projects/<proj>/<session>/subagents/` — plain Task subagents directly
/// inside it, and deep-research / ultracode dynamic workflows nested under
/// `subagents/workflows/wf_*/` — all named `agent-*.jsonl`. Returns an empty
/// vec when the directory is absent (the common case for a session that never
/// spawned a subagent). Shared by the recorder and the statusline fallback so
/// the two paths can never disagree about which files count.
pub fn subagent_transcripts(main: &Path) -> Vec<PathBuf> {
    // `<...>/<session>.jsonl` -> `<...>/<session>` -> `<...>/<session>/subagents`
    let root = main.with_extension("").join("subagents");
    let mut out = Vec::new();
    collect_agent_files(&root, &mut out, 0);
    out
}

fn collect_agent_files(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
    // The real tree is at most `subagents/workflows/wf_*/agent-*.jsonl`; bound
    // recursion so a pathological symlink loop can't wedge the hook.
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            collect_agent_files(&path, out, depth + 1);
        } else if ft.is_file() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("agent-") && name.ends_with(".jsonl") {
                    out.push(path);
                }
            }
        }
    }
}

pub fn parse_assistant_events(text: &str, fallback_model: Option<&str>) -> Vec<AssistantEvent> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        // Cheap pre-filter: only `assistant` lines carry usage and they're a
        // minority of the transcript, so skip the full JSON parse for the rest
        // (this also drops blank lines). The `raw.ty` check below stays as the
        // authoritative filter — the substring can appear in other line types.
        if !line.contains("\"assistant\"") {
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
                input: usage.input_tokens.unwrap_or(0),
                output: usage.output_tokens.unwrap_or(0),
                cache_creation: usage.cache_creation_input_tokens.unwrap_or(0),
                cache_read: usage.cache_read_input_tokens.unwrap_or(0),
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

    #[test]
    fn null_usage_field_counts_remaining_tokens() {
        // serde(default) alone rejects explicit nulls; the whole line (and its
        // valid tokens) used to be dropped. Option<i64> must tolerate it.
        let line = r#"{"type":"assistant","uuid":"u-n","message":{"model":"m","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":null}}}"#;
        let events = parse_assistant_events(line, None);
        assert_eq!(events.len(), 1, "null field must not drop the event");
        assert_eq!(events[0].usage.input, 100);
        assert_eq!(events[0].usage.output, 50);
        assert_eq!(events[0].usage.cache_read, 0);
    }

    #[test]
    fn discovers_plain_and_workflow_subagent_transcripts() {
        // Mirror the on-disk layout:
        //   <proj>/<session>.jsonl                                   (main)
        //   <proj>/<session>/subagents/agent-aaa.jsonl              (Task subagent)
        //   <proj>/<session>/subagents/workflows/wf_x/agent-bbb.jsonl (workflow)
        let proj = std::env::temp_dir().join(format!("xcu-layout-{}", std::process::id()));
        std::fs::create_dir_all(&proj).unwrap();
        let session = "11111111-2222-3333-4444-555555555555";
        let main = proj.join(format!("{session}.jsonl"));
        std::fs::write(&main, "{}\n").unwrap();

        let sub = proj.join(session).join("subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("agent-aaa.jsonl"), "{}\n").unwrap();

        let wf = sub.join("workflows").join("wf_deadbeef");
        std::fs::create_dir_all(&wf).unwrap();
        std::fs::write(wf.join("agent-bbb.jsonl"), "{}\n").unwrap();

        // Decoys that must NOT be picked up.
        std::fs::write(sub.join("scratch.jsonl"), "{}\n").unwrap();
        std::fs::write(sub.join("agent-notes.txt"), "x").unwrap();

        let mut found: Vec<String> = subagent_transcripts(&main)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();

        assert_eq!(found, vec!["agent-aaa.jsonl", "agent-bbb.jsonl"]);
        std::fs::remove_dir_all(&proj).ok();
    }

    #[test]
    fn subagents_dir_absent_yields_empty() {
        let proj = std::env::temp_dir().join(format!("xcu-nosubs-{}", std::process::id()));
        std::fs::create_dir_all(&proj).unwrap();
        let main = proj.join("66666666-7777-8888-9999-000000000000.jsonl");
        std::fs::write(&main, "{}\n").unwrap();
        assert!(subagent_transcripts(&main).is_empty());
        std::fs::remove_dir_all(&proj).ok();
    }
}
