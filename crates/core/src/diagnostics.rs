use crate::{
    adapter,
    model::Provider,
    processes::{self, Cancel, Tree},
    redact,
};
use anyhow::{bail, ensure, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};
const CAP: usize = 128 * 1024;
const VERSION: &str = "2025-03-26";
fn init() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":VERSION,"capabilities":{},"clientInfo":{"name":"agenthub","version":env!("CARGO_PKG_VERSION")}}})
}
fn ready() -> Value {
    json!({"jsonrpc":"2.0","method":"notifications/initialized"})
}
fn list(id: u32, cursor: Option<&str>) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":cursor.map(|c|json!({"cursor":c})).unwrap_or(json!({}))})
}
fn result(msg: &Value, id: u32) -> Result<&Value> {
    ensure!(
        msg["jsonrpc"] == "2.0" && msg["id"] == id,
        "Unexpected MCP response"
    );
    msg.get("result")
        .context("MCP request failed (server response omitted)")
}
fn negotiated(v: &Value) -> Result<&str> {
    let ver = v["protocolVersion"]
        .as_str()
        .context("No protocol version")?;
    ensure!(
        matches!(
            ver,
            "2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25"
        ),
        "Unsupported MCP protocol"
    );
    Ok(ver)
}
fn tools(page: &Value, names: &mut Vec<String>) -> Result<Option<String>> {
    for tool in page["tools"].as_array().context("Invalid tool list")? {
        let n = tool["name"].as_str().context("Missing tool name")?;
        ensure!(
            names.len() < 512 && n.len() <= 256,
            "Tool list limit exceeded"
        );
        names.push(redact::text(n));
    }
    Ok(page
        .get("nextCursor")
        .and_then(Value::as_str)
        .map(str::to_owned))
}
async fn frame(reader: &mut (impl AsyncBufRead + Unpin), remaining: &mut usize) -> Result<Value> {
    let mut data = vec![];
    loop {
        let buf = reader.fill_buf().await?;
        ensure!(!buf.is_empty(), "MCP server closed its output");
        let n = buf
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(buf.len());
        ensure!(n <= *remaining, "MCP output limit exceeded");
        *remaining -= n;
        let complete = buf[n - 1] == b'\n';
        data.extend_from_slice(&buf[..n]);
        reader.consume(n);
        if complete {
            return serde_json::from_slice(&data)
                .map_err(|_| anyhow::anyhow!("Invalid MCP message"));
        }
    }
}
async fn receive(
    reader: &mut (impl AsyncBufRead + Unpin),
    budget: &mut usize,
    id: u32,
) -> Result<Value> {
    loop {
        let msg = frame(reader, budget).await?;
        if msg.get("id").is_none() && msg.get("method").is_some() {
            continue;
        }
        return Ok(result(&msg, id)?.clone());
    }
}
async fn send(w: &mut tokio::process::ChildStdin, v: Value) -> Result<()> {
    let mut data = serde_json::to_vec(&v)?;
    data.push(b'\n');
    w.write_all(&data).await?;
    w.flush().await?;
    Ok(())
}
fn expand(s: &str) -> Result<String> {
    let re = regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\{env:([A-Za-z_][A-Za-z0-9_]*)\}")
        .unwrap();
    let mut output = String::new();
    let mut end = 0;
    for c in re.captures_iter(s) {
        let m = c.get(0).unwrap();
        output.push_str(&s[end..m.start()]);
        let k = c.get(1).or_else(|| c.get(2)).unwrap().as_str();
        output.push_str(
            &std::env::var(k)
                .map_err(|_| anyhow::anyhow!("Required environment variable unavailable"))?,
        );
        end = m.end();
    }
    output.push_str(&s[end..]);
    Ok(output)
}
async fn stdio(v: &Value, project: &Path) -> Result<Vec<String>> {
    let (bin, args) = if let Some(s) = v["command"].as_str() {
        (s, v["args"].as_array().cloned().unwrap_or_default())
    } else {
        let cmd = v["command"].as_array().context("Missing command")?;
        (
            cmd.first()
                .and_then(Value::as_str)
                .context("Missing executable")?,
            cmd[1..].to_vec(),
        )
    };
    let mut cmd = Command::new(expand(bin)?);
    for a in args {
        cmd.arg(expand(a.as_str().context("Invalid argument")?)?);
    }
    let cwd = v["cwd"]
        .as_str()
        .map(|p| project.join(p))
        .unwrap_or_else(|| project.to_owned());
    cmd.current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for key in [
        "PATH",
        "HOME",
        "USERPROFILE",
        "SystemRoot",
        "APPDATA",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
        "LANG",
    ]
    .into_iter()
    .chain(
        v["env_vars"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str),
    ) {
        if let Some(x) = std::env::var_os(key) {
            cmd.env(key, x);
        }
    }
    for map in ["env", "environment"] {
        if let Some(env) = v[map].as_object() {
            for (k, x) in env {
                cmd.env(k, expand(x.as_str().context("Invalid environment field")?)?);
            }
        }
    }
    let (mut child, _tree) = Tree::spawn(&mut cmd)?;
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut input = child.stdin.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let work = async {
        let mut budget = CAP;
        send(&mut input, init()).await?;
        let response = receive(&mut output, &mut budget, 1).await?;
        negotiated(&response)?;
        send(&mut input, ready()).await?;
        if response["capabilities"].get("tools").is_none() {
            return Ok(vec![]);
        }
        let mut names = vec![];
        let mut cursor = None;
        for id in 2..34 {
            send(&mut input, list(id, cursor.as_deref())).await?;
            cursor = tools(&receive(&mut output, &mut budget, id).await?, &mut names)?;
            if cursor.is_none() {
                return Ok(names);
            }
        }
        bail!("MCP page limit exceeded")
    };
    let drain = async {
        processes::limited(stderr, 65536).await?;
        std::future::pending::<Result<Vec<String>>>().await
    };
    tokio::select! {r=work=>r,r=drain=>r}
}
struct Http {
    client: reqwest::Client,
    url: String,
    headers: reqwest::header::HeaderMap,
    session: Option<String>,
    version: Option<String>,
    remaining: usize,
}
impl Http {
    async fn post(&mut self, message: Value, expect: bool) -> Result<Value> {
        let mut req = self
            .client
            .post(&self.url)
            .headers(self.headers.clone())
            .header("Accept", "application/json, text/event-stream")
            .json(&message);
        if let Some(s) = &self.session {
            req = req.header("Mcp-Session-Id", s)
        }
        if let Some(s) = &self.version {
            req = req.header("MCP-Protocol-Version", s)
        }
        let res = req
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("MCP connection failed"))?;
        ensure!(
            res.status().is_success(),
            "HTTP {}. Use your CLI authentication flow if required",
            res.status().as_u16()
        );
        if let Some(s) = res.headers().get("mcp-session-id") {
            self.session = Some(s.to_str()?.into())
        }
        if !expect {
            return Ok(Value::Null);
        }
        let sse = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let mut stream = res.bytes_stream();
        let mut buf = vec![];
        while let Some(bytes) = stream.next().await {
            let b = bytes.map_err(|_| anyhow::anyhow!("MCP body unavailable"))?;
            ensure!(b.len() <= self.remaining, "MCP output limit exceeded");
            self.remaining -= b.len();
            buf.extend_from_slice(&b);
            if sse {
                for l in buf
                    .split_inclusive(|b| *b == b'\n')
                    .filter(|l| l.ends_with(b"\n"))
                {
                    if let Some(d) = l.strip_prefix(b"data:") {
                        if let Ok(v) = serde_json::from_slice::<Value>(d) {
                            if v.get("id") == message.get("id") {
                                return Ok(v);
                            }
                        }
                    }
                }
            }
        }
        serde_json::from_slice(&buf).map_err(|_| anyhow::anyhow!("Invalid MCP response"))
    }
}
async fn http(v: &Value) -> Result<Vec<String>> {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    let mut headers = HeaderMap::new();
    for key in ["http_headers", "headers", "env_http_headers"] {
        if let Some(m) = v[key].as_object() {
            for (k, x) in m {
                let raw = x.as_str().context("Invalid HTTP header")?;
                let val = if key == "env_http_headers" {
                    std::env::var(raw).context("Header environment variable unavailable")?
                } else {
                    expand(raw)?
                };
                headers.insert(
                    HeaderName::from_bytes(k.as_bytes())
                        .map_err(|_| anyhow::anyhow!("Invalid header name"))?,
                    HeaderValue::from_str(&val)
                        .map_err(|_| anyhow::anyhow!("Invalid header value"))?,
                );
            }
        }
    }
    if let Some(k) = v["bearer_token_env_var"].as_str() {
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!(
                "Bearer {}",
                std::env::var(k).context("Use your CLI authentication flow")?
            ))
            .map_err(|_| anyhow::anyhow!("Invalid bearer token"))?,
        );
    }
    let url = expand(v["url"].as_str().context("Missing URL")?)?;
    let mut h = Http {
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        url,
        headers,
        session: None,
        version: None,
        remaining: CAP,
    };
    let first = h.post(init(), true).await?;
    let hello = result(&first, 1)?;
    h.version = Some(negotiated(hello)?.into());
    h.post(ready(), false).await?;
    if hello["capabilities"].get("tools").is_none() {
        return Ok(vec![]);
    }
    let mut names = vec![];
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    for id in 2..34 {
        let r = h.post(list(id, cursor.as_deref()), true).await?;
        cursor = tools(result(&r, id)?, &mut names)?;
        if cursor.is_none() {
            return Ok(names);
        }
        ensure!(seen.insert(cursor.clone()), "Repeated MCP cursor")
    }
    bail!("MCP page limit exceeded")
}
pub async fn diagnose(
    p: Provider,
    v: &Value,
    project: &Path,
    approved: bool,
    cancel: &Cancel,
    timeout: Duration,
) -> Result<Vec<String>> {
    ensure!(approved, "Explicit diagnostic approval required");
    adapter::validate_mcp(p, v)?;
    ensure!(
        !matches!(v["type"].as_str(), Some("sse" | "ws" | "websocket")),
        "Legacy SSE/WebSocket diagnostics are unsupported; use your CLI authentication flow"
    );
    let task = async {
        if v.get("url").is_some() {
            http(v).await
        } else {
            stdio(v, project).await
        }
    };
    tokio::select! {v=tokio::time::timeout(timeout,task)=>v.map_err(|_|anyhow::anyhow!("MCP diagnostic timed out"))?,_=processes::cancelled(cancel)=>bail!("Diagnostic cancelled")}
}
