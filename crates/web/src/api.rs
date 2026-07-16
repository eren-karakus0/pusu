//! PUSU ingress istemcisi — Watched alarmları store'a yazdırmak için.
//!
//! OnChain alarmlar buraya gelmiyor (onlar doğrudan BULK'a, bkz. [`crate::bulk`]);
//! burası yalnızca watcher'ın yürüteceği alarmları PUSU sunucusuna iletiyor.

use crate::config::PUSU_API_URL;
use pusu_core::Alert;
use serde_json::Value;

/// İmzalı alarm + blob gövdesini `POST /alerts`'e gönder.
pub async fn create_alert(body: &Value) -> Result<(), String> {
    let resp = gloo_net::http::Request::post(&format!("{PUSU_API_URL}/alerts"))
        .json(body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_err(resp, "alarm kaydedilemedi").await
}

/// Beklemedeki alarmı iptal et: `POST /alerts/{id}/cancel?owner=`.
pub async fn cancel_alert(id: &str, owner: &str) -> Result<(), String> {
    let url = format!("{PUSU_API_URL}/alerts/{id}/cancel?owner={owner}");
    let resp = gloo_net::http::Request::post(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_err(resp, "alarm iptal edilemedi").await
}

/// Sonlanmış alarmı listeden kaldır: `DELETE /alerts/{id}?owner=`.
pub async fn delete_alert(id: &str, owner: &str) -> Result<(), String> {
    let url = format!("{PUSU_API_URL}/alerts/{id}?owner={owner}");
    let resp = gloo_net::http::Request::delete(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_err(resp, "alarm kaldırılamadı").await
}

/// Yanıtı sonuca çevir: ok değilse `{"error": ...}` gövdesinden mesajı al.
async fn ok_or_err(resp: gloo_net::http::Response, fallback: &str) -> Result<(), String> {
    if resp.ok() {
        return Ok(());
    }
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    Err(v["error"].as_str().unwrap_or(fallback).to_string())
}

/// Kullanıcının alarmlarını `GET /alerts?owner=`'dan çek (en yeni önce).
///
/// Yalnızca Watched alarmlar döner — OnChain olanları borsa tutuyor, PUSU
/// saklamıyor. Liste ekranı bunu kullanıcıya açıkça söylüyor.
pub async fn list_alerts(owner: &str) -> Result<Vec<Alert>, String> {
    let url = format!("{PUSU_API_URL}/alerts?owner={owner}");
    let resp = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        return Err(v["error"]
            .as_str()
            .unwrap_or("alarmlar yüklenemedi")
            .to_string());
    }
    resp.json::<Vec<Alert>>().await.map_err(|e| e.to_string())
}
