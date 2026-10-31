//! HTTP for the machines (their `curl`): a page in the browser cannot make
//! requests to other sites, so the server makes them. A machine's request
//! may not reach this side's own network: addresses that are loopback,
//! private, link-local (cloud metadata), shared or multicast are refused,
//! unless named with `--http-allow ADDR:PORT`. The check is in the
//! resolver, so it holds for redirects and for names that resolve again.
//! At most 10 s and 4 MiB per request; `--no-http` turns it off.
use std::net::{IpAddr, SocketAddr};
use std::sync::OnceLock;
use std::time::Duration;

use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};

pub const MAX_BODY: u64 = 4 << 20;

pub struct Settings {
    pub enabled: bool,
    pub allow: Vec<SocketAddr>,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

pub fn configure(s: Settings) {
    let _ = SETTINGS.set(s);
}

fn settings() -> &'static Settings {
    SETTINGS.get_or_init(|| Settings { enabled: true, allow: Vec::new() })
}

/// Addresses on this side: the server's own machine and network.
pub fn local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_multicast()
                || a.is_broadcast()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(v4) => local(IpAddr::V4(v4)),
            None => {
                let s = a.segments();
                a.is_loopback() || a.is_unspecified() || a.is_multicast() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80
            }
        },
    }
}

#[derive(Debug, Default)]
struct Guard {
    dns: DefaultResolver,
}

impl Resolver for Guard {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let all = self.dns.resolve(uri, config, timeout)?;
        let mut ok = self.empty();
        for a in all.iter() {
            if !local(a.ip()) || settings().allow.contains(a) {
                ok.push(*a);
            }
        }
        if ok.is_empty() {
            let host = uri.host().unwrap_or("?");
            return Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{host} is on the server's own network (refused)"),
            )));
        }
        Ok(ok)
    }
}

pub struct Answer {
    pub status: u16,
    /// `Name: value` lines.
    pub headers: String,
    pub body: Vec<u8>,
}

/// `headers`: `Name: value` lines.
pub fn fetch(method: &str, url: &str, headers: &str, body: &[u8]) -> Result<Answer, String> {
    if !settings().enabled {
        return Err(String::from("this server does not make HTTP requests (--no-http)"));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(String::from("only http:// and https:// URLs"));
    }
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .max_redirects(5)
        .user_agent("ruos-curl/1")
        .build();
    let agent = ureq::Agent::with_parts(config, ureq::unversioned::transport::DefaultConnector::default(), Guard::default());
    let mut req = ureq::http::Request::builder().method(method).uri(url);
    for l in headers.lines() {
        if let Some((k, v)) = l.split_once(':') {
            req = req.header(k.trim(), v.trim());
        }
    }
    let req = req.body(body.to_vec()).map_err(|e| format!("bad request: {e}"))?;
    let mut resp = agent.run(req).map_err(|e| match e {
        ureq::Error::Io(io) => io.to_string(),
        e => e.to_string(),
    })?;
    let status = resp.status().as_u16();
    let mut h = String::new();
    for (k, v) in resp.headers() {
        h += &format!("{}: {}\n", k, v.to_str().unwrap_or("?"));
    }
    let body = if method == "HEAD" {
        Vec::new()
    } else {
        resp.body_mut().with_config().limit(MAX_BODY).read_to_vec().map_err(|e| format!("body: {e}"))?
    };
    Ok(Answer { status, headers: h, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses() {
        for a in ["127.0.0.1", "10.1.2.3", "172.16.0.1", "192.168.1.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:127.0.0.1"] {
            assert!(local(a.parse().unwrap()), "{a}");
        }
        for a in ["93.184.215.14", "8.8.8.8", "2606:4700::1111"] {
            assert!(!local(a.parse().unwrap()), "{a}");
        }
    }

    #[test]
    fn refuses_loopback() {
        let e = fetch("GET", "http://127.0.0.1:9/", "", b"").err().unwrap();
        assert!(e.contains("refused"), "{e}");
        assert!(fetch("GET", "file:///etc/passwd", "", b"").is_err());
    }
}
