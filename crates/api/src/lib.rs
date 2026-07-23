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
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use pusu_core::{Alert, AlertAction, AlertId};
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
        .route("/health", get(health))
        .route("/alerts", post(create_alert).get(list_alerts))
        .route("/alerts/{id}/cancel", post(cancel_alert))
        .route("/alerts/{id}", delete(dismiss_alert))
        .route("/notifications", get(notifications))
        .route("/notifications/read", post(read_notifications))
        .route("/contact", post(set_contact).get(get_contact))
        // Tarayıcı farklı origin'den çağırıyor. Staging'de gevşek; prod'da
        // frontend origin'ine daraltılacak.
        .layer(CorsLayer::permissive())
        .with_state(store)
}

/// DB'yi de yoklayan health-check. Statik "ok" yerine gerçek bir `SELECT 1` —
/// Postgres düşükken sağlıklı görünmek, sessizce alarm kaybetmenin yoludur.
async fn health(State(store): State<Store>) -> impl IntoResponse {
    match store.ping().await {
        Ok(()) => (StatusCode::OK, "ok"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "db unavailable"),
    }
}

/// Sahip doğrulaması için ortak sorgu — `?owner=<pubkey>`.
///
/// Gerçek auth değil (owner pubkey zaten public); yalnızca id + owner ikisini
/// birden bilmeyi şart koşuyor. Watched armed alarmın iptali fon hareketi
/// içermediği için staging'de yeterli — gerçek imza-tabanlı auth Faz 2.
#[derive(Debug, Deserialize)]
struct OwnerQuery {
    owner: String,
}

async fn list_alerts(
    State(store): State<Store>,
    Query(q): Query<OwnerQuery>,
) -> Result<Response, ApiError> {
    if q.owner.trim().is_empty() {
        return Err(ApiError::bad("owner gerekli"));
    }
    let alerts = store.list_by_owner(&q.owner).await?;
    Ok(Json(alerts).into_response())
}

/// `POST /alerts/{id}/cancel?owner=` — alarmı iptal et.
///
/// İki yol, alarmın durumuna göre:
/// - **Armed**: borsada emir yok, iptal yerel ve anında → `200` cancelled.
/// - **Working**: defterde canlı limit emri var; api imzalayamaz, iptal isteğini
///   watcher'a bırakıyoruz → `202` cancel_requested (watcher birazdan geri çeker).
async fn cancel_alert(
    State(store): State<Store>,
    Path(id): Path<String>,
    Query(q): Query<OwnerQuery>,
) -> Result<Response, ApiError> {
    if q.owner.trim().is_empty() {
        return Err(ApiError::bad("owner gerekli"));
    }
    let aid = AlertId::new(id);

    if store.cancel_armed(&aid, &q.owner).await? {
        store
            .audit(&aid, "cancelled", &json!({ "via": "api" }))
            .await?;
        return Ok(Json(json!({ "id": aid.as_str(), "state": "cancelled" })).into_response());
    }

    if store.request_cancel(&aid, &q.owner).await? {
        store
            .audit(&aid, "cancel_requested", &json!({ "via": "api" }))
            .await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({ "id": aid.as_str(), "state": "cancel_requested" })),
        )
            .into_response());
    }

    Err(ApiError::conflict(
        "iptal edilemedi: alarm bulunamadı, sahibi değilsin ya da sonlanmış",
    ))
}

/// `DELETE /alerts/{id}?owner=` — sonlanmış alarmı listeden kaldır.
async fn dismiss_alert(
    State(store): State<Store>,
    Path(id): Path<String>,
    Query(q): Query<OwnerQuery>,
) -> Result<Response, ApiError> {
    if q.owner.trim().is_empty() {
        return Err(ApiError::bad("owner gerekli"));
    }
    let aid = AlertId::new(id);
    if !store.delete_owned_terminal(&aid, &q.owner).await? {
        return Err(ApiError::conflict(
            "kaldırılamadı: alarm bulunamadı, sahibi değilsin ya da hâlâ aktif (önce iptal et)",
        ));
    }
    store
        .audit(&aid, "dismissed", &json!({ "via": "api" }))
        .await?;
    Ok(Json(json!({ "id": aid.as_str(), "deleted": true })).into_response())
}

/// `GET /notifications?owner=` — kullanıcının son bildirimleri + okunmamış sayısı.
///
/// Notify alarmları koşulu tuttuğunda watcher outbox'a yazıyor; burası onları
/// uygulama-içi zil için sunuyor. Auth notu `list_alerts` ile aynı: owner pubkey
/// zaten public, gerçek imza-tabanlı auth Faz 4.
async fn notifications(
    State(store): State<Store>,
    Query(q): Query<OwnerQuery>,
) -> Result<Response, ApiError> {
    if q.owner.trim().is_empty() {
        return Err(ApiError::bad("owner gerekli"));
    }
    let items = store.list_notifications(&q.owner, 50).await?;
    let unread = store.unread_count(&q.owner).await?;
    Ok(Json(json!({ "notifications": items, "unread": unread })).into_response())
}

/// `POST /notifications/read?owner=` — okunmamışları okundu işaretle. İşaretlenen sayı.
async fn read_notifications(
    State(store): State<Store>,
    Query(q): Query<OwnerQuery>,
) -> Result<Response, ApiError> {
    if q.owner.trim().is_empty() {
        return Err(ApiError::bad("owner gerekli"));
    }
    let marked = store.mark_notifications_read(&q.owner).await?;
    Ok(Json(json!({ "marked": marked })).into_response())
}

/// İletişim kanalı ayarlama isteği. Boş/eksik alan = değiştirme.
#[derive(Debug, Deserialize)]
pub struct SetContact {
    owner: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    telegram: Option<String>,
}

/// `POST /contact` — Notify bildirimleri için e-posta / telegram ayarla.
///
/// Boş string → `None` (o alanı değiştirme; store `COALESCE` ile koruyor).
/// E-posta kaba doğrulanıyor (`@` + uzunluk); asıl doğrulama zaten teslimde
/// (geçersizse Resend reddeder), ama açık çöpü baştan eleyelim.
async fn set_contact(
    State(store): State<Store>,
    Json(req): Json<SetContact>,
) -> Result<Response, ApiError> {
    if req.owner.trim().is_empty() {
        return Err(ApiError::bad("owner gerekli"));
    }
    let clean = |o: &Option<String>| {
        o.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let email = clean(&req.email);
    let telegram = clean(&req.telegram);
    if let Some(e) = &email {
        if !e.contains('@') || e.len() > 254 {
            return Err(ApiError::bad("geçersiz e-posta"));
        }
    }
    store
        .set_contact(&req.owner, email.as_deref(), telegram.as_deref())
        .await?;
    Ok(Json(json!({ "ok": true })).into_response())
}

/// `GET /contact?owner=` — ayarlı e-posta / telegram (prefill için).
async fn get_contact(
    State(store): State<Store>,
    Query(q): Query<OwnerQuery>,
) -> Result<Response, ApiError> {
    if q.owner.trim().is_empty() {
        return Err(ApiError::bad("owner gerekli"));
    }
    let (email, telegram) = store.get_contact(&q.owner).await?;
    Ok(Json(json!({ "email": email, "telegram": telegram })).into_response())
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
    /// İstek geçerli ama alarmın durumu izin vermiyor (ör. aktif alarmı silmek).
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl ApiError {
    fn bad(msg: &str) -> Self {
        Self::BadRequest(msg.to_string())
    }

    fn conflict(msg: &str) -> Self {
        Self::Conflict(msg.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            Self::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
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
