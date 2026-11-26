use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::config::{
    BATCH_POOL_CAP_BYTES, CONFIG_CACHE_CAP_BYTES, LIVE_POOL_CAP_BYTES, MAX_ACTIVE_BATCH_ENGINES,
    PHASE_B_RETAIN_CAP_BYTES,
};

#[derive(Debug)]
pub struct ByteQuotaPool {
    cap: usize,
    used: AtomicUsize,
}

impl ByteQuotaPool {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            used: AtomicUsize::new(0),
        }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }

    pub fn try_reserve(self: &Arc<Self>, bytes: usize) -> Option<QuotaGuard> {
        let mut cur = self.used.load(Ordering::Acquire);
        loop {
            let next = cur.checked_add(bytes)?;
            if next > self.cap {
                return None;
            }
            match self
                .used
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    return Some(QuotaGuard {
                        pool: Arc::clone(self),
                        bytes,
                    });
                }
                Err(actual) => cur = actual,
            }
        }
    }
}

#[derive(Debug)]
pub struct QuotaGuard {
    pool: Arc<ByteQuotaPool>,
    bytes: usize,
}

impl QuotaGuard {
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn try_grow(&mut self, extra: usize) -> bool {
        if extra == 0 {
            return true;
        }
        let mut cur = self.pool.used.load(Ordering::Acquire);
        loop {
            let Some(next) = cur.checked_add(extra) else {
                return false;
            };
            if next > self.pool.cap {
                return false;
            }
            match self.pool.used.compare_exchange_weak(
                cur,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.bytes += extra;
                    return true;
                }
                Err(actual) => cur = actual,
            }
        }
    }

    pub fn shrink_by(&mut self, release: usize) {
        let dec = release.min(self.bytes);
        if dec > 0 {
            self.pool.used.fetch_sub(dec, Ordering::AcqRel);
            self.bytes -= dec;
        }
    }
}

impl Drop for QuotaGuard {
    fn drop(&mut self) {
        if self.bytes > 0 {
            self.pool.used.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

#[derive(Debug)]
pub struct ServerQuotas {
    pub batch_pool: Arc<ByteQuotaPool>,
    pub live_pool: Arc<ByteQuotaPool>,
    pub config_cache_pool: Arc<ByteQuotaPool>,
    pub phase_b_retain_pool: Arc<ByteQuotaPool>,
    pub batch_semaphore: Arc<Semaphore>,
    pub ws_connections: AtomicUsize,
}

impl Default for ServerQuotas {
    fn default() -> Self {
        Self {
            batch_pool: Arc::new(ByteQuotaPool::new(BATCH_POOL_CAP_BYTES)),
            live_pool: Arc::new(ByteQuotaPool::new(LIVE_POOL_CAP_BYTES)),
            config_cache_pool: Arc::new(ByteQuotaPool::new(CONFIG_CACHE_CAP_BYTES)),
            phase_b_retain_pool: Arc::new(ByteQuotaPool::new(PHASE_B_RETAIN_CAP_BYTES)),
            batch_semaphore: Arc::new(Semaphore::new(MAX_ACTIVE_BATCH_ENGINES)),
            ws_connections: AtomicUsize::new(0),
        }
    }
}
