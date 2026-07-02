use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

use tokio::time::{Duration, sleep};

use crate::config::{AppConfig, ChannelConfig};

#[derive(Clone)]
pub struct RateLimiters {
    buckets: Arc<HashMap<String, Option<Arc<Mutex<TokenBucket>>>>>,
}

struct TokenBucket {
    per_second: f64,
    burst: f64,
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiters {
    pub fn new(config: &AppConfig) -> Self {
        let buckets = config
            .channels
            .iter()
            .map(|(name, channel)| {
                let (per_second, burst) = effective_rate_limit(config, channel);
                let bucket = if !config.rate_limit.enabled || per_second <= 0.0 {
                    None
                } else {
                    Some(Arc::new(Mutex::new(TokenBucket {
                        per_second,
                        burst: burst as f64,
                        tokens: burst as f64,
                        last_refill: Instant::now(),
                    })))
                };
                (name.clone(), bucket)
            })
            .collect();
        Self {
            buckets: Arc::new(buckets),
        }
    }

    pub async fn acquire(&self, channel: &str) {
        let Some(Some(bucket)) = self.buckets.get(channel).cloned() else {
            return;
        };
        loop {
            let wait = {
                let Ok(mut bucket) = bucket.lock() else {
                    return;
                };
                bucket.refill();
                if bucket.tokens >= 1.0 {
                    bucket.tokens -= 1.0;
                    return;
                }
                Duration::from_secs_f64((1.0 - bucket.tokens) / bucket.per_second)
            };
            sleep(wait).await;
        }
    }
}

impl TokenBucket {
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.burst);
        self.last_refill = now;
    }
}

fn effective_rate_limit(config: &AppConfig, channel: &ChannelConfig) -> (f64, usize) {
    if let Some(rate_limit) = channel.rate_limit() {
        (rate_limit.per_second, rate_limit.burst)
    } else {
        (
            config.rate_limit.default_per_second,
            config.rate_limit.default_burst,
        )
    }
}
