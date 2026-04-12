use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tonic::Status;

struct Entry {
    count: u32,
    window_start: Instant,
}

const CLEANUP_THRESHOLD: usize = 10_000;

pub struct RateLimiter {
    state: Arc<Mutex<HashMap<IpAddr, Entry>>>,
    max_requests: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_requests: u32, window: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
            max_requests,
            window,
        }
    }

    pub fn check_rate_limit(&self, addr: IpAddr) -> Result<(), Status> {
        let mut state = self.state.lock().unwrap();
        let now = Instant::now();

        let entry = state.entry(addr).or_insert(Entry {
            count: 0,
            window_start: now,
        });

        if now.duration_since(entry.window_start) >= self.window {
            entry.count = 1;
            entry.window_start = now;
            return Ok(());
        }

        entry.count += 1;
        if entry.count > self.max_requests {
            return Err(Status::resource_exhausted(
                "rate limit exceeded, try again later",
            ));
        }

        // Safety valve: prune stale entries if the map grows too large
        if state.len() > CLEANUP_THRESHOLD {
            let window = self.window;
            state.retain(|_, e| now.duration_since(e.window_start) < window);
        }

        Ok(())
    }

    pub fn spawn_cleanup_task(&self) {
        let state = Arc::clone(&self.state);
        let window = self.window;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(600)).await;
                let mut map = state.lock().unwrap();
                let now = Instant::now();
                map.retain(|_, e| now.duration_since(e.window_start) < window);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn allows_up_to_max_requests() {
        let limiter = RateLimiter::new(5, Duration::from_secs(60));
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        for _ in 0..5 {
            assert!(limiter.check_rate_limit(ip).is_ok());
        }
    }

    #[test]
    fn rejects_over_max_requests() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        for _ in 0..3 {
            limiter.check_rate_limit(ip).unwrap();
        }

        let err = limiter.check_rate_limit(ip).unwrap_err();
        assert_eq!(err.code(), tonic::Code::ResourceExhausted);
    }

    #[test]
    fn resets_after_window_expires() {
        let limiter = RateLimiter::new(2, Duration::from_millis(50));
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

        for _ in 0..2 {
            limiter.check_rate_limit(ip).unwrap();
        }
        assert!(limiter.check_rate_limit(ip).is_err());

        std::thread::sleep(Duration::from_millis(60));

        assert!(limiter.check_rate_limit(ip).is_ok());
    }

    #[test]
    fn independent_limits_per_ip() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let ip_a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let ip_b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

        limiter.check_rate_limit(ip_a).unwrap();
        limiter.check_rate_limit(ip_b).unwrap();

        assert!(limiter.check_rate_limit(ip_a).is_err());
        assert!(limiter.check_rate_limit(ip_b).is_err());
    }
}
