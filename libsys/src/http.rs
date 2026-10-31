//! HTTP (curl): the server makes the request (a page in the browser cannot
//! reach other sites), so this goes through the store to the server. Only
//! a store client that came from the store's `web` channel may ask: that
//! channel is the `http` capability (`http` in a namespace).
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::store::{CONNECT, HTTP_FETCH, HTTP_READ, Store};
use crate::{Channel, Result, ns};

const OK: u8 = 0;

pub struct Response {
    pub status: u16,
    /// `Name: value` lines.
    pub headers: String,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy)]
pub struct Http(pub Channel);

impl Http {
    pub fn get() -> Option<Http> {
        ns::get("http").map(|h| Http(Channel(h)))
    }

    /// Another client with the same capability (to hand to a program).
    pub fn connect(&self) -> Result<Http> {
        let (a, b) = Channel::create()?;
        self.0.write(&[CONNECT], &[b.0])?;
        Ok(Http(a))
    }

    /// `headers`: `Name: value` lines. Err(why) when there is no answer
    /// (refused, no server, timed out…).
    pub fn fetch(&self, method: &str, url: &str, headers: &str, body: &[u8]) -> Result<core::result::Result<Response, String>> {
        let s = Store(self.0);
        let (st, head) = s.raw(HTTP_FETCH, &[method.as_bytes(), url.as_bytes(), headers.as_bytes(), body])?;
        let head = String::from_utf8_lossy(&head).into_owned();
        if st != OK {
            return Ok(Err(head));
        }
        let (first, headers) = head.split_once('\n').unwrap_or((&head, ""));
        let mut w = first.split(' ');
        let status = w.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let len: usize = w.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let mut body = Vec::with_capacity(len);
        while body.len() < len {
            let (st, part) = s.raw(HTTP_READ, &[format!("{}", body.len()).as_bytes()])?;
            if st != OK || part.is_empty() {
                break;
            }
            body.extend_from_slice(&part);
        }
        Ok(Ok(Response { status, headers: String::from(headers), body }))
    }
}
