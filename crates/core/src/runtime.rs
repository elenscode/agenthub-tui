use crate::{
    adapter, diagnostics,
    model::*,
    operations::{self, Image, Intent, Plan, Target},
    processes::{self, Cancel},
    redact,
};
use anyhow::{bail, ensure, Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::process::Command;
pub const PROTOCOL: u32 = 1;
pub const LIMIT: usize = 32 * 1024 * 1024;
pub const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");
#[derive(Clone, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub id: String,
    pub context: Context,
    pub action: Action,
}
#[derive(Clone, Serialize, Deserialize)]
pub enum Action {
    Hello,
    Scan,
    Prepare {
        target: Target,
        intent: Intent,
        copy: bool,
    },
    Current(Target),
    Read(Asset),
    Import {
        path: PathBuf,
        kind: Kind,
    },
    Apply {
        plan: Plan,
        backup_id: String,
        approved_hash: String,
    },
    Diagnose {
        asset: Asset,
        approved: bool,
    },
}
#[derive(Serialize, Deserialize)]
pub struct Reply {
    pub protocol: u32,
    pub id: String,
    pub data: Option<Value>,
    pub error: Option<String>,
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
pub async fn dispatch(c: &Context, action: Action, cancel: &Cancel) -> Result<Value> {
    match action {
        Action::Hello => {
            Ok(json!({"version":HELPER_VERSION,"arch":std::env::consts::ARCH,"protocol":PROTOCOL}))
        }
        Action::Scan => {
            let mut r = adapter::discover(c, adapter::versions(cancel).await);
            adapter::verify_codex(c, &mut r, cancel).await;
            Ok(serde_json::to_value(r)?)
        }
        Action::Prepare {
            target,
            intent,
            copy,
        } => {
            let version = adapter::version(target.provider, cancel).await;
            Ok(serde_json::to_value(operations::prepare(
                c,
                target,
                intent,
                version.as_deref(),
                copy,
            )?)?)
        }
        Action::Current(t) => Ok(serde_json::to_value(operations::snapshot(&t.path(c)?)?)?),
        Action::Read(asset) => read_registration(c, &asset, adapter::versions(cancel).await),
        Action::Import { path, kind } => {
            operations::check_path(&path)?;
            match kind {
                Kind::Skill => Ok(serde_json::to_value(operations::snapshot(&path)?)?),
                Kind::Mcp => {
                    let b = adapter::read(&path)?;
                    adapter::parse(&path, std::str::from_utf8(&b)?)
                }
                _ => bail!("Read-only asset type"),
            }
        }
        Action::Apply {
            plan,
            backup_id,
            approved_hash,
        } => {
            ensure!(
                uuid::Uuid::parse_str(&backup_id).is_ok()
                    && hash(&serde_json::to_vec(&plan)?) == approved_hash,
                "Missing approved snapshot"
            );
            let version = adapter::version(plan.target.provider, cancel).await;
            operations::apply_checked(c, &plan, version.as_deref())?;
            Ok(json!(true))
        }
        Action::Diagnose { asset, approved } => {
            ensure!(approved, "Explicit approval required");
            let v = read_registration(c, &asset, adapter::versions(cancel).await)?;
            let names = diagnostics::diagnose(
                asset.provider,
                &v,
                &c.project,
                true,
                cancel,
                Duration::from_secs(12),
            )
            .await?;
            Ok(json!(names))
        }
    }
}
pub async fn serve(req: Request, cancel: &Cancel) -> Reply {
    let id = req.id;
    let work = async {
        ensure!(
            req.protocol == PROTOCOL && uuid::Uuid::parse_str(&id).is_ok(),
            "Incompatible protocol or invalid request ID"
        );
        dispatch(&req.context, req.action, cancel).await
    };
    match work.await {
        Ok(data) => Reply {
            protocol: PROTOCOL,
            id,
            data: Some(data),
            error: None,
        },
        Err(e) => Reply {
            protocol: PROTOCOL,
            id,
            data: None,
            error: Some(redact::text(&e.to_string())),
        },
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Endpoint {
    pub context: Context,
    pub distro: Option<String>,
    pub helper: Option<String>,
    pub helper_hash: Option<String>,
}
impl Endpoint {
    pub async fn call(&self, action: Action, cancel: &Cancel) -> Result<Value> {
        if self.distro.is_none() {
            return dispatch(&self.context, action, cancel).await;
        }
        let distro = self.distro.as_ref().unwrap();
        let helper = self.helper.as_ref().context("Helper unavailable")?;
        let expected = self
            .helper_hash
            .as_deref()
            .context("Missing Helper integrity record")?;
        let actual = wsl_text(distro, &["sha256sum", "--", helper], cancel).await?;
        ensure!(
            actual.split_whitespace().next() == Some(expected),
            "Helper changed; reconnect before continuing"
        );
        let id = uuid::Uuid::new_v4().to_string();
        let mut input = serde_json::to_vec(&Request {
            protocol: PROTOCOL,
            id: id.clone(),
            context: self.context.clone(),
            action,
        })?;
        input.push(b'\n');
        ensure!(input.len() <= LIMIT, "Request exceeds protocol limit");
        let mut cmd = wsl(distro)?;
        cmd.args(["--exec", helper.as_str(), "--leased"]);
        let bytes = processes::rpc(&mut cmd, input, LIMIT, 45, cancel).await?;
        let reply: Reply = serde_json::from_slice(&bytes).context("Invalid Helper reply")?;
        ensure!(
            reply.protocol == PROTOCOL && reply.id == id,
            "Mismatched Helper reply"
        );
        reply.data.ok_or_else(|| {
            anyhow::anyhow!("{}", reply.error.unwrap_or_else(|| "Helper failed".into()))
        })
    }
    pub async fn scan(&self, cancel: &Cancel) -> Result<Registry> {
        Ok(serde_json::from_value(
            self.call(Action::Scan, cancel).await?,
        )?)
    }
    pub async fn current(&self, t: Target, cancel: &Cancel) -> Result<Image> {
        Ok(serde_json::from_value(
            self.call(Action::Current(t), cancel).await?,
        )?)
    }
}
fn wsl_command() -> Command {
    #[cfg(windows)]
    {
        return Command::new(
            std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("C:/Windows"))
                .join("System32/wsl.exe"),
        );
    }
    #[cfg(not(windows))]
    Command::new("wsl.exe")
}
pub fn wsl(distro: &str) -> Result<Command> {
    ensure!(
        !distro.is_empty() && !distro.starts_with('-') && !distro.chars().any(char::is_control),
        "Invalid WSL distribution"
    );
    let mut cmd = wsl_command();
    cmd.args(["--distribution", distro]);
    Ok(cmd)
}
pub async fn wsl_text(d: &str, args: &[&str], cancel: &Cancel) -> Result<String> {
    let mut cmd = wsl(d)?;
    cmd.arg("--exec").args(args);
    let out = processes::run(&mut cmd, vec![], 65536, 12, cancel).await?;
    Ok(String::from_utf8(out)?.trim().to_string())
}
pub async fn distros(cancel: &Cancel) -> Result<Vec<String>> {
    let mut cmd = wsl_command();
    cmd.args(["--list", "--quiet"]);
    let bytes = processes::run(&mut cmd, vec![], 65536, 15, cancel).await?;
    let text = if bytes.contains(&0) {
        String::from_utf16_lossy(
            &bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>(),
        )
    } else {
        String::from_utf8(bytes)?
    };
    Ok(text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect())
}
pub async fn wsl_setup(
    d: &str,
    project: &str,
    bin: &[u8],
    expected: &str,
    approved: bool,
    cancel: &Cancel,
) -> Result<Endpoint> {
    let home = wsl_text(d, &["printenv", "HOME"], cancel).await?;
    ensure!(
        home.starts_with('/') && !home.contains('\n'),
        "Invalid WSL home"
    );
    ensure!(
        wsl_text(d, &["id", "-u"], cancel).await? != "0",
        "Helper requires a non-root WSL user"
    );
    let folder = format!("{home}/.local/lib/agenthub");
    let path = format!("{folder}/agenthub-wsl-helper");
    ensure!(
        wsl_text(d, &["readlink", "-m", &path], cancel).await? == path,
        "Helper path contains a symbolic link"
    );
    ensure!(
        !bin.is_empty() && hash(bin) == expected,
        "No integrity-checked Helper bundled for this architecture"
    );
    let checksum = |x: &str| x.split_whitespace().next() == Some(expected);
    if !wsl_text(d, &["sha256sum", &path], cancel)
        .await
        .is_ok_and(|s| checksum(&s))
    {
        ensure!(approved, "Helper installation approval required");
        let mut file = tempfile::NamedTempFile::new()?;
        std::io::Write::write_all(&mut file, bin)?;
        file.as_file().sync_all()?;
        let src = wsl_text(
            d,
            &[
                "wslpath",
                "-a",
                "-u",
                file.path()
                    .to_str()
                    .context("Invalid temporary path")?
                    .trim_start_matches(r"\\?\"),
            ],
            cancel,
        )
        .await?;
        wsl_text(d, &["mkdir", "-p", "--", &folder], cancel).await?;
        let stage = format!("{folder}/.{}", uuid::Uuid::new_v4());
        wsl_text(d, &["install", "-m", "700", "--", &src, &stage], cancel).await?;
        ensure!(
            checksum(&wsl_text(d, &["sha256sum", &stage], cancel).await?),
            "Helper integrity check failed"
        );
        wsl_text(d, &["mv", "-f", "--", &stage, &path], cancel).await?;
    }
    let canonical = wsl_text(d, &["realpath", "-e", "--", project], cancel).await?;
    wsl_text(d, &["test", "-d", &canonical], cancel).await?;
    let mut context = Context::isolated(home.clone().into(), canonical.into(), format!("WSL/{d}"));
    // Join Linux paths on the Linux side; the Windows host uses a different separator.
    context.codex = format!("{home}/.codex").into();
    context.claude = format!("{home}/.claude").into();
    context.opencode = format!("{home}/.config/opencode").into();
    context.policy = "/etc/claude-code".into();
    let ep = Endpoint {
        context,
        distro: Some(d.into()),
        helper: Some(path),
        helper_hash: Some(expected.to_owned()),
    };
    let hello = ep.call(Action::Hello, cancel).await?;
    ensure!(
        hello["version"] == HELPER_VERSION && hello["protocol"] == PROTOCOL,
        "Incompatible WSL Helper"
    );
    Ok(ep)
}
pub async fn shared_host_ids(registry: &mut Registry, ep: &Endpoint, cancel: &Cancel) {
    let Some(d) = &ep.distro else { return };
    let mut seen = std::collections::BTreeMap::new();
    for asset in &mut registry.assets {
        if let Some(id) = seen.get(&asset.path) {
            asset.physical = String::clone(id);
            continue;
        }
        let Some(path) = asset.path.to_str() else {
            continue;
        };
        if let Ok(win) = wsl_text(d, &["wslpath", "-w", path], cancel).await {
            let p = Path::new(&win);
            if p.is_file() {
                let id = adapter::identity(p, "Windows");
                seen.insert(asset.path.clone(), id.clone());
                asset.physical = id;
            }
        }
    }
}
