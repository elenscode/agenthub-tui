use crate::{adapter, model::*, operations, processes::Cancel};
use anyhow::{bail, ensure, Context as _, Result};
use serde_json::Value;
use std::path::PathBuf;

pub async fn scan(c: &Context, cancel: &Cancel) -> Registry {
    let mut r = adapter::discover(c, adapter::versions(cancel).await);
    adapter::verify_codex(c, &mut r, cancel).await;
    r
}

#[derive(Clone)]
pub enum Source {
    Registered(Box<Asset>),
    Import { path: PathBuf, kind: Kind },
}
impl Source {
    pub async fn read(&self, c: &Context, cancel: &Cancel) -> Result<Value> {
        match self {
            Self::Registered(a) => read_registration(c, a, adapter::versions(cancel).await),
            Self::Import { path, kind } => {
                operations::check_path(path)?;
                match kind {
                    Kind::Skill => Ok(serde_json::to_value(operations::snapshot(path)?)?),
                    Kind::Mcp => adapter::parse(path, std::str::from_utf8(&adapter::read(path)?)?),
                    _ => bail!("Read-only asset type"),
                }
            }
        }
    }
}
fn read_registration(
    c: &Context,
    a: &Asset,
    versions: std::collections::BTreeMap<Provider, Option<String>>,
) -> Result<Value> {
    let r = adapter::discover(c, versions);
    let now = r
        .assets
        .iter()
        .find(|b| b.id == a.id && b.kind == a.kind)
        .context("Asset is no longer registered")?;
    ensure!(
        now.content_hash == a.content_hash,
        "Source changed; refresh required"
    );
    if a.kind == Kind::Skill {
        return Ok(serde_json::to_value(operations::snapshot(
            now.path.parent().context("Invalid Skill path")?,
        )?)?);
    }
    ensure!(a.kind == Kind::Mcp, "Unsupported read");
    let raw = adapter::read(&now.path)?;
    ensure!(hash(&raw) == now.content_hash, "Source changed during read");
    let v = adapter::parse(&now.path, std::str::from_utf8(&raw)?)?;
    if let Some(project) = now
        .locator
        .strip_prefix("projects/")
        .and_then(|s| s.rsplit_once("/mcpServers/"))
    {
        return Ok(v["projects"][project.0]["mcpServers"][project.1].clone());
    }
    Ok(v[a.provider.mcp_key()][&now.name].clone())
}
