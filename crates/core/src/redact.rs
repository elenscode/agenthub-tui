use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
pub const MASK: &str = "[redacted]";
static SENSITIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)token|secret|pass(word|wd)?|authorization|cookie|api.?key|private.?key|credential|bearer|client.?key").unwrap()
});
static LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(bearer\s+\S+|(?:sk-|gh[pousr]_|github_pat_|xox[baprs]-|AKIA)[A-Za-z0-9_\-]{6,}|https?://[^\s/]+:[^\s/]+@)"#).unwrap()
});
pub fn text(s: &str) -> String {
    let mut pem = false;
    let mut secret_indent: Option<usize> = None;
    let mut quote_block: Option<&str> = None;
    s.lines()
        .map(|l| {
            if l.contains("-----BEGIN") && l.contains("PRIVATE KEY") {
                pem = true;
            }
            let indent = l.len() - l.trim_start().len();
            let continued = secret_indent.is_some_and(|base| l.trim().is_empty() || indent > base);
            if !continued {
                secret_indent = None;
            }
            let quoted = quote_block.is_some();
            if let Some(delimiter) = quote_block {
                if l.contains(delimiter) {
                    quote_block = None;
                }
            }
            let sensitive = SENSITIVE.is_match(l);
            if sensitive {
                let end = l.trim_end();
                if [":", "|", ">", "|-", ">-", "|+", ">+"]
                    .iter()
                    .any(|suffix| end.ends_with(suffix))
                {
                    secret_indent = Some(indent);
                }
                for delimiter in ["\"\"\"", "'''"] {
                    if l.matches(delimiter).count() % 2 == 1 {
                        quote_block = Some(delimiter);
                    }
                }
            }
            let hide = pem
                || quoted
                || continued
                || sensitive
                || LINE.is_match(l)
                || l.contains("?access_")
                || (l.contains("://") && l.contains('?'));
            if l.contains("-----END") && l.contains("PRIVATE KEY") {
                pem = false
            }
            if hide {
                MASK.into()
            } else {
                l.chars()
                    .filter(|c| !c.is_control() || *c == '\t')
                    .collect::<String>()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
pub fn json(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| {
                    let mask = (k == "command" && v.is_array())
                        || SENSITIVE.is_match(k)
                        || matches!(
                            k.to_ascii_lowercase().as_str(),
                            "env"
                                | "environment"
                                | "headers"
                                | "args"
                                | "arguments"
                                | "http_headers"
                                | "url"
                        );
                    (
                        text(k),
                        if mask {
                            Value::String(MASK.into())
                        } else {
                            json(v)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(json).collect()),
        Value::String(s) => Value::String(text(s)),
        _ => v.clone(),
    }
}
pub fn sensitive(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|s| {
        SENSITIVE.is_match(s)
            || LINE.is_match(s)
            || s.to_lowercase().contains("\"env\"")
            || s.to_lowercase().contains("headers")
            || s.contains("\"environment\"")
            || s.contains("\"args\"")
            || s.contains("\"command\":[")
    })
}
