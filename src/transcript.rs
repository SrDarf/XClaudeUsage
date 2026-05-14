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
