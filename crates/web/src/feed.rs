//! Tarayıcı-tarafı market verisi — canlı grafik için.
//!
//! Web `pusu-feed`'i import edemiyor (reqwest/tokio wasm değil), o yüzden aynı
//! BULK REST çağrılarını `gloo_net` ile yeniden yapıyoruz. `Kline` tipi
//! `pusu-core`'da paylaşımlı (aynı serde şekli).
//!
//! İki kural feed'den taşındı: canlıda `startTime` **GÖNDERME** (filtreli yanıt
//! ~60 sn bayat); son mum **AÇIK** olabilir (`T > now`) → grafikte oluşan mum
//! olarak çizilir, "kapanış" mantığı [`pusu_core::last_closed`] ile.

use crate::config::BULK_URL;
use pusu_core::Kline;

/// Bir hesabın tam durumu (`POST /account` → `[0].fullAccount`): positions[],
/// openOrders[], margin{}. Boşsa `Null`.
pub async fn account(user: &str) -> Result<serde_json::Value, String> {
    let url = format!("{BULK_URL}/account");
    let v: serde_json::Value = gloo_net::http::Request::post(&url)
        .json(&serde_json::json!({ "type": "fullAccount", "user": user }))
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(v.get(0)
        .and_then(|x| x.get("fullAccount"))
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

/// BULK'ta işlem gören pariteler (`/exchangeInfo` → status=="TRADING"), sıralı.
pub async fn markets() -> Result<Vec<String>, String> {
    let url = format!("{BULK_URL}/exchangeInfo");
    let list: Vec<serde_json::Value> = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let mut syms: Vec<String> = list
        .iter()
        .filter(|m| m["status"].as_str() == Some("TRADING"))
        .filter_map(|m| m["symbol"].as_str().map(String::from))
        .collect();
    syms.sort();
    Ok(syms)
}

/// Mum serisi (filtresiz/taze — `startTime` yok). ~7 günlük pencere.
pub async fn klines(symbol: &str, interval: &str) -> Result<Vec<Kline>, String> {
    let url = format!("{BULK_URL}/klines?symbol={symbol}&interval={interval}");
    let resp = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json::<Vec<Kline>>().await.map_err(|e| e.to_string())
}

/// Anlık mark price. `/ticker/{symbol}` → `markPrice` (symbol **path** parametresi;
/// `?symbol=` 404 döner).
pub async fn mark(symbol: &str) -> Result<f64, String> {
    let url = format!("{BULK_URL}/ticker/{symbol}");
    let v: serde_json::Value = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    v["markPrice"]
        .as_f64()
        .ok_or_else(|| "markPrice yok".to_string())
}
