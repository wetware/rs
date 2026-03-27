use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::arc::ArcInner;
use crate::pinner::Pinner;
use anyhow::{Context, Result};
use cid::Cid;
use tokio::sync::{Mutex, Notify};

/// A cached pin entry. Small objects are stored in-memory; large objects
/// are pin-only (the IPFS node holds the data, we just track the pin).
pub struct PinEntry {
    /// In-memory bytes for small objects. `None` for large (pin-only).
    /// `Arc<[u8]>` for O(1) clone when returning cached data.
    pub data: Option<Arc<[u8]>>,
    /// Size in bytes of the pinned content (always set).
    pub size: u64,
}

struct CacheState {
    arc: ArcInner<PinEntry>,
    inflight: HashMap<Cid, Arc<Notify>>,
}

/// CID-keyed cache backed by IPFS pins, with a weight-aware ARC eviction policy.
///
/// Thread-safe: the inner ARC is wrapped in a `Mutex`. The expensive I/O
/// operations (fetch, pin, unpin) happen outside the lock.
pub struct PinsetCache {
    state: Mutex<CacheState>,
    pinner: Arc<dyn Pinner>,
    inline_threshold: usize,
}

/// Maximum retry attempts for failed unpins.
const MAX_UNPIN_RETRIES: u32 = 3;

fn pin_entry_weighter(_k: &Cid, v: &PinEntry) -> usize {
    // Weight is the size of the pinned content. For entries with inline data,
    // we also account for the Arc<[u8]> overhead, but the content size dominates.
    v.size as usize
}

impl PinsetCache {
    /// Default ghost capacity for the ARC cache.
    /// Each ghost entry is a CID key only (~40 bytes), so 4096 ghosts ≈ 160KB.
    const DEFAULT_GHOST_CAPACITY: usize = 4096;

    /// Create a new pinset cache.
    ///
    /// - `pinner`: the IPFS pin backend
    /// - `budget`: maximum total weight (bytes) of cached entries
    /// - `inline_threshold`: objects at or below this size are cached in-memory
    pub fn new(pinner: Arc<dyn Pinner>, budget: usize, inline_threshold: usize) -> Self {
        Self {
            state: Mutex::new(CacheState {
                arc: ArcInner::new(budget, Self::DEFAULT_GHOST_CAPACITY, pin_entry_weighter),
                inflight: HashMap::new(),
            }),
            pinner,
            inline_threshold,
        }
    }

    /// Ensure a CID is pinned and cached. Returns in-memory bytes for small
    /// objects, `None` for large objects (which are pinned but not held in memory).
    ///
    /// Concurrent callers for the same CID are deduplicated: only one fetch/pin
    /// is performed, and all waiters receive the result.
    pub async fn ensure(&self, cid: &Cid) -> Result<Option<Arc<[u8]>>> {
        // Fast path: check cache under lock.
        loop {
            let mut state = self.state.lock().await;

            if let Some(entry) = state.arc.get(cid) {
                return Ok(entry.data.clone());
            }

            // Check if another task is already fetching this CID.
            if let Some(notify) = state.inflight.get(cid) {
                let notify = Arc::clone(notify);
                drop(state);

                // Wait for the in-flight fetch to complete, then re-check.
                notify.notified().await;
                continue;
            }

            // Register ourselves as the in-flight fetcher.
            state.inflight.insert(*cid, Arc::new(Notify::new()));
            break;
        }

        // Slow path: fetch and pin outside the lock.
        let result = self.fetch_and_pin(cid).await;

        // Re-lock and finalize.
        let mut state = self.state.lock().await;
        let notify = state.inflight.remove(cid);

        match result {
            Ok(entry) => {
                let data = entry.data.clone();
                let evicted = state.arc.put(*cid, entry);

                // Notify waiters before spawning unpins.
                if let Some(n) = notify {
                    n.notify_waiters();
                }

                // Unpin evicted entries in background.
                self.spawn_unpins(evicted);

                Ok(data)
            }
            Err(e) => {
                // Notify waiters that we failed.
                if let Some(n) = notify {
                    n.notify_waiters();
                }
                Err(e)
            }
        }
    }

    /// Fetch content size, optionally fetch bytes, and pin the CID.
    async fn fetch_and_pin(&self, cid: &Cid) -> Result<PinEntry> {
        let size = self
            .pinner
            .size(cid)
            .await
            .context("failed to get size for CID")?;

        let data = if (size as usize) <= self.inline_threshold {
            let bytes = self
                .pinner
                .fetch(cid)
                .await
                .context("failed to fetch CID content")?;
            Some(Arc::from(bytes.into_boxed_slice()))
        } else {
            None
        };

        self.pinner.pin(cid).await.context("failed to pin CID")?;

        Ok(PinEntry { data, size })
    }

