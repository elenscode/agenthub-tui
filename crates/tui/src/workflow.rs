use agenthub_core::{
    adapter,
    model::*,
    operations::{self, Intent, Plan, Target, Vault},
    processes::Cancel,
    protection::UserProtection,
    redact,
    runtime::Source,
};
use anyhow::{bail, ensure, Context as _, Result};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Op {
    NewMcp,
    NewSkill,
    Edit,
    Clone,
    Delete,
    ImportMcp,
    ImportSkill,
    Convert,
    Restore,
}
#[derive(Clone)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub value: String,
}
#[derive(Clone)]
pub struct Form {
    pub op: Op,
    pub fields: Vec<Field>,
    pub cursor: usize,
    pub source: Option<Asset>,
    pub markdown: Option<String>,
}
impl Form {
    pub fn new(op: Op, asset: Option<Asset>) -> Result<Self> {
        let needs = matches!(op, Op::Edit | Op::Clone | Op::Delete | Op::Convert);
        let src = if needs {
            Some(asset.context("Select an asset")?)
        } else {
            None
        };
        if let Some(a) = &src {
            ensure!(
                matches!(a.kind, Kind::Mcp | Kind::Skill),
                "This asset type is read-only"
            );
            if matches!(op, Op::Delete | Op::Edit) {
                ensure!(
                    a.permission == Permission::Write,
                    "Read-only, managed or unverified source"
                )
            }
            if op == Op::Convert {
                ensure!(a.kind == Kind::Skill, "Select a Skill to convert")
            }
        }
        let mut f = Self {
            op,
            fields: vec![],
            cursor: 0,
            source: src.clone(),
            markdown: None,
        };
        let field = |key, label, value: String| Field { key, label, value };
        if op == Op::Restore {
            f.fields.push(field("id", "백업 ID", String::new()));
            f.fields.push(field(
                "resource",
                "Skill 내부 상대 경로 (빈 값은 작업 전체)",
                String::new(),
            ));
            return Ok(f);
        }
        if !matches!(op, Op::Edit | Op::Delete) {
            f.fields.extend([
                field(
                    "provider",
                    "CLI (codex / opencode / claude)",
                    src.as_ref()
                        .map(|a| a.provider.binary())
                        .unwrap_or("codex")
                        .into(),
                ),
                field("scope", "범위 (project / global)", "project".into()),
                field(
                    "name",
                    "대상 이름",
                    src.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
                ),
            ]);
        }
        if op == Op::NewMcp || op == Op::Edit && src.as_ref().is_some_and(|a| a.kind == Kind::Mcp) {
            f.fields.extend([
                field("bin", "실행 파일 또는 HTTP(S) URL", String::new()),
                field(
                    "args",
                    "인자 (JSON 배열, 빈 값은 기존 값 유지)",
                    String::new(),
                ),
            ]);
        }
        if op == Op::NewSkill {
            f.fields.push(field("desc", "Skill 설명", String::new()));
        }
        if matches!(op, Op::ImportSkill | Op::ImportMcp) {
            f.fields
                .push(field("path", "원본 절대 경로", String::new()));
        }
        Ok(f)
    }
    pub(crate) fn get(&self, k: &str) -> &str {
        self.fields
            .iter()
            .find(|f| f.key == k)
            .map(|f| f.value.trim())
            .unwrap_or("")
    }
}
#[derive(Clone)]
pub struct Guard {
    source: Source,
    digest: String,
}
pub struct Review {
    pub preview: String,
    pub plan: Plan,
    pub context: Context,
    pub guards: Vec<Guard>,
    pub impact: Vec<String>,
}
async fn guarded(
    c: &Context,
    source: Source,
    guards: &mut Vec<Guard>,
    cancel: &Cancel,
) -> Result<Value> {
    let value = source.read(c, cancel).await?;
    guards.push(Guard {
        source,
        digest: hash(&serde_json::to_vec(&value)?),
    });
    Ok(value)
}
pub async fn plan(
    f: Form,
    c: &Context,
    r: &Registry,
    state: &Path,
    cancel: &Cancel,
) -> Result<Review> {
    if f.op == Op::Restore {
        let vault = Vault {
            dir: state.join("backups"),
            protection: UserProtection::new(state),
        };
        let p = vault.load(f.get("id"))?;

        ensure!(
            c.environment == p.environment && c.project == p.project,
            "Select the snapshot's project"
        );
        let current = operations::snapshot(&p.target.path(c)?)?;
        let p = operations::restore_selected(
            p,
            current,
            (!f.get("resource").is_empty()).then(|| f.get("resource")),
        )?;
        return Ok(review(p, c.clone(), vec![], r));
    }
    let mut guards = vec![];
    let src = f.source.as_ref();
    let fixed = matches!(f.op, Op::Edit | Op::Delete);
    let target = if fixed {
        let a = src.unwrap();
        Target {
            provider: a.provider,
            kind: a.kind,
            scope: a.scope,
            name: a.name.clone(),
        }
    } else {
        let provider = Provider::ALL
            .into_iter()
            .find(|p| p.binary() == f.get("provider"))
            .context("Invalid provider")?;
        let scope = match f.get("scope") {
            "project" => Scope::Project,
            "global" => Scope::Global,
            _ => bail!("Invalid scope"),
        };
        Target {
            kind: src.map(|a| a.kind).unwrap_or(
                if matches!(f.op, Op::NewSkill | Op::ImportSkill) {
                    Kind::Skill
                } else {
                    Kind::Mcp
                },
            ),
            provider,
            scope,
            name: f.get("name").into(),
        }
    };
    let mut source = Value::Null;
    if let Some(a) = src {
        source = guarded(
            c,
            Source::Registered(Box::new(a.clone())),
            &mut guards,
            cancel,
        )
        .await?;
    }
    if matches!(f.op, Op::ImportMcp | Op::ImportSkill) {
        source = guarded(
            c,
            Source::Import {
                path: PathBuf::from(f.get("path")),
                kind: target.kind,
            },
            &mut guards,
            cancel,
        )
        .await?
    }
    let copy = matches!(
        f.op,
        Op::Clone | Op::Convert | Op::ImportSkill | Op::ImportMcp
    );
    let intent = match f.op {
        Op::Delete => Intent::Delete,
        Op::Edit if target.kind == Kind::Skill => {
            Intent::Markdown(f.markdown.context("Editor did not return a draft")?)
        }
        Op::Clone | Op::Convert | Op::ImportSkill | Op::ImportMcp if target.kind == Kind::Skill => {
            Intent::Skill {
                tree: serde_json::from_value(source)?,
                replace: false,
            }
        }
        Op::Clone | Op::ImportMcp => {
            ensure!(
                src.is_none_or(|a| a.provider == target.provider),
                "MCP cloning requires the same CLI schema"
            );
            let native = source
                .get(target.provider.mcp_key())
                .and_then(|m| m.get(&target.name))
                .cloned()
                .unwrap_or(source);
            Intent::Create(native)
        }
        Op::NewSkill => {
            ensure!(!f.get("desc").is_empty(), "Enter a description");
            Intent::Skill {
                tree: operations::skill_seed(&target.name, f.get("desc")),
                replace: false,
            }
        }
        Op::NewMcp | Op::Edit => {
            let mut v = json!({});
            let bin = f.get("bin");
            let args = if f.get("args").is_empty() {
                None
            } else {
                Some(
                    serde_json::from_str::<Vec<String>>(f.get("args"))
                        .map_err(|_| anyhow::anyhow!("Arguments must be a JSON string array"))?,
                )
            };
            if bin.starts_with("https://") || bin.starts_with("http://") {
                ensure!(args.is_none(), "HTTP transport takes no process arguments");
                v["url"] = json!(bin);
                if target.provider != Provider::Codex {
                    v["type"] = json!(if target.provider == Provider::OpenCode {
                        "remote"
                    } else {
                        "http"
                    })
                }
            } else if target.provider == Provider::OpenCode {
                if !bin.is_empty() || args.is_some() {
                    let old = source["command"].as_array();
                    let name = if bin.is_empty() {
                        old.and_then(|a| a.first())
                            .and_then(Value::as_str)
                            .context("Enter executable")?
                    } else {
                        bin
                    };
                    let mut cmd = vec![json!(name)];
                    if let Some(a) = args {
                        cmd.extend(a.into_iter().map(Value::String))
                    } else if let Some(old) = old {
                        cmd.extend(old.iter().skip(1).cloned())
                    }
                    v["command"] = Value::Array(cmd);
                    v["type"] = json!("local")
                }
            } else {
                if !bin.is_empty() {
                    v["command"] = json!(bin)
                }
                if let Some(args) = args {
                    v["args"] = json!(args)
                }
            }
            if f.op == Op::Edit {
                Intent::Edit(v)
            } else {
                Intent::Create(v)
            }
        }
        _ => bail!("Unsupported action"),
    };
    let version = adapter::version(target.provider, cancel).await;
    let p = operations::prepare(c, target, intent, version.as_deref(), copy)?;
    Ok(review(p, c.clone(), guards, r))
}
fn review(plan: Plan, context: Context, guards: Vec<Guard>, r: &Registry) -> Review {
    let t = &plan.target;
    let direct = r
        .assets
        .iter()
        .filter(|a| {
            a.environment == plan.environment
                && a.provider == t.provider
                && a.scope == t.scope
                && a.kind == t.kind
                && (a.path == plan.path || a.name == t.name)
        })
        .map(|a| a.physical.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let impact = r
        .assets
        .iter()
        .filter(|a| direct.contains(&a.physical))
        .map(|a| {
            format!(
                "{} · {:?} · {:?} · {}",
                a.environment, a.provider, a.scope, a.name
            )
        })
        .collect();
    Review {
        preview: plan.preview(),
        plan,
        context,
        guards,
        impact,
    }
}
pub async fn commit(
    review: Review,
    state: &Path,
    approved: bool,
    credentials: bool,
    cancel: &Cancel,
) -> Result<String> {
    let vault = Vault {
        dir: state.join("backups"),
        protection: UserProtection::new(state),
    };
    let p = review.plan;
    let (id, _lock) = vault.record(&p, approved, credentials)?;
    audit(state, "backup_verified", Some(&id))?;
    for g in review.guards {
        let raw = g.source.read(&review.context, cancel).await?;
        ensure!(
            hash(&serde_json::to_vec(&raw)?) == g.digest,
            "Source changed; review again"
        );
    }
    let version = adapter::version(p.target.provider, cancel).await;
    let res = operations::apply_checked(&review.context, &p, version.as_deref());
    let outcome = match &res {
        Ok(_) => "applied",
        Err(e) if e.to_string().contains("Partial recovery") => "partial_recovery",
        Err(e) if e.to_string().contains("rolled back") => "rolled_back",
        Err(_) => "apply_failed",
    };
    let audited = audit(state, outcome, Some(&id));
    if let Err(e) = res {
        bail!("{e} · snapshot {id}")
    }
    if audited.is_err() {
        return Ok(format!(
            "적용 및 재검증 완료 · 감사 로그 저장 실패 · 백업 {id}"
        ));
    }
    Ok(format!("적용 및 재검증 완료 · 백업 {id}"))
}
pub fn audit(state: &Path, event: &str, id: Option<&str>) -> Result<()> {
    fs::create_dir_all(state)?;
    let path = state.join("audit.jsonl");
    operations::check_path(&path)?;
    if path.metadata().is_ok_and(|m| m.len() > 512 * 1024) {
        let old = state.join("audit.previous.jsonl");
        if old.exists() {
            fs::remove_file(&old)?
        }
        fs::rename(&path, old)?;
    }
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)?;
    writeln!(
        file,
        "{}",
        json!({"time":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),"event":event,"snapshot":id})
    )?;
    Ok(())
}
pub fn export(state: &Path, r: &Registry) -> Result<PathBuf> {
    fs::create_dir_all(state)?;
    let mut f = tempfile::Builder::new()
        .prefix("diagnostics-")
        .suffix(".json")
        .tempfile_in(state)?;
    let data = json!({"version":1,"asset_count":r.assets.len(),"issue_count":r.issues.len(),"cli_versions":r.versions});
    f.write_all(&serde_json::to_vec_pretty(&data)?)?;
    let (_, path) = f.keep()?;
    audit(state, "diagnostics_exported", None)?;
    Ok(path)
}
pub fn edit_markdown(s: &str) -> Result<String> {
    let mut f = tempfile::Builder::new().suffix(".md").tempfile()?;
    f.write_all(s.as_bytes())?;
    f.flush()?;
    let path = f.into_temp_path();
    let editor = std::env::var_os("VISUAL")
        .or_else(|| std::env::var_os("EDITOR"))
        .unwrap_or_else(|| "vi".into());
    let status = std::process::Command::new(editor).arg(&path).status()?;
    ensure!(status.success(), "Editor returned unsuccessfully");
    Ok(String::from_utf8(agenthub_core::adapter::read(&path)?)?)
}
pub fn history(state: &Path) -> String {
    let vault = Vault {
        dir: state.join("backups"),
        protection: UserProtection::new(state),
    };
    vault
        .history()
        .into_iter()
        .map(|id| {
            let label = vault
                .load(&id)
                .map(|p| format!("{} {:?} {}", p.environment, p.target.kind, p.target.name))
                .unwrap_or_else(|_| "unavailable".into());
            format!("{id}  {}", redact::text(&label))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod live_flow {
    use super::*;
    use agenthub_core::{processes::cancel_token, runtime};
    #[tokio::test]
    #[ignore = "requires installed, pinned Codex and Claude CLIs"]
    async fn live_tui_creation_edit_clone_import_conversion_and_delete() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        let project = root.join("project");
        let home = root.join("home");
        fs::create_dir_all(&project)?;
        fs::create_dir_all(&home)?;
        let ctx = Context::isolated(home, project, "fixture".into());
        let c = cancel_token();
        let state = root.join("state");
        let imported = root.join("import.json");
        fs::write(
            &imported,
            r#"{"command":"fixture-only","env":{"TOKEN":"synthetic-value"}}"#,
        )?;
        let steps = [
            (Op::NewMcp, "local", None),
            (Op::Edit, "local", Some("local")),
            (Op::Clone, "copy", Some("local")),
            (Op::ImportMcp, "imported", None),
            (Op::NewSkill, "review", None),
            (Op::Convert, "review-copy", Some("review")),
            (Op::Delete, "copy", Some("copy")),
        ];
        for (op, name, source) in steps {
            let r = runtime::scan(&ctx, &c).await;
            let a = source
                .and_then(|s| {
                    r.assets
                        .iter()
                        .find(|a| a.name == s && a.provider == Provider::Codex)
                })
                .cloned();
            let mut form = Form::new(op, a)?;
            for field in &mut form.fields {
                match field.key {
                    "name" => field.value = name.into(),
                    "bin" => {
                        field.value = if op == Op::Edit {
                            "changed-fixture"
                        } else {
                            "fixture-only"
                        }
                        .into()
                    }
                    "desc" => field.value = "configuration review".into(),
                    "provider" if op == Op::Convert => field.value = "claude".into(),
                    "path" => field.value = imported.to_string_lossy().into_owned(),
                    _ => (),
                }
            }
            let review = plan(form.clone(), &ctx, &r, &state, &c).await?;
            assert!(!review.preview.contains("synthetic-value"));
            if op == Op::ImportMcp {
                assert!(review.plan.credential_copy);
                assert!(commit(review, &state, true, false, &c).await.is_err());
                let accepted = plan(form, &ctx, &r, &state, &c).await?;
                commit(accepted, &state, true, true, &c).await?;
            } else {
                commit(review, &state, true, false, &c).await?;
            }
            let updated = runtime::scan(&ctx, &c).await;
            assert_eq!(
                updated.assets.iter().any(|a| a.name == name),
                op != Op::Delete
            );
        }
        Ok(())
    }
}
