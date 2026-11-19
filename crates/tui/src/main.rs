mod setup;
mod ui;
mod workflow;
use agenthub_core::{adapter, model::Context, processes::cancel_token};
use anyhow::{ensure, Context as _, Result};
use std::{fs, io::IsTerminal, path::PathBuf};
fn main() {
    if let Err(e) = start() {
        eprintln!("AgentHub: {}", agenthub_core::redact::text(&e.to_string()));
        std::process::exit(1)
    }
}
fn start() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let has = |s: &str| args.iter().any(|x| x == s);
    let arg = |k: &str| {
        args.iter()
            .position(|x| x == k)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
    };
    if has("--version") {
        println!("agenthub {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if has("--help") {
        println!("agenthub [--project PATH] [--setup]\nagenthub --scan [--project PATH] [--fixture-home PATH]\nagenthub --probe-wsl --distro NAME --wsl-project PATH [--approve-install]\nagenthub --self-test-dpapi");
        return Ok(());
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(3)
        .build()?;
    if has("--probe-wsl") {
        let w = setup::Wsl {
            distro: arg("--distro").context("Missing distribution")?.into(),
            project: arg("--wsl-project").context("Missing WSL project")?.into(),
        };
        let ep = rt.block_on(setup::connect(&w, has("--approve-install")))?;
        let r = rt.block_on(ep.scan(&cancel_token()))?;
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"environment":ep.context.environment,"versions":r.versions,"asset_count":r.assets.len(),"issues":r.issues})
            )?
        );
        return Ok(());
    }
    if has("--self-test-dpapi") {
        return workflow::dpapi_smoke();
    }
    let project = arg("--project")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()?;
    if has("--scan") {
        let c = if let Some(home) = arg("--fixture-home") {
            Context::isolated(
                PathBuf::from(home).canonicalize()?,
                project,
                "fixture".into(),
            )
        } else {
            Context::local(&project)?
        };
        let v = rt.block_on(adapter::versions(&cancel_token()));
        let mut r = adapter::discover(&c, v);
        rt.block_on(adapter::verify_codex(&c, &mut r, &cancel_token()));
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }
    ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "Interactive terminal required; try --scan"
    );
    let state = setup::state_dir()?;
    let file = state.join("settings-v1.json");
    let settings = if has("--setup") || !file.exists() {
        let s = setup::wizard(&rt, &project)?;
        setup::save(&file, &s)?;
        s
    } else {
        let mut s: setup::Settings = serde_json::from_slice(&fs::read(file)?)?;
        ensure!(s.version == 1, "Unsupported settings version");
        if arg("--project").is_some() {
            s.project = project;
            s.wsl = None
        }
        s
    };
    let (mut eps, issues) = setup::endpoints(&rt, &settings)?;
    if let Some(home) = arg("--fixture-home") {
        let home = PathBuf::from(home).canonicalize()?;
        let tmp = std::env::temp_dir().canonicalize()?;
        ensure!(home.starts_with(&tmp)&&settings.project.starts_with(&tmp)&&home.parent()==settings.project.parent(),"Interactive fixtures require sibling home and project folders in the OS temp directory");
        eps[0].context = Context::isolated(
            home,
            settings.project.clone(),
            eps[0].context.environment.clone(),
        );
    }
    ui::run(rt, eps, issues, state, settings.wsl)
}
