//! net: the virtio-net driver (`virtio-drivers`) under the `smoltcp`
//! TCP/IP stack. QEMU user networking: we are 10.0.2.15/24 behind the
//! gateway 10.0.2.2. Namespace: `mmio`, `irq`, `client`. TCP client
//! connections are served as socket channels (libsys::net).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use libsys::net::CONNECT;
use libsys::wire::{FAILED, OK};
use libsys::{Channel, Error, Handle, Irq, Mmio, Port, ns, println, signal};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use virtio::RosHal;
use virtio::virtio_drivers::device::net::{RxBuffer, VirtIONet};
use virtio::virtio_drivers::transport::mmio::MmioTransport;

libsys::entry!(main);

const QUEUE: usize = 16;
const BUF_LEN: usize = 2048;
const SOCK_BUF: usize = 65536;

type Nic = VirtIONet<RosHal, MmioTransport<'static>, QUEUE>;

/// The NIC as a smoltcp device.
struct Dev(Rc<RefCell<Nic>>);

struct Rx(Rc<RefCell<Nic>>, RxBuffer);

struct Tx(Rc<RefCell<Nic>>);

impl phy::RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        let r = f(self.1.packet());
        let _ = self.0.borrow_mut().recycle_rx_buffer(self.1);
        r
    }
}

impl phy::TxToken for Tx {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut nic = self.0.borrow_mut();
        let mut tx = nic.new_tx_buffer(len);
        let r = f(tx.packet_mut());
        let _ = nic.send(tx);
        r
    }
}

impl phy::Device for Dev {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx;

    fn receive(&mut self, _t: Instant) -> Option<(Rx, Tx)> {
        let buf = {
            let mut nic = self.0.borrow_mut();
            if !nic.can_recv() {
                return None;
            }
            nic.receive().ok()?
        };
        Some((Rx(self.0.clone(), buf), Tx(self.0.clone())))
    }

    fn transmit(&mut self, _t: Instant) -> Option<Tx> {
        self.0.borrow().can_send().then(|| Tx(self.0.clone()))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.max_transmission_unit = 1514;
        c.max_burst_size = Some(1);
        c.medium = Medium::Ethernet;
        c
    }
}

fn now() -> Instant {
    Instant::from_micros((libsys::clock_ns() / 1000) as i64)
}

struct Conn {
    sock: SocketHandle,
    chan: Channel,
    /// Waiting for the connection: the client to answer.
    client: Option<usize>,
    /// Bytes from the channel the socket has not taken yet.
    out: Vec<u8>,
    chan_closed: bool,
}

const KEY_IRQ: u64 = 1 << 40;
const KEY_SOCK: u64 = 1 << 41;

