//! --usb-bridge: the link to the "BlackBox" CoreMIDI driver (tools/blackbox_midi_driver), TCP on
//! 127.0.0.1 with the driver listening. Each MIDI message is a frame: its length as 4 bytes
//! little-endian, then its bytes. The driver shows its device as offline while no one is
//! connected, so dropping a `Link` is how the emulator hangs up.
//!
//! All of this runs on the emulator's thread and must not hold it up: the socket is non-blocking,
//! the connecting happens on a short-lived thread of its own, and output that the driver is not
//! taking is bounded.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DRIVER_PORT: u16 = 47811;

/// The longest MIDI message either way, SysEx included: the driver's MAX_MESSAGE, the same value.
/// Far beyond any real SysEx dump, and a limit on what a bad length prefix can make us wait for.
pub const MAX_MESSAGE: usize = 1 << 20;
const HEADER: usize = 4;
/// Frames waiting for the driver; when full, new messages are dropped whole.
const MAX_PENDING: usize = 4 * (MAX_MESSAGE + HEADER);
/// How much to read in one go, so that a flood from the driver cannot keep the emulator here.
const READ_BUDGET: usize = 64 * 1024;
const RETRY: Duration = Duration::from_millis(500);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

pub fn driver_addr() -> SocketAddr {
    (Ipv4Addr::LOCALHOST, DRIVER_PORT).into()
}

/// Whole messages out of a byte stream that arrives in arbitrary pieces.
#[derive(Default)]
struct Decoder {
    buf: VecDeque<u8>,
}

impl Decoder {
    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes);
    }

    fn clear(&mut self) {
        self.buf.clear();
    }

    /// The next whole message, if it has arrived. Err: a length over the limit, after which the
    /// stream cannot be trusted to line up with any frame.
    fn next(&mut self) -> Result<Option<Vec<u8>>, u32> {
        if self.buf.len() < HEADER {
            return Ok(None);
        }
        let len = u32::from_le_bytes(std::array::from_fn(|i| self.buf[i]));
        if len as usize > MAX_MESSAGE {
            return Err(len);
        }
        if self.buf.len() < HEADER + len as usize {
            return Ok(None);
        }
        self.buf.drain(..HEADER);
        Ok(Some(self.buf.drain(..len as usize).collect()))
    }
}

/// The connection while the device is on the bus, trying again while there is none. Dropping it
/// closes the socket.
pub struct Link {
    addr: SocketAddr,
    stream: Option<TcpStream>,
    connecting: Option<mpsc::Receiver<std::io::Result<TcpStream>>>,
    next_try: Instant,
    /// the driver's absence has been reported: do not repeat it every retry
    said_absent: bool,
    decoder: Decoder,
    out: Vec<u8>,
    sent: usize,
    said_full: bool,
}

impl Link {
    pub fn new(addr: SocketAddr) -> Link {
        Link {
            addr,
            stream: None,
            connecting: None,
            next_try: Instant::now(),
            said_absent: false,
            decoder: Decoder::default(),
            out: Vec::new(),
            sent: 0,
            said_full: false,
        }
    }

