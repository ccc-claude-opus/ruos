//! console: the line discipline. Its namespace has `serial` and a first
//! `client` channel; more clients arrive with CONNECT. Clients send WRITE and
//! READ_LINE requests (libsys::console). Input is echoed as it is typed
//! while a client waits for a line; type-ahead is echoed when its line is
//! handed out, so transcripts read in order. Ctrl-C drops the line being
//! typed; ↑ and ↓ (ESC [ A, ESC [ B) bring back earlier lines.
#![no_std]
#![no_main]

use libsys::console::{CONNECT, MAX_LINE, READ_LINE, WRITE};
use libsys::{Channel, Error, Handle, Port, Serial, ns, signal};

libsys::entry!(main);

const MAX_CLIENTS: usize = 16;
/// Lines typed ahead (test scripts send all of theirs at once).
const MAX_LINES: usize = 1024;
const SERIAL_KEY: u64 = 1000;

/// Columns a character takes on a terminal (wide East Asian ones: 2).
fn width(c: char) -> usize {
    match c as u32 {
        0x1100..=0x115f | 0x2e80..=0xa4cf | 0xac00..=0xd7a3 | 0xf900..=0xfaff | 0xfe30..=0xfe4f | 0xff00..=0xff60
        | 0xffe0..=0xffe6 | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

struct Line {
    b: [u8; MAX_LINE],
    n: usize,
}

struct Console {
    serial: Serial,
    clients: [Option<Channel>; MAX_CLIENTS],
    /// Clients waiting for a line, oldest first.
    readers: [usize; MAX_CLIENTS],
    nreaders: usize,
    /// In static memory: too big for the 64 KiB stack.
    lines: &'static mut [Line; MAX_LINES],
    nlines: usize,
    cur: Line,
    /// Whether `cur` has been echoed so far.
    cur_echoed: bool,
    last_cr: bool,
    /// Lines entered, oldest first (in static memory like `lines`).
    history: &'static mut [Line; HISTORY],
    nhistory: usize,
    /// Where ↑ / ↓ are in the history (None: at the new line).
    at: Option<usize>,
    /// An escape sequence so far: 1 after ESC, 2 after ESC [.
    esc: u8,
}

const HISTORY: usize = 64;

impl Console {
    fn echo(&self, b: &[u8]) {
        let _ = self.serial.write(b);
    }

    /// Replaces the line being typed (redrawn if it is shown).
    fn set_line(&mut self, b: &[u8]) {
        if self.cur_echoed || self.nreaders > 0 {
            let w: usize = core::str::from_utf8(&self.cur.b[..self.cur.n]).map(|s| s.chars().map(width).sum()).unwrap_or(self.cur.n);
            if w > 0 {
                let mut seq = [0u8; 16];
                let n = fmt_back(w, &mut seq);
                self.echo(&seq[..n]);
            }
            self.echo(b);
            self.cur_echoed = true;
        }
        let n = b.len().min(MAX_LINE);
        self.cur.b[..n].copy_from_slice(&b[..n]);
        self.cur.n = n;
    }

    fn recall(&mut self, up: bool) {
        let next = match (up, self.at) {
            (true, None) if self.nhistory > 0 => Some(self.nhistory - 1),
            (true, Some(i)) => Some(i.saturating_sub(1)),
            (false, Some(i)) if i + 1 < self.nhistory => Some(i + 1),
            (false, Some(_)) => None,
            _ => return,
        };
        self.at = next;
        let line = match next {
            Some(i) => {
                let h = &self.history[i];
                let mut b = [0u8; MAX_LINE];
                b[..h.n].copy_from_slice(&h.b[..h.n]);
                (b, h.n)
            }
            None => ([0u8; MAX_LINE], 0),
        };
        self.set_line(&line.0[..line.1]);
    }

    fn remember(&mut self) {
        let n = self.cur.n;
        if n == 0 || (self.nhistory > 0 && self.history[self.nhistory - 1].b[..self.history[self.nhistory - 1].n] == self.cur.b[..n]) {
            return;
        }
        if self.nhistory == HISTORY {
            self.history.rotate_left(1);
            self.nhistory -= 1;
        }
        let h = &mut self.history[self.nhistory];
        h.b[..n].copy_from_slice(&self.cur.b[..n]);
        h.n = n;
        self.nhistory += 1;
    }

    fn key(&mut self, c: u8) {
        // ESC [ A / ESC [ B: the arrow keys; other sequences are dropped.
        match (self.esc, c) {
            (0, 0x1b) => {
                self.esc = 1;
                return;
            }
            (1, b'[') => {
                self.esc = 2;
                return;
            }
            (1, _) => self.esc = 0,
            (2, b'0'..=b'9' | b';') => return,
            (2, _) => {
                self.esc = 0;
                match c {
                    b'A' => self.recall(true),
                    b'B' => self.recall(false),
                    _ => {}
                }
                return;
            }
            _ => {}
        }
        if c == 0x03 {
            self.echo(b"^C\n");
            self.cur.n = 0;
            self.cur_echoed = false;
            return;
        }
        let cr = c == b'\r';
        if c == b'\n' && self.last_cr {
            self.last_cr = false;
            return;
        }
        self.last_cr = cr;
        let live = self.nreaders > 0;
        match c {
            b'\r' | b'\n' => {
                if live {
                    self.echo(b"\n");
                }
                self.remember();
                self.at = None;
                if self.nlines < MAX_LINES {
                    let l = &mut self.lines[self.nlines];
                    l.b[..self.cur.n].copy_from_slice(&self.cur.b[..self.cur.n]);
                    l.n = self.cur.n;
                    self.nlines += 1;
                }
                // A line typed while someone waited was echoed already.
                let echoed = live || self.cur_echoed;
                self.cur.n = 0;
                self.cur_echoed = false;
                self.deliver(echoed);
            }
            0x7f | 0x08 => {
                // A whole UTF-8 character goes; the cursor moves back by
                // its width (CJK characters take two columns).
                let mut start = self.cur.n;
                while start > 0 {
                    start -= 1;
                    if self.cur.b[start] & 0xc0 != 0x80 {
                        break;
                    }
                }
                if start < self.cur.n {
                    let w = core::str::from_utf8(&self.cur.b[start..self.cur.n])
                        .ok()
                        .and_then(|s| s.chars().next())
                        .map(width)
                        .unwrap_or(1);
                    self.cur.n = start;
                    if live {
                        self.echo(if w == 2 { b"\x1b[2D\x1b[K" } else { b"\x1b[D\x1b[K" });
                    }
                }
            }
            _ if self.cur.n < MAX_LINE => {
                self.cur.b[self.cur.n] = c;
                self.cur.n += 1;
                if live {
                    self.echo(&[c]);
                    self.cur_echoed = true;
                }
            }
            _ => {}
        }
    }

    /// Hands queued lines to waiting readers.
    fn deliver(&mut self, mut echoed: bool) {
        while self.nreaders > 0 && self.nlines > 0 {
            let who = self.readers[0];
            self.readers.copy_within(1..self.nreaders, 0);
            self.nreaders -= 1;
            let n = self.lines[0].n;
            let mut msg = [0u8; 1 + MAX_LINE];
            msg[0] = READ_LINE;
            msg[1..1 + n].copy_from_slice(&self.lines[0].b[..n]);
            if !echoed {
                self.echo(&self.lines[0].b[..n]);
                self.echo(b"\n");
            }
            echoed = false;
            for i in 1..self.nlines {
                let (a, b) = self.lines.split_at_mut(i);
                a[i - 1].b = b[0].b;
                a[i - 1].n = b[0].n;
            }
            self.nlines -= 1;
            if let Some(c) = self.clients[who] {
                let _ = c.write(&msg[..1 + n], &[]);
            }
        }
        // Someone is waiting for a line still being typed: show it so far.
        if self.nreaders > 0 && !self.cur_echoed && self.cur.n > 0 {
            self.echo(&self.cur.b[..self.cur.n]);
            self.cur_echoed = true;
        }
    }

    fn add(&mut self, c: Channel, port: &Port) -> bool {
        match self.clients.iter().position(|x| x.is_none()) {
            Some(i) => {
                self.clients[i] = Some(c);
                let _ = port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, i as u64);
                true
            }
            None => false,
        }
    }

    /// Serves one client's queued requests; false once it is gone.
    fn serve(&mut self, i: usize, port: &Port) -> bool {
        let Some(c) = self.clients[i] else { return false };
        let mut buf = [0u8; 1 + 512];
        let mut hs = [Handle(0); 1];
        loop {
            match c.read(&mut buf, &mut hs) {
                Ok((n, nh)) => match buf.first() {
                    Some(&WRITE) if n > 0 => self.echo(&buf[1..n]),
                    Some(&READ_LINE) => {
                        if self.nreaders < MAX_CLIENTS {
                            self.readers[self.nreaders] = i;
                            self.nreaders += 1;
                        }
                        self.deliver(false);
                    }
                    Some(&CONNECT) if nh == 1 => {
                        if !self.add(Channel(hs[0]), port) {
                            let _ = hs[0].close();
                        }
                    }
                    _ => {}
                },
                Err(Error::ShouldWait) => return true,
                Err(_) => {
                    let _ = c.close();
                    self.clients[i] = None;
                    let mut k = 0;
                    for j in 0..self.nreaders {
                        if self.readers[j] != i {
                            self.readers[k] = self.readers[j];
                            k += 1;
                        }
                    }
                    self.nreaders = k;
                    return false;
                }
            }
        }
    }
}

fn main(_arg: usize) -> i64 {
    let Ok(port) = Port::create() else { return 1 };
    let Some(serial) = ns::get("serial") else { return 3 };
    const EMPTY: Line = Line { b: [0; MAX_LINE], n: 0 };
    static mut LINES: [Line; MAX_LINES] = [EMPTY; MAX_LINES];
    static mut HIST: [Line; HISTORY] = [EMPTY; HISTORY];
    let mut con = Console {
        serial: Serial(serial),
        clients: [None; MAX_CLIENTS],
        readers: [0; MAX_CLIENTS],
        nreaders: 0,
        lines: unsafe { &mut *(&raw mut LINES) },
        nlines: 0,
        history: unsafe { &mut *(&raw mut HIST) },
        nhistory: 0,
        at: None,
        esc: 0,
        cur: EMPTY,
        cur_echoed: false,
        last_cr: false,
    };
    if let Some(c) = ns::get("client") {
        con.add(Channel(c), &port);
    }
    let _ = port.bind(con.serial.0, signal::READABLE, SERIAL_KEY);
    loop {
        let Ok(p) = port.wait(u64::MAX) else { return 2 };
        if p.key == SERIAL_KEY {
            let mut buf = [0u8; 256];
            while let Ok(n) = con.serial.read(&mut buf) {
                for &c in &buf[..n] {
                    con.key(c);
                }
            }
            let _ = port.bind(con.serial.0, signal::READABLE, SERIAL_KEY);
        } else {
            let i = p.key as usize;
            if con.serve(i, &port) {
                if let Some(c) = con.clients[i] {
                    let _ = port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, i as u64);
                }
            }
            if con.clients.iter().all(|c| c.is_none()) {
                return 0;
            }
        }
    }
}

/// ESC [ n D ESC [ K: back `n` columns and erase to the end of the line.
fn fmt_back(n: usize, out: &mut [u8; 16]) -> usize {
    let mut d = [0u8; 4];
    let mut k = 0;
    let mut x = n.min(9999);
    loop {
        d[k] = b'0' + (x % 10) as u8;
        k += 1;
        x /= 10;
        if x == 0 {
            break;
        }
    }
    out[0] = 0x1b;
    out[1] = b'[';
    let mut i = 2;
    while k > 0 {
        k -= 1;
        out[i] = d[k];
        i += 1;
    }
    out[i..i + 4].copy_from_slice(b"D\x1b[K");
    i + 4
}
