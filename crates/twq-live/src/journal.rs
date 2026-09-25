//! Append-only JSONL journal written by a background thread so file IO never blocks
//! the trading loop.

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::thread::JoinHandle;

use anyhow::Result;
use crossbeam_channel::{unbounded, Sender};

pub struct Journal {
    tx: Option<Sender<String>>,
    handle: Option<JoinHandle<()>>,
}

impl Journal {
    pub fn disabled() -> Self {
        Self { tx: None, handle: None }
    }

    pub fn open(path: &Path) -> Result<Self> {
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        let (tx, rx) = unbounded::<String>();
        let handle = std::thread::Builder::new().name("journal".into()).spawn(move || {
            let mut w = BufWriter::new(f);
            // Order-level events are rare, so flush each one: the journal stays
            // complete even if the process is killed.
            for line in rx {
                let _ = w.write_all(line.as_bytes());
                let _ = w.write_all(b"\n");
                let _ = w.flush();
            }
            let _ = w.flush();
        })?;
        Ok(Self { tx: Some(tx), handle: Some(handle) })
    }

    #[inline]
    pub fn log(&self, value: serde_json::Value) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(value.to_string());
        }
    }

    pub fn enabled(&self) -> bool {
        self.tx.is_some()
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
