use crate::{
    setup,
    workflow::{self, Form, Op, Review},
};
use agenthub_core::{
    model::*,
    operations::Image,
    processes::{cancel_token, Cancel},
    redact,
    runtime::{Action, Endpoint},
};
use anyhow::{bail, ensure, Context as _, Result};
use crossterm::{
    event::{self, Event, KeyCode as K, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    DefaultTerminal, Frame,
};
use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, mpsc},
    time::{Duration, Instant},
};
use tokio::runtime::Runtime;
const HELP:&str="j/k ↑↓ 이동  ·  Enter 상세  ·  Tab 탭 전환  ·  h/l 그룹 접기/펴기\n/ 이름·설명·본문 검색  ·  Space 비교 기준 선택  ·  r 다시 탐색\n1 CLI  ·  2 종류  ·  3 범위  ·  4 환경  ·  5 상태 (반복해서 전환)\n: 명령 팔레트  ·  x 현재 작업 취소  ·  Esc 뒤로  ·  q 종료\n\nnew-mcp / new-skill     새 자산 만들기\nedit / clone / delete   선택한 자산 관리\nimport-mcp / import-skill    로컬 파일·폴더 가져오기\nconvert                 Skill 폴더를 다른 CLI로 변환\ndiagnose                MCP 연결 및 도구 목록 조회\nhistory / restore       백업 목록 · 선택 복원\nexport                  로컬 진단 요약 내보내기\nretry                   WSL 재연결 및 재탐색";
const COMMANDS: &[&str] = &[
    "new-mcp",
    "new-skill",
    "edit",
    "clone",
    "delete",
    "import-mcp",
    "import-skill",
    "convert",
    "diagnose",
    "history",
    "restore",
    "export",
    "retry",
];
enum View {
    Explore,
    Search(String),
    Command(String),
    Form(Form),
    Review(Box<Review>, bool),
    ConfirmDiag(Asset),
    ConfirmExport,
    Text(String, String),
}
enum Output {
    Review(Box<Review>),
    Committed(String),
    Tools(Vec<String>),
    Retried(Box<Endpoint>),
}
struct Active {
    label: &'static str,
    cancel: Cancel,
    mutable: bool,
}
struct App {
    eps: Vec<Endpoint>,
    startup: Vec<String>,
    state: PathBuf,
    wsl: Option<setup::Wsl>,
    r: Registry,
    unavailable: BTreeSet<String>,
    view: View,
    query: String,
    provider: usize,
    kind: usize,
    scope: usize,
    env: usize,
    status_filter: usize,
    selected: usize,
    tab: usize,
    detail: bool,
    pinned: Option<Asset>,
    collapsed: BTreeSet<Kind>,
    scroll: u16,
    status: String,
    job: Option<Active>,
    pending_scan: bool,
    scanning: Option<(Cancel, usize)>,
    scan_tx: mpsc::Sender<(String, Result<Registry>)>,
    scan_rx: mpsc::Receiver<(String, Result<Registry>)>,
    tx: mpsc::Sender<Result<Output>>,
    rx: mpsc::Receiver<Result<Output>>,
    watched: mpsc::Receiver<()>,
    watcher: Option<RecommendedWatcher>,
    last: Instant,
    changed: Option<Instant>,
}
const KINDS: [Kind; 7] = [
    Kind::Mcp,
    Kind::Skill,
    Kind::Agent,
    Kind::Command,
    Kind::Instructions,
    Kind::Plugin,
    Kind::Hook,
];
const SCOPES: [Scope; 5] = [
    Scope::Global,
    Scope::Project,
    Scope::Plugin,
    Scope::Managed,
    Scope::Reference,
];
fn panel(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::DarkGray))
}
impl App {
    fn new(
        eps: Vec<Endpoint>,
        startup: Vec<String>,
        state: PathBuf,
        wsl: Option<setup::Wsl>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let (scan_tx, scan_rx) = mpsc::channel();
        let (wtx, watched) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            if result.is_ok_and(|e| !matches!(e.kind, notify::EventKind::Access(_))) {
                let _ = wtx.send(());
            }
        })
        .ok();
        let mut a = Self {
            eps,
            startup,
            state,
            wsl,
            r: Registry::default(),
            unavailable: Default::default(),
            view: View::Explore,
            query: String::new(),
            provider: 0,
            kind: 0,
            scope: 0,
            env: 0,
            status_filter: 0,
            selected: 0,
            tab: 0,
            detail: false,
            pinned: None,
            collapsed: Default::default(),
            scroll: 0,
            status: String::new(),
            job: None,
            pending_scan: true,
            scanning: None,
            scan_tx,
            scan_rx,
            tx,
            rx,
            watched,
            watcher,
            last: Instant::now(),
            changed: None,
        };
        a.configure_watches();
        a
    }
    fn configure_watches(&mut self) {
        let Some(w) = &mut self.watcher else { return };
        let c = &self.eps[0].context;
        for path in [&c.project, &c.home] {
            let _ = w.watch(path, RecursiveMode::NonRecursive);
        }
        let mut roots = vec![
            c.codex.clone(),
            c.claude.clone(),
            c.opencode.clone(),
            c.project.join(".agents"),
            c.project.join(".opencode"),
            c.project.join(".claude"),
            c.project.join(".codex"),
            c.home.join(".agents"),
        ];
        roots.extend(
            self.r
                .assets
                .iter()
                .filter(|a| a.environment == c.environment)
                .filter_map(|a| a.path.parent().map(Path::to_path_buf)),
        );
        roots.sort();
        roots.dedup();
        for p in roots {
            if p.is_dir() {
                let mode = if p == c.project || p == c.home {
                    RecursiveMode::NonRecursive
                } else {
                    RecursiveMode::Recursive
                };
                let _ = w.watch(&p, mode);
            }
        }
    }
    fn rows(&self) -> Vec<&Asset> {
        self.r
            .find(
                &self.query,
                self.provider.checked_sub(1).map(|i| Provider::ALL[i]),
                self.kind.checked_sub(1).map(|i| KINDS[i]),
                self.scope.checked_sub(1).map(|i| SCOPES[i]),
                self.env
                    .checked_sub(1)
                    .map(|i| self.eps[i].context.environment.as_str()),
            )
            .into_iter()
            .filter(|a| match self.status_filter {
                1 => a.permission == Permission::Write,
                2 => a.confidence == Confidence::Unknown,
                3 => a.effective == Some(true),
                _ => true,
            })
            .collect()
    }
    fn visible(&self) -> Vec<&Asset> {
        let mut seen = BTreeSet::new();
        self.rows()
            .into_iter()
            .filter(|a| !self.collapsed.contains(&a.kind) || seen.insert(a.kind))
            .collect()
    }
    fn current(&self) -> Option<Asset> {
        self.visible().get(self.selected).copied().cloned()
    }
    fn start<F, Fut>(
        &mut self,
        rt: &Runtime,
        label: &'static str,
        mutable: bool,
        task: F,
    ) -> Result<()>
    where
        F: FnOnce(Cancel) -> Fut,
        Fut: std::future::Future<Output = Result<Output>> + Send + 'static,
    {
        ensure!(self.job.is_none(), "작업 진행 중");
        let cancel = cancel_token();
        let tx = self.tx.clone();
        let future = task(cancel.clone());
        rt.spawn(async move {
            let _ = tx.send(future.await);
        });
        self.job = Some(Active {
            label,
            cancel,
            mutable,
        });
        self.status = label.into();
        Ok(())
    }
    fn scan(&mut self, rt: &Runtime) {
        self.pending_scan = false;
        self.last = Instant::now();
        let cancel = cancel_token();
        self.scanning = Some((cancel.clone(), self.eps.len()));
        for ep in self.eps.clone() {
            let tx = self.scan_tx.clone();
            let c = cancel.clone();
            rt.spawn(async move {
                let label = ep.context.environment.clone();
                let mut scan = ep.scan(&c).await;
                if let Ok(r) = &mut scan {
                    agenthub_core::runtime::shared_host_ids(r, &ep, &c).await;
                }
                let _ = tx.send((label, scan));
            });
        }
    }
    fn scanned(&mut self, env: String, scan: Result<Registry>) {
        let id = self.current().map(|a| a.id);
        self.r.assets.retain(|a| a.environment != env);
        let prefix = format!("{env} · ");
        self.r.issues.retain(|s| !s.starts_with(&prefix));
        for issue in &self.startup {
            if !self.r.issues.contains(issue) {
                self.r.issues.push(issue.clone())
            }
        }
        match scan {
            Ok(r) => {
                self.unavailable.remove(&env);
                if env == self.eps[0].context.environment {
                    self.r.versions = r.versions;
                }
                self.r.assets.extend(r.assets);
                self.r
                    .issues
                    .extend(r.issues.into_iter().map(|i| format!("{prefix}{i}")));
            }
            Err(e) => {
                self.unavailable.insert(env);
                self.r
                    .issues
                    .push(format!("{prefix}{}", redact::text(&e.to_string())));
            }
        }
        self.r.assets.sort_by(|a, b| {
            (a.kind, &a.name, a.provider, &a.environment).cmp(&(
                b.kind,
                &b.name,
                b.provider,
                &b.environment,
            ))
        });
        self.r.reindex();
        self.selected = id
            .and_then(|id| self.visible().iter().position(|a| a.id == id))
            .unwrap_or(0);
        if self.job.is_none() {
            self.status = format!(
                "{}개 자산 · 상태 메시지 {}개",
                self.r.assets.len(),
                self.r.issues.len()
            );
        }
        self.configure_watches();
    }
    fn tick(&mut self, rt: &Runtime) {
        while let Ok((env, r)) = self.scan_rx.try_recv() {
            self.scanned(env, r);
            if let Some((_, remaining)) = &mut self.scanning {
                *remaining -= 1;
                if *remaining == 0 {
                    self.scanning = None;
                }
            }
        }
        if let Ok(result) = self.rx.try_recv() {
            self.job = None;
            match result {
                Err(e) => {
                    self.status = redact::text(&e.to_string());
                    self.view = View::Text("작업 결과".into(), self.status.clone());
                    let _ = workflow::audit(&self.state, "task_failed", None);
                }
                Ok(Output::Review(r)) => {
                    self.scroll = 0;
                    self.view = View::Review(r, false);
                    self.status = "Diff를 검토한 뒤 a 키로 승인하세요".into();
                }
                Ok(Output::Committed(s)) => {
                    self.status = s.clone();
                    self.view = View::Text("작업 결과".into(), s);
                    self.pending_scan = true;
                }
                Ok(Output::Tools(names)) => {
                    self.view = View::Text(
                        "MCP · 연결 성공".into(),
                        format!("도구 {}개\n{}", names.len(), names.join("\n")),
                    );
                }
                Ok(Output::Retried(ep)) => {
                    self.eps.retain(|e| e.distro != ep.distro);
                    self.eps.push(*ep);
                    self.startup.clear();
                    self.pending_scan = true;
                }
            }
        }
        if self.watched.try_iter().count() > 0 {
            self.changed = Some(Instant::now())
        }
        if self
            .changed
            .is_some_and(|i| i.elapsed() > Duration::from_millis(350))
        {
            self.changed = None;
            self.pending_scan = true
        }
        if self.scanning.is_none()
            && self.job.is_none()
            && matches!(self.view, View::Explore | View::Search(_))
            && (self.pending_scan || self.last.elapsed() > Duration::from_secs(30))
        {
            self.scan(rt);
        }
    }
    fn prepare(&mut self, f: Form, rt: &Runtime) -> Result<()> {
        if let Some(src) = &f.source {
            ensure!(
                !self.unavailable.contains(&src.environment),
                "Source environment is offline; use retry"
            );
        }
        if let Ok(index) = f.get("env").parse::<usize>() {
            if let Some(ep) = self.eps.get(index) {
                ensure!(
                    !self.unavailable.contains(&ep.context.environment),
                    "Target environment is offline; use retry"
                );
            }
        }

        let (eps, r, state) = (self.eps.clone(), self.r.clone(), self.state.clone());
        self.start(rt, "Diff 준비 중", false, move |c| async move {
            Ok(Output::Review(Box::new(
                workflow::plan(f, &eps, &r, &state, &c).await?,
            )))
        })?;
        self.view = View::Explore;
        Ok(())
    }
    fn command(&mut self, cmd: &str, rt: &Runtime, terminal: &mut DefaultTerminal) -> Result<()> {
        let source = self.current();
        self.scroll = 0;
        let op = match cmd.trim() {
            "new-mcp" => Op::NewMcp,
            "new-skill" => Op::NewSkill,
            "edit" => Op::Edit,
            "clone" => Op::Clone,
            "delete" => Op::Delete,
            "import-mcp" => Op::ImportMcp,
            "import-skill" => Op::ImportSkill,
            "convert" => Op::Convert,
            "restore" => Op::Restore,
            "diagnose" => {
                let a = source.context("MCP를 선택하세요")?;
                ensure!(a.kind == Kind::Mcp, "MCP를 선택하세요");
                self.view = View::ConfirmDiag(a);
                return Ok(());
            }
            "history" => {
                let list = workflow::history(&self.state);
                self.view = View::Text(
                    "백업 이력 · :restore".into(),
                    if list.is_empty() {
                        "백업 없음".into()
                    } else {
                        list
                    },
                );
                return Ok(());
            }
            "export" => {
                self.view = View::ConfirmExport;
                return Ok(());
            }
            "retry" => {
                if let Some(w) = self.wsl.clone() {
                    self.start(rt, "WSL 재연결 중", false, move |_| async move {
                        Ok(Output::Retried(Box::new(setup::connect(&w, false).await?)))
                    })?;
                } else {
                    self.pending_scan = true
                }
                self.view = View::Explore;
                return Ok(());
            }
            _ => bail!("명령을 찾을 수 없습니다"),
        };
        ensure!(self.job.is_none(), "현재 작업이 끝난 뒤 다시 시도하세요");
        let mut form = Form::new(op, source, &self.eps)?;
        if op == Op::Edit && form.source.as_ref().is_some_and(|a| a.kind == Kind::Skill) {
            let a = form.source.as_ref().unwrap();
            let ep = self
                .eps
                .iter()
                .find(|e| e.context.environment == a.environment)
                .context("Environment unavailable")?;
            let tree: Image = serde_json::from_value(
                rt.block_on(ep.call(Action::Read(a.clone()), &cancel_token()))?,
            )?;
            let bytes = &tree.get("SKILL.md").context("Missing SKILL.md")?.data;
            let raw = std::str::from_utf8(bytes)?;
            disable_raw_mode()?;
            execute!(io::stdout(), LeaveAlternateScreen)?;
            let edited = workflow::edit_markdown(raw);
            execute!(io::stdout(), EnterAlternateScreen)?;
            enable_raw_mode()?;
            terminal.clear()?;
            form.markdown = Some(edited?);
        }
        if form.fields.is_empty() {
            self.prepare(form, rt)
        } else {
            self.view = View::Form(form);
            Ok(())
        }
    }
    fn key(&mut self, key: KeyEvent, rt: &Runtime, term: &mut DefaultTerminal) -> Result<bool> {
        if key.kind != KeyEventKind::Press {
            return Ok(true);
        }
        if key.code == K::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.job.as_ref().is_some_and(|j| j.mutable) {
                self.status = "적용 및 검증 중".into();
                return Ok(true);
            }
            return Ok(false);
        }
        if key.code == K::Esc {
            self.view = View::Explore;
            self.detail = false;
            self.scroll = 0;
            return Ok(true);
        }
        if matches!(key.code, K::PageUp | K::PageDown) {
            if key.code == K::PageUp {
                self.scroll = self.scroll.saturating_sub(10)
            } else {
                self.scroll = self.scroll.saturating_add(10)
            }
            return Ok(true);
        }
        match &mut self.view {
            View::Explore => match key.code {
                K::Char('q') => return Ok(!self.job.as_ref().is_none_or(|j| !j.mutable)),
                K::Down | K::Char('j') => {
                    self.selected = (self.selected + 1).min(self.visible().len().saturating_sub(1));
                    self.scroll = 0
                }
                K::Up | K::Char('k') => {
                    self.selected = self.selected.saturating_sub(1);
                    self.scroll = 0
                }
                K::Char('h') => {
                    if let Some(a) = self.current() {
                        self.collapsed.insert(a.kind);
                        self.selected = self
                            .visible()
                            .iter()
                            .position(|v| v.kind == a.kind)
                            .unwrap_or(0);
                    }
                }
                K::Char('l') => {
                    if let Some(a) = self.current() {
                        self.collapsed.remove(&a.kind);
                    }
                }
                K::Tab | K::Right => {
                    self.tab = (self.tab + 1) % 4;
                    self.scroll = 0
                }
                K::Left => {
                    self.tab = (self.tab + 3) % 4;
                    self.scroll = 0
                }
                K::Enter => self.detail = true,
                K::Char('/') => self.view = View::Search(self.query.clone()),
                K::Char(':') => self.view = View::Command(String::new()),
                K::Char('?') => self.view = View::Text("도움말".into(), HELP.into()),
                K::Char('1') => {
                    self.provider = (self.provider + 1) % 4;
                    self.selected = 0
                }
                K::Char('2') => {
                    self.kind = (self.kind + 1) % 8;
                    self.selected = 0
                }
                K::Char('3') => {
                    self.scope = (self.scope + 1) % 6;
                    self.selected = 0
                }
                K::Char('4') => {
                    self.env = (self.env + 1) % (self.eps.len() + 1);
                    self.selected = 0
                }
                K::Char('5') => {
                    self.status_filter = (self.status_filter + 1) % 4;
                    self.selected = 0
                }
                K::Char(' ') => {
                    self.pinned = self.current();
                    self.status =
                        "비교 기준을 선택했습니다. 다른 자산의 Diff 탭에서 비교하세요".into()
                }
                K::Char('r') => self.pending_scan = true,
                K::Char('x') => {
                    if let Some(j) = &self.job {
                        if !j.mutable {
                            j.cancel.store(true, Ordering::Relaxed)
                        }
                    }
                }
                K::Char('!') => {
                    self.view = View::Text("환경 상태".into(), self.r.issues.join("\n"))
                }
                _ => (),
            },
            View::Search(buf) => {
                match key.code {
                    K::Enter => {
                        self.view = View::Explore;
                        return Ok(true);
                    }
                    K::Backspace => {
                        buf.pop();
                    }
                    K::Char(c) => buf.push(c),
                    _ => (),
                }
                self.query = buf.clone();
                self.selected = 0;
            }
            View::Command(buf) => match key.code {
                K::Backspace => {
                    buf.pop();
                }
                K::Char(c) => buf.push(c),
                K::Tab => {
                    if let Some(s) = COMMANDS.iter().find(|s| s.starts_with(buf.as_str())) {
                        *buf = s.to_string()
                    }
                }
                K::Enter => {
                    let cmd = buf.clone();
                    self.view = View::Explore;
                    self.command(&cmd, rt, term)?;
                }
                _ => (),
            },
            View::Form(f) => match key.code {
                K::Tab | K::Down => f.cursor = (f.cursor + 1) % f.fields.len(),
                K::BackTab | K::Up => f.cursor = (f.cursor + f.fields.len() - 1) % f.fields.len(),
                K::Backspace => {
                    f.fields[f.cursor].value.pop();
                }
                K::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    f.fields[f.cursor].value.clear()
                }
                K::Char(c) => f.fields[f.cursor].value.push(c),
                K::Enter => {
                    let form = f.clone();
                    self.prepare(form, rt)?;
                }
                _ => (),
            },
            View::Review(review, credential) => {
                if key.code == K::Char('a') && !*credential && review.plan.credential_copy {
                    *credential = true;
                    return Ok(true);
                }
                if key.code == K::Char('a') && !*credential
                    || key.code == K::Char('y') && *credential
                {
                    let View::Review(r, cred) = std::mem::replace(&mut self.view, View::Explore)
                    else {
                        unreachable!()
                    };
                    let state = self.state.clone();
                    self.start(rt, "백업 · 적용 · 재검증 중", true, move |c| async move {
                        Ok(Output::Committed(
                            workflow::commit(*r, &state, true, cred, &c).await?,
                        ))
                    })?;
                }
            }
            View::ConfirmDiag(a) => {
                if key.code == K::Char('y') {
                    let a = a.clone();
                    let ep = self
                        .eps
                        .iter()
                        .find(|e| e.context.environment == a.environment)
                        .context("Unavailable environment")?
                        .clone();
                    self.start(rt, "MCP 진단 중 · x 취소", false, move |c| async move {
                        let v = ep
                            .call(
                                Action::Diagnose {
                                    asset: a,
                                    approved: true,
                                },
                                &c,
                            )
                            .await?;
                        Ok(Output::Tools(serde_json::from_value(v)?))
                    })?;
                    self.view = View::Explore;
                }
            }
            View::ConfirmExport => {
                if key.code == K::Char('y') {
                    let p = workflow::export(&self.state, &self.r)?;
                    self.view = View::Text("내보내기 완료".into(), p.display().to_string());
                }
            }
            View::Text(_, _) => {
                if key.code == K::Enter {
                    self.view = View::Explore
                }
            }
        }
        Ok(true)
    }
    fn draw(&self, f: &mut Frame) {
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(3),
                Constraint::Length(2),
            ])
            .split(f.area());
        let project = self.eps[0].context.project.display();
        f.render_widget(
            Paragraph::new(format!(
                " AgentHub   {project}\n CLI {}   종류 {}   범위 {}   환경 {}   상태 {}    / {}",
                self.provider
                    .checked_sub(1)
                    .map(|i| Provider::ALL[i].binary())
                    .unwrap_or("All"),
                self.kind
                    .checked_sub(1)
                    .map(|i| format!("{:?}", KINDS[i]))
                    .unwrap_or("All".into()),
                self.scope
                    .checked_sub(1)
                    .map(|i| format!("{:?}", SCOPES[i]))
                    .unwrap_or("All".into()),
                self.env
                    .checked_sub(1)
                    .map(|i| self.eps[i].context.environment.as_str())
                    .unwrap_or("All"),
                ["All", "Write", "Unknown", "Effective"][self.status_filter],
                redact::text(&self.query)
            ))
            .style(Style::default().fg(Color::Cyan)),
            areas[0],
        );
        let footer = format!(
            " {}   {}\n j/k 이동  Tab 상세 탭  / 검색  : 명령  ? 도움말  ! 환경 상태  q 종료",
            self.job
                .as_ref()
                .map(|j| j.label)
                .unwrap_or(if self.scanning.is_some() {
                    "탐색 중"
                } else {
                    ""
                }),
            self.status
        );
        f.render_widget(Paragraph::new(footer), areas[2]);
        let modal=match &self.view{
 View::Text(t,s)=>Some((t.clone(),s.clone())),View::Command(buf)=>Some(("명령 팔레트 · Tab 완성 · Enter 실행".into(),format!(": {buf}\n\n{}",COMMANDS.iter().filter(|s|s.starts_with(buf.as_str())).copied().collect::<Vec<_>>().join("\n")))),
 View::Form(form)=>{let mut s=self.eps.iter().enumerate().map(|(i,e)|format!("{i} = {}",e.context.environment)).collect::<Vec<_>>().join("   ");s.push_str("\n\n");for(i,field)in form.fields.iter().enumerate(){let val=if matches!(field.key,"args"|"bin")&&!field.value.is_empty(){"•".repeat(field.value.chars().count().min(72))}else{redact::text(&field.value)};s.push_str(&format!("{} {}   {}\n",if i==form.cursor{"›"}else{" "},field.label,val))}Some(("편집 폼 · Tab 다음 필드 · Ctrl+U 지우기 · Enter Diff".into(),s))}
 View::Review(r,cred)=>Some((if *cred{"자격증명 복사 별도 승인 · y 승인 · Esc 취소"}else{"변경 미리보기 · PgUp/PgDn · a 승인 · Esc 취소"}.into(),format!("영향 받는 등록\n{}\n\n{}",r.impact.join("\n"),r.preview))),
 View::ConfirmDiag(a)=>Some(("MCP 수동 진단 · y 실행 · Esc 취소".into(),format!("{} · {:?} · {}\n{}\n\n설정된 서버가 시작되거나 원격 서버에 연결됩니다.\n초기화 및 도구 목록 조회 · 12초 제한\n인증이 필요하면 해당 CLI의 공식 인증 흐름을 사용하세요.",a.environment,a.provider,a.name,a.source))),
 View::ConfirmExport=>Some(("진단 요약 내보내기 · y 저장 · Esc 취소".into(),format!("자산 {}개, 오류 수, CLI 버전, 환경 수를 로컬 진단 JSON으로 저장합니다.\n{}",self.r.assets.len(),self.state.display()))),_=>None};
        if let Some((title, content)) = modal {
            self.text(f, areas[1], &title, &content);
            return;
        }
        let narrow = areas[1].width < 88;
        let sections = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(areas[1]);
        if !narrow || !self.detail {
            let visible = self.visible();
            let rows = visible
                .iter()
                .map(|a| {
                    ListItem::new(format!(
                        "{} {:?}  {}\n   {:?} · {} · {:?}",
                        if self.collapsed.contains(&a.kind) {
                            "▸"
                        } else {
                            "▾"
                        },
                        a.kind,
                        a.name,
                        a.provider,
                        a.environment,
                        a.scope
                    ))
                })
                .collect::<Vec<_>>();
            let mut state = ListState::default();
            if !rows.is_empty() {
                state.select(Some(self.selected));
            }
            let title = if matches!(self.view, View::Search(_)) {
                "검색 중 · Enter 완료"
            } else {
                "자산 탐색"
            };
            f.render_stateful_widget(
                List::new(rows)
                    .block(panel(title))
                    .highlight_style(
                        Style::default()
                            .bg(Color::DarkGray)
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("› "),
                if narrow { areas[1] } else { sections[0] },
                &mut state,
            );
        }
        if !narrow || self.detail {
            let area = if narrow { areas[1] } else { sections[1] };
            let content=self.current().map(|a|match self.tab{
 0=>format!("{}\n{}\n\n{:?} · {:?} · {:?}\n{}\n{}\n\nFound: true\nConfigured: {:?}   Effective: {:?}\n{:?} · {:?}\n\n공유 파일 등록 {}\n{}",a.name,a.description,a.kind,a.provider,a.scope,a.environment,a.path.display(),a.configured,a.effective,a.confidence,a.permission,self.r.linked(&a.physical).len(),a.evidence.join("\n")),
 1=>format!("{}  {}\n\n{}",a.path.display(),redact::text(&a.locator),a.source),
 2=>self.pinned.as_ref().map(|base|format!("{} → {}\n{}",base.name,a.name,similar::TextDiff::from_lines(base.source.as_str(),a.source.as_str()).unified_diff())).unwrap_or("Space로 비교 기준을 선택하세요".into()),
 _=>format!("{}\n\n{}\n\n{}",a.evidence.join("\n"),self.r.linked(&a.physical).iter().map(|b|format!("{} · {:?} · {}",b.environment,b.provider,b.name)).collect::<Vec<_>>().join("\n"),self.r.issues.join("\n"))
 }).unwrap_or_else(||format!("검색 결과 없음\n{}",self.r.issues.join("\n")));
            self.text(
                f,
                area,
                ["개요", "원본 (마스킹)", "Diff", "상태"][self.tab],
                &content,
            );
        }
    }
    fn text(&self, f: &mut Frame, area: Rect, title: &str, s: &str) {
        f.render_widget(
            Paragraph::new(s)
                .block(panel(title))
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0)),
            area,
        )
    }
}
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}
pub fn run(
    rt: Runtime,
    eps: Vec<Endpoint>,
    issues: Vec<String>,
    state: PathBuf,
    wsl: Option<setup::Wsl>,
) -> Result<()> {
    let mut app = App::new(eps, issues, state, wsl);
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut term = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;
    loop {
        app.tick(&rt);
        term.draw(|f| app.draw(f))?;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(k) = event::read()? {
                match app.key(k, &rt, &mut term) {
                    Ok(false) => break,
                    Ok(true) => (),
                    Err(e) => {
                        app.status = redact::text(&e.to_string());
                        app.view = View::Text("작업 결과".into(), app.status.clone());
                    }
                }
            }
        }
    }
    if let Some((scan, _)) = app.scanning {
        scan.store(true, Ordering::Relaxed);
    }
    if let Some(job) = app.job {
        job.cancel.store(true, Ordering::Relaxed)
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn at08_wsl_failure_keeps_host_registry_interactive() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Context::isolated(
            fixtures.join("home"),
            fixtures.join("project"),
            "Windows".into(),
        );
        let r = agenthub_core::adapter::discover(
            &ctx,
            Provider::ALL
                .into_iter()
                .map(|p| (p, Some(p.tested().into())))
                .collect(),
        );
        let ep = Endpoint {
            context: ctx,
            distro: None,
            helper: None,
            helper_hash: None,
        };
        let mut app = App::new(vec![ep], vec![], tmp.path().into(), None);
        app.scanned("Windows".into(), Ok(r));
        let count = app.r.assets.len();
        app.scanned(
            "WSL/test".into(),
            Err(anyhow::anyhow!("invalid helper protocol")),
        );
        assert_eq!(app.r.assets.len(), count);
        assert!(app.unavailable.contains("WSL/test"));
        assert!(app.current().is_some());
        assert!(app
            .r
            .issues
            .iter()
            .any(|s| s.contains("invalid helper protocol")));
        for width in [50, 140] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 30)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let rendered = format!("{:?}", terminal.backend().buffer());
            assert!(!rendered.contains("CANARY-ONLY-FOR-TESTS"));
        }
    }
}
