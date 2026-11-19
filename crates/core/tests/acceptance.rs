use agenthub_core::{
    adapter, diagnostics,
    model::*,
    operations::{self, Intent, Plan, Target, Vault},
    processes::cancel_token,
    protection::Protection,
    redact,
    runtime::{self, Action, Request},
};
use anyhow::{ensure, Result};
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    XChaCha20Poly1305, XNonce,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};
use tempfile::{tempdir, TempDir};
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}
fn context() -> (TempDir, Context) {
    let tmp = tempdir().unwrap();
    let root = fixtures();
    for path in ["home", "project"] {
        for e in walkdir::WalkDir::new(root.join(path))
            .into_iter()
            .map(Result::unwrap)
            .filter(|e| !e.path().components().any(|c| c.as_os_str() == "arg0-linux"))
        {
            let to = tmp.path().join(e.path().strip_prefix(&root).unwrap());
            if e.file_type().is_dir() {
                fs::create_dir_all(to).unwrap()
            } else {
                fs::copy(e.path(), to).unwrap();
            }
        }
    }
    let c = Context::isolated(
        tmp.path().join("home"),
        tmp.path().join("project"),
        "fixture".into(),
    );
    (tmp, c)
}
fn versions() -> BTreeMap<Provider, Option<String>> {
    Provider::ALL
        .into_iter()
        .map(|p| (p, Some(p.tested().into())))
        .collect()
}
fn target(p: Provider, k: Kind, name: &str) -> Target {
    Target {
        provider: p,
        kind: k,
        name: name.into(),
        scope: Scope::Project,
    }
}
fn plan(c: &Context, t: &Target, i: Intent) -> Result<Plan> {
    operations::prepare(c, t.clone(), i, Some(t.provider.tested()), false)
}
struct Cipher(XChaCha20Poly1305);
impl Cipher {
    fn new() -> Self {
        Self(XChaCha20Poly1305::new(&XChaCha20Poly1305::generate_key(
            &mut OsRng,
        )))
    }
}
impl Protection for Cipher {
    fn seal(&self, b: &[u8]) -> Result<Vec<u8>> {
        let n = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let mut x = n.to_vec();
        x.extend(self.0.encrypt(&n, b).unwrap());
        Ok(x)
    }
    fn open(&self, b: &[u8]) -> Result<Vec<u8>> {
        ensure!(b.len() >= 24, "short");
        self.0
            .decrypt(XNonce::from_slice(&b[..24]), &b[24..])
            .map_err(|_| anyhow::anyhow!("integrity"))
    }
}
fn vault(tmp: &TempDir) -> Vault<Cipher> {
    Vault {
        dir: tmp.path().join("vault"),
        protection: Cipher::new(),
    }
}
fn commit(c: &Context, v: &Vault<Cipher>, p: &Plan) -> Result<String> {
    let (id, _lock) = v.record(p, true, true)?;
    operations::apply_checked(c, p, Some(p.target.provider.tested()))?;
    Ok(id)
}

