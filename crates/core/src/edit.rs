use crate::adapter::{parse, validate_mcp};
use crate::model::Provider;
use anyhow::{ensure, Context, Result};
use jsonc_parser::cst::{CstInputValue as Node, CstRootNode};
use serde_json::Value;
use std::path::Path;
fn node(v: &Value) -> Node {
    match v {
        Value::Null => Node::Null,
        Value::Bool(b) => Node::Bool(*b),
        Value::Number(n) => Node::Number(n.to_string()),
        Value::String(s) => Node::String(s.clone()),
        Value::Array(a) => Node::Array(a.iter().map(node).collect()),
        Value::Object(m) => Node::Object(m.iter().map(|(k, v)| (k.clone(), node(v))).collect()),
    }
}
fn unique(object: &jsonc_parser::cst::CstObject) -> Result<()> {
    let mut keys = std::collections::BTreeSet::new();
    for prop in object.properties() {
        let name = prop
            .name()
            .context("Invalid property name")?
            .decoded_value()
            .map_err(|_| anyhow::anyhow!("Invalid property name"))?;
        ensure!(keys.insert(name), "Duplicate JSON property; write blocked");
    }
    Ok(())
}
/// Only update fields supplied by the form. Unknown native keys and untouched trivia survive.
pub fn mcp(
    p: Provider,
    path: &Path,
    bytes: Option<&[u8]>,
    name: &str,
    value: Option<&Value>,
    existing: bool,
) -> Result<Vec<u8>> {
    let raw = std::str::from_utf8(bytes.unwrap_or(if p == Provider::Codex { b"" } else { b"{}" }))?;
    let v = parse(path, raw)?;
    ensure!(v.is_object(), "Configuration must be an object");
    let section = p.mcp_key();
    let previous = v.get(section).and_then(|s| s.get(name));
    ensure!(
        previous.is_some() == existing,
        "Entry appeared, disappeared, or already exists"
    );
    let mut expected = v.clone();
    if let Some(fields) = value {
        let mut merged = previous.cloned().unwrap_or(serde_json::json!({}));
        let map = merged.as_object_mut().context("Invalid MCP entry")?;
        for (k, v) in fields.as_object().context("MCP fields must be an object")? {
            map.insert(k.clone(), v.clone());
        }
        validate_mcp(p, &merged)?;
        expected[section][name] = merged;
    } else {
        expected[section]
            .as_object_mut()
            .context("Invalid MCP table")?
            .remove(name);
    }
    let new = if p == Provider::Codex {
        use toml_edit::{DocumentMut, Item, Table};
        let mut doc = raw
            .parse::<DocumentMut>()
            .map_err(|_| anyhow::anyhow!("Invalid TOML"))?;
        if !doc.contains_key(section) {
            doc[section] = Item::Table(Table::new())
        }
        let table = doc[section]
            .as_table_like_mut()
            .context("Invalid MCP table")?;
        if let Some(v) = value {
            if !table.contains_key(name) {
                table.insert(name, Item::Table(Table::new()));
            }
            let target = table
                .get_mut(name)
                .and_then(Item::as_table_like_mut)
                .context("Invalid MCP entry")?;
            let mut updates = toml_edit::ser::to_document(v)?.into_table();
            for key in v.as_object().unwrap().keys() {
                if previous.and_then(|p| p.get(key)) == v.get(key) {
                    continue;
                }
                let mut new = updates.remove(key).context("Invalid TOML value")?;
                if let (Some(old), Some(next)) =
                    (target.get(key).and_then(Item::as_value), new.as_value_mut())
                {
                    *next.decor_mut() = old.decor().clone();
                }
                target.insert(key, new);
            }
        } else {
            table.remove(name);
        }
        doc.to_string()
    } else {
        let doc = CstRootNode::parse(raw, &Default::default())
            .map_err(|_| anyhow::anyhow!("Invalid JSON configuration"))?;
        let obj = doc.object_value().context("Invalid MCP configuration")?;
        unique(&obj)?;
        let section = obj
            .object_value_or_create(section)
            .context("Invalid MCP section")?;
        unique(&section)?;
        if let Some(v) = value {
            let target = section
                .object_value_or_create(name)
                .context("Invalid MCP entry")?;
            unique(&target)?;
            for (k, field) in v.as_object().unwrap() {
                if previous.and_then(|p| p.get(k)) == Some(field) {
                    continue;
                }
                if let Some(prop) = target.get(k) {
                    prop.set_value(node(field))
                } else {
                    target.append(k, node(field));
                }
            }
        } else {
            section.get(name).context("Entry missing")?.remove();
        }
        doc.to_string()
    };
    let parsed = parse(path, &new)?;
    ensure!(
        parsed == expected,
        "Changes to unrelated native fields or ambiguous keys; write blocked"
    );
    if value.is_some() {
        validate_mcp(p, &parsed[section][name])?;
    }
    Ok(new.into_bytes())
}
