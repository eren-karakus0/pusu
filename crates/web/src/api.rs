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
    ok_or_err(resp, "couldn't save alert").await
}

/// Beklemedeki alarmı iptal et: `POST /alerts/{id}/cancel?owner=`.
pub async fn cancel_alert(id: &str, owner: &str) -> Result<(), String> {
    let url = format!("{PUSU_API_URL}/alerts/{id}/cancel?owner={owner}");
    let resp = gloo_net::http::Request::post(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_err(resp, "couldn't cancel alert").await
}

/// Sonlanmış alarmı listeden kaldır: `DELETE /alerts/{id}?owner=`.
pub async fn delete_alert(id: &str, owner: &str) -> Result<(), String> {
    let url = format!("{PUSU_API_URL}/alerts/{id}?owner={owner}");
    let resp = gloo_net::http::Request::delete(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_err(resp, "couldn't remove alert").await
}

/// Kullanıcının bildirimleri + okunmamış sayısı: `GET /notifications?owner=`.
///
/// `(satırlar, okunmamış)` döner. Satırlar ham JSON (`{body:{symbol,message},
/// created_at_ms, read, …}`) — zil bunları render ediyor.
pub async fn list_notifications(owner: &str) -> Result<(Vec<Value>, u64), String> {
    let url = format!("{PUSU_API_URL}/notifications?owner={owner}");
    let resp = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.ok() {
        return Err("couldn't load notifications".to_string());
    }
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    let items = v["notifications"].as_array().cloned().unwrap_or_default();
    let unread = v["unread"].as_u64().unwrap_or(0);
    Ok((items, unread))
}

/// Okunmamışları okundu işaretle: `POST /notifications/read?owner=`.
pub async fn mark_notifications_read(owner: &str) -> Result<(), String> {
    let url = format!("{PUSU_API_URL}/notifications/read?owner={owner}");
    let resp = gloo_net::http::Request::post(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_err(resp, "couldn't mark read").await
}

/// Ayarlı bildirim e-postası: `GET /contact?owner=` (prefill için).
pub async fn get_contact_email(owner: &str) -> Result<Option<String>, String> {
    let url = format!("{PUSU_API_URL}/contact?owner={owner}");
    let resp = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.ok() {
        return Ok(None);
    }
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(v["email"].as_str().map(String::from))
}

/// Bildirim e-postasını ayarla: `POST /contact`.
pub async fn set_contact_email(owner: &str, email: &str) -> Result<(), String> {
    let resp = gloo_net::http::Request::post(&format!("{PUSU_API_URL}/contact"))
        .json(&serde_json::json!({ "owner": owner, "email": email }))
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    ok_or_err(resp, "couldn't save email").await
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
            .unwrap_or("couldn't load alerts")
            .to_string());
    }
    resp.json::<Vec<Alert>>().await.map_err(|e| e.to_string())
}