#[test]
fn at01_discover_three_providers_seven_kinds() {
    let (_dir, c) = context();
    let r = adapter::discover(&c, versions());
    assert_eq!(
        r.assets
            .iter()
            .map(|a| a.kind)
            .collect::<BTreeSet<_>>()
            .len(),
        7
    );
    assert_eq!(
        r.assets
            .iter()
            .map(|a| a.provider)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    assert!(r
        .assets
        .iter()
        .all(|a| a.path.is_absolute() && !a.id.is_empty() && a.environment == "fixture"));
    assert_eq!(
        r.assets
            .iter()
            .map(|a| &a.id)
            .collect::<BTreeSet<_>>()
            .len(),
        r.assets.len()
    );
    let skills = r.find("review", None, Some(Kind::Skill), None, None);
    assert_eq!(skills.len(), 2);
    assert_ne!(skills[0].id, skills[1].id);
    assert_eq!(skills[0].physical, skills[1].physical);
    assert!(r
        .assets
        .iter()
        .any(|a| a.scope == Scope::Reference && a.confidence == Confidence::Unknown));
}
#[test]
fn at02_resolution_evidence_and_disabled() {
    let (_, c_guard) = ((), context());
    let c = &c_guard.1;
    fs::write(c.opencode.join("opencode.json"),r#"{"mcp":{"local-source":{"type":"local","command":["other"]},"disabled":{"type":"local","command":["none"],"enabled":false}}}"#).unwrap();
    let r = adapter::discover(c, versions());
    let a = r
        .assets
        .iter()
        .filter(|a| {
            a.provider == Provider::OpenCode && a.kind == Kind::Mcp && a.name == "local-source"
        })
        .collect::<Vec<_>>();
    assert_eq!(a.len(), 2);
    for x in a {
        assert_eq!(x.configured, Some(true));
        assert_eq!(x.effective, Some(x.scope == Scope::Project));
        assert!(x.evidence.iter().any(|v| v.contains("precedence")));
    }
    assert_eq!(
        r.assets
            .iter()
            .find(|a| a.name == "disabled")
            .unwrap()
            .effective,
        Some(false)
    );
}
#[test]
fn at03_search_filters_and_rescan() {
    let (_dir, c) = context();
    let filter = |r: &Registry, q: &str| {
        r.find(
            q,
            Some(Provider::Codex),
            Some(Kind::Skill),
            Some(Scope::Project),
            Some("fixture"),
        )
        .len()
    };
    let r = adapter::discover(&c, versions());
    for q in ["review", "ancestry", "kumquat"] {
        assert_eq!(filter(&r, q), 1)
    }
    let path = c
        .skills(Provider::Codex, Scope::Project)
        .join("review/SKILL.md");
    let raw = fs::read_to_string(&path)
        .unwrap()
        .replace("kumquat", "pomelo");
    fs::write(path, raw).unwrap();
    let r = adapter::discover(&c, versions());
    assert_eq!(filter(&r, "pomelo"), 1);
    assert_eq!(filter(&r, "kumquat"), 0);
}
#[test]
fn at04_native_crud_and_field_preservation() {
    let (tmp, c) = context();
    let v = vault(&tmp);
    for p in Provider::ALL {
        let t = target(p, Kind::Mcp, "first");
        let config = if p == Provider::OpenCode {
            json!({"type":"local","command":["first"],"environment":{"LABEL":"value"}})
        } else {
            json!({"command":"first","env":{"LABEL":"value"}})
        };
        commit(
            &c,
            &v,
            &plan(&c, &t, Intent::Create(config.clone())).unwrap(),
        )
        .unwrap();
        let patch = if p == Provider::OpenCode {
            json!({"command":["next"]})
        } else {
            json!({"command":"next"})
        };
        commit(&c, &v, &plan(&c, &t, Intent::Edit(patch)).unwrap()).unwrap();
        let path = t.path(&c).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        if p == Provider::OpenCode {
            assert!(raw.contains("// Keep this comment"));
        }
        assert!(raw.contains("LABEL"));
        let a = adapter::parse(&path, &raw).unwrap();
        let saved = a[p.mcp_key()]["first"].clone();
        for name in ["clone", "imported"] {
            let cloned = target(p, Kind::Mcp, name);
            commit(
                &c,
                &v,
                &plan(&c, &cloned, Intent::Create(saved.clone())).unwrap(),
            )
            .unwrap();
        }
        commit(&c, &v, &plan(&c, &t, Intent::Delete).unwrap()).unwrap();
        let m = adapter::parse(&path, &fs::read_to_string(path.clone()).unwrap()).unwrap();
        assert!(m[p.mcp_key()].get("first").is_none());
        assert!(m[p.mcp_key()].get("clone").is_some());
        let sk = target(p, Kind::Skill, "fresh");
        let tree = operations::skill_seed("fresh", "description");
        commit(
            &c,
            &v,
            &plan(
                &c,
                &sk,
                Intent::Skill {
                    tree,
                    replace: false,
                },
            )
            .unwrap(),
        )
        .unwrap();
        commit(
            &c,
            &v,
            &plan(
                &c,
                &sk,
                Intent::Markdown("---\nname: fresh\ndescription: edited\n---\nbody".into()),
            )
            .unwrap(),
        )
        .unwrap();
        commit(&c, &v, &plan(&c, &sk, Intent::Delete).unwrap()).unwrap();
        assert!(!sk.path(&c).unwrap().exists());
    }
}
#[test]
fn at05_folder_conversion_and_links() {
    let (tmp, c) = context();
    let v = vault(&tmp);
    let original = c.skills(Provider::Codex, Scope::Project).join("review");
    let marker = tmp.path().join("executed");
    fs::write(
        original.join("scripts/example.sh"),
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    let tree = operations::snapshot(&original).unwrap();
    let t = target(Provider::Claude, Kind::Skill, "review-copy");
    let p = plan(
        &c,
        &t,
        Intent::Skill {
            tree,
            replace: false,
        },
    )
    .unwrap();
    assert!(p.warnings.iter().any(|w| w.contains("allowed-tools")));
    assert!(p.warnings.iter().any(|w| w.contains("unresolved")));
    assert!(p.warnings.iter().any(|w| w.contains("scripts")));
    assert!(!p.path.exists());
    commit(&c, &v, &p).unwrap();
    assert_eq!(
        fs::read(p.path.join("references/checklist.md")).unwrap(),
        fs::read(original.join("references/checklist.md")).unwrap()
    );
    assert_eq!(operations::snapshot(&p.path).unwrap(), p.after);
    assert!(!marker.exists());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc", original.join("outside")).unwrap();
        assert!(operations::snapshot(&original).is_err());
    }
}
#[tokio::test]
async fn at06_diagnostics_limits_and_no_tool_calls() {
    let (tmp, c) = context();
    let script = fixtures().join("mcp_test_server.py");
    let log = tmp.path().join("methods");
    let python = if cfg!(windows) { "python" } else { "python3" };
    let config = |mode: &str| json!({"command":python,"args":[script,mode,log]});
    let diag = |v: Value, yes, cancel, limit| {
        let proj = c.project.clone();
        async move {
            diagnostics::diagnose(
                Provider::Codex,
                &v,
                &proj,
                yes,
                &cancel,
                Duration::from_millis(limit),
            )
            .await
        }
    };
    assert!(diag(config("ok"), false, cancel_token(), 1000)
        .await
        .is_err());
    assert!(!log.exists());
    assert_eq!(
        diag(config("ok"), true, cancel_token(), 3000)
            .await
            .unwrap(),
        vec!["read_fixture"]
    );
    assert_eq!(
        fs::read_to_string(&log)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["initialize", "notifications/initialized", "tools/list"]
    );
    for (mode, ms, err) in [
        ("timeout", 150, "timed out"),
        ("noisy", 3000, "limit"),
        ("failure", 2000, "closed"),
    ] {
        assert!(diag(config(mode), true, cancel_token(), ms)
            .await
            .unwrap_err()
            .to_string()
            .contains(err));
    }
    let c = cancel_token();
    let stop = c.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(120)).await;
        stop.store(true, Ordering::Relaxed);
    });
    assert!(diag(config("timeout"), true, c, 5000)
        .await
        .unwrap_err()
        .to_string()
        .contains("cancelled"));
}
#[test]
fn at07_protected_backup_lock_conflict_restore() {
    let (tmp, c) = context();
    let v = vault(&tmp);
    let t = target(Provider::Claude, Kind::Mcp, "canary");
    let new = Intent::Create(json!({"command":"fixture","env":{"TOKEN":"CANARY-ONLY-FOR-TESTS"}}));
    let mut p = plan(&c, &t, new).unwrap();
    let original = fs::read(&p.path).unwrap();
    p.credential_copy = true;
    assert!(v.record(&p, false, false).is_err());
    assert!(v.record(&p, true, false).is_err());
    assert!(!v.dir.exists());
    let (id, lock) = v.record(&p, true, true).unwrap();
    assert!(v.record(&p, true, true).is_err());
    drop(lock);
    let backup = v.dir.join(format!("{id}.ahb"));
    assert!(!String::from_utf8_lossy(&fs::read(&backup).unwrap()).contains("CANARY"));
    fs::write(&p.path, b"{}").unwrap();
    assert!(operations::apply_checked(&c, &p, Some(t.provider.tested())).is_err());
    assert_eq!(fs::read(&p.path).unwrap(), b"{}");
    fs::write(&p.path, &original).unwrap();
    commit(&c, &v, &p).unwrap();
    let saved = v.load(&id).unwrap();
    assert!(operations::restoration(saved.clone(), Default::default()).is_err());
    let restore = operations::restoration(saved, operations::snapshot(&p.path).unwrap()).unwrap();
    commit(&c, &v, &restore).unwrap();
    assert_eq!(fs::read(&p.path).unwrap(), original);
    let mut broken = fs::read(&backup).unwrap();
    broken[26] ^= 1;
    fs::write(backup, broken).unwrap();
    assert!(v.load(&id).is_err());
}
#[test]
fn at08_isolated_provider_failure_and_policy() {
    let (_tmp, c) = context();
    fs::write(c.config(Provider::OpenCode, Scope::Project), "{bad").unwrap();
    let mut versions = versions();
    versions.insert(Provider::Claude, None);
    let r = adapter::discover(&c, versions);
    assert!(r.assets.iter().any(|a| a.provider == Provider::Codex));
    assert!(r.issues.iter().any(|s| s.contains("parse")));
    assert!(r
        .assets
        .iter()
        .filter(|a| a.provider == Provider::Claude)
        .all(|a| a.permission != Permission::Write));
    let t = target(Provider::Claude, Kind::Mcp, "managed");
    fs::create_dir_all(&c.policy).unwrap();
    fs::write(c.policy.join("managed-settings.json"), "{}").unwrap();
    assert!(plan(&c, &t, Intent::Create(json!({"command":"no"}))).is_err());
}
#[tokio::test]
async fn at08_incompatible_helper_protocol() {
    let (_tmp, c) = context();
    let reply = runtime::serve(
        Request {
            protocol: 999,
            id: uuid::Uuid::new_v4().to_string(),
            context: c,
            action: Action::Hello,
        },
        &cancel_token(),
    )
    .await;
    assert!(reply.error.unwrap().contains("protocol"));
}
#[test]
fn at09_no_secret_in_registry_diff_or_diagnostics() {
    assert!(!redact::text("client_secret: |\n  CANARY\nnext: safe").contains("CANARY"));
    let (_tmp, c) = context();
    let r = adapter::discover(&c, versions());
    assert!(!serde_json::to_string(&r)
        .unwrap()
        .contains("CANARY-ONLY-FOR-TESTS"));
    let t = target(Provider::Claude, Kind::Mcp, "manual-http");
    assert!(!plan(&c, &t, Intent::Delete)
        .unwrap()
        .preview()
        .contains("CANARY-ONLY-FOR-TESTS"));
    let s = redact::json(
        &json!({"command":["node","CANARY-ONLY-FOR-TESTS"],"env":{"BASIC":"CANARY-ONLY-FOR-TESTS"},"authorization":"CANARY-ONLY-FOR-TESTS"}),
    );
    assert!(!s.to_string().contains("CANARY"));
    assert!(
        !redact::text("-----BEGIN PRIVATE KEY-----\nCANARY\n-----END PRIVATE KEY-----")
            .contains("CANARY")
    );
}

