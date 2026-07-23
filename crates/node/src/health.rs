//! "Watcher'ı kim izliyor?" — node'un health/ready/metrics HTTP sunucusu.
//!
//! Node başsız bir tokio döngüsü; ölse ya da geri kalsa dışarıdan kimse
//! bilmiyordu. Uptime, Watched alarmların ürün sözü olduğundan bu kör nokta.
//! Bu modül döngüden atomiklerle beslenen küçük bir axum sunucusu açıyor:
//!
//! - `GET /health`  → süreç ayakta mı (liveness). Her zaman 200.
//! - `GET /ready`   → tick döngüsü canlı mı: son tick `3 × poll_interval`'dan
//!   yeniyse 200, değilse **503** (donmuş/çökmüş watcher). Load balancer /
//!   supervisor bunu okur.
//! - `GET /metrics` → Prometheus text: tick sayısı, feed hataları, ateşleme
//!   sonuçları, **uncertain** (insan bakmalı) sayacı, uptime.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Router};
use pusu_engine::{Outcome, Tick};

/// Tick döngüsünden okunan, sunucuya paylaşılan canlı durum. Hepsi atomik —
/// kilit yok, döngü writer, HTTP handler reader.
pub struct Health {
    started_ms: u64,
    poll_interval_ms: u64,
    last_tick_ms: AtomicU64,
    tick_total: AtomicU64,
    feed_errors_total: AtomicU64,
    fired_total: AtomicU64,
    notified_total: AtomicU64,
    missed_total: AtomicU64,
    /// Gönderildi ama sonucu bilinmiyor — insan bakmalı. Uyarı bunun üstüne kurulur.
    uncertain_total: AtomicU64,
}

impl Health {
    pub fn new(poll_interval_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            started_ms: now_ms(),
            poll_interval_ms: poll_interval_ms.max(1),
            last_tick_ms: AtomicU64::new(0),
            tick_total: AtomicU64::new(0),
            feed_errors_total: AtomicU64::new(0),
            fired_total: AtomicU64::new(0),
            notified_total: AtomicU64::new(0),
            missed_total: AtomicU64::new(0),
            uncertain_total: AtomicU64::new(0),
        })
    }

    /// Bir turdan sonra sayaçları güncelle.
    pub fn record(&self, now_ms: u64, tick: &Tick) {
        self.last_tick_ms.store(now_ms, Relaxed);
        self.tick_total.fetch_add(1, Relaxed);
        self.feed_errors_total
            .fetch_add(tick.feed_errors.len() as u64, Relaxed);
        self.fired_total.fetch_add(tick.fired.len() as u64, Relaxed);
        self.notified_total
            .fetch_add(tick.notified.len() as u64, Relaxed);
        self.missed_total
            .fetch_add(tick.missed.len() as u64, Relaxed);
        let uncertain = tick
            .fired
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Unknown { .. }))
            .count() as u64;
        self.uncertain_total.fetch_add(uncertain, Relaxed);
    }

    /// Son tick, kabul edilebilir pencereden (3 × poll) eski mi? Hiç tick
    /// olmadıysa da (henüz ısınıyor) hazır değil.
    fn stale(&self, now_ms: u64) -> bool {
        let last = self.last_tick_ms.load(Relaxed);
        last == 0 || now_ms.saturating_sub(last) > self.poll_interval_ms * 3
    }

    /// Prometheus text gövdesi.
    fn render(&self) -> String {
        let uptime = now_ms().saturating_sub(self.started_ms) / 1000;
        format!(
            "# HELP pusu_up 1 if the node is running\n\
             # TYPE pusu_up gauge\n\
             pusu_up 1\n\
             # TYPE pusu_uptime_seconds counter\n\
             pusu_uptime_seconds {uptime}\n\
             # TYPE pusu_last_tick_timestamp_ms gauge\n\
             pusu_last_tick_timestamp_ms {}\n\
             # TYPE pusu_tick_total counter\n\
             pusu_tick_total {}\n\
             # TYPE pusu_feed_errors_total counter\n\
             pusu_feed_errors_total {}\n\
             # TYPE pusu_alerts_fired_total counter\n\
             pusu_alerts_fired_total {}\n\
             # TYPE pusu_alerts_notified_total counter\n\
             pusu_alerts_notified_total {}\n\
             # TYPE pusu_alerts_missed_total counter\n\
             pusu_alerts_missed_total {}\n\
             # TYPE pusu_alerts_uncertain_total counter\n\
             pusu_alerts_uncertain_total {}\n",
            self.last_tick_ms.load(Relaxed),
            self.tick_total.load(Relaxed),
            self.feed_errors_total.load(Relaxed),
            self.fired_total.load(Relaxed),
            self.notified_total.load(Relaxed),
            self.missed_total.load(Relaxed),
            self.uncertain_total.load(Relaxed),
        )
    }
}

/// Sunucuyu çalıştır (döngüden ayrı bir task olarak spawn edilir).
pub async fn serve(health: Arc<Health>, addr: SocketAddr) -> std::io::Result<()> {
    let app = Router::new()
        .route("/health", get(|| async { "ok\n" }))
        .route("/ready", get(ready))
        .route("/metrics", get(metrics))
        .with_state(health);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}

async fn ready(State(h): State<Arc<Health>>) -> impl IntoResponse {
    let now = now_ms();
    if h.stale(now) {
        let age = now.saturating_sub(h.last_tick_ms.load(Relaxed));
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("stale: son tick {age}ms önce\n"),
        )
    } else {
        (StatusCode::OK, "ready\n".to_string())
    }
}

async fn metrics(State(h): State<Arc<Health>>) -> impl IntoResponse {
    ([("content-type", "text/plain; version=0.0.4")], h.render())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pusu_core::AlertId;
    use pusu_engine::{Outcome, Report};

    #[test]
    fn record_sayaclari_dogru_artiriyor() {
        let h = Health::new(1000);
        let mut t = Tick::default();
        t.notified.push(AlertId::new("n1"));
        t.missed.push(AlertId::new("m1"));
        t.fired.push(Report {
            id: AlertId::new("f1"),
            outcome: Outcome::Filled {
                avg_price: 90_000.0,
                size: 0.01,
            },
        });
        t.fired.push(Report {
            id: AlertId::new("u1"),
            outcome: Outcome::Unknown { raw: "504".into() },
        });
        h.record(1_700_000_000_000, &t);

        let m = h.render();
        assert!(m.contains("pusu_tick_total 1"));
        assert!(m.contains("pusu_alerts_notified_total 1"));
        assert!(m.contains("pusu_alerts_missed_total 1"));
        assert!(m.contains("pusu_alerts_fired_total 2"));
        // İkisinden biri Unknown → tam olarak 1 uncertain.
        assert!(m.contains("pusu_alerts_uncertain_total 1"));
        assert!(m.contains("pusu_last_tick_timestamp_ms 1700000000000"));
    }

    #[test]
    fn stale_penceresi_uc_kat_poll() {
        let h = Health::new(1000); // eşik = 3000ms
        assert!(h.stale(5_000), "hiç tick yok → hazır değil");
        h.record(10_000, &Tick::default());
        assert!(!h.stale(11_000), "1s sonra taze");
        assert!(!h.stale(12_999), "3s sınırında taze");
        assert!(h.stale(13_001), ">3s → bayat (503)");
    }
}
