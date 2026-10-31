//! The controller (Kubernetes' controller manager and scheduler), run by
//! the leader only. From the records it decides which pods there should be
//! (`pod/P`) and on which node: Deployments are scaled and rolled out
//! (one new pod more than wanted, an old one goes once enough are ready),
//! and each new pod is scheduled: nodes that are up (their lease is
//! fresh), that can run the image (it has the node's architecture,
//! wasm32) and match the Deployment's `node`, the one with the fewest pods
//! first. A node that is gone loses its pods; they are made again
//! elsewhere.
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libsys::store::Store;
use objstore::Index;

use crate::api::Api;
use crate::kubelet::revision;

/// A node is up while it wrote its record this recently (ms).
pub const NODE_LEASE: u64 = 8000;
/// A node gone this long is forgotten.
const NODE_FORGET: u64 = 5 * 60 * 1000;

pub fn node_up(api: &Api, node: &str) -> bool {
    api.get(&format!("nodes/{node}")).is_some_and(|r| api.now().saturating_sub(r.updated) < NODE_LEASE)
}

struct PodRec {
    name: String,
    rev: u32,
    node: String,
}

fn ready(api: &Api, p: &PodRec) -> bool {
    api.get(&format!("status/{}", p.name)).is_some_and(|s| s.field("node") == Some(p.node.as_str()) && s.num("ready") == 1)
}

pub struct Controller {
    /// Image → its architectures.
    arches: BTreeMap<String, Vec<String>>,
}

impl Controller {
    pub fn new() -> Controller {
        Controller { arches: BTreeMap::new() }
    }

    fn arches(&mut self, store: &Store, image: &str) -> &[String] {
        if !self.arches.contains_key(image) {
            let a = store
                .index(image)
                .and_then(|id| store.object(&id))
                .ok()
                .and_then(|b| Index::decode(&b).ok())
                .map(|i| i.manifests.into_iter().map(|(a, _)| a).collect())
                .unwrap_or_default();
            self.arches.insert(String::from(image), a);
        }
        &self.arches[image]
    }

    /// The node for a new pod of `image`, or None (no node fits).
    fn schedule(&mut self, api: &Api, store: &Store, image: &str, want: Option<&str>) -> Option<String> {
        let arches = Vec::from(self.arches(store, image));
        let mut best: Option<(u64, String)> = None;
        for (k, r) in api.prefix("nodes/") {
            let node = &k[6..];
            if !node_up(api, node) || want.is_some_and(|w| w != node) {
                continue;
            }
            let arch = r.field("arch").unwrap_or("");
            let native = arches.iter().any(|a| a == arch);
            if !native && !arches.iter().any(|a| a == "wasm32") {
                continue;
            }
            let pods = api.prefix("pod/").filter(|(_, p)| p.field("node") == Some(node)).count() as u64;
            let score = pods + if native { 0 } else { 100 };
            if best.as_ref().is_none_or(|(s, _)| score < *s) {
                best = Some((score, String::from(node)));
            }
        }
        best.map(|(_, n)| n)
    }

