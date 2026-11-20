mod folders;
mod ui;
mod workflow;
use agenthub_core::model::Context;
use anyhow::{bail, ensure, Context as _, Result};
use std::{io::IsTerminal, path::PathBuf};

const USAGE: &str = "사용법: agenthub [폴더]\n폴더를 생략하면 현재 디렉터리에서 시작합니다.";
fn main() {
    if let Err(e) = start() {
        eprintln!("AgentHub: {}", agenthub_core::redact::text(&e.to_string()));
        std::process::exit(1)
    }
}
fn start() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    ensure!(args.next().is_none(), "{USAGE}");
    let project = match first.as_deref().and_then(|s| s.to_str()) {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return Ok(());
        }
        Some("-V" | "--version") => {
            println!("agenthub {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(s) if s.starts_with('-') => bail!("알 수 없는 옵션 {s}\n{USAGE}"),
        _ => first.map(PathBuf::from).unwrap_or(std::env::current_dir()?),
    };
    let ctx = Context::local(&project)?;
    ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "대화형 터미널에서 실행하세요"
    );
    let state = std::env::var_os("AGENTHUB_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::state_dir().map(|p| p.join("agenthub")))
        .context("사용자 상태 디렉터리를 찾을 수 없습니다")?;
    ensure!(state.is_absolute(), "상태 디렉터리는 절대 경로여야 합니다");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(3)
        .build()?;
    ui::run(rt, ctx, state)
}
