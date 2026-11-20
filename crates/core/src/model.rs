use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
pub fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Provider {
    Codex,
    OpenCode,
    Claude,
}
impl Provider {
    pub const ALL: [Self; 3] = [Self::Codex, Self::OpenCode, Self::Claude];
    pub fn binary(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Claude => "claude",
        }
    }
    pub fn mcp_key(self) -> &'static str {
        match self {
            Self::Codex => "mcp_servers",
            Self::OpenCode => "mcp",
            Self::Claude => "mcpServers",
        }
    }
    pub fn tested(self) -> &'static str {
        match self {
            Self::Codex => "0.167.0-alpha.4",
            Self::OpenCode => "1.19.6",
            Self::Claude => "2.1.327",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Kind {
    Mcp,
    Skill,
    Agent,
    Command,
    Instructions,
    Plugin,
    Hook,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Scope {
    Global,
    Project,
    Plugin,
    Managed,
    Reference,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    Verified,
    Inferred,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    Read,
    Write,
    Managed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    pub environment: String,
    pub home: PathBuf,
    pub project: PathBuf,
    pub codex: PathBuf,
    pub opencode: PathBuf,
    pub claude: PathBuf,
    pub policy: PathBuf,
}
impl Context {
    pub fn isolated(home: PathBuf, project: PathBuf, environment: String) -> Self {
        Self {
            codex: home.join(".codex"),
            opencode: home.join(".config/opencode"),
            claude: home.join(".claude"),
            policy: home.join(".agenthub-test-policy"),
            home,
            project,
            environment,
        }
    }
    pub fn local(project: &Path) -> anyhow::Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Home directory unavailable"))?;
        let project = project.canonicalize()?;
        anyhow::ensure!(project.is_dir(), "Expected a project directory");
        let mut c = Self::isolated(home, project, "Linux".into());
        let envpath = |k| {
            std::env::var_os(k)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        c.codex = envpath("CODEX_HOME").unwrap_or(c.codex);
        c.opencode = envpath("XDG_CONFIG_HOME")
            .map(|p| p.join("opencode"))
            .unwrap_or(c.opencode);
        c.claude = envpath("CLAUDE_CONFIG_DIR").unwrap_or(c.claude);
        c.policy = "/etc/claude-code".into();
        Ok(c)
    }
    pub fn config(&self, p: Provider, s: Scope) -> PathBuf {
        let proj = s == Scope::Project;
        match (p, proj) {
            (Provider::Codex, true) => self.project.join(".codex/config.toml"),
            (Provider::Codex, false) => self.codex.join("config.toml"),
            (Provider::Claude, true) => self.project.join(".mcp.json"),
            (Provider::Claude, false) if self.claude != self.home.join(".claude") => {
                self.claude.join(".claude.json")
            }
            (Provider::Claude, false) => self.home.join(".claude.json"),
            (Provider::OpenCode, _) => {
                let d = if proj { &self.project } else { &self.opencode };
                let jsonc = d.join("opencode.jsonc");
                if jsonc.exists() {
                    jsonc
                } else {
                    d.join("opencode.json")
                }
            }
        }
    }
    pub fn skills(&self, p: Provider, s: Scope) -> PathBuf {
        let proj = s == Scope::Project;
        match (p, proj) {
            (Provider::Codex, true) => self.project.join(".agents/skills"),
            (Provider::Codex, false) => self.home.join(".agents/skills"),
            (Provider::OpenCode, true) => self.project.join(".opencode/skills"),
            (Provider::OpenCode, false) => self.opencode.join("skills"),
            (Provider::Claude, true) => self.project.join(".claude/skills"),
            (Provider::Claude, false) => self.claude.join("skills"),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub provider: Provider,
    pub kind: Kind,
    pub scope: Scope,
    pub name: String,
    pub path: PathBuf,
    pub locator: String,
    pub environment: String,
    pub physical: String,
    pub content_hash: String,
    pub description: String,
    pub source: String,
    pub configured: Option<bool>,
    pub effective: Option<bool>,
    pub confidence: Confidence,
    pub evidence: Vec<String>,
    pub permission: Permission,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    pub assets: Vec<Asset>,
    pub versions: BTreeMap<Provider, Option<String>>,
    pub issues: Vec<String>,
    #[serde(skip)]
    pub(crate) search_index: BTreeMap<String, String>,
}
impl Registry {
    pub fn reindex(&mut self) {
        self.search_index = self
            .assets
            .iter()
            .map(|a| {
                (
                    a.id.clone(),
                    format!("{}\n{}\n{}", a.name, a.description, a.source).to_lowercase(),
                )
            })
            .collect();
    }

    pub fn find(
        &self,
        query: &str,
        provider: Option<Provider>,
        kind: Option<Kind>,
        scope: Option<Scope>,
        env: Option<&str>,
    ) -> Vec<&Asset> {
        let q = query.to_lowercase();
        self.assets
            .iter()
            .filter(|a| {
                provider.is_none_or(|v| v == a.provider)
                    && kind.is_none_or(|v| v == a.kind)
                    && scope.is_none_or(|v| v == a.scope)
                    && env.is_none_or(|v| v == a.environment)
                    && (q.is_empty()
                        || self
                            .search_index
                            .get(&a.id)
                            .map(|doc| doc.contains(&q))
                            .unwrap_or_else(|| {
                                [&a.name, &a.description, &a.source]
                                    .iter()
                                    .any(|v| v.to_lowercase().contains(&q))
                            }))
            })
            .collect()
    }
    pub fn linked(&self, id: &str) -> Vec<&Asset> {
        self.assets.iter().filter(|a| a.physical == id).collect()
    }
}