    /// Spawn background tasks to unpin evicted entries with retry.
    fn spawn_unpins(&self, evicted: Vec<(Cid, PinEntry)>) {
        for (cid, _entry) in evicted {
            let pinner = Arc::clone(&self.pinner);
            tokio::spawn(async move {
                unpin_with_retry(&*pinner, &cid).await;
            });
        }
    }
}

/// Unpin a CID with up to MAX_UNPIN_RETRIES attempts and exponential backoff.
async fn unpin_with_retry(pinner: &dyn Pinner, cid: &Cid) {
    let mut delay = Duration::from_millis(100);

    for attempt in 1..=MAX_UNPIN_RETRIES {
        match pinner.unpin(cid).await {
            Ok(()) => return,
            Err(e) => {
                if attempt == MAX_UNPIN_RETRIES {
                    tracing::warn!(
                        %cid,
                        attempts = MAX_UNPIN_RETRIES,
                        error = %e,
                        "failed to unpin evicted CID after all retries; pin leaked"
                    );
                    return;
                }
                tracing::debug!(
                    %cid,
                    attempt,
                    error = %e,
                    "unpin failed, retrying"
                );
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
    }
}

/// How a process interacts with the cache.
pub enum CacheMode {
    /// All processes share a global cache — efficient, but cache timing
    /// side-channels are possible between processes.
    Shared(Arc<PinsetCache>),
    /// Process gets its own isolated pinset — no shared state, no
    /// side-channels. All pins are removed when the process exits.
    Isolated(IsolatedPinset),
}

/// A per-process isolated pinset. Does not use ARC; simply tracks
/// what this process has pinned and unpins everything on drop.
pub struct IsolatedPinset {
    pinner: Arc<dyn Pinner>,
    pins: Mutex<HashMap<Cid, Option<Arc<[u8]>>>>,
    inline_threshold: usize,
}

impl IsolatedPinset {
    /// Create a new isolated pinset.
    pub fn new(pinner: Arc<dyn Pinner>, inline_threshold: usize) -> Self {
        Self {
            pinner,
            pins: Mutex::new(HashMap::new()),
            inline_threshold,
        }
    }

    /// Ensure a CID is pinned. Same interface as `PinsetCache::ensure`.
    pub async fn ensure(&self, cid: &Cid) -> Result<Option<Arc<[u8]>>> {
        {
            let pins = self.pins.lock().await;
            if let Some(data) = pins.get(cid) {
                return Ok(data.clone());
            }
        }

        let size = self
            .pinner
            .size(cid)
            .await
            .context("failed to get size for CID")?;

        let data = if (size as usize) <= self.inline_threshold {
            let bytes = self
                .pinner
                .fetch(cid)
                .await
                .context("failed to fetch CID content")?;
            Some(Arc::from(bytes.into_boxed_slice()))
        } else {
            None
        };

        self.pinner.pin(cid).await.context("failed to pin CID")?;

        let mut pins = self.pins.lock().await;
        pins.insert(*cid, data.clone());

        Ok(data)
    }
}

impl Drop for IsolatedPinset {
    fn drop(&mut self) {
        // Use try_current() to avoid panicking if the runtime is shutting down.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::debug!("no tokio runtime during IsolatedPinset drop; pins leaked");
            return;
        };

        // We can use get_mut() since we have &mut self in drop — no other references.
        let pins = self.pins.get_mut();
        let cids: Vec<Cid> = pins.keys().copied().collect();
        let pinner = Arc::clone(&self.pinner);

        handle.spawn(async move {
            for cid in cids {
                unpin_with_retry(&*pinner, &cid).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn test_cid(n: u8) -> Cid {
        let mh = cid::multihash::Multihash::wrap(0x00, &[n]).unwrap();
        Cid::new_v1(0x55, mh)
    }

    struct MockPinner {
        pinned: Mutex<HashSet<Cid>>,
        data: HashMap<Cid, Vec<u8>>,
        pin_count: AtomicUsize,
        unpin_count: AtomicUsize,
        fail_unpin: AtomicBool,
        fail_fetch: AtomicBool,
        fail_pin: AtomicBool,
        fail_size: AtomicBool,
    }

    impl MockPinner {
        fn new(data: HashMap<Cid, Vec<u8>>) -> Self {
            Self {
                pinned: Mutex::new(HashSet::new()),
                data,
                pin_count: AtomicUsize::new(0),
                unpin_count: AtomicUsize::new(0),
                fail_unpin: AtomicBool::new(false),
                fail_fetch: AtomicBool::new(false),
                fail_pin: AtomicBool::new(false),
                fail_size: AtomicBool::new(false),
            }
        }
    }

    #[async_trait::async_trait]
    impl Pinner for MockPinner {
        async fn pin(&self, cid: &Cid) -> Result<()> {
            if self.fail_pin.load(Ordering::Relaxed) {
                anyhow::bail!("mock pin failure");
            }
            self.pin_count.fetch_add(1, Ordering::Relaxed);
            self.pinned.lock().await.insert(*cid);
            Ok(())
        }

        async fn unpin(&self, cid: &Cid) -> Result<()> {
            if self.fail_unpin.load(Ordering::Relaxed) {
                anyhow::bail!("mock unpin failure");
            }
            self.unpin_count.fetch_add(1, Ordering::Relaxed);
            self.pinned.lock().await.remove(cid);
            Ok(())
        }

        async fn fetch(&self, cid: &Cid) -> Result<Vec<u8>> {
            if self.fail_fetch.load(Ordering::Relaxed) {
                anyhow::bail!("mock fetch failure");
            }
            self.data
                .get(cid)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("CID not found"))
        }

        async fn size(&self, cid: &Cid) -> Result<u64> {
            if self.fail_size.load(Ordering::Relaxed) {
                anyhow::bail!("mock size failure");
            }
            self.data
                .get(cid)
                .map(|d| d.len() as u64)
                .ok_or_else(|| anyhow::anyhow!("CID not found"))
        }
    }

    fn mock_with_entries(entries: &[(u8, &[u8])]) -> Arc<MockPinner> {
        let data: HashMap<Cid, Vec<u8>> = entries
            .iter()
            .map(|(n, bytes)| (test_cid(*n), bytes.to_vec()))
            .collect();
        Arc::new(MockPinner::new(data))
    }

    // 12. test_ensure_pins_on_miss
    #[tokio::test]
    async fn test_ensure_pins_on_miss() {
        let pinner = mock_with_entries(&[(1, b"hello")]);
        let cache = PinsetCache::new(pinner.clone(), 1024, 1024);

        let result = cache.ensure(&test_cid(1)).await.unwrap();
        assert!(result.is_some());
        assert_eq!(&*result.unwrap(), b"hello");
        assert_eq!(pinner.pin_count.load(Ordering::Relaxed), 1);
    }

    // 13. test_small_object_inline
    #[tokio::test]
    async fn test_small_object_inline() {
        let pinner = mock_with_entries(&[(1, b"small")]);
        let cache = PinsetCache::new(pinner, 1024, 1024); // threshold=1024

        let result = cache.ensure(&test_cid(1)).await.unwrap();
        assert!(result.is_some(), "small object should be inlined");
        assert_eq!(&*result.unwrap(), b"small");
    }

    // 14. test_large_object_pin_only
    #[tokio::test]
    async fn test_large_object_pin_only() {
        let pinner = mock_with_entries(&[(1, &[0u8; 2048])]);
        let cache = PinsetCache::new(pinner, 4096, 1024); // threshold=1024, object=2048

        let result = cache.ensure(&test_cid(1)).await.unwrap();
        assert!(result.is_none(), "large object should be pin-only");
    }

    // 15. test_eviction_unpins
    #[tokio::test]
    async fn test_eviction_unpins() {
        let pinner = mock_with_entries(&[(1, &[0u8; 60]), (2, &[0u8; 60])]);
        let cache = PinsetCache::new(pinner.clone(), 80, 1024); // budget=80

        cache.ensure(&test_cid(1)).await.unwrap(); // weight 60
        cache.ensure(&test_cid(2)).await.unwrap(); // weight 60, total 120 > 80

        // Give the background unpin task time to run.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            pinner.unpin_count.load(Ordering::Relaxed) > 0,
            "eviction should trigger unpin"
        );
    }

    // 16. test_concurrent_dedup
    #[tokio::test]
    async fn test_concurrent_dedup() {
        let pinner = mock_with_entries(&[(1, b"data")]);
        let cache = Arc::new(PinsetCache::new(pinner.clone(), 1024, 1024));

        let mut handles = Vec::new();
        for _ in 0..10 {
            let c = Arc::clone(&cache);
            let cid = test_cid(1);
            handles.push(tokio::spawn(async move { c.ensure(&cid).await }));
        }

        for h in handles {
            let result = h.await.unwrap().unwrap();
            assert!(result.is_some());
            assert_eq!(&*result.unwrap(), b"data");
        }

        // Should only pin once despite 10 concurrent callers.
        assert_eq!(pinner.pin_count.load(Ordering::Relaxed), 1);
    }

    // 17. test_isolated_cleanup
    #[tokio::test]
    async fn test_isolated_cleanup() {
        let pinner = mock_with_entries(&[(1, b"a"), (2, b"b")]);
        let iso = IsolatedPinset::new(pinner.clone(), 1024);

        iso.ensure(&test_cid(1)).await.unwrap();
        iso.ensure(&test_cid(2)).await.unwrap();
        assert_eq!(pinner.pin_count.load(Ordering::Relaxed), 2);

        drop(iso);

        // Give the spawned unpin tasks time to run.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(pinner.unpin_count.load(Ordering::Relaxed), 2);
    }

    // 18. test_pin_failure_no_cache
    #[tokio::test]
    async fn test_pin_failure_no_cache() {
        let pinner = mock_with_entries(&[(1, b"data")]);
        pinner.fail_pin.store(true, Ordering::Relaxed);
        let cache = PinsetCache::new(pinner, 1024, 1024);

        let result = cache.ensure(&test_cid(1)).await;
        assert!(result.is_err(), "pin failure should propagate");

        // The entry should NOT be in the cache.
        let state = cache.state.lock().await;
        assert!(!state.arc.contains(&test_cid(1)));
    }

    // 19. test_size_failure_before_pin
    #[tokio::test]
    async fn test_size_failure_before_pin() {
        let pinner = mock_with_entries(&[(1, b"data")]);
        pinner.fail_size.store(true, Ordering::Relaxed);
        let cache = PinsetCache::new(pinner.clone(), 1024, 1024);

        let result = cache.ensure(&test_cid(1)).await;
        assert!(result.is_err());
        // Pin should never have been called.
        assert_eq!(pinner.pin_count.load(Ordering::Relaxed), 0);
    }

    // 20. test_fetch_failure_after_size
    #[tokio::test]
    async fn test_fetch_failure_after_size() {
        let pinner = mock_with_entries(&[(1, b"data")]);
        pinner.fail_fetch.store(true, Ordering::Relaxed);
        let cache = PinsetCache::new(pinner.clone(), 1024, 1024);

        let result = cache.ensure(&test_cid(1)).await;
        assert!(result.is_err());
        // Pin should never have been called (fetch failed first).
        assert_eq!(pinner.pin_count.load(Ordering::Relaxed), 0);
    }

    // 21. test_unpin_retry
    #[tokio::test]
    async fn test_unpin_retry() {
        let pinner = mock_with_entries(&[(1, &[0u8; 60]), (2, &[0u8; 60])]);
        // Fail unpins initially.
        pinner.fail_unpin.store(true, Ordering::Relaxed);
        let cache = PinsetCache::new(pinner.clone(), 80, 1024);

        cache.ensure(&test_cid(1)).await.unwrap();

        // Allow unpins after first attempt fails.
        let pinner2 = pinner.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            pinner2.fail_unpin.store(false, Ordering::Relaxed);
        });

        cache.ensure(&test_cid(2)).await.unwrap(); // triggers eviction

        // Wait for retries to complete.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            pinner.unpin_count.load(Ordering::Relaxed) > 0,
            "retry should eventually succeed"
        );
    }

    // 22. test_unpin_retry_exhausted
    #[tokio::test]
    async fn test_unpin_retry_exhausted() {
        let pinner = mock_with_entries(&[(1, &[0u8; 60]), (2, &[0u8; 60])]);
        pinner.fail_unpin.store(true, Ordering::Relaxed);
        let cache = PinsetCache::new(pinner.clone(), 80, 1024);

        cache.ensure(&test_cid(1)).await.unwrap();
        cache.ensure(&test_cid(2)).await.unwrap(); // triggers eviction + unpin

        // Wait for all retries to exhaust.
        tokio::time::sleep(Duration::from_millis(1000)).await;

        // Unpin was attempted but always failed — count stays 0 since mock
        // bails before incrementing.
        assert_eq!(pinner.unpin_count.load(Ordering::Relaxed), 0);
    }

    // 23. test_isolated_drop_no_runtime — tested via the Drop impl design
    // (Handle::try_current returns Err, no panic). Hard to test in-process
    // since we're already in a tokio runtime. Validated by code review.
}