#[tokio::test]
async fn at06_streamable_http_negotiates_session_and_paginates() {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let mut methods = vec![];
        for index in 0..4 {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut reader = BufReader::new(socket);
            let mut line = String::new();
            let mut length = 0;
            let mut session = false;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let header = line.to_ascii_lowercase();
                if let Some(value) = header.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap()
                }
                if header.trim() == "mcp-session-id: abc" {
                    session = true;
                }
            }
            if index > 0 {
                assert!(session)
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            methods.push(request["method"].as_str().unwrap().to_owned());
            let (status,ct,body)=match index{
 0=>("200 OK","application/json",json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}}}}).to_string()),
 1=>("202 Accepted","application/json",String::new()),
 2=>("200 OK","text/event-stream",format!("data: {}\n\n",json!({"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"one"}],"nextCursor":"next"}}))),
 _=>{assert_eq!(request["params"]["cursor"],"next");("200 OK","application/json",json!({"jsonrpc":"2.0","id":3,"result":{"tools":[{"name":"two"}]}}).to_string())}};
            write!(reader.get_mut(),"HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\nMcp-Session-Id: abc\r\n\r\n{body}",body.len()).unwrap();
        }
        methods
    });
    let tmp = tempdir().unwrap();
    let found = diagnostics::diagnose(
        Provider::Claude,
        &json!({"type":"http","url":url}),
        tmp.path(),
        true,
        &cancel_token(),
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(found, ["one", "two"]);
    assert_eq!(
        worker.join().unwrap(),
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/list"
        ]
    );
}
#[test]
fn at07_selective_restore_preserves_independent_edits() {
    let (tmp, c) = context();
    let v = vault(&tmp);
    let t = target(Provider::Claude, Kind::Skill, "selective");
    let src = c.skills(Provider::Codex, Scope::Project).join("review");
    let p = plan(
        &c,
        &t,
        Intent::Skill {
            tree: operations::snapshot(&src).unwrap(),
            replace: false,
        },
    )
    .unwrap();
    commit(&c, &v, &p).unwrap();
    fs::write(p.path.join("references/checklist.md"), "external edit").unwrap();
    let current = operations::snapshot(&p.path).unwrap();
    assert!(operations::restoration(p.clone(), current.clone()).is_err());
    let restore = operations::restore_selected(p.clone(), current, Some("scripts")).unwrap();
    commit(&c, &v, &restore).unwrap();
    assert!(!p.path.join("scripts").exists());
    assert_eq!(
        fs::read_to_string(p.path.join("references/checklist.md")).unwrap(),
        "external edit"
    );
}
