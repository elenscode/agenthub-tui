use crate::{
    adapter::{self, frontmatter, valid_name},
    edit,
    model::*,
    protection::Protection,
    redact,
};
use anyhow::{bail, ensure, Context as _, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime},
};
use tempfile::NamedTempFile;
use walkdir::WalkDir;
mod binary {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        v: &Vec<u8>,
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Vec<u8>, D::Error> {
        STANDARD
            .decode(String::deserialize(d)?)
            .map_err(serde::de::Error::custom)
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct Entry {
    pub directory: bool,
    pub mode: u32,
    #[serde(with = "binary")]
    pub data: Vec<u8>,
}
pub type Image = BTreeMap<String, Entry>;
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct Target {
    pub provider: Provider,
    pub kind: Kind,
    pub scope: Scope,
    pub name: String,
}
impl Target {
    pub fn path(&self, c: &Context) -> Result<PathBuf> {
        ensure!(
            matches!(self.scope, Scope::Project | Scope::Global) && valid_name(&self.name),
            "Invalid name or read-only scope"
        );
        match self.kind {
            Kind::Mcp => Ok(c.config(self.provider, self.scope)),
            Kind::Skill => {
                if self.provider == Provider::OpenCode {
                    ensure!(
                        self.name.split('-').all(|part| !part.is_empty()
                            && part
                                .bytes()
                                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())),
                        "OpenCode Skill names require lowercase hyphen-separated words"
                    );
                }
                Ok(c.skills(self.provider, self.scope).join(&self.name))
            }
            _ => bail!("This asset type is read-only"),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub enum Intent {
    Create(Value),
    Edit(Value),
    Delete,
    Skill { tree: Image, replace: bool },
    Markdown(String),
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Plan {
    pub target: Target,
    pub path: PathBuf,
    pub environment: String,
    pub project: PathBuf,
    pub before: Image,
    pub after: Image,
    pub warnings: Vec<String>,
    pub credential_copy: bool,
}
pub fn digest(image: &Image) -> String {
    hash(&serde_json::to_vec(image).unwrap())
}
fn entry(data: Vec<u8>) -> Entry {
    Entry {
        directory: false,
        mode: 0o600,
        data,
    }
}
fn safe_part(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.ends_with([' ', '.'])
        && !s.contains(['\\', '/', ':', '\0', '*', '?', '"', '<', '>', '|'])
        && !s.chars().any(char::is_control)
        && !matches!(
            s.split('.')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase()
                .as_str(),
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
pub fn check_path(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute() && !path.components().any(|x| matches!(x, Component::ParentDir)),
        "Absolute path without traversal required"
    );
    for p in path.ancestors() {
        match fs::symlink_metadata(p) {
            Ok(m) => {
                ensure!(
                    !m.file_type().is_symlink(),
                    "Symbolic link requires manual review"
                );
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    ensure!(
                        m.file_attributes() & 0x400 == 0,
                        "Reparse point requires manual review"
                    );
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn write_access(path: &Path) -> Result<()> {
    check_path(path)?;
    if !path.try_exists()? {
        return Ok(());
    }
    for e in WalkDir::new(path).follow_links(false) {
        let e = e?;
        let m = e.metadata()?;
        ensure!(
            !m.permissions().readonly() || (cfg!(windows) && m.is_dir()),
            "Read-only resource; write blocked"
        );
        #[cfg(windows)]
        if m.is_file() {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            };
            let handle = File::open(e.path())?;
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            ensure!(
                unsafe { GetFileInformationByHandle(handle.as_raw_handle() as _, &mut info) } != 0
                    && info.nNumberOfLinks == 1,
                "Unverified or hard-linked resource; write blocked"
            );
        }
    }
    Ok(())
}
pub fn snapshot(path: &Path) -> Result<Image> {
    check_path(path)?;
    let mut out = Image::new();
    if !path.try_exists()? {
        return Ok(out);
    }
    let mut size = 0u64;
    for e in WalkDir::new(path).follow_links(false).sort_by_file_name() {
        let e = e?;
        let file = e.path();
        check_path(file)?;
        let m = fs::metadata(file)?;
        ensure!(m.is_dir() || m.is_file(), "Unsupported file type");
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                m.is_dir() || m.nlink() == 1,
                "Hard-linked file requires manual review"
            );
            m.mode() & 0o777
        };
        #[cfg(not(unix))]
        let mode = 0;
        let key = file
            .strip_prefix(path)?
            .components()
            .map(|p| p.as_os_str().to_str().context("Non-Unicode name"))
            .collect::<Result<Vec<_>>>()?
            .join("/");
        ensure!(
            key.is_empty() || key.split('/').all(safe_part),
            "Nonportable or unsafe filename"
        );
        size += m.len() * u64::from(m.is_file());
        ensure!(
            size <= 8 * 1024 * 1024 && out.len() < 2048,
            "Resource exceeds snapshot limit"
        );
        out.insert(
            key,
            Entry {
                directory: m.is_dir(),
                mode,
                data: if m.is_file() {
                    adapter::read(file)?
                } else {
                    vec![]
                },
            },
        );
    }
    Ok(out)
}
pub fn skill_seed(name: &str, description: &str) -> Image {
    let mut t = Image::new();
    t.insert(
        "".into(),
        Entry {
            directory: true,
            mode: 0o700,
            data: vec![],
        },
    );
    t.insert(
        "SKILL.md".into(),
        entry(
            format!(
                "---\nname: {name}\ndescription: {}\n---\n\n",
                serde_json::to_string(description).unwrap()
            )
            .into_bytes(),
        ),
    );
    t
}
pub fn skill_review(tree: &Image, name: &str) -> Result<Vec<String>> {
    ensure!(
        tree.get("").is_some_and(|e| e.directory),
        "Expected a Skill folder"
    );
    let mut warnings = BTreeSet::new();
    let doc = std::str::from_utf8(&tree.get("SKILL.md").context("Missing SKILL.md")?.data)?;
    let (meta, body) = frontmatter(doc)?;
    ensure!(
        meta["name"] == name
            && meta["description"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
        "Skill name or description invalid"
    );
    if let Some(m) = meta.as_object() {
        for k in m
            .keys()
            .filter(|k| !matches!(k.as_str(), "name" | "description"))
        {
            warnings.insert(format!(
                "Provider-specific field retained: {}",
                redact::text(k)
            ));
        }
    }
    let mut folded = BTreeSet::new();
    for (key, e) in tree {
        ensure!(
            key.is_empty() || key.split('/').all(safe_part),
            "Unsafe Skill path"
        );
        ensure!(
            folded.insert(key.to_lowercase()),
            "Case-insensitive name collision"
        );
        if !key.is_empty() {
            let parent = key.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            ensure!(
                tree.get(parent).is_some_and(|e| e.directory),
                "Missing parent directory"
            );
        }
        if e.directory {
            continue;
        }
        if let Ok(content) = std::str::from_utf8(&e.data) {
            for part in content.split_whitespace() {
                let reference = part.trim_matches(|ch: char| {
                    matches!(ch, '"' | '\'' | '(' | ')' | ',' | ';' | '`')
                });
                let bytes = reference.as_bytes();
                if reference.starts_with('/')
                    || reference.starts_with("~/")
                    || reference.starts_with("../")
                    || reference.contains("://")
                    || (bytes.len() > 2
                        && bytes[0].is_ascii_alphabetic()
                        && bytes[1] == b':'
                        && matches!(bytes[2], b'/' | b'\\'))
                {
                    warnings.insert(format!(
                        "External path or dependency in {}",
                        redact::text(key)
                    ));
                }
                if ["scripts/", "references/", "assets/"]
                    .iter()
                    .any(|prefix| reference.starts_with(prefix))
                    && !tree.contains_key(reference)
                {
                    warnings.insert(format!(
                        "Unresolved local file reference in {}",
                        redact::text(key)
                    ));
                }
            }
        }
        if e.mode & 0o111 != 0 || key.starts_with("scripts/") {
            warnings.insert(format!(
                "Executable content copied without execution: {}",
                redact::text(key)
            ));
        }
    }
    let links = regex::Regex::new(r"\]\(([^)\s]+)").unwrap();
    for target in links.captures_iter(&body).map(|c| c[1].to_string()) {
        if target.starts_with('#') {
            continue;
        }
        if target.contains("://")
            || target.starts_with(['/', '~'])
            || target.contains("..")
            || !tree.contains_key(target.trim_start_matches("./"))
        {
            warnings.insert(format!(
                "External or unresolved reference: {}",
                redact::text(&target)
            ));
        }
    }
    Ok(warnings.into_iter().collect())
}
pub fn renamed(tree: &mut Image, name: &str) -> Result<()> {
    ensure!(valid_name(name), "Invalid Skill name");
    let file = tree.get_mut("SKILL.md").context("Missing SKILL.md")?;
    let raw = std::str::from_utf8(&file.data)?;
    let (meta, _) = frontmatter(raw)?;
    if meta["name"] != name {
        let line = regex::Regex::new(r"(?m)^name:[^\r\n]*").unwrap();
        let end = raw.find("\n---").context("Invalid Skill header")?;
        let header = &raw[..end];
        ensure!(
            line.is_match(header),
            "Complex YAML name requires manual editing"
        );
        file.data = format!(
            "{}{}",
            line.replace(header, format!("name: {name}")),
            &raw[end..]
        )
        .into_bytes();
    }
    Ok(())
}
pub fn prepare(
    c: &Context,
    t: Target,
    intent: Intent,
    version: Option<&str>,
    copy: bool,
) -> Result<Plan> {
    ensure!(
        version == Some(t.provider.tested()) && adapter::policy_clear(c, t.provider),
        "Version or managed policy prevents writes"
    );
    let path = t.path(c)?;
    write_access(&path)?;
    let before = snapshot(&path)?;
    let mut after = before.clone();
    let mut warnings = vec![];
    let mut credential_copy = false;
    match (t.kind, intent) {
        (Kind::Mcp, op) => {
            let (v, exists) = match &op {
                Intent::Create(v) => (Some(v), false),
                Intent::Edit(v) => (Some(v), true),
                Intent::Delete => (None, true),
                _ => bail!("Invalid MCP action"),
            };
            let raw = edit::mcp(
                t.provider,
                &path,
                before.get("").map(|e| e.data.as_slice()),
                &t.name,
                v,
                exists,
            )?;
            credential_copy =
                copy && v.is_some_and(|v| redact::sensitive(v.to_string().as_bytes()));
            after.entry("".into()).or_insert_with(|| entry(vec![])).data = raw;
        }
        (Kind::Skill, Intent::Delete) => {
            ensure!(!before.is_empty(), "Skill missing");
            after.clear();
        }
        (Kind::Skill, Intent::Markdown(text)) => {
            after.get_mut("SKILL.md").context("Skill missing")?.data = text.into_bytes();
            warnings = skill_review(&after, &t.name)?;
        }
        (Kind::Skill, Intent::Skill { mut tree, replace }) => {
            ensure!(
                replace != before.is_empty(),
                "Skill collision or missing edit target"
            );
            renamed(&mut tree, &t.name)?;
            warnings = skill_review(&tree, &t.name)?;
            credential_copy = copy
                && tree.iter().any(|(name, e)| {
                    let n = name.to_ascii_lowercase();
                    n.ends_with(".pem")
                        || n.ends_with(".key")
                        || n.split('/')
                            .any(|p| p.starts_with(".env") || p.starts_with("id_rsa"))
                        || redact::sensitive(name.as_bytes())
                        || redact::sensitive(&e.data)
                        || std::str::from_utf8(&e.data).is_err()
                });
            after = tree;
        }
        _ => bail!("Unsupported action"),
    }
    #[cfg(unix)]
    if t.kind == Kind::Skill {
        for e in after.values_mut() {
            if e.mode == 0 {
                e.mode = if e.directory { 0o700 } else { 0o600 };
            }
        }
    }
    #[cfg(windows)]
    for e in after.values_mut() {
        e.mode = 0;
    }
    ensure!(before != after, "No changes");
    Ok(Plan {
        target: t,
        path,
        environment: c.environment.clone(),
        project: c.project.clone(),
        before,
        after,
        warnings,
        credential_copy,
    })
}
impl Plan {
    pub fn preview(&self) -> String {
        let mut out = format!(
            "{}\n{} → {}\n",
            redact::text(&self.path.to_string_lossy()),
            &digest(&self.before)[..12],
            &digest(&self.after)[..12]
        );
        let keys: BTreeSet<_> = self.before.keys().chain(self.after.keys()).collect();
        for key in keys {
            let (old, new) = (self.before.get(key), self.after.get(key));
            if old == new {
                continue;
            }
            out.push_str(&format!(
                "\n{} {}\n",
                if old.is_none() {
                    "+"
                } else if new.is_none() {
                    "-"
                } else {
                    "~"
                },
                redact::text(key)
            ));
            let display = |e: Option<&Entry>| -> String {
                let Some(e) = e else { return String::new() };
                if e.directory {
                    return "[directory]".into();
                }
                if self.target.kind == Kind::Mcp {
                    return adapter::parse(&self.path, std::str::from_utf8(&e.data).unwrap_or(""))
                        .ok()
                        .and_then(|v| {
                            v.get(self.target.provider.mcp_key())?
                                .get(&self.target.name)
                                .cloned()
                        })
                        .map(|v| serde_json::to_string_pretty(&redact::json(&v)).unwrap())
                        .unwrap_or_default();
                }
                if key.ends_with(".md") {
                    if let Ok(s) = std::str::from_utf8(&e.data) {
                        return redact::text(s);
                    }
                }
                format!("[{} bytes, SHA-256 {}]", e.data.len(), hash(&e.data))
            };
            let (a, b) = (display(old), display(new));
            out.push_str(
                &similar::TextDiff::from_lines(&a, &b)
                    .unified_diff()
                    .context_radius(2)
                    .to_string(),
            );
        }
        for w in &self.warnings {
            out.push_str(&format!("\n! {w}"));
        }
        out
    }
}
fn install_file(path: &Path, e: &Entry, replace: bool) -> Result<()> {
    let parent = path.parent().context("Invalid path")?;
    fs::create_dir_all(parent)?;
    check_path(path)?;
    let mut f = NamedTempFile::new_in(parent)?;
    f.write_all(&e.data)?;
    f.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.as_file()
            .set_permissions(fs::Permissions::from_mode(e.mode))?;
    }
    if !replace {
        f.persist_noclobber(path)?;
        return Ok(());
    }
    #[cfg(windows)]
    if path.exists() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
        let w = |p: &Path| {
            p.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        let temporary = f.into_temp_path();
        let (dest, src) = (w(path), w(&temporary));
        let ok = unsafe {
            ReplaceFileW(
                dest.as_ptr(),
                src.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        ensure!(ok != 0, "Windows ACL-preserving replacement failed");
        return Ok(());
    }
    f.persist(path)?;
    Ok(())
}
fn set_entry(path: &Path, from: Option<&Entry>, to: Option<&Entry>) -> Result<()> {
    check_path(path)?;
    if let (Some(a), Some(b)) = (from, to) {
        ensure!(a.directory == b.directory, "File/directory collision");
    }
    match to {
        None => {
            if from.unwrap().directory {
                fs::remove_dir(path)?
            } else {
                fs::remove_file(path)?
            }
        }
        Some(e) if e.directory => {
            if from.is_none() {
                fs::create_dir_all(path)?
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(e.mode))?;
            }
        }
        Some(e) => install_file(path, e, from.is_some())?,
    };
    Ok(())
}
fn location(root: &Path, key: &str) -> PathBuf {
    if key.is_empty() {
        root.into()
    } else {
        root.join(key)
    }
}
fn matches_entry(path: &Path, expected: Option<&Entry>) -> Result<bool> {
    check_path(path)?;
    if !path.try_exists()? {
        return Ok(expected.is_none());
    }
    let Some(e) = expected else { return Ok(false) };
    if e.directory {
        return Ok(path.is_dir());
    }
    Ok(path.is_file() && adapter::read(path)? == e.data)
}
pub fn apply_checked(c: &Context, plan: &Plan, version: Option<&str>) -> Result<()> {
    apply_each(c, plan, version, |_| Ok(()))
}
fn apply_each(
    c: &Context,
    plan: &Plan,
    version: Option<&str>,
    mut after_entry: impl FnMut(&Path) -> Result<()>,
) -> Result<()> {
    ensure!(
        c.environment == plan.environment
            && c.project == plan.project
            && plan.target.path(c)? == plan.path,
        "Target context changed"
    );
    ensure!(
        version == Some(plan.target.provider.tested())
            && adapter::policy_clear(c, plan.target.provider),
        "Capability changed"
    );
    if plan.target.kind == Kind::Skill && !plan.after.is_empty() {
        skill_review(&plan.after, &plan.target.name)?;
    }
    for key in plan.before.keys().chain(plan.after.keys()) {
        ensure!(
            key.is_empty() || key.split('/').all(safe_part),
            "Unsafe relative path"
        );
    }
    write_access(&plan.path)?;
    let current = snapshot(&plan.path)?;
    if current != plan.before {
        let mut difference = plan.clone();
        difference.after = current;
        bail!(
            "External change. Review the current source and create a new plan.\n{}",
            difference.preview()
        );
    }
    let mut keys: Vec<_> = plan
        .before
        .keys()
        .chain(plan.after.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|k| plan.before.get(k) != plan.after.get(k))
        .collect();
    keys.sort_by(|a, b| {
        let del_a = !plan.after.contains_key(a);
        let del_b = !plan.after.contains_key(b);
        del_b
            .cmp(&del_a)
            .then_with(|| if del_a { b.cmp(a) } else { a.cmp(b) })
    });
    let mut done = Vec::new();
    let result = (|| -> Result<()> {
        for k in keys {
            let path = location(&plan.path, &k);
            let (a, b) = (plan.before.get(&k), plan.after.get(&k));
            ensure!(matches_entry(&path, a)?, "Resource changed during apply");
            set_entry(&path, a, b)?;
            done.push(k);
            after_entry(&path)?;
        }
        ensure!(
            snapshot(&plan.path)? == plan.after,
            "Post-write verification failed"
        );
        if plan.target.kind == Kind::Mcp {
            if let Some(e) = plan.after.get("") {
                adapter::parse(&plan.path, std::str::from_utf8(&e.data)?)?;
            }
        }
        Ok(())
    })();
    if let Err(cause) = result {
        let mut failed = vec![];
        for k in done.iter().rev() {
            let path = location(&plan.path, k);
            let (a, b) = (plan.before.get(k), plan.after.get(k));
            if !matches_entry(&path, b).unwrap_or(false) || set_entry(&path, b, a).is_err() {
                failed.push(if k.is_empty() {
                    "(resource root)".into()
                } else {
                    redact::text(k)
                })
            }
        }
        if !failed.is_empty() {
            bail!(
                "Partial recovery; inspect resources {:?} and restore from history",
                failed
            )
        }
        bail!("{cause}; applied entries rolled back")
    }
    Ok(())
}
pub struct Vault<P> {
    pub dir: PathBuf,
    pub protection: P,
}
impl<P: Protection> Vault<P> {
    pub fn record(
        &self,
        plan: &Plan,
        approved: bool,
        allow_credentials: bool,
    ) -> Result<(String, File)> {
        ensure!(
            approved && (!plan.credential_copy || allow_credentials),
            "Explicit approval required"
        );
        check_path(&self.dir)?;
        fs::create_dir_all(&self.dir)?;
        check_path(&self.dir)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join("operations.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock).context("Another operation is in progress")?;
        let plain = serde_json::to_vec(plan)?;
        let sealed = self.protection.seal(&plain)?;
        ensure!(
            self.protection.open(&sealed)? == plain,
            "Backup verification failed"
        );
        let id = uuid::Uuid::new_v4().to_string();
        let mut f = NamedTempFile::new_in(&self.dir)?;
        f.write_all(&sealed)?;
        f.as_file().sync_all()?;
        let f = f.persist_noclobber(self.dir.join(format!("{id}.ahb")))?;
        f.sync_all()?;
        ensure!(
            digest(&self.load(&id)?.before) == digest(&plan.before),
            "Persisted backup unreadable"
        );
        self.prune(&id);
        Ok((id, lock))
    }
    pub fn load(&self, id: &str) -> Result<Plan> {
        ensure!(uuid::Uuid::parse_str(id).is_ok(), "Invalid backup ID");
        let raw = fs::read(self.dir.join(format!("{id}.ahb")))?;
        serde_json::from_slice(&self.protection.open(&raw)?).context("Invalid protected snapshot")
    }
    pub fn history(&self) -> Vec<String> {
        let mut entries = vec![];
        if let Ok(dir) = self.dir.read_dir() {
            for e in dir.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|s| s == "ahb") {
                    if let Some(id) = p.file_stem().and_then(|i| i.to_str()) {
                        if let Ok(m) = e.metadata() {
                            entries.push((
                                m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                                id.to_owned(),
                            ))
                        }
                    }
                }
            }
        }
        entries.sort_by(|a, b| b.cmp(a));
        entries.into_iter().map(|(_, i)| i).collect()
    }
    fn prune(&self, current: &str) {
        for (i, id) in self
            .history()
            .iter()
            .filter(|id| id.as_str() != current)
            .enumerate()
        {
            let path = self.dir.join(format!("{id}.ahb"));
            let old = path
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|t| t > Duration::from_secs(30 * 86400));
            if i >= 29 || old {
                let _ = fs::remove_file(path);
            }
        }
    }
}
pub fn restoration(saved: Plan, current: Image) -> Result<Plan> {
    restore_selected(saved, current, None)
}
pub fn restore_selected(mut saved: Plan, current: Image, selected: Option<&str>) -> Result<Plan> {
    let mut next = current.clone();
    let keys: BTreeSet<_> = saved
        .before
        .keys()
        .chain(saved.after.keys())
        .cloned()
        .collect();
    if let Some(key) = selected {
        ensure!(keys.contains(key), "Resource is not in this snapshot");
    }
    for k in keys {
        if selected.is_some_and(|key| k != key && !k.starts_with(&format!("{key}/"))) {
            continue;
        }
        let (before, after, now) = (saved.before.get(&k), saved.after.get(&k), current.get(&k));
        if before == after || now == before {
            continue;
        }
        if now != after {
            let mut diff = saved.clone();
            diff.before = saved.after.clone();
            diff.after = current;
            bail!(
                "Restore conflict. This resource changed outside the operation.\n{}",
                diff.preview()
            );
        }
        if let Some(e) = before {
            next.insert(k, e.clone());
        } else {
            next.remove(&k);
        }
    }
    // Keep parent directories that now hold unrelated files.
    for k in next.clone().keys() {
        let mut parent = k.as_str();
        while !parent.is_empty() {
            parent = parent.rsplit_once('/').map(|(x, _)| x).unwrap_or("");
            if !next.contains_key(parent) {
                let e = current
                    .get(parent)
                    .or_else(|| saved.before.get(parent))
                    .context("Missing restore parent")?;
                next.insert(parent.into(), e.clone());
            }
        }
    }
    ensure!(
        next != current,
        "Selected resources already match the pre-change snapshot"
    );
    saved.before = current;
    saved.after = next;
    saved.credential_copy = false;
    saved.warnings = vec!["Restoring selected snapshot resources".into()];
    Ok(saved)
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn at07_full_and_partial_rollback_preserve_external_files() {
        for external in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let project = temp.path().join("project");
            fs::create_dir_all(&project).unwrap();
            let c = Context::isolated(temp.path().join("home"), project, "test".into());
            let t = Target {
                provider: Provider::Codex,
                kind: Kind::Skill,
                name: "test".into(),
                scope: Scope::Project,
            };
            let v = Some(t.provider.tested());
            let p = prepare(
                &c,
                t,
                Intent::Skill {
                    tree: skill_seed("test", "desc"),
                    replace: false,
                },
                v,
                false,
            )
            .unwrap();
            let result = apply_each(&c, &p, v, |at| {
                if external {
                    fs::write(at.join("external"), b"owned by another program")?;
                }
                bail!("Injected failure")
            });
            let msg = result.unwrap_err().to_string();
            if external {
                assert!(msg.contains("Partial recovery"));
                assert_eq!(
                    fs::read(p.path.join("external")).unwrap(),
                    b"owned by another program"
                );
            } else {
                assert!(msg.contains("rolled back"));
                assert!(!p.path.exists());
            }
        }
    }
}
