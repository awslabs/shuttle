// Rate limiter test demonstrating a subtle concurrency bug that Shuttle can catch
// This is based on the example from the Shuttle blog post

use shuttle::future::block_on;
use shuttle::check_random;
use shuttle_tokio_impl::sync::Mutex;
use shuttle_tokio_impl::time::sleep;
use shuttle_tokio_impl::spawn;
use std::sync::Arc;
use std::time::Duration;

// Async rate limiter implementation with a race condition bug
struct AsyncRateLimiter {
    requests_in_window: Arc<Mutex<usize>>,
    max_requests: usize,
}

impl AsyncRateLimiter {
    fn new(max_requests: usize) -> Self {
        AsyncRateLimiter {
            requests_in_window: Arc::new(Mutex::new(0)),
            max_requests,
        }
    }

    async fn try_acquire(&self) -> Result<RateLimitPermit, &'static str> {
        // BUG: Check and increment happen in separate critical sections
        let count = *self.requests_in_window.lock().await;

        if count >= self.max_requests {
            return Err("rate limit exceeded");
        }

        // Another task can acquire the lock and increment between the check above and here
        *self.requests_in_window.lock().await += 1;

        // Simulate some async work (e.g., recording the request timestamp)
        sleep(Duration::from_micros(1)).await;

        Ok(RateLimitPermit {
            limiter: self.requests_in_window.clone(),
        })
    }
}

struct RateLimitPermit {
    limiter: Arc<Mutex<usize>>,
}

impl Drop for RateLimitPermit {
    fn drop(&mut self) {
        // In a real implementation, this would happen after the time window expires
        // For testing purposes, we decrement immediately when the permit is dropped
        let limiter = self.limiter.clone();
        // Note: We can't await in Drop, so in a real implementation this would
        // need a different design. This is simplified for the test.
        let _ = limiter.try_lock().map(|mut count| *count -= 1);
    }
}

// Shuttle async test - systematically explores different task interleavings
// This test RELIABLY catches the bug
#[test]
fn test_async_rate_limiter_shuttle() {
    check_random(|| {
        block_on(async {
            let limiter = Arc::new(AsyncRateLimiter::new(5));
            let mut handles = vec![];

            // Spawn 10 async tasks all trying to acquire permits
            // Only 5 should succeed
            for _ in 0..10 {
                let limiter = limiter.clone();
                handles.push(spawn(async move {
                    limiter.try_acquire().await
                }));
            }

            let results: Vec<_> = futures::future::join_all(handles)
                .await
                .into_iter()
                .map(|r| r.unwrap())
                .collect();

            let successful = results.iter().filter(|r| r.is_ok()).count();

            // Shuttle will find an interleaving where this assertion fails
            assert!(
                successful <= 5,
                "allowed {} requests, but max is 5!",
                successful
            );
        })
    }, 100);
}

// Fixed async implementation: check and increment in the same critical section
struct AsyncRateLimiterFixed {
    requests_in_window: Arc<Mutex<usize>>,
    max_requests: usize,
}

impl AsyncRateLimiterFixed {
    fn new(max_requests: usize) -> Self {
        AsyncRateLimiterFixed {
            requests_in_window: Arc::new(Mutex::new(0)),
            max_requests,
        }
    }

    async fn try_acquire(&self) -> Result<RateLimitPermitFixed, &'static str> {
        // FIX: Keep the lock held for the entire check-and-increment operation
        let mut count = self.requests_in_window.lock().await;

        if *count >= self.max_requests {
            return Err("rate limit exceeded");
        }

        *count += 1;
        drop(count); // Explicitly release the lock before doing work

        // Simulate some async work (e.g., recording the request timestamp)
        sleep(Duration::from_micros(1)).await;

        Ok(RateLimitPermitFixed {
            limiter: self.requests_in_window.clone(),
        })
    }
}

struct RateLimitPermitFixed {
    limiter: Arc<Mutex<usize>>,
}

impl Drop for RateLimitPermitFixed {
    fn drop(&mut self) {
        let limiter = self.limiter.clone();
        let _ = limiter.try_lock().map(|mut count| *count -= 1);
    }
}

// Shuttle test for the fixed version - should always pass
#[test]
fn test_async_rate_limiter_fixed_shuttle() {
    check_random(|| {
        block_on(async {
            let limiter = Arc::new(AsyncRateLimiterFixed::new(5));
            let mut handles = vec![];

            for _ in 0..10 {
                let limiter = limiter.clone();
                handles.push(spawn(async move {
                    limiter.try_acquire().await
                }));
            }

            let results: Vec<_> = futures::future::join_all(handles)
                .await
                .into_iter()
                .map(|r| r.unwrap())
                .collect();

            let successful = results.iter().filter(|r| r.is_ok()).count();

            // This should always pass because the fix is correct
            assert!(
                successful <= 5,
                "allowed {} requests, but max is 5!",
                successful
            );
        })
    }, 100);
}
