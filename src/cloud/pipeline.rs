// libSQL HTTP /v2/pipeline client. The Turso edge accepts a batch of
// statements + a `close` directive in one POST and returns one ExecuteResult
// per statement. See https://docs.turso.tech/sdk/http/reference for the
// protocol; this implementation is intentionally minimal and matches the JS
// version in xclaude-record.js:libsqlPipeline.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use super::NETWORK_TIMEOUT;

#[derive(Debug, Clone)]
pub struct Statement {
    pub sql: String,
    pub args: Vec<Value>,
}

/// One decoded row of an EXECUTE result. Column order matches the SELECT.
pub type Row = Vec<Value>;

#[derive(Debug)]
pub struct ExecuteResult {
    pub cols: Vec<String>,
    pub rows: Vec<Row>,
}

#[derive(Deserialize)]
struct PipelineResponse {
    #[serde(default)]
    results: Vec<RawResultItem>,
}

#[derive(Deserialize)]
struct RawResultItem {
    #[serde(default, rename = "type")]
    ty: Option<String>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    response: Option<RawResultResponse>,
}

#[derive(Deserialize)]
struct RawResultResponse {
    #[serde(default)]
    result: Option<RawExecuteResult>,
}

#[derive(Deserialize)]
struct RawExecuteResult {
    #[serde(default)]
    cols: Vec<RawCol>,
    #[serde(default)]
    rows: Vec<Vec<RawCell>>,
}

#[derive(Deserialize)]
struct RawCol {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct RawCell {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    value: Option<Value>,
}

/// Execute `statements` against the libSQL HTTP endpoint at `base_url`.
/// Returns one `Option<ExecuteResult>` per input statement (None for
/// statements that only mutate, Some for SELECTs).
pub fn execute(
    base_url: &str,
    token: &str,
    statements: &[Statement],
) -> Result<Vec<Option<ExecuteResult>>> {
    let mut requests: Vec<Value> = statements
        .iter()
        .map(|s| {
            let args: Vec<Value> = s.args.iter().map(encode_cell).collect();
            json!({
                "type": "execute",
                "stmt": { "sql": s.sql, "args": args },
            })
        })
        .collect();
    requests.push(json!({ "type": "close" }));
    let body = json!({ "requests": requests });

    let url = format!("{}/v2/pipeline", base_url.trim_end_matches('/'));
    let response = ureq::post(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .timeout(NETWORK_TIMEOUT)
        .send_json(body);

    let response = match response {
        Ok(r) => r,
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            anyhow::bail!("libsql HTTP {code}: {}", body.chars().take(200).collect::<String>());
        }
        Err(e) => return Err(e).context("libsql request failed"),
    };

    let parsed: PipelineResponse = response.into_json().context("libsql response not JSON")?;

    let mut out = Vec::with_capacity(statements.len());
    for (i, item) in parsed.results.into_iter().enumerate() {
        if i >= statements.len() {
            // skip close ack
            break;
        }
        if item.ty.as_deref() != Some("ok") {
            anyhow::bail!(
                "libsql stmt {i} failed: {}",
                item.error
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "unknown".into())
            );
        }
        out.push(item.response.and_then(decode_response));
    }
    // If the server returned fewer results than expected, pad with None so
    // indices into `out` remain meaningful.
    while out.len() < statements.len() {
        out.push(None);
    }
    Ok(out)
}

fn decode_response(resp: RawResultResponse) -> Option<ExecuteResult> {
    let result = resp.result?;
    let cols: Vec<String> = result
        .cols
        .into_iter()
        .map(|c| c.name.unwrap_or_default())
        .collect();
    let rows: Vec<Row> = result
        .rows
        .into_iter()
        .map(|row| row.into_iter().map(decode_cell).collect())
        .collect();
    Some(ExecuteResult { cols, rows })
}

fn decode_cell(cell: RawCell) -> Value {
    match cell.ty.as_str() {
        "null" => Value::Null,
        "integer" => match cell.value {
            Some(Value::String(s)) => s.parse::<i64>().map(|n| n.into()).unwrap_or(Value::Null),
            Some(v) => v,
            None => Value::Null,
        },
        "float" => match cell.value {
            Some(Value::Number(n)) => Value::Number(n),
            Some(Value::String(s)) => s
                .parse::<f64>()
                .ok()
                .and_then(|f| serde_json::Number::from_f64(f).map(Value::Number))
                .unwrap_or(Value::Null),
            _ => Value::Null,
        },
        _ => cell.value.unwrap_or(Value::Null),
    }
}

fn encode_cell(v: &Value) -> Value {
    match v {
        Value::Null => json!({ "type": "null" }),
        Value::Bool(b) => json!({ "type": "integer", "value": if *b { "1" } else { "0" } }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                json!({ "type": "integer", "value": i.to_string() })
            } else if let Some(f) = n.as_f64() {
                json!({ "type": "float", "value": f })
            } else {
                json!({ "type": "text", "value": n.to_string() })
            }
        }
        Value::String(s) => json!({ "type": "text", "value": s }),
        // Arrays / objects: serialize as text. libSQL doesn't have a native
        // representation and the JS version also stringifies.
        other => json!({ "type": "text", "value": other.to_string() }),
    }
}

/// Helper: pull an integer column from a Row by index, defaulting to 0.
pub fn row_i64(row: &Row, idx: usize) -> i64 {
    row.get(idx)
        .and_then(|v| match v {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        })
        .unwrap_or(0)
}

pub fn row_str(row: &Row, idx: usize) -> String {
    row.get(idx)
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}
