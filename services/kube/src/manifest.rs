//! Manifests: what `kube apply` is given, one object per file (TOML).
//!
//! ```toml
//! kind = "Deployment"       # or "Space", "Config"
//! name = "web"
//! space = "demo"            # optional: the Space its pods' quotas come out of
//! image = "worker"
//! args = ["5", "1"]         # optional: the program's arguments
//! replicas = 3              # optional, 1
//! restart = "always"        # always | on-failure | never
//! serves = "echo"           # optional: its pods are the service echo (get `listen`)
//! uses = ["echo"]           # optional: services its pods may call (get `svc:echo`)
//! config = "greeting"       # optional: a Config's files and env on top of the image
//! node = "test"             # optional: only on this node (machine)
//!
//! [resources]               # each pod's quota (a Space: the whole space's)
//! mem = "8M"
//! tasks = 2
//!
//! [labels]
//! app = "web"
//! ```
//!
//! A Config: `kind = "Config"`, `name`, `[files]` (path = text) and `[env]`.
//!
//! A Function (doc/serverless.md): `kind = "Function"`, `name`, `image`,
//! `args`, `uses`, `timeout` (s, 10), `max` (instances per node, 4),
//! `idle` (s, 30), `reuse` (true), `expose` (false: requests from outside,
//! doc/ingress.md), `[resources]`.
//!
//! `uses` entries: a service `NAME` (`svc:NAME`), a Function `fn:NAME`, a
//! KV namespace `kv:NAME`, `http` (curl's capability), or a backend
//! database `sql:NAME` / `redis:NAME`.
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    kind: String,
    name: String,
    space: Option<String>,
    image: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    replicas: Option<u32>,
    restart: Option<String>,
    #[serde(default)]
    resources: Resources,
    #[serde(default)]
    labels: BTreeMap<String, String>,
    serves: Option<String>,
    #[serde(default)]
    uses: Vec<String>,
    config: Option<String>,
    node: Option<String>,
    #[serde(default)]
    files: BTreeMap<String, String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    timeout: Option<u64>,
    max: Option<u32>,
    idle: Option<u64>,
    reuse: Option<bool>,
    expose: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Resources {
    mem: Option<String>,
    tasks: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Restart {
    Always,
    OnFailure,
    Never,
}

/// What a Deployment's pods are made from; a change replaces them.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    pub space: Option<String>,
    pub image: String,
    pub args: Vec<String>,
    pub restart: Restart,
    pub mem: String,
    pub tasks: u64,
    /// `key=value`, sorted.
    pub labels: Vec<String>,
    pub serves: Option<String>,
    pub uses: Vec<String>,
    pub config: Option<String>,
    pub node: Option<String>,
}

/// A Function: what each instance is made from, and how they come and go.
#[derive(Clone, Debug, PartialEq)]
pub struct FnSpec {
    pub image: String,
    pub args: Vec<String>,
    pub uses: Vec<String>,
    pub mem: String,
    pub tasks: u64,
    pub timeout: u64,
    pub max: u32,
    pub idle: u64,
    pub reuse: bool,
    /// Requests from outside reach it (doc/ingress.md).
    pub expose: bool,
}

pub enum Object {
    Deployment { name: String, replicas: u32, template: Template },
    Space { name: String, mem: String, tasks: u64 },
    Config { name: String, files: Vec<(String, String)>, env: Vec<(String, String)> },
    Function { name: String, spec: FnSpec },
}

/// A `uses` entry: NAME, fn:NAME, kv:NAME, sql:NAME, redis:NAME or http.
fn check_use(u: &str) -> Result<(), String> {
    match u.split_once(':') {
        Some(("fn" | "kv" | "sql" | "redis", n)) => check_name(n),
        Some(_) => Err(format!("bad uses entry {u:?} (NAME, fn:NAME, kv:NAME, sql:NAME, redis:NAME, http)")),
        None => check_name(u),
    }
}

/// Lowercase letters, digits and `-`, like a k8s name (and short, for the tables).
fn check_name(n: &str) -> Result<(), String> {
    let ok = !n.is_empty() && n.len() <= 16 && n.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
    if ok { Ok(()) } else { Err(format!("bad name {n:?} (a-z, 0-9, -; at most 16)")) }
}

fn check_size(s: &str) -> Result<(), String> {
    libsys::dock::parse_size(s).map(|_| ()).ok_or_else(|| format!("bad size {s:?}"))
}

pub fn parse(text: &str) -> Result<Object, String> {
    let r: Raw = toml::from_str(text).map_err(|e| format!("{e}").replace('\n', " "))?;
    check_name(&r.name)?;
    match r.kind.as_str() {
        "Deployment" => {
            let image = r.image.ok_or("a Deployment needs image")?;
            let restart = match r.restart.as_deref().unwrap_or("always") {
                "always" => Restart::Always,
                "on-failure" => Restart::OnFailure,
                "never" => Restart::Never,
                x => return Err(format!("bad restart {x:?} (always, on-failure, never)")),
            };
            let mem = r.resources.mem.unwrap_or_else(|| String::from("8M"));
            check_size(&mem)?;
            for s in r.space.iter().chain(&r.serves).chain(&r.config) {
                check_name(s)?;
            }
            for u in &r.uses {
                check_use(u)?;
            }
            for k in r.labels.keys() {
                if k == "kube" || k.contains(['=', ',', ' ']) {
                    return Err(format!("bad label {k:?}"));
                }
            }
            let template = Template {
                space: r.space.filter(|s| s != "default"),
                image,
                args: r.args,
                restart,
                mem,
                tasks: r.resources.tasks.unwrap_or(2),
                labels: r.labels.iter().map(|(k, v)| format!("{k}={v}")).collect(),
                serves: r.serves,
                uses: r.uses,
                config: r.config,
                node: r.node,
            };
            Ok(Object::Deployment { name: r.name, replicas: r.replicas.unwrap_or(1), template })
        }
        "Space" => {
            let mem = r.resources.mem.ok_or("a Space needs resources.mem")?;
            check_size(&mem)?;
            Ok(Object::Space { name: r.name, mem, tasks: r.resources.tasks.ok_or("a Space needs resources.tasks")? })
        }
        "Config" => {
            if let Some(f) = r.files.keys().find(|f| !f.starts_with('/')) {
                return Err(format!("bad file {f:?} (a path from /)"));
            }
            Ok(Object::Config { name: r.name, files: r.files.into_iter().collect(), env: r.env.into_iter().collect() })
        }
        "Function" => {
            for u in &r.uses {
                check_use(u)?;
            }
            let mem = r.resources.mem.unwrap_or_else(|| String::from("8M"));
            check_size(&mem)?;
            let spec = FnSpec {
                image: r.image.ok_or("a Function needs image")?,
                args: r.args,
                uses: r.uses,
                mem,
                tasks: r.resources.tasks.unwrap_or(2),
                timeout: r.timeout.unwrap_or(10).clamp(1, 300),
                max: r.max.unwrap_or(4).clamp(1, 16),
                idle: r.idle.unwrap_or(30).clamp(1, 3600),
                reuse: r.reuse.unwrap_or(true),
                expose: r.expose.unwrap_or(false),
            };
            Ok(Object::Function { name: r.name, spec })
        }
        k => Err(format!("unknown kind {k:?} (Deployment, Space, Config, Function)")),
    }
}
