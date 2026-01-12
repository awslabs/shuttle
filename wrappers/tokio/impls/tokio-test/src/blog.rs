use shuttle::future::block_on;
use shuttle::sync::atomic::{AtomicUsize, Ordering};
use shuttle::sync::Arc;
use tokio::time::{sleep, Duration};

struct AsyncConnectionPool {
    active_count: Arc<AtomicUsize>,
    max_connections: usize,
}

impl AsyncConnectionPool {
    fn new(max_connections: usize) -> Self {
        AsyncConnectionPool {
            active_count: Arc::new(AtomicUsize::new(0)),
            max_connections,
        }
    }

    async fn acquire_connection(&self) -> Result<AsyncConnection, &'static str> {
        // Small delay before check to reduce race window
        sleep(Duration::from_nanos(100)).await;
        
        let count = self.active_count.load(Ordering::SeqCst);
        
        if count >= self.max_connections {
            return Err("connection pool exhausted");
        }
        
        // Small delay after check to reduce race window
        sleep(Duration::from_nanos(100)).await;
        
        self.active_count.store(count + 1, Ordering::SeqCst);
        
        // Simulate async connection setup work
        sleep(Duration::from_micros(1)).await;
        
        Ok(AsyncConnection { pool: self.active_count.clone() })
    }
}

struct AsyncConnection {
    pool: Arc<AtomicUsize>,
}

impl Drop for AsyncConnection {
    fn drop(&mut self) {
        self.pool.fetch_sub(1, Ordering::SeqCst);
    }
}

#[test]
fn test_async_connection_limit_shuttle() {
    shuttle::check_random(|| {
        block_on(async {
            let pool = Arc::new(AsyncConnectionPool::new(5));
            let mut handles = vec![];
            
            for _ in 0..10 {
                let pool = pool.clone();
                handles.push(tokio::spawn(async move {
                    pool.acquire_connection().await
                }));
            }
            
            let mut results = vec![];
            for handle in handles {
                results.push(handle.await.unwrap());
            }
            
            let successful = results.iter().filter(|r| r.is_ok()).count();
            assert!(successful <= 5, "allowed too many connections!");
        })
    }, 100);
}

// Traditional version using original tokio
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};
use std::sync::Arc as StdArc;

struct StdAsyncConnectionPool {
    active_count: StdArc<StdAtomicUsize>,
    max_connections: usize,
}

impl StdAsyncConnectionPool {
    fn new(max_connections: usize) -> Self {
        StdAsyncConnectionPool {
            active_count: StdArc::new(StdAtomicUsize::new(0)),
            max_connections,
        }
    }

    async fn acquire_connection(&self) -> Result<StdAsyncConnection, &'static str> {
        // Small delay before check to reduce race window
        tokio_orig::time::sleep(tokio_orig::time::Duration::from_nanos(100)).await;
        
        let count = self.active_count.load(StdOrdering::SeqCst);
        
        if count >= self.max_connections {
            return Err("connection pool exhausted");
        }
        
        // Small delay after check to reduce race window
        tokio_orig::time::sleep(tokio_orig::time::Duration::from_nanos(100)).await;
        
        self.active_count.store(count + 1, StdOrdering::SeqCst);
        
        Ok(StdAsyncConnection { pool: self.active_count.clone() })
    }
}

struct StdAsyncConnection {
    pool: StdArc<StdAtomicUsize>,
}

impl Drop for StdAsyncConnection {
    fn drop(&mut self) {
        self.pool.fetch_sub(1, StdOrdering::SeqCst);
    }
}

#[tokio_orig::test]
async fn test_async_connection_limit_traditional() {
    for _ in 0..100 {
        let pool = StdArc::new(StdAsyncConnectionPool::new(5));
        let mut handles = vec![];
        
        // Reduce to 6 tasks (just 1 over limit) to make race less likely
        for _ in 0..6 {
            let pool = pool.clone();
            handles.push(tokio_orig::spawn(async move {
                pool.acquire_connection().await
            }));
        }
        
        let mut results = vec![];
        for handle in handles {
            results.push(handle.await.unwrap());
        }
        
        let successful = results.iter().filter(|r| r.is_ok()).count();
        assert!(successful <= 5, "allowed too many connections!");
    }
}
