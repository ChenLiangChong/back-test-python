//! TCP client for a broker bridge. A dedicated reader thread parses incoming lines
//! and timestamps them on arrival (`Instant`) so tick-to-order latency can be measured
//! end-to-end inside the engine.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver};

use crate::protocol::{encode, BridgeEvent, EngineCmd};

pub struct Inbound {
    pub recv_at: Instant,
    pub ev: BridgeEvent,
}

pub struct BridgeConn {
    writer: TcpStream,
    buf: Vec<u8>,
    pub rx: Receiver<Inbound>,
    reader: Option<JoinHandle<()>>,
}

impl BridgeConn {
    pub fn connect(addr: impl ToSocketAddrs, timeout: Duration) -> Result<Self> {
        let addr = addr.to_socket_addrs()?.next().context("bad bridge address")?;
        let stream =
            TcpStream::connect_timeout(&addr, timeout).with_context(|| format!("connecting to bridge {addr}"))?;
        Self::from_stream(stream)
    }

    pub fn from_stream(stream: TcpStream) -> Result<Self> {
        stream.set_nodelay(true)?;
        let read_half = stream.try_clone()?;
        let (tx, rx) = bounded::<Inbound>(1 << 16);
        let reader = std::thread::Builder::new().name("bridge-reader".into()).spawn(move || {
            let mut r = BufReader::with_capacity(1 << 16, read_half);
            let mut line = String::with_capacity(256);
            loop {
                line.clear();
                match r.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let recv_at = Instant::now();
                        let t = line.trim();
                        if t.is_empty() {
                            continue;
                        }
                        let ev = match BridgeEvent::parse(t) {
                            Ok(ev) => ev,
                            Err(e) => BridgeEvent::Error { msg: format!("bad message from bridge ({e}): {t}") },
                        };
                        if tx.send(Inbound { recv_at, ev }).is_err() {
                            break;
                        }
                    }
                }
            }
        })?;
        Ok(Self { writer: stream, buf: Vec::with_capacity(256), rx, reader: Some(reader) })
    }

    /// Serialize and write one command (single `write` syscall, Nagle disabled).
    #[inline]
    pub fn send(&mut self, cmd: &EngineCmd) -> Result<()> {
        encode(cmd, &mut self.buf);
        self.writer.write_all(&self.buf)?;
        Ok(())
    }

    pub fn shutdown(&mut self) {
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
    }
}

impl Drop for BridgeConn {
    fn drop(&mut self) {
        self.shutdown();
    }
}
