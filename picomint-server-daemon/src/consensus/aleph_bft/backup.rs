use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::io::{AsyncRead, AsyncWrite, Cursor};
use picomint_redb::Database;
use tracing::info;

use crate::LOG_CONSENSUS;
use crate::consensus::db::ALEPH_UNITS;

pub struct UnitLoader {
    cursor: Cursor<Vec<u8>>,
}

impl UnitLoader {
    pub fn new(db: Database) -> Self {
        let units: Vec<Vec<u8>> = db
            .begin_read()
            .iter(&ALEPH_UNITS, |r| r.map(|(_, v)| v).collect());

        if !units.is_empty() {
            info!(
                target: LOG_CONSENSUS,
                units_len = %units.len(),
                "Recovering from an in-session-shutdown"
            );
        }

        Self {
            cursor: Cursor::new(units.into_iter().flatten().collect()),
        }
    }
}

impl AsyncRead for UnitLoader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.cursor).poll_read(cx, buf)
    }
}

pub struct UnitSaver {
    db: Database,
    units_index: u64,
    buf: Vec<u8>,
}

impl UnitSaver {
    pub fn new(db: Database) -> Self {
        let units_index = db
            .begin_read()
            .iter(&ALEPH_UNITS, |r| r.next_back().map_or(0, |(k, _)| k + 1));

        Self {
            db,
            units_index,
            buf: Vec::new(),
        }
    }
}

impl AsyncWrite for UnitSaver {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.buf.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        if !this.buf.is_empty() {
            let tx = this.db.begin_write();
            let unit = std::mem::take(&mut this.buf);
            tx.insert(&ALEPH_UNITS, &this.units_index, &unit);
            this.units_index += 1;
            tx.commit();
        }

        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}