fn main(_arg: usize) -> i64 {
    let (Some(m), Some(i), Some(c)) = (ns::get("mmio"), ns::get("irq"), ns::get("client")) else { return 1 };
    let nic = match virtio::transport(Mmio(m)).ok().and_then(|t| Nic::new(t, BUF_LEN).ok()) {
        Some(n) => n,
        None => {
            println!("net: no virtio-net device");
            return 2;
        }
    };
    let mac = nic.mac_address();
    let nic = Rc::new(RefCell::new(nic));
    nic.borrow_mut().enable_interrupts();
    let mut dev = Dev(nic.clone());
    let mut iface = Interface::new(Config::new(HardwareAddress::Ethernet(EthernetAddress(mac))), &mut dev, now());
    iface.update_ip_addrs(|a| {
        let _ = a.push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24));
    });
    let _ = iface.routes_mut().add_default_ipv4_route(Ipv4Address::new(10, 0, 2, 2));
    let mut sockets = SocketSet::new(Vec::new());
    println!(
        "net: virtio-net {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, 10.0.2.15/24 (smoltcp)",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );

    let irq = Irq(i);
    let Ok(port) = Port::create() else { return 3 };
    let mut clients: Vec<Option<Channel>> = alloc::vec![Some(Channel(c))];
    let mut conns: Vec<Option<Conn>> = Vec::new();
    let mut next_port = 49152u16;
    let _ = port.bind(irq.0, signal::READABLE, KEY_IRQ);
    let _ = port.bind(c, signal::READABLE | signal::PEER_CLOSED, 0);
    let mut buf = alloc::vec![0u8; libsys::MAX_MSG_BYTES];
    loop {
        let delay = iface.poll_delay(now(), &sockets).map(|d| d.total_micros() * 1000).unwrap_or(u64::MAX);
        match port.wait(delay.min(20_000_000)) {
            Ok(p) if p.key == KEY_IRQ => {
                let _ = nic.borrow_mut().ack_interrupt();
                let _ = irq.ack();
                let _ = port.bind(irq.0, signal::READABLE, KEY_IRQ);
            }
            Ok(p) if p.key >= KEY_SOCK => {
                if let Some(k) = conns.get_mut((p.key - KEY_SOCK) as usize).and_then(|x| x.as_mut()) {
                    loop {
                        match k.chan.read(&mut buf, &mut []) {
                            Ok((n, _)) => k.out.extend_from_slice(&buf[..n]),
                            Err(Error::ShouldWait) => {
                                let _ = port.bind(k.chan.0, signal::READABLE | signal::PEER_CLOSED, p.key);
                                break;
                            }
                            Err(_) => {
                                k.chan_closed = true;
                                break;
                            }
                        }
                    }
                }
            }
            Ok(p) => {
                let i = p.key as usize;
                let Some(ch) = clients[i] else { continue };
                let mut hs = [Handle(0); 1];
                loop {
                    match ch.read(&mut buf, &mut hs) {
                        Ok((7, 1)) if buf[0] == CONNECT => {
                            let ip = IpAddress::v4(buf[1], buf[2], buf[3], buf[4]);
                            let rport = u16::from_be_bytes([buf[5], buf[6]]);
                            let mut s = tcp::Socket::new(
                                tcp::SocketBuffer::new(alloc::vec![0; SOCK_BUF]),
                                tcp::SocketBuffer::new(alloc::vec![0; SOCK_BUF]),
                            );
                            // Give up on a connection nobody answers.
                            s.set_timeout(Some(smoltcp::time::Duration::from_secs(60)));
                            next_port = next_port.wrapping_add(1).max(49152);
                            if s.connect(iface.context(), (ip, rport), next_port).is_err() {
                                let _ = ch.write(&[FAILED], &[]);
                                let _ = hs[0].close();
                                continue;
                            }
                            let sock = sockets.add(s);
                            let k = conns.len() as u64;
                            let chan = Channel(hs[0]);
                            let _ = port.bind(chan.0, signal::READABLE | signal::PEER_CLOSED, KEY_SOCK + k);
                            conns.push(Some(Conn { sock, chan, client: Some(i), out: Vec::new(), chan_closed: false }));
                        }
                        Ok((1, 1)) if buf[0] == libsys::net::CLIENT => {
                            // Another client (no reply).
                            clients.push(Some(Channel(hs[0])));
                            let _ = port.bind(hs[0], signal::READABLE | signal::PEER_CLOSED, (clients.len() - 1) as u64);
                        }
                        Ok(_) => {
                            let _ = ch.write(&[FAILED], &[]);
                        }
                        Err(Error::ShouldWait) => {
                            let _ = port.bind(ch.0, signal::READABLE | signal::PEER_CLOSED, i as u64);
                            break;
                        }
                        Err(_) => {
                            clients[i] = None;
                            break;
                        }
                    }
                }
            }
            Err(_) => {}
        }
        // Run the stack, then move data between sockets and channels, and
        // run it again so that what was queued goes out.
        for _ in 0..2 {
            iface.poll(now(), &mut dev, &mut sockets);
            for slot in conns.iter_mut() {
                let Some(k) = slot else { continue };
                let s = sockets.get_mut::<tcp::Socket>(k.sock);
                if let Some(ci) = k.client {
                    match s.state() {
                        tcp::State::Established => {
                            if let Some(ch) = clients[ci] {
                                let _ = ch.write(&[OK], &[]);
                            }
                            k.client = None;
                        }
                        tcp::State::Closed => {
                            if let Some(ch) = clients[ci] {
                                let _ = ch.write(&[FAILED], &[]);
                            }
                            k.client = None;
                            k.chan_closed = true;
                        }
                        _ => {}
                    }
                }
                while s.can_recv() {
                    let n = s.recv(|d| {
                        let n = d.len().min(libsys::MAX_MSG_BYTES);
                        let _ = k.chan.write(&d[..n], &[]);
                        (n, n)
                    });
                    if !matches!(n, Ok(x) if x > 0) {
                        break;
                    }
                }
                if !k.out.is_empty() && s.can_send() {
                    if let Ok(n) = s.send_slice(&k.out) {
                        k.out.drain(..n);
                    }
                }
                if k.chan_closed && k.out.is_empty() {
                    s.close();
                }
                // The other side is done (or it is all over): so are we.
                let gone = matches!(s.state(), tcp::State::Closed | tcp::State::TimeWait) || (!s.may_recv() && k.client.is_none());
                if gone && (k.chan_closed || !s.may_recv()) {
                    let _ = k.chan.close();
                    sockets.remove(k.sock);
                    *slot = None;
                }
            }
        }
    }
}
