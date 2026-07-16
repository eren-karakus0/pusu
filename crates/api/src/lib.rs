//! PUSU ingress: tarayıcının imzaladığı Watched alarmlarını store'a yazar.
//!
//! Zincirin son halkası: **tarayıcı imzalar → api saklar → node watcher
//! gönderir**. OnChain alarmlar buraya gelmez — onlar doğrudan borsaya gidiyor
//! (sunucumuz ölse de çalışsınlar diye); burası yalnızca bizim uptime'ımıza
//! bağlı olan Watched sınıfını topluyor.
//!
//! # Sunucu neden imza görmüyor
//!
//! Gelen blob'lar kullanıcının tarayıcıda **zaten imzaladığı**, değiştirilemez
//! paketler (bkz. `pusu-sign`). Burada yalnızca saklıyoruz; imza yetkisi yok.
//! Bu yüzden endpoint auth'suz da görece güvenli: saldırgan ancak kullanıcının
//! gerçekten imzaladığı bir emri saklatabilir, uyduramaz. (Yine de spam'e karşı
//! auth Faz 2'de eklenecek — şimdilik staging.)

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use pusu_core::{Alert, AlertAction};
use pusu_store::{BlobRole, Store, StoreError};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;

/// Tarayıcıdan gelen alarm oluşturma isteği.
#[derive(Debug, Deserialize)]
pub struct CreateAlert {
    /// Domain alarmı (id, koşul, aksiyon, armed_at_ms, entry_oid… tarayıcıda dolu).
    alert: Alert,
    /// Giriş blob'u — işlem alarmında zorunlu, Notify'da yok.
    #[serde(default)]
    entry: Option<BlobIn>,
    /// Ön-imzalı iptal — yalnız limit girişte.
    #[serde(default)]
    cancel: Option<BlobIn>,
}

/// Bir ön-imzalı blob: nonce (dedup/idempotency, §8.11) + POST'lanacak gövde.
#[derive(Debug, Deserialize)]
pub struct BlobIn {
    nonce: u64,
    payload: Value,
}

/// Router'ı kur. `store` state olarak paylaşılıyor.
pub fn router(store: Store) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/alerts", post(create_alert))
        // Tarayıcı farklı origin'den çağırıyor. Staging'de gevşek; prod'da
        // frontend origin'ine daraltılacak.
        .layer(CorsLayer::permissive())
        .with_state(store)
}

async fn create_alert(
    State(store): State<Store>,
    Json(req): Json<CreateAlert>,
) -> Result<Response, ApiError> {
    // OnChain buraya ait değil: borsaya gönderilir, saklanmaz.
    if req.alert.execution().is_onchain() {
        return Err(ApiError::bad("OnChain alarm borsaya gönderilir, saklanmaz"));
    }

    // İşlem alarmının girişi olmalı; blob gerçekten bu sub-account'a mı ait?
    if matches!(req.alert.action, AlertAction::Trade(_)) {
        let Some(entry) = &req.entry else {
            return Err(ApiError::bad("işlem alarmı için giriş blob'u gerekli"));
        };
        check_account(&entry.payload, &req.alert.account)?;
    }
    if let Some(cancel) = &req.cancel {
        check_account(&cancel.payload, &req.alert.account)?;
    }

    store.upsert_user(&req.alert.owner).await?;
    store.insert_alert(&req.alert).await?;
    if let Some(e) = &req.entry {
        store
            .put_blob(&req.alert.id, BlobRole::Entry, e.nonce, &e.payload)
            .await?;
    }
    if let Some(c) = &req.cancel {
        store
            .put_blob(&req.alert.id, BlobRole::Cancel, c.nonce, &c.payload)
            .await?;
    }
    store
        .audit(&req.alert.id, "created", &json!({ "via": "api" }))
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": req.alert.id.as_str() })),
    )
        .into_response())
}

/// Blob'un `account`'u alarmın sub-account'uyla aynı mı? Yanlış hesaba blob
/// saklamayı engelliyor.
fn check_account(payload: &Value, expected: &str) -> Result<(), ApiError> {
    match payload.get("account").and_then(Value::as_str) {
        Some(acct) if acct == expected => Ok(()),
        Some(_) => Err(ApiError::bad("blob account'u alarmın hesabıyla uyuşmuyor")),
        None => Err(ApiError::bad("blob'da account alanı yok")),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl ApiError {
    fn bad(msg: &str) -> Self {
        Self::BadRequest(msg.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            // İç hatayı istemciye sızdırmıyoruz; ayrıntı günlüğe.
            Self::Store(e) => {
                tracing::error!("store hatası: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "alarm kaydedilemedi".to_string(),
                )
            }
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}
