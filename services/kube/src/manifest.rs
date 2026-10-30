//! Manifests: what `kube apply` is given, one object per file (TOML).
//!
//! ```toml
//! kind = "Deployment"       # or "Space"
//! name = "web"
//! space = "demo"            # optional: the Space its pods' quotas come out of
//! image = "worker"
//! args = ["5", "1"]         # optional: the program's arguments
//! replicas = 3              # optional, 1
//! restart = "always"        # always | on-failure | never
//!
//! [resources]               # each pod's quota (a Space: the whole space's)
//! mem = "8M"
//! tasks = 2
//!
//! [labels]
//! app = "web"
//! ```
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
}

pub enum Object {
    Deployment { name: String, replicas: u32, template: Template },
    Space { name: String, mem: String, tasks: u64 },
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
            if let Some(s) = &r.space {
                check_name(s)?;
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
            };
            Ok(Object::Deployment { name: r.name, replicas: r.replicas.unwrap_or(1), template })
        }
        "Space" => {
            let mem = r.resources.mem.ok_or("a Space needs resources.mem")?;
            check_size(&mem)?;
            Ok(Object::Space { name: r.name, mem, tasks: r.resources.tasks.ok_or("a Space needs resources.tasks")? })
        }
        k => Err(format!("unknown kind {k:?} (Deployment, Space)")),
    }
}
