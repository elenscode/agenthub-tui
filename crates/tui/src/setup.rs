use agenthub_core::{
    model::Context,
    processes::cancel_token,
    redact,
    runtime::{self, Endpoint},
};
use anyhow::{ensure, Context as _, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};
const BUNDLE: &[(&str, &[u8], &str)] = include!(concat!(env!("OUT_DIR"), "/bundle.rs"));
#[derive(Clone, Serialize, Deserialize)]
pub struct Wsl {
    pub distro: String,
    pub project: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    pub version: u32,
    pub project: PathBuf,
    pub wsl: Option<Wsl>,
}
pub fn state_dir() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("AGENTHUB_STATE_DIR") {
        let path = PathBuf::from(p);
        ensure!(path.is_absolute(), "State directory must be absolute");
        return Ok(path);
    }
    Ok(dirs::data_local_dir()
        .context("Local application directory unavailable")?
        .join("AgentHub"))
}
fn ask(prompt: &str) -> Result<String> {
    print!("{prompt} ");
    io::stdout().flush()?;
    let mut s = String::new();
    ensure!(io::stdin().read_line(&mut s)? > 0, "Input closed");
    Ok(s.trim().to_string())
}
pub fn save(file: &Path, settings: &Settings) -> Result<()> {
    let dir = file.parent().unwrap();
    fs::create_dir_all(dir)?;
    let mut f = tempfile::NamedTempFile::new_in(dir)?;
    f.write_all(&serde_json::to_vec_pretty(settings)?)?;
    f.as_file().sync_all()?;
    f.persist(file)?;
    Ok(())
}
pub fn wizard(rt: &tokio::runtime::Runtime, default: &Path) -> Result<Settings> {
    println!("AgentHub · 초기 설정\n프로젝트 하나와 이 프로젝트에서 확인할 WSL 환경을 선택합니다.");
    let v = rt.block_on(agenthub_core::adapter::versions(&cancel_token()));
    for (p, x) in v {
        println!("{p:?}  {}", x.as_deref().unwrap_or("Unknown"));
    }
    let path = ask(&format!(
        "Windows/호스트 프로젝트 [{}]:",
        redact::text(&default.display().to_string())
    ))?;
    let project = if path.is_empty() {
        default.into()
    } else {
        PathBuf::from(path)
    }
    .canonicalize()?;
    ensure!(project.is_dir(), "Expected a project directory");
    let mut wsl = None;
    if cfg!(windows) {
        match rt.block_on(runtime::distros(&cancel_token())) {
            Ok(names) if !names.is_empty() => {
                for (i, n) in names.iter().enumerate() {
                    println!("{}  {}", i + 1, redact::text(n))
                }
                let answer = ask("WSL 배포판 번호 (Enter = Windows만):")?;
                if !answer.is_empty() {
                    let d = names
                        .get(
                            answer
                                .parse::<usize>()?
                                .checked_sub(1)
                                .context("Invalid number")?,
                        )
                        .context("Unknown distribution")?
                        .clone();
                    let default = rt
                        .block_on(runtime::wsl_text(
                            &d,
                            &[
                                "wslpath",
                                "-u",
                                project
                                    .to_str()
                                    .context("Invalid project path")?
                                    .trim_start_matches(r"\\?\"),
                            ],
                            &cancel_token(),
                        ))
                        .unwrap_or_default();
                    let s = ask(&format!("WSL 프로젝트 절대 경로 [{default}]:"))?;
                    let p = if s.is_empty() { default } else { s };
                    ensure!(
                        p.starts_with('/'),
                        "Expected an absolute Linux project path"
                    );
                    wsl = Some(Wsl {
                        distro: d,
                        project: p,
                    });
                }
            }
            _ => println!("WSL 배포판 목록을 읽을 수 없습니다. Windows에서 계속합니다."),
        }
    }
    Ok(Settings {
        version: 1,
        project,
        wsl,
    })
}
pub async fn connect(w: &Wsl, approved: bool) -> Result<Endpoint> {
    let c = cancel_token();
    let arch = runtime::wsl_text(&w.distro, &["uname", "-m"], &c).await?;
    let (_, image, hash) = BUNDLE
        .iter()
        .find(|(a, _, _)| *a == arch)
        .context("Unsupported WSL architecture")?;
    runtime::wsl_setup(&w.distro, &w.project, image, hash, approved, &c).await
}
pub fn endpoints(
    rt: &tokio::runtime::Runtime,
    s: &Settings,
) -> Result<(Vec<Endpoint>, Vec<String>)> {
    let mut eps = vec![Endpoint {
        context: Context::local(&s.project)?,
        distro: None,
        helper: None,
        helper_hash: None,
    }];
    let mut issues = vec![];
    if let Some(w) = &s.wsl {
        let mut result = rt.block_on(connect(w, false));
        if result
            .as_ref()
            .is_err_and(|e| e.to_string().contains("installation approval"))
        {
            let reply = ask(&format!(
                "{}에 번들 Helper를 설치하거나 교체합니다. 승인하려면 INSTALL 입력:",
                redact::text(&w.distro)
            ))?;
            if reply == "INSTALL" {
                result = rt.block_on(connect(w, true))
            }
        }
        match result {
            Ok(ep) => eps.push(ep),
            Err(e) => issues.push(format!(
                "WSL/{}: {}",
                redact::text(&w.distro),
                redact::text(&e.to_string())
            )),
        }
    }
    Ok((eps, issues))
}
