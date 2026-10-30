//! Podfile.toml, read with the `toml` crate (shared by podimg on the host
//! and `dock build` in the OS).
//!
//! ```toml
//! name = "hello"            # image name
//! tag = "latest"            # optional
//! entry = "/bin/hello"      # program to start
//! from = "base"             # optional: layers go on top of this image
//! arches = "wasm32"         # optional: only these architectures (comma list)
//!
//! [programs]                # built per architecture, found in --programs dirs
//! "/bin/hello" = "hello"
//!
//! [files]                   # copied from the Podfile's directory
//! "/etc/motd" = "motd.txt"
//!
//! [env]
//! GREETING = "hi"
//! ```
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::format;

use serde::Deserialize;

#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Podfile {
    pub name: String,
    #[serde(default = "latest")]
    pub tag: String,
    #[serde(default)]
    pub entry: String,
    pub from: Option<String>,
    #[serde(default, deserialize_with = "comma_list")]
    pub arches: Option<Vec<String>>,
    #[serde(default)]
    pub programs: BTreeMap<String, String>,
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn latest() -> String {
    "latest".into()
}

fn comma_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Vec<String>>, D::Error> {
    let s: Option<String> = Option::deserialize(d)?;
    Ok(s.map(|s| s.split(',').map(|a| a.trim().to_string()).collect()))
}

pub fn parse(src: &str) -> Result<Podfile, String> {
    let p: Podfile = toml::from_str(src).map_err(|e| format!("Podfile: {e}"))?;
    if p.entry.is_empty() && p.from.is_none() {
        return Err("Podfile: entry is missing".into());
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses() {
        extern crate std;
        use alloc::vec;
        let p = super::parse(
            "name = \"hello\" # the image\nentry = \"/bin/hello\"\narches = \"wasm32, riscv64\"\n[programs]\n\"/bin/hello\" = \"hello\"\n[env]\nA = \"b # c\"\n",
        )
        .unwrap();
        assert_eq!(p.name, "hello");
        assert_eq!(p.tag, "latest");
        assert_eq!(p.programs["/bin/hello"], "hello");
        assert_eq!(p.env["A"], "b # c");
        assert_eq!(p.arches, Some(vec!["wasm32".into(), "riscv64".into()]));
        assert!(super::parse("entry = \"/x\"").is_err());
        assert!(super::parse("name = \"x\"\nentry = \"/x\"\n[nope]\na = \"b\"").is_err());
    }
}
