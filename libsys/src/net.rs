//! The net service's protocol: [CONNECT, IPv4 (4 bytes), port (u16 BE)]
//! with one channel handle, the socket: the reply is [OK] once connected
//! (or [FAILED]). Then the socket channel carries the stream's bytes both
//! ways; closing it closes the connection, and the service closes its end
//! when the other side does.
use alloc::vec::Vec;

use embedded_websocket::{
    WebSocketClient, WebSocketOptions, WebSocketReceiveMessageType, WebSocketSendMessageType,
};

use crate::wire::OK;
use crate::{Channel, Error, Result};

pub const CONNECT: u8 = 1;
/// [CLIENT] + a channel handle: serve another client on it (no reply).
pub const CLIENT: u8 = 2;

#[derive(Clone, Copy)]
pub struct Net(pub Channel);

impl Net {
    /// Another client channel of the net service.
    pub fn client(&self) -> Result<Net> {
        let (a, b) = Channel::create()?;
        self.0.write(&[CLIENT], &[b.0])?;
        Ok(Net(a))
    }

    pub fn connect(&self, ip: [u8; 4], port: u16) -> Result<Channel> {
        let (mine, theirs) = Channel::create()?;
        let mut req = Vec::from(&[CONNECT][..]);
        req.extend_from_slice(&ip);
        req.extend_from_slice(&port.to_be_bytes());
        self.0.write(&req, &[theirs.0])?;
        let mut r = [0u8; 16];
        let (n, _) = self.0.recv(&mut r, &mut [])?;
        if n == 0 || r[0] != OK {
            let _ = mine.close();
            return Err(Error::PeerClosed);
        }
        Ok(mine)
    }
}

/// A WebSocket client over a stream socket (binary messages; framing by
/// the `embedded-websocket` crate).
pub struct WebSocket {
    sock: Channel,
    ws: WebSocketClient<ClockRng>,
    /// Bytes received and not yet read as frames.
    buf: Vec<u8>,
}

/// Masking keys only need to be unpredictable to proxies: a clock-seeded
/// xorshift is enough.
pub struct ClockRng(u64);

impl rand_core::RngCore for ClockRng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for b in dest {
            *b = self.next_u64() as u8;
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> core::result::Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl rand_core::CryptoRng for ClockRng {}

impl WebSocket {
    /// HTTP upgrade of `path` on `sock`.
    pub fn open(sock: Channel, host: &str, path: &str) -> Result<WebSocket> {
        let rng = ClockRng(crate::clock_ns() | 1);
        let mut ws = WebSocket { sock, ws: WebSocketClient::new_client(rng), buf: Vec::new() };
        let opts = WebSocketOptions { path, host, origin: "", sub_protocols: None, additional_headers: None };
        let mut req = [0u8; 1024];
        let (n, key) = ws.ws.client_connect(&opts, &mut req).map_err(|_| Error::InvalidArgs)?;
        sock.write(&req[..n], &[])?;
        loop {
            match ws.ws.client_accept(&key, &ws.buf) {
                Ok((n, _)) => {
                    ws.buf.drain(..n);
                    return Ok(ws);
                }
                Err(embedded_websocket::Error::HttpHeaderIncomplete) => ws.fill()?,
                Err(_) => return Err(Error::NotSupported),
            }
        }
    }

    fn fill(&mut self) -> Result<()> {
        let mut b = alloc::vec![0u8; crate::MAX_MSG_BYTES];
        let (n, _) = self.sock.recv(&mut b, &mut [])?;
        self.buf.extend_from_slice(&b[..n]);
        Ok(())
    }

    pub fn send(&mut self, data: &[u8]) -> Result<()> {
        let mut f = alloc::vec![0u8; data.len() + 14];
        let n = self.ws.write(WebSocketSendMessageType::Binary, true, data, &mut f).map_err(|_| Error::InvalidArgs)?;
        for c in f[..n].chunks(crate::MAX_MSG_BYTES) {
            self.sock.write(c, &[])?;
        }
        Ok(())
    }

    /// The next whole message.
    pub fn recv(&mut self) -> Result<Vec<u8>> {
        let mut msg = Vec::new();
        let mut out = alloc::vec![0u8; crate::MAX_MSG_BYTES + 16];
        loop {
            if self.buf.is_empty() {
                self.fill()?;
            }
            match self.ws.read(&self.buf, &mut out) {
                Ok(r) => {
                    self.buf.drain(..r.len_from);
                    match r.message_type {
                        WebSocketReceiveMessageType::Binary | WebSocketReceiveMessageType::Text => {
                            msg.extend_from_slice(&out[..r.len_to]);
                            if r.end_of_message {
                                return Ok(msg);
                            }
                        }
                        WebSocketReceiveMessageType::CloseMustReply | WebSocketReceiveMessageType::CloseCompleted => {
                            return Err(Error::PeerClosed);
                        }
                        _ => {}
                    }
                }
                Err(embedded_websocket::Error::ReadFrameIncomplete) => self.fill()?,
                Err(_) => return Err(Error::Fault),
            }
        }
    }
}
