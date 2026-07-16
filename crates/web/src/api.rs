//! PUSU ingress istemcisi — Watched alarmları store'a yazdırmak için.
//!
//! OnChain alarmlar buraya gelmiyor (onlar doğrudan BULK'a, bkz. [`crate::bulk`]);
//! burası yalnızca watcher'ın yürüteceği alarmları PUSU sunucusuna iletiyor.

use crate::config::PUSU_API_URL;
use serde_json::Value;

/// İmzalı alarm + blob gövdesini `POST /alerts`'e gönder.
pub async fn create_alert(body: &Value) -> Result<(), String> {
    let resp = gloo_net::http::Request::post(&format!("{PUSU_API_URL}/alerts"))
        .json(body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if resp.ok() {
        return Ok(());
    }
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    Err(v["error"]
        .as_str()
        .unwrap_or("alarm kaydedilemedi")
        .to_string())
}