    fn new_pod(&mut self, api: &mut Api, store: &Store, d: &str, rev: u32) {
        let mut next = 0;
        api.update(&format!("ctl/{d}"), |old| {
            let old = old.unwrap_or("");
            next = crate::api::field(old, "next").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) + 1;
            let rolling = crate::api::field(old, "rolling").unwrap_or("0");
            format!("next {next}\nrolling {rolling}\n")
        });
        let name = format!("{d}-{next}");
        let node = revision(api, d, rev).and_then(|(image, t)| self.schedule(api, store, &image, t.node.as_deref()));
        let now = api.now();
        let node = node.unwrap_or_else(|| String::from("-"));
        api.put(&format!("pod/{name}"), Some(0), &format!("deploy {d}\nrev {rev}\nnode {node}\ncreated {now}\n"));
        if node == "-" {
            api.event(d, &format!("FailedScheduling pod {name}: no node fits"));
        } else {
            api.event(d, &format!("Scheduled pod {name} on {node}"));
        }
    }

    fn remove_pod(api: &mut Api, d: &str, p: &PodRec, why: &str) {
        api.delete(&format!("pod/{}", p.name));
        // Its node removes it and its status; a node that is gone cannot.
        if !node_up(api, &p.node) {
            api.delete(&format!("status/{}", p.name));
        }
        api.event(d, &format!("Removed pod {} ({why})", p.name));
    }

    fn set_rolling(api: &mut Api, d: &str, on: bool) {
        api.update(&format!("ctl/{d}"), |old| {
            let next = old.and_then(|o| crate::api::field(o, "next")).unwrap_or("0");
            format!("next {next}\nrolling {}\n", on as u8)
        });
    }

    pub fn run(&mut self, api: &mut Api, store: &Store) {
        let deploys: Vec<(String, u32, u32)> =
            api.prefix("deploy/").map(|(k, r)| (String::from(&k[7..]), r.num("replicas") as u32, r.num("rev") as u32)).collect();
        // Pods, control records and events of Deployments that are gone.
        let orphans: Vec<String> = api
            .prefix("pod/")
            .filter(|(_, r)| !deploys.iter().any(|(d, _, _)| Some(d.as_str()) == r.field("deploy")))
            .map(|(k, _)| String::from(k))
            .collect();
        for k in orphans {
            api.delete(&k);
        }
        let stale: Vec<String> = api
            .prefix("ctl/")
            .chain(api.prefix("events/"))
            .filter(|(k, _)| {
                let name = k.split_once('/').map(|(_, x)| x).unwrap_or("");
                !deploys.iter().any(|(d, _, _)| d == name) && !(k.starts_with("events/") && api.get(&format!("fn/{name}")).is_some())
            })
            .map(|(k, _)| String::from(k))
            .collect();
        for k in stale {
            api.delete(&k);
        }
        let now = api.now();
        let gone: Vec<String> =
            api.prefix("nodes/").filter(|(_, r)| now.saturating_sub(r.updated) >= NODE_FORGET).map(|(k, _)| String::from(k)).collect();
        for k in gone {
            api.delete(&k);
        }
        for (d, want, rev) in deploys {
            self.deployment(api, store, &d, want as usize, rev);
        }
    }

    fn pods(api: &Api, d: &str) -> Vec<PodRec> {
        api.prefix("pod/")
            .filter(|(_, r)| r.field("deploy") == Some(d))
            .map(|(k, r)| PodRec { name: String::from(&k[4..]), rev: r.num("rev") as u32, node: String::from(r.field("node").unwrap_or("-")) })
            .collect()
    }

    fn deployment(&mut self, api: &mut Api, store: &Store, d: &str, want: usize, rev: u32) {
        // Pods on nodes that are gone are made again elsewhere; pods no node
        // could take get another try.
        for p in Self::pods(api, d) {
            if p.node == "-" {
                if let Some(node) = revision(api, d, p.rev).and_then(|(image, t)| self.schedule(api, store, &image, t.node.as_deref())) {
                    let key = format!("pod/{}", p.name);
                    api.update(&key, |old| {
                        let old = old.unwrap_or("");
                        let created = crate::api::field(old, "created").unwrap_or("0");
                        format!("deploy {d}\nrev {}\nnode {node}\ncreated {created}\n", p.rev)
                    });
                    api.event(d, &format!("Scheduled pod {} on {node}", p.name));
                }
            } else if !node_up(api, &p.node) {
                Self::remove_pod(api, d, &p, &format!("node {} is gone", p.node));
            }
        }
        let old = |api: &Api| Self::pods(api, d).into_iter().filter(|p| p.rev != rev).count();
        if old(api) > 0 {
            if api.get(&format!("ctl/{d}")).is_none_or(|r| r.num("rolling") == 0) {
                Self::set_rolling(api, d, true);
            }
            // Old pods that are not ready help nobody.
            for p in Self::pods(api, d) {
                if p.rev != rev && !ready(api, &p) {
                    Self::remove_pod(api, d, &p, "old revision");
                }
            }
            while old(api) > 0 && Self::pods(api, d).len() > want + 1 {
                let p = Self::pods(api, d).into_iter().find(|p| p.rev != rev).unwrap();
                Self::remove_pod(api, d, &p, "old revision");
            }
            let pods = Self::pods(api, d);
            let new = pods.iter().filter(|p| p.rev == rev).count();
            if new < want && pods.len() < want + 1 {
                self.new_pod(api, store, d, rev);
            }
            let pods = Self::pods(api, d);
            let n_ready = pods.iter().filter(|p| ready(api, p)).count();
            if n_ready > want
                && let Some(p) = pods.iter().find(|p| p.rev != rev)
            {
                Self::remove_pod(api, d, p, "old revision");
            }
        } else {
            let mut pods = Self::pods(api, d);
            // The newest go first: by number.
            pods.sort_by_key(|p| p.name.rsplit('-').next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0));
            while pods.len() > want {
                let p = pods.pop().unwrap();
                Self::remove_pod(api, d, &p, "scaled down");
            }
            for _ in pods.len()..want {
                self.new_pod(api, store, d, rev);
            }
        }
        let pods = Self::pods(api, d);
        let rolling = api.get(&format!("ctl/{d}")).is_some_and(|r| r.num("rolling") == 1);
        if rolling && pods.iter().all(|p| p.rev == rev) && pods.iter().filter(|p| ready(api, p)).count() >= want {
            Self::set_rolling(api, d, false);
            api.event(d, &format!("Rollout of revision {rev} complete"));
        }
    }
}
