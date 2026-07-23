//! Ortak HTTP istemcisi — **timeout'lu**.
//!
//! Timeout olmadan (`reqwest::Client::new()`) asılı bir BULK bağlantısı,
//! tek-thread watcher tick döngüsünü **süresiz** dondurur → sessiz tam kesinti.
//! Uptime ürünün sözü olduğundan bu kabul edilemez: her istek 10 sn'de,
//! bağlantı 5 sn'de kesilir. Zaman aşan istek `Uncertain`'a düşer ve reconcile
//! toparlar — sonsuza dek asılı kalmaktan iyi.

use std::time::Duration;

/// Zaman aşımlı reqwest istemcisi. Build (nadiren) başarısızsa varsayılana düşer.
pub(crate) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default()
}
