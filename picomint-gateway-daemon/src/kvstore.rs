//! redb-backed [`KVStoreSync`] / [`KVStore`] implementation for the embedded
//! LDK node. All ldk-node state lives in a single table (`LDK_NODE`) keyed by
//! `(primary_namespace, secondary_namespace, key)`, colocated with the rest of
//! the gateway's redb file.
//!
//! Both traits are implemented on the same type because
//! `ldk_node::Builder::build_with_store` requires
//! `Arc<dyn SyncAndAsyncKVStore>` (= `KVStore + KVStoreSync`). The sync path
//! hits redb directly; the async path goes through `spawn_blocking` so we
//! don't block the runtime on redb I/O. Mirrors ldk-node's in-tree
//! `SqliteStore` pattern.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use lightning::io;
use lightning::util::persist::{KVStore, KVStoreSync};
use picomint_redb::Database;

use crate::db::LDK_NODE;

pub struct RedbKvStore {
    inner: Arc<RedbKvStoreInner>,
}

impl RedbKvStore {
    pub fn new(db: Database) -> Self {
        Self {
            inner: Arc::new(RedbKvStoreInner { db }),
        }
    }
}

struct RedbKvStoreInner {
    db: Database,
}

impl RedbKvStoreInner {
    fn read_internal(&self, p: &str, s: &str, k: &str) -> io::Result<Vec<u8>> {
        let tuple = (p.to_string(), s.to_string(), k.to_string());

        self.db
            .begin_read()
            .as_ref()
            .get(&LDK_NODE, &tuple)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("key not found: {p}/{s}/{k}"),
                )
            })
    }

    fn write_internal(&self, p: &str, s: &str, k: &str, buf: Vec<u8>) -> io::Result<()> {
        let tuple = (p.to_string(), s.to_string(), k.to_string());

        let dbtx = self.db.begin_write();
        dbtx.as_ref().insert(&LDK_NODE, &tuple, &buf);
        dbtx.commit();

        Ok(())
    }

    fn remove_internal(&self, p: &str, s: &str, k: &str) -> io::Result<()> {
        let tuple = (p.to_string(), s.to_string(), k.to_string());

        let dbtx = self.db.begin_write();
        dbtx.as_ref().remove(&LDK_NODE, &tuple);
        dbtx.commit();

        Ok(())
    }

    fn list_internal(&self, p: &str, s: &str) -> io::Result<Vec<String>> {
        // LDK namespaces are restricted to `KVSTORE_NAMESPACE_KEY_ALPHABET`
        // (alphanumeric, `_`, `-`), so `\0` is guaranteed greater than any
        // valid secondary namespace prefix — safe as the exclusive upper bound.
        let start = (p.to_string(), s.to_string(), String::new());
        let end = (p.to_string(), format!("{s}\0"), String::new());

        let keys = self
            .db
            .begin_read()
            .as_ref()
            .range(&LDK_NODE, start..end, |r| {
                r.map(|((_, _, key), _)| key).collect::<Vec<_>>()
            });

        Ok(keys)
    }
}

impl KVStoreSync for RedbKvStore {
    fn read(&self, p: &str, s: &str, k: &str) -> io::Result<Vec<u8>> {
        self.inner.read_internal(p, s, k)
    }

    fn write(&self, p: &str, s: &str, k: &str, buf: Vec<u8>) -> io::Result<()> {
        self.inner.write_internal(p, s, k, buf)
    }

    fn remove(&self, p: &str, s: &str, k: &str, _lazy: bool) -> io::Result<()> {
        self.inner.remove_internal(p, s, k)
    }

    fn list(&self, p: &str, s: &str) -> io::Result<Vec<String>> {
        self.inner.list_internal(p, s)
    }
}

impl KVStore for RedbKvStore {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<u8>>> + Send>> {
        let (p, s, k) = (
            primary_namespace.to_string(),
            secondary_namespace.to_string(),
            key.to_string(),
        );
        let inner = Arc::clone(&self.inner);
        let fut = tokio::task::spawn_blocking(move || inner.read_internal(&p, &s, &k));
        Box::pin(async move {
            fut.await.unwrap_or_else(|e| {
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("LDK KVStore read join error: {e}"),
                ))
            })
        })
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send>> {
        let (p, s, k) = (
            primary_namespace.to_string(),
            secondary_namespace.to_string(),
            key.to_string(),
        );
        let inner = Arc::clone(&self.inner);
        let fut = tokio::task::spawn_blocking(move || inner.write_internal(&p, &s, &k, buf));
        Box::pin(async move {
            fut.await.unwrap_or_else(|e| {
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("LDK KVStore write join error: {e}"),
                ))
            })
        })
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        _lazy: bool,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send>> {
        let (p, s, k) = (
            primary_namespace.to_string(),
            secondary_namespace.to_string(),
            key.to_string(),
        );
        let inner = Arc::clone(&self.inner);
        let fut = tokio::task::spawn_blocking(move || inner.remove_internal(&p, &s, &k));
        Box::pin(async move {
            fut.await.unwrap_or_else(|e| {
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("LDK KVStore remove join error: {e}"),
                ))
            })
        })
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<String>>> + Send>> {
        let (p, s) = (
            primary_namespace.to_string(),
            secondary_namespace.to_string(),
        );
        let inner = Arc::clone(&self.inner);
        let fut = tokio::task::spawn_blocking(move || inner.list_internal(&p, &s));
        Box::pin(async move {
            fut.await.unwrap_or_else(|e| {
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("LDK KVStore list join error: {e}"),
                ))
            })
        })
    }
}
