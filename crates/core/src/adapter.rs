use crate::{
    model::*,
    processes::{self, Cancel},
    redact,
};
use anyhow::{ensure, Context as _, Result};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use tokio::process::Command;
use walkdir::WalkDir;

pub const FILE_LIMIT: u64 = 2 * 1024 * 1024;
pub fn read(path: &Path) -> Result<Vec<u8>> {
    ensure!(fs::metadata(path)?.is_file(), "Expected a regular file");
    let mut out = Vec::new();
    fs::File::open(path)?
        .take(FILE_LIMIT + 1)
        .read_to_end(&mut out)?;
    ensure!(out.len() as u64 <= FILE_LIMIT, "File exceeds scan limit");
    Ok(out)
}
pub fn parse(path: &Path, raw: &str) -> Result<Value> {
    match path.extension().and_then(|x| x.to_str()) {
        Some("toml") => toml_edit::de::from_str(raw).map_err(|_| anyhow::anyhow!("Invalid TOML")),
        Some("jsonc") => jsonc_parser::parse_to_serde_value(raw, &Default::default())
            .map_err(|_| anyhow::anyhow!("Invalid JSONC"))?
            .context("Empty configuration"),
        _ => serde_json::from_str(raw).map_err(|_| anyhow::anyhow!("Invalid JSON")),
    }
}
fn executable(p: Provider) -> Option<Command> {
    let name = format!("{}{}", p.binary(), std::env::consts::EXE_SUFFIX);
    let mut dirs: Vec<_> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|d| d.is_absolute())
        .collect();
    if let Some(home) = dirs::home_dir() {
        dirs.extend([
            home.join(".local/bin"),
            home.join(".opencode/bin"),
            home.join(".bun/bin"),
            home.join(".cargo/bin"),
        ]);
    }
    let paths = dirs.into_iter().map(|d| d.join(&name));
    #[cfg(windows)]
    let paths = paths.chain(windows_npm_binaries(p));
    paths.into_iter().find(|f| f.is_file()).map(Command::new)
}
#[cfg(windows)]
fn windows_npm_binaries(p: Provider) -> Vec<PathBuf> {
    let Some(base) = std::env::var_os("APPDATA").map(PathBuf::from) else {
        return vec![];
    };
    let npm = base.join("npm/node_modules");
    let (arch, triple) = if cfg!(target_arch = "aarch64") {
        ("arm64", "aarch64-pc-windows-msvc")
    } else {
        ("x64", "x86_64-pc-windows-msvc")
    };
    match p {
        Provider::Claude => vec![
            npm.join("@anthropic-ai/claude-code/bin/claude.exe"),
            npm.join(format!("@anthropic-ai/claude-code-win32-{arch}/claude.exe")),
        ],
        Provider::OpenCode => [npm.clone(), npm.join("opencode-ai/node_modules")]
            .into_iter()
            .map(|n| n.join(format!("opencode-windows-{arch}/bin/opencode.exe")))
            .collect(),
        Provider::Codex => [npm.clone(), npm.join("@openai/codex/node_modules")]
            .into_iter()
            .map(|n| {
                n.join(format!(
                    "@openai/codex-win32-{arch}/vendor/{triple}/codex/codex.exe"
                ))
            })
            .chain(Some(npm.join(format!(
                "@openai/codex/vendor/{triple}/codex/codex.exe"
            ))))
            .collect(),
    }
}
pub async fn version(p: Provider, cancel: &Cancel) -> Option<String> {
    let mut cmd = executable(p)?;
    cmd.arg("--version").current_dir(std::env::temp_dir());
    let bytes = processes::run(&mut cmd, vec![], 8192, 8, cancel)
        .await
        .ok()?;
    let s = std::str::from_utf8(&bytes).ok()?;
    s.split_whitespace()
        .find(|w| {
            w.as_bytes().first().is_some_and(u8::is_ascii_digit)
                && w.len() < 64
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c))
        })
        .map(str::to_owned)
}
pub async fn versions(cancel: &Cancel) -> BTreeMap<Provider, Option<String>> {
    let (a, b, c) = tokio::join!(
        version(Provider::Codex, cancel),
        version(Provider::OpenCode, cancel),
        version(Provider::Claude, cancel)
    );
    Provider::ALL.into_iter().zip([a, b, c]).collect()
}
pub fn identity(path: &Path, env: &str) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(m) = fs::metadata(path) {
            return format!("{env}:{}:{}", m.dev(), m.ino());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        if let Ok(f) = fs::File::open(path) {
            unsafe {
                let mut i: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
                if GetFileInformationByHandle(f.as_raw_handle() as _, &mut i) != 0 {
                    return format!(
                        "{env}:{}:{}:{}",
                        i.dwVolumeSerialNumber, i.nFileIndexHigh, i.nFileIndexLow
                    );
                }
            }
        }
    }
    format!(
        "{env}:{}",
        path.canonicalize()
            .unwrap_or_else(|_| path.to_owned())
            .display()
    )
}
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        && !matches!(
            s.to_ascii_uppercase().as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
}
pub fn frontmatter(raw: &str) -> Result<(Value, String)> {
    let normalized = raw.replace("\r\n", "\n");
    let (meta, body) = normalized
        .strip_prefix("---\n")
        .and_then(|s| s.split_once("\n---"))
        .context("Missing YAML frontmatter")?;
    ensure!(
        body.is_empty() || body.starts_with('\n'),
        "Invalid frontmatter delimiter"
    );
    Ok((
        serde_yaml::from_str(meta).map_err(|_| anyhow::anyhow!("Invalid YAML frontmatter"))?,
        body.trim_start_matches('\n').into(),
    ))
}
pub fn validate_mcp(p: Provider, v: &Value) -> Result<()> {
    ensure!(v.is_object(), "MCP entry must be an object");
    let nonempty = |k: &str| v[k].as_str().is_some_and(|s| !s.is_empty());
    let strings = |a: &Value| a.as_array().is_some_and(|a| a.iter().all(Value::is_string));
    let local = if p == Provider::OpenCode {
        v["type"] == "local"
            && strings(&v["command"])
            && v["command"].as_array().is_some_and(|a| !a.is_empty())
    } else {
        nonempty("command") && (v.get("args").is_none() || strings(&v["args"]))
    };
    let remote = nonempty("url")
        && if p == Provider::OpenCode {
            v["type"] == "remote"
        } else {
            true
        };
    ensure!(local ^ remote, "Expected one supported MCP transport");
    ensure!(
        !(v.get("url").is_some() && v.get("command").is_some()),
        "Conflicting MCP transports"
    );
    if let Some(flag) = v.get("enabled") {
        ensure!(flag.is_boolean(), "enabled must be a boolean");
    }
    for field in [
        "env",
        "environment",
        "headers",
        "http_headers",
        "env_http_headers",
    ] {
        if let Some(map) = v.get(field) {
            ensure!(
                map.as_object()
                    .is_some_and(|m| m.values().all(Value::is_string)),
                "Expected a map of strings"
            );
        }
    }
    if p == Provider::Claude {
        if let Some(ty) = v.get("type") {
            ensure!(
                if local {
                    ty == "stdio"
                } else {
                    ty == "http" || ty == "sse" || ty == "ws"
                },
                "Unrecognized Claude MCP type"
            );
        }
    }
    if let Some(url) = v["url"].as_str() {
        ensure!(
            url.starts_with("http://")
                || url.starts_with("https://")
                || url.starts_with("ws://")
                || url.starts_with("wss://")
                || url.starts_with("${")
                || url.starts_with("{env:"),
            "Unsupported MCP URL"
        );
    }
    Ok(())
}
pub fn policy_files(c: &Context, p: Provider) -> Vec<PathBuf> {
    match p {
        Provider::Claude => vec![
            c.policy.join("managed-settings.json"),
            c.policy.join("managed-mcp.json"),
            c.policy.join("managed-settings.d"),
        ],
        Provider::Codex => vec![if cfg!(windows) {
            std::env::var_os("ProgramData")
                .map(PathBuf::from)
                .unwrap_or("C:/ProgramData".into())
                .join("OpenAI/Codex/requirements.toml")
        } else {
            "/etc/codex/requirements.toml".into()
        }],
        Provider::OpenCode => vec![if cfg!(windows) {
            std::env::var_os("ProgramData")
                .map(PathBuf::from)
                .unwrap_or("C:/ProgramData".into())
                .join("opencode")
        } else {
            "/etc/opencode".into()
        }],
    }
}
pub fn policy_clear(c: &Context, p: Provider) -> bool {
    #[cfg(windows)]
    if p == Provider::Claude {
        use windows_sys::Win32::System::Registry::*;
        let path: Vec<u16> = r"SOFTWARE\Policies\ClaudeCode"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            let mut key = std::ptr::null_mut();
            let status = unsafe { RegOpenKeyExW(hive, path.as_ptr(), 0, KEY_READ, &mut key) };
            if status == 0 {
                unsafe { RegCloseKey(key) };
                return false;
            }
            if status != 2 && status != 3 {
                return false;
            }
        }
    }
    policy_files(c, p)
        .iter()
        .all(|f| matches!(f.try_exists(), Ok(false)))
}
pub fn writable(c: &Context, p: Provider, k: Kind, s: Scope, path: &Path) -> bool {
    matches!(s, Scope::Global | Scope::Project)
        && policy_clear(c, p)
        && match k {
            Kind::Mcp => path == c.config(p, s),
            Kind::Skill => {
                path.file_name().is_some_and(|n| n == "SKILL.md")
                    && path.parent().and_then(Path::parent) == Some(c.skills(p, s).as_path())
            }
            _ => false,
        }
}
struct Candidate<'a> {
    provider: Provider,
    kind: Kind,
    scope: Scope,
    path: &'a Path,
    key: &'a str,
    name: &'a str,
    desc: &'a str,
    source: String,
    configured: Option<bool>,
    bytes: &'a [u8],
}
struct Scanner<'a> {
    c: &'a Context,
    r: Registry,
    seen: BTreeSet<String>,
}
impl Scanner<'_> {
    fn add(&mut self, item: Candidate<'_>) {
        let Candidate {
            provider: p,
            kind: k,
            scope: s,
            path,
            key,
            name,
            desc,
            source,
            configured,
            bytes,
        } = item;
        let id = hash(
            format!(
                "{:?}",
                (&self.c.environment, &self.c.project, p, k, s, path, key)
            )
            .as_bytes(),
        );
        if !self.seen.insert(id.clone()) {
            return;
        }
        let known = self.r.versions.get(&p).and_then(Option::as_deref) == Some(p.tested());
        let permission = if s == Scope::Managed {
            Permission::Managed
        } else if known
            && configured == Some(true)
            && writable(self.c, p, k, s, path)
            && !fs::metadata(path).is_ok_and(|m| m.permissions().readonly())
        {
            Permission::Write
        } else {
            Permission::Read
        };
        self.r.assets.push(Asset {
            id,
            provider: p,
            kind: k,
            scope: s,
            name: redact::text(name),
            path: path.into(),
            locator: redact::text(key),
            environment: self.c.environment.clone(),
            physical: identity(path, &self.c.environment),
            content_hash: hash(bytes),
            description: redact::text(desc),
            source,
            configured,
            effective: None,
            confidence: if known && configured == Some(true) {
                Confidence::Inferred
            } else {
                Confidence::Unknown
            },
            evidence: vec![if known {
                "Local source; runtime activation unverified"
            } else {
                "CLI version unavailable or outside tested matrix"
            }
            .into()],
            permission,
        })
    }
    fn doc(&mut self, p: Provider, k: Kind, s: Scope, path: &Path) {
        self.doc_at(p, k, s, path, 0);
    }
    fn doc_at(&mut self, p: Provider, k: Kind, s: Scope, path: &Path, depth: u8) {
        if !path.is_file() {
            return;
        }
        let Ok(bytes) = read(path) else {
            self.r
                .issues
                .push(format!("Cannot read {}", path.display()));
            return;
        };
        let Ok(raw) = std::str::from_utf8(&bytes) else {
            self.r
                .issues
                .push(format!("Invalid UTF-8: {}", path.display()));
            return;
        };
        let meta = frontmatter(raw).ok().map(|(v, _)| v).unwrap_or(Value::Null);
        let fallback = if k == Kind::Skill {
            path.parent().and_then(Path::file_name)
        } else {
            path.file_stem()
        }
        .and_then(|s| s.to_str())
        .unwrap_or("?");
        let name = meta["name"].as_str().unwrap_or(fallback);
        let desc = meta["description"].as_str().unwrap_or("");
        let previous = self.seen.len();
        self.add(Candidate {
            provider: p,
            kind: k,
            scope: s,
            path,
            key: "",
            name,
            desc,
            source: redact::text(raw),
            configured: Some(k != Kind::Skill || valid_name(name) && !desc.is_empty()),
            bytes: &bytes,
        });
        if p == Provider::Claude
            && k == Kind::Instructions
            && depth < 5
            && self.seen.len() != previous
        {
            for word in raw.split_whitespace().filter_map(|w| w.strip_prefix('@')) {
                let relative = word.trim_end_matches([',', ';', ')']);
                if relative.is_empty() || relative.contains("://") {
                    continue;
                }
                let child = if let Some(end) = relative.strip_prefix("~/") {
                    self.c.home.join(end)
                } else {
                    path.parent().unwrap().join(relative)
                };
                if child.is_file() {
                    self.doc_at(p, k, Scope::Reference, &child, depth + 1);
                }
            }
        }
    }
    fn directory(&mut self, p: Provider, k: Kind, s: Scope, root: &Path) {
        if !root.is_dir() {
            return;
        }
        if k == Kind::Skill {
            if root.join("SKILL.md").is_file() {
                self.doc(p, k, s, &root.join("SKILL.md"));
                return;
            }
            if let Ok(it) = root.read_dir() {
                for e in it.flatten() {
                    self.doc(p, k, s, &e.path().join("SKILL.md"))
                }
            }
        } else {
            for e in WalkDir::new(root)
                .max_depth(8)
                .follow_links(false)
                .into_iter()
                .flatten()
                .filter(|e| e.file_type().is_file())
            {
                let path = e.path();
                if matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("md" | "toml" | "js" | "ts" | "json")
                ) {
                    self.doc(p, k, s, path)
                }
            }
        }
    }
    fn config(&mut self, p: Provider, s: Scope, path: &Path) {
        if !path.is_file() {
            return;
        }
        let result = (|| -> Result<_> {
            let b = read(path)?;
            let v = parse(path, std::str::from_utf8(&b)?)?;
            Ok((b, v))
        })();
        let Ok((bytes, v)) = result else {
            self.r
                .issues
                .push(format!("Cannot parse {}", path.display()));
            return;
        };
        for (key, k) in [
            (p.mcp_key(), Kind::Mcp),
            ("agents", Kind::Agent),
            ("agent", Kind::Agent),
            ("command", Kind::Command),
            ("commands", Kind::Command),
            ("hooks", Kind::Hook),
            ("plugins", Kind::Plugin),
            ("enabledPlugins", Kind::Plugin),
        ] {
            if let Some(m) = v[key].as_object() {
                for (name, item) in m {
                    if k == Kind::Agent && !item.is_object() {
                        continue;
                    }
                    let configured = if k == Kind::Mcp {
                        Some(validate_mcp(p, item).is_ok())
                    } else {
                        Some(true)
                    };
                    self.add(Candidate {
                        provider: p,
                        kind: k,
                        scope: s,
                        path,
                        key: &format!("{key}/{name}"),
                        name,
                        desc: item["description"].as_str().unwrap_or(""),
                        source: serde_json::to_string_pretty(&redact::json(item)).unwrap(),
                        configured,
                        bytes: &bytes,
                    })
                }
            }
        }
        if p == Provider::Claude {
            if let Some(m) = v["projects"].as_object() {
                for (project, settings) in m {
                    if Path::new(project).canonicalize().ok() != self.c.project.canonicalize().ok()
                    {
                        continue;
                    }
                    if let Some(entries) = settings["mcpServers"].as_object() {
                        for (name, item) in entries {
                            self.add(Candidate {
                                provider: p,
                                kind: Kind::Mcp,
                                scope: Scope::Project,
                                path,
                                key: &format!("projects/{project}/mcpServers/{name}"),
                                name,
                                desc: "Claude local scope",
                                source: redact::json(item).to_string(),
                                configured: Some(validate_mcp(p, item).is_ok()),
                                bytes: &bytes,
                            })
                        }
                    }
                }
            }
        }
        if p == Provider::Codex {
            if let Some(agents) = v["agents"].as_object() {
                for a in agents.values() {
                    if let Some(file) = a["config_file"].as_str() {
                        self.doc(
                            p,
                            Kind::Agent,
                            Scope::Reference,
                            &path.parent().unwrap().join(file),
                        );
                    }
                }
            }
            if let Some(items) = v.pointer("/skills/config").and_then(Value::as_array) {
                for item in items {
                    if let Some(file) = item["path"].as_str() {
                        let d = path.parent().unwrap().join(file);
                        if d.is_dir() {
                            self.directory(p, Kind::Skill, Scope::Reference, &d)
                        } else {
                            self.doc(p, Kind::Skill, Scope::Reference, &d)
                        }
                    }
                }
            }
        }
        let parent = path.parent().unwrap();
        for (key, k) in [
            ("instructions", Kind::Instructions),
            ("plugin", Kind::Plugin),
            ("plugins", Kind::Plugin),
            ("skills/paths", Kind::Skill),
            ("skills/urls", Kind::Skill),
        ] {
            if let Some(items) = v.pointer(&format!("/{key}")).and_then(Value::as_array) {
                for (i, entry) in items.iter().enumerate() {
                    let Some(reference) = entry
                        .as_str()
                        .or_else(|| entry.as_array()?.first()?.as_str())
                    else {
                        continue;
                    };
                    let dest = if let Some(t) = reference.strip_prefix("~/") {
                        self.c.home.join(t)
                    } else {
                        parent.join(reference)
                    };
                    if !reference.contains("://") && dest.exists() {
                        if k == Kind::Skill {
                            self.directory(p, k, Scope::Reference, &dest)
                        } else if dest.is_file() {
                            self.doc(p, k, Scope::Reference, &dest)
                        }
                    } else {
                        self.add(Candidate {
                            provider: p,
                            kind: k,
                            scope: Scope::Reference,
                            path,
                            key: &format!("{key}/{i}"),
                            name: reference,
                            desc: "Unverified reference",
                            source: redact::text(reference),
                            configured: None,
                            bytes: &bytes,
                        })
                    }
                }
            }
        }
    }
    fn provider(&mut self, p: Provider) {
        let c = self.c;
        if p == Provider::Codex {
            let cache = c.codex.join("plugins/cache");
            for e in WalkDir::new(cache)
                .max_depth(7)
                .into_iter()
                .flatten()
                .filter(|e| {
                    e.file_name() == "plugin.json"
                        && e.path()
                            .parent()
                            .and_then(Path::file_name)
                            .is_some_and(|n| n == ".codex-plugin")
                })
            {
                self.doc(p, Kind::Plugin, Scope::Plugin, e.path());
                if let Some(dir) = e.path().parent().and_then(Path::parent) {
                    self.directory(p, Kind::Skill, Scope::Plugin, &dir.join("skills"));
                    self.config(p, Scope::Plugin, &dir.join("hooks/hooks.json"));
                    self.config(p, Scope::Plugin, &dir.join(".mcp.json"));
                }
            }
        }
        if p == Provider::OpenCode && c.environment != "fixture" {
            if let Some(path) = std::env::var_os("OPENCODE_CONFIG")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
            {
                self.config(p, Scope::Reference, &path);
            }
            if let Some(root) = std::env::var_os("OPENCODE_CONFIG_DIR")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
            {
                for (folder, k) in [
                    ("skills", Kind::Skill),
                    ("agents", Kind::Agent),
                    ("commands", Kind::Command),
                    ("plugins", Kind::Plugin),
                ] {
                    self.directory(p, k, Scope::Reference, &root.join(folder));
                }
            }
        }
        for s in [Scope::Global, Scope::Project] {
            let project = s == Scope::Project;
            self.config(p, s, &c.config(p, s));
            self.directory(p, Kind::Skill, s, &c.skills(p, s));
            let base = match (p, project) {
                (Provider::Claude, true) => c.project.join(".claude"),
                (Provider::Claude, false) => c.claude.clone(),
                (Provider::Codex, true) => c.project.join(".codex"),
                (Provider::Codex, false) => c.codex.clone(),
                (Provider::OpenCode, true) => c.project.join(".opencode"),
                (Provider::OpenCode, false) => c.opencode.clone(),
            };
            match p {
                Provider::Codex => {
                    self.doc(
                        p,
                        Kind::Instructions,
                        s,
                        &(if project { &c.project } else { &c.codex }).join("AGENTS.md"),
                    );
                    self.doc(
                        p,
                        Kind::Instructions,
                        s,
                        &(if project { &c.project } else { &c.codex }).join("AGENTS.override.md"),
                    );
                    self.directory(p, Kind::Agent, s, &base.join("agents"));
                    if !project {
                        self.directory(p, Kind::Command, s, &base.join("prompts"))
                    }
                    self.config(p, s, &base.join("hooks.json"));
                }
                Provider::OpenCode => {
                    let d = if project { &c.project } else { &base };
                    self.config(p, s, &d.join("opencode.json"));
                    self.config(p, s, &d.join("opencode.jsonc"));
                    self.doc(p, Kind::Instructions, s, &d.join("AGENTS.md"));
                    for (folder, k) in [
                        ("agents", Kind::Agent),
                        ("agent", Kind::Agent),
                        ("commands", Kind::Command),
                        ("command", Kind::Command),
                        ("plugins", Kind::Plugin),
                        ("plugin", Kind::Plugin),
                        ("skill", Kind::Skill),
                    ] {
                        self.directory(p, k, s, &base.join(folder))
                    }
                    for owner in [Provider::Codex, Provider::Claude] {
                        self.directory(p, Kind::Skill, s, &c.skills(owner, s))
                    }
                }
                Provider::Claude => {
                    self.doc(
                        p,
                        Kind::Instructions,
                        s,
                        &(if project { &c.project } else { &base }).join("CLAUDE.md"),
                    );
                    if project {
                        self.doc(p, Kind::Instructions, s, &base.join("CLAUDE.md"));
                        self.doc(p, Kind::Instructions, s, &c.project.join("CLAUDE.local.md"));
                    }
                    self.directory(p, Kind::Instructions, s, &base.join("rules"));
                    for (folder, k) in [("commands", Kind::Command), ("agents", Kind::Agent)] {
                        self.directory(p, k, s, &base.join(folder))
                    }
                    self.config(p, s, &base.join("settings.json"));
                    if project {
                        self.config(p, s, &base.join("settings.local.json"))
                    }
                    if !project {
                        let manifest = base.join("plugins/installed_plugins.json");
                        self.config(p, Scope::Plugin, &manifest);
                        if let Ok(raw) = read(&manifest) {
                            if let Ok(v) = serde_json::from_slice::<Value>(&raw) {
                                if let Some(plugins) = v["plugins"].as_object() {
                                    for installs in plugins.values().filter_map(Value::as_array) {
                                        for install in installs {
                                            if let Some(d) = install["installPath"].as_str() {
                                                let dir = Path::new(d);
                                                if dir.is_absolute() && dir.is_dir() {
                                                    for (f, k) in [
                                                        ("skills", Kind::Skill),
                                                        ("agents", Kind::Agent),
                                                        ("commands", Kind::Command),
                                                    ] {
                                                        self.directory(
                                                            p,
                                                            k,
                                                            Scope::Plugin,
                                                            &dir.join(f),
                                                        )
                                                    }
                                                    self.config(
                                                        p,
                                                        Scope::Plugin,
                                                        &dir.join("hooks/hooks.json"),
                                                    );
                                                    self.config(
                                                        p,
                                                        Scope::Plugin,
                                                        &dir.join(".mcp.json"),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        for path in policy_files(c, p) {
            if path.is_dir() {
                for e in WalkDir::new(&path)
                    .max_depth(1)
                    .into_iter()
                    .flatten()
                    .filter(|e| e.file_type().is_file())
                {
                    self.config(p, Scope::Managed, e.path())
                }
            } else {
                self.config(p, Scope::Managed, &path)
            }
        }
    }
}
pub fn discover(c: &Context, versions: BTreeMap<Provider, Option<String>>) -> Registry {
    let mut scan = Scanner {
        c,
        r: Registry {
            versions,
            ..Default::default()
        },
        seen: BTreeSet::new(),
    };
    for p in Provider::ALL {
        scan.provider(p);
        if scan.r.versions.get(&p).and_then(Option::as_ref).is_none() {
            scan.r.issues.push(format!("{p:?}: version Unknown"))
        }
    }
    for p in Provider::ALL {
        if !policy_clear(c, p) {
            scan.r.issues.push(format!(
                "{p:?}: managed or inaccessible policy, writes disabled"
            ));
        }
    }
    if !scan
        .r
        .assets
        .iter()
        .any(|a| a.provider == Provider::OpenCode && a.kind == Kind::Hook)
    {
        scan.r.issues.push("OpenCode Hook: Unknown; plugin runtime hooks cannot be enumerated from a static manifest".into());
    }
    let mut groups: BTreeMap<_, Vec<usize>> = BTreeMap::new();
    for (i, a) in scan.r.assets.iter().enumerate() {
        groups
            .entry((a.provider, a.kind, a.name.clone()))
            .or_default()
            .push(i);
    }
    for indices in groups.values() {
        let multiple = indices.len() > 1;
        let rule = indices.iter().all(|&i| {
            let a = &scan.r.assets[i];
            a.confidence == Confidence::Inferred
                && a.configured == Some(true)
                && a.kind == Kind::Mcp
                && matches!(a.scope, Scope::Project | Scope::Global)
                && a.provider != Provider::Claude
                && !(a.provider == Provider::Codex && a.scope == Scope::Project)
                && a.path == c.config(a.provider, a.scope)
                && policy_clear(c, a.provider)
        });
        let top = indices
            .iter()
            .filter(|&&i| scan.r.assets[i].scope == Scope::Project)
            .count();
        for &i in indices {
            let a = &mut scan.r.assets[i];
            if multiple {
                a.evidence
                    .push(format!("{} registrations share the name", indices.len()));
            }
            if rule && (!multiple || top == 1) {
                a.effective = Some(
                    (!multiple || a.scope == Scope::Project)
                        && serde_json::from_str::<Value>(&a.source)
                            .ok()
                            .is_none_or(|v| v.get("enabled") != Some(&Value::Bool(false))),
                );
                a.evidence.push("Project MCP entries take precedence over user entries; inferred from configuration".into())
            } else if multiple {
                a.evidence
                    .push("Precedence cannot be established for these sources".into())
            }
        }
    }
    scan.r.assets.sort_by(|a, b| {
        (a.kind, &a.name, a.provider, a.scope).cmp(&(b.kind, &b.name, b.provider, b.scope))
    });
    scan.r.reindex();
    scan.r
}

/// The pinned Codex release's get command reads configured_servers and does not contact the MCP server.
pub async fn verify_codex(c: &Context, r: &mut Registry, cancel: &Cancel) {
    if r.versions.get(&Provider::Codex).and_then(Option::as_deref) != Some(Provider::Codex.tested())
    {
        return;
    }
    let names: BTreeSet<_> = r
        .assets
        .iter()
        .filter(|a| a.kind == Kind::Mcp && a.provider == Provider::Codex && valid_name(&a.name))
        .map(|a| a.name.clone())
        .collect();
    for name in names.into_iter().take(32) {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let Some(mut cmd) = executable(Provider::Codex) else {
            return;
        };
        cmd.current_dir(&c.project)
            .env("CODEX_HOME", &c.codex)
            .args(["mcp", "get", "--json", "--", &name]);
        let Ok(bytes) = processes::run(&mut cmd, vec![], 65536, 3, cancel).await else {
            continue;
        };
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if v["name"] != name {
            continue;
        }
        let matching: Vec<_> = r
            .assets
            .iter()
            .enumerate()
            .filter(|(_, a)| a.kind == Kind::Mcp && a.provider == Provider::Codex && a.name == name)
            .filter_map(|(i, a)| {
                let raw = read(&a.path).ok()?;
                if hash(&raw) != a.content_hash {
                    return None;
                }
                let config = parse(&a.path, std::str::from_utf8(&raw).ok()?).ok()?;
                let entry = &config["mcp_servers"][&name];
                let transport = &v["transport"];
                let same = [
                    "command",
                    "args",
                    "env",
                    "env_vars",
                    "cwd",
                    "url",
                    "bearer_token_env_var",
                    "http_headers",
                    "env_http_headers",
                ]
                .iter()
                .all(|k| {
                    let default = match *k {
                        "args" | "env_vars" => serde_json::json!([]),
                        _ => Value::Null,
                    };
                    entry.get(k).unwrap_or(&default) == transport.get(k).unwrap_or(&default)
                });
                same.then_some(i)
            })
            .collect();
        if matching.len() == 1 {
            for (i, other) in r.assets.iter_mut().enumerate() {
                if i != matching[0]
                    && other.provider == Provider::Codex
                    && other.kind == Kind::Mcp
                    && other.name == name
                {
                    other.effective = Some(false);
                    other
                        .evidence
                        .push("CLI resolved the name to a different source".into());
                }
            }
            let a = &mut r.assets[matching[0]];
            a.configured = Some(true);
            a.effective = v["enabled"].as_bool();
            a.confidence = Confidence::Verified;
            a.evidence.push(
                "codex mcp get --json matched this local transport and reported its enabled state"
                    .into(),
            );
        }
    }
}