    /// Moves the connecting along.
    pub fn poll(&mut self, now: Instant) {
        if self.stream.is_some() {
            return;
        }
        if let Some(rx) = &self.connecting {
            let result = match rx.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => Err(ErrorKind::BrokenPipe.into()),
            };
            self.connecting = None;
            self.next_try = now + RETRY;
            match result {
                Ok(stream) => {
                    self.stream = Some(stream);
                    self.said_absent = false;
                    eprintln!("USB: connected to the BlackBox MIDI driver");
                }
                Err(e) if !self.said_absent => {
                    self.said_absent = true;
                    eprintln!("USB: no BlackBox MIDI driver at {} ({e}), retrying", self.addr);
                }
                Err(_) => {}
            }
        } else if now >= self.next_try {
            self.next_try = now + RETRY;
            let (tx, rx) = mpsc::channel();
            let addr = self.addr;
            // (std cannot connect without blocking. If the link is dropped meanwhile, the send
            // fails and the new connection is closed at once.)
            let spawned = std::thread::Builder::new().name("usb-bridge-connect".into()).spawn(move || {
                let _ = tx.send(connect(addr));
            });
            if spawned.is_ok() {
                self.connecting = Some(rx);
            }
        }
    }

    /// Reads what is there, within the budget, and hands over the whole messages.
    pub fn receive(&mut self, now: Instant, mut deliver: impl FnMut(&[u8])) {
        let Some(mut stream) = self.stream.as_ref() else { return };
        let mut buf = [0u8; 4096];
        let mut budget = READ_BUDGET;
        let mut failure = None;
        while budget > 0 && failure.is_none() {
            match stream.read(&mut buf) {
                Ok(0) => failure = Some("closed by the driver".to_string()),
                Ok(n) => {
                    budget = budget.saturating_sub(n);
                    self.decoder.push(&buf[..n]);
                    loop {
                        match self.decoder.next() {
                            Ok(Some(msg)) if msg.is_empty() => {}
                            Ok(Some(msg)) => deliver(&msg),
                            Ok(None) => break,
                            Err(len) => {
                                failure = Some(format!("frame of {len} bytes"));
                                break;
                            }
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => failure = Some(e.to_string()),
            }
        }
        if let Some(why) = failure {
            self.lost(now, &why);
        }
    }

    /// Queues a message; without a connection it goes nowhere.
    pub fn send(&mut self, msg: &[u8]) {
        if self.stream.is_none() || msg.is_empty() {
            return;
        }
        if msg.len() > MAX_MESSAGE || self.pending() + HEADER + msg.len() > MAX_PENDING {
            if !self.said_full {
                self.said_full = true;
                eprintln!("USB: the BlackBox MIDI driver is not keeping up, dropping MIDI");
            }
            return;
        }
        // (what has been written goes once it is a queue's worth: that bounds the buffer)
        if self.sent >= MAX_PENDING {
            self.out.drain(..self.sent);
            self.sent = 0;
        }
        self.out.extend_from_slice(&(msg.len() as u32).to_le_bytes());
        self.out.extend_from_slice(msg);
    }

    /// Writes as much of the queue as the socket takes now.
    pub fn flush(&mut self, now: Instant) {
        let Some(mut stream) = self.stream.as_ref() else { return };
        let mut failure = None;
        while self.sent < self.out.len() {
            match stream.write(&self.out[self.sent..]) {
                Ok(n) => self.sent += n,
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => {
                    failure = Some(e.to_string());
                    break;
                }
            }
        }
        if self.sent == self.out.len() {
            self.out.clear();
            self.sent = 0;
            self.said_full = false;
        }
        if let Some(why) = failure {
            self.lost(now, &why);
        }
    }

    fn pending(&self) -> usize {
        self.out.len() - self.sent
    }

    fn lost(&mut self, now: Instant, why: &str) {
        eprintln!("USB: lost the BlackBox MIDI driver ({why}), retrying");
        self.stream = None;
        self.next_try = now + RETRY;
        self.decoder.clear();
        self.out.clear();
        self.sent = 0;
        self.said_full = false;
    }
}

fn connect(addr: SocketAddr) -> std::io::Result<TcpStream> {
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    stream.set_nodelay(true)?;
    stream.set_nonblocking(true)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn frame(msg: &[u8]) -> Vec<u8> {
        let mut out = (msg.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(msg);
        out
    }

    #[test]
    fn decoder_takes_frames_in_any_pieces() {
        let mut stream = frame(&[0x90, 60, 100]);
        stream.extend(frame(&[0xf8]));
        stream.extend(frame(&[]));
        let big: Vec<u8> = (0..70_000u32).map(|i| i as u8).collect();
        stream.extend(frame(&big));

        for piece in [1, 3, 5, 4096, stream.len()] {
            let mut d = Decoder::default();
            let mut got = Vec::new();
            for chunk in stream.chunks(piece) {
                d.push(chunk);
                while let Some(msg) = d.next().unwrap() {
                    got.push(msg);
                }
            }
            assert_eq!(got, vec![vec![0x90, 60, 100], vec![0xf8], vec![], big.clone()], "pieces of {piece}");
            assert!(d.buf.is_empty());
        }
    }

    #[test]
    fn decoder_refuses_an_impossible_length() {
        let mut d = Decoder::default();
        d.push(&((MAX_MESSAGE + 1) as u32).to_le_bytes());
        assert_eq!(d.next(), Err(MAX_MESSAGE as u32 + 1));

        let mut d = Decoder::default();
        d.push(&(MAX_MESSAGE as u32).to_le_bytes());
        assert_eq!(d.next(), Ok(None)); // the longest allowed: just wait for it
    }

    /// Time moves by a whole retry per step here, so tests do not wait for the real interval.
    struct Rig {
        link: Link,
        now: Instant,
        got: Vec<Vec<u8>>,
    }

    impl Rig {
        fn new(addr: SocketAddr) -> Rig {
            Rig { link: Link::new(addr), now: Instant::now(), got: Vec::new() }
        }

        fn step(&mut self) {
            self.now += RETRY;
            self.link.poll(self.now);
            let got = &mut self.got;
            self.link.receive(self.now, |m| got.push(m.to_vec()));
            self.link.flush(self.now);
            std::thread::sleep(Duration::from_millis(1));
        }

        fn until(&mut self, what: &str, mut done: impl FnMut(&Rig) -> bool) {
            for _ in 0..3000 {
                if done(self) {
                    return;
                }
                self.step();
            }
            panic!("timed out waiting for: {what}");
        }

        fn connected(&mut self) {
            self.until("the connection", |r| r.link.stream.is_some());
        }
    }

    fn listener() -> (TcpListener, SocketAddr) {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = l.local_addr().unwrap();
        (l, addr)
    }

    fn accept(l: &TcpListener) -> TcpStream {
        let (s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s
    }

    fn read_frame(s: &mut TcpStream) -> Vec<u8> {
        let mut h = [0u8; 4];
        s.read_exact(&mut h).unwrap();
        let mut msg = vec![0; u32::from_le_bytes(h) as usize];
        s.read_exact(&mut msg).unwrap();
        msg
    }

    #[test]
    fn finds_a_driver_that_appears_late() {
        let (l, addr) = listener();
        drop(l); // nothing is listening
        let mut r = Rig::new(addr);
        r.until("an attempt to fail", |r| r.link.said_absent);
        assert!(r.link.stream.is_none());

        r.link.send(&[0x90, 60, 100]); // no connection: goes nowhere
        assert_eq!(r.link.pending(), 0);

        let l = TcpListener::bind(addr).unwrap();
        r.connected();
        assert!(!r.link.said_absent);
        let mut peer = accept(&l);
        r.link.send(&[0x90, 60, 100]);
        r.link.flush(r.now);
        assert_eq!(read_frame(&mut peer), [0x90, 60, 100]);
    }

    #[test]
    fn carries_frames_both_ways_in_pieces() {
        let (l, addr) = listener();
        let mut r = Rig::new(addr);
        r.connected();
        let mut peer = accept(&l);
        peer.set_nodelay(true).unwrap();

        let sysex: Vec<u8> = std::iter::once(0xf0).chain((0..20_000u32).map(|i| (i % 128) as u8)).chain([0xf7]).collect();
        r.link.send(&sysex);
        r.link.send(&[0xc0, 5]);
        r.link.flush(r.now);
        assert_eq!(read_frame(&mut peer), sysex);
        assert_eq!(read_frame(&mut peer), [0xc0, 5]);

        // cut in the middle of the length and of the message
        let mut bytes = frame(&[0x90, 60, 100]);
        bytes.extend(frame(&sysex));
        peer.write_all(&bytes[..2]).unwrap();
        r.step();
        assert!(r.got.is_empty());
        peer.write_all(&bytes[2..12]).unwrap();
        r.until("the first message", |r| r.got.len() == 1);
        peer.write_all(&bytes[12..]).unwrap();
        r.until("the second message", |r| r.got.len() == 2);
        assert_eq!(r.got, vec![vec![0x90, 60, 100], sysex]);
    }

    #[test]
    fn reconnects_after_the_driver_goes_and_forgets_the_partial_frame() {
        let (l, addr) = listener();
        let mut r = Rig::new(addr);
        r.connected();
        let mut peer = accept(&l);
        peer.write_all(&frame(&[0xf0, 1, 2, 0xf7])[..5]).unwrap(); // half a frame, then gone
        r.step();
        drop(peer);
        r.until("noticing the loss", |r| r.link.stream.is_none());
        assert!(r.got.is_empty());

        r.connected();
        let mut peer = accept(&l);
        peer.write_all(&frame(&[0xf8])).unwrap();
        r.until("a message on the new connection", |r| !r.got.is_empty());
        assert_eq!(r.got, vec![vec![0xf8]]);
    }

    #[test]
    fn an_impossible_frame_ends_the_connection() {
        let (l, addr) = listener();
        let mut r = Rig::new(addr);
        r.connected();
        let mut peer = accept(&l);
        peer.write_all(&u32::MAX.to_le_bytes()).unwrap();
        r.until("the hang-up", |r| r.link.stream.is_none());
        let mut byte = [0u8];
        assert!(matches!(peer.read(&mut byte), Ok(0) | Err(_)));
        assert!(r.got.is_empty());
    }

    #[test]
    fn a_driver_that_stops_reading_costs_only_dropped_messages() {
        let (l, addr) = listener();
        let mut r = Rig::new(addr);
        r.connected();
        let mut peer = accept(&l);

        let msg: Vec<u8> = std::iter::once(0xf0).chain(std::iter::repeat(0x55).take(100_000)).chain([0xf7]).collect();
        let started = Instant::now();
        for _ in 0..200 { // 20 MB: far beyond the socket buffers and the queue
            r.link.send(&msg);
            r.link.flush(r.now);
            assert!(r.link.pending() <= MAX_PENDING);
        }
        assert!(started.elapsed() < Duration::from_secs(5), "the writes must not wait for the peer");
        assert!(r.link.said_full);
        assert!(r.link.stream.is_some());
        assert!(r.link.out.capacity() <= 3 * MAX_PENDING);

        // whatever did get queued is still whole frames, partly written or not
        let reader = std::thread::spawn(move || {
            let mut frames = 0usize;
            let mut byte = [0u8; 4];
            while peer.read_exact(&mut byte).is_ok() {
                let mut body = vec![0; u32::from_le_bytes(byte) as usize];
                peer.read_exact(&mut body).unwrap();
                assert_eq!(body.len(), 100_002);
                assert_eq!((body[0], body[100_001]), (0xf0, 0xf7));
                frames += 1;
            }
            frames
        });
        r.until("the queue to drain", |r| r.link.pending() == 0);
        assert!(!r.link.said_full);
        drop(r); // hang up: the reader sees the end
        assert!(reader.join().unwrap() >= 40); // MAX_PENDING's worth, at least
    }

    #[test]
    fn a_message_over_the_limit_is_dropped_and_the_link_lives() {
        let (l, addr) = listener();
        let mut r = Rig::new(addr);
        r.connected();
        let mut peer = accept(&l);
        r.link.send(&vec![0x55; MAX_MESSAGE + 1]);
        assert_eq!(r.link.pending(), 0);
        let longest = vec![0x55; MAX_MESSAGE];
        r.link.send(&longest);
        let reader = std::thread::spawn(move || read_frame(&mut peer));
        r.until("the longest message to go out", |r| r.link.pending() == 0);
        assert_eq!(reader.join().unwrap(), longest);
    }

    #[test]
    fn dropping_the_link_hangs_up() {
        let (l, addr) = listener();
        let mut r = Rig::new(addr);
        r.connected();
        let mut peer = accept(&l);
        drop(r);
        let mut byte = [0u8];
        assert_eq!(peer.read(&mut byte).unwrap(), 0);
    }

    #[test]
    fn dropping_the_link_while_connecting_leaves_no_connection() {
        let (l, addr) = listener();
        let mut r = Rig::new(addr);
        r.link.poll(r.now); // the attempt is under way, or even done
        assert!(r.link.connecting.is_some());
        drop(r);
        l.set_nonblocking(true).unwrap();
        // whether it got in or not, a connection from it ends immediately
        std::thread::sleep(Duration::from_millis(50));
        if let Ok((s, _)) = l.accept() {
            s.set_nonblocking(false).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            assert_eq!((&s).read(&mut [0u8]).unwrap(), 0);
        }
    }
}
