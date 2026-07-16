//! `pusu-api` entegrasyon testleri — gerçek Postgres + router `oneshot`.
//!
//! `PUSU_TEST_DB` set değilse atlanıyor (bkz. `pusu-store` testleri). Harness
//! aynı: tek paylaşımlı runtime + tek havuz, testler seri, her biri öncesinde
//! truncate.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pusu_core::{
    Alert, AlertAction, AlertId, AlertState, Condition, Cross, Entry, Exits, Interval, Side,
    Symbol, TradeSpec,
};
use pusu_store::{BlobRole, Store};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use std::sync::{Mutex, OnceLock};
use tower::ServiceExt;

struct Ortam {
    rt: tokio::runtime::Runtime,
    pool: Option<sqlx::PgPool>,
}

fn ortam() -> &'static Ortam {
    static ORTAM: OnceLock<Ortam> = OnceLock::new();
    ORTAM.get_or_init(|| {
        // Blob'lar at-rest şifreli; testlerde sabit bir anahtar yeter.
        if std::env::var("PUSU_BLOB_KEY").is_err() {
            std::env::set_var(
                "PUSU_BLOB_KEY",
                "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=",
            );
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let pool = rt.block_on(async {
            let url = std::env::var("PUSU_TEST_DB").ok()?;
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .connect(&url)
                .await
                .expect("test DB'sine bağlanılamadı");
            Store::from_pool(pool.clone()).await.expect("migration");
            Some(pool)
        });
        Ortam { rt, pool }
    })
}

fn calistir<F, Fut>(govde: F)
where
    F: FnOnce(Store) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    static KILIT: Mutex<()> = Mutex::new(());
    let env = ortam();
    let Some(pool) = env.pool.clone() else {
        eprintln!("PUSU_TEST_DB yok — test atlanıyor");
        return;
    };
    let _guard = KILIT.lock().unwrap_or_else(|e| e.into_inner());
    env.rt.block_on(async move {
        sqlx::query("TRUNCATE alerts, presigned_blobs, audit_log, users CASCADE")
            .execute(&pool)
            .await
            .unwrap();
        govde(Store::new(pool)).await;
    });
}

/// Router'a bir istek gönder, (durum, gövde) döndür.
async fn iste(store: &Store, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let app = pusu_api::router(store.clone());
    let mut b = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&v).unwrap())
        }
        None => Body::empty(),
    };
    let resp = app.oneshot(b.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let val = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, val)
}

const SUB: &str = "sub-pk";
const MASTER: &str = "master-pk";

fn watched_limit(id: &str) -> Alert {
    Alert {
        id: AlertId::new(id),
        owner: MASTER.into(),
        account: SUB.into(),
        condition: Condition::CandleClose {
            symbol: Symbol::new("BTC-USD"),
            interval: Interval::M15,
            cross: Cross::Above,
            price: 90_000.0,
        },
        invalidate: None,
        action: AlertAction::Trade(TradeSpec {
            symbol: Symbol::new("BTC-USD"),
            side: Side::Buy,
            size: 0.004,
            entry: Entry::Limit { price: 89_500.0 },
            exits: Some(Exits::simple(87_000.0, 95_000.0)),
        }),
        state: AlertState::Armed,
        armed_at_ms: 1_784_000_000_000,
        entry_oid: Some("giris-oid".into()),
        fill_deadline_ms: None,
    }
}

fn blob(account: &str, nonce: u64) -> Value {
    json!({
        "nonce": nonce,
        "payload": {
            "account": account,
            "signer": MASTER,
            "actions": [{ "l": { "c": "BTC-USD" } }],
            "nonce": nonce,
            "signature": "sahte-imza",
        }
    })
}

fn create_body(alert: &Alert, entry: Option<Value>, cancel: Option<Value>) -> Value {
    json!({
        "alert": serde_json::to_value(alert).unwrap(),
        "entry": entry,
        "cancel": cancel,
    })
}

#[test]
fn saglik_ucu_ok() {
    calistir(|store| async move {
        let (status, _) = iste(&store, "GET", "/health", None).await;
        assert_eq!(status, StatusCode::OK);
    });
}

#[test]
fn watched_limit_alarm_bloblariyla_saklaniyor() {
    calistir(|store| async move {
        let a = watched_limit("a1");
        let body = create_body(&a, Some(blob(SUB, 1)), Some(blob(SUB, 2)));
        let (status, out) = iste(&store, "POST", "/alerts", Some(body)).await;

        assert_eq!(status, StatusCode::CREATED, "yanıt: {out}");
        assert_eq!(out["id"], "a1");

        // Alarm canlı yüklenebiliyor.
        let canli = store.load_live().await.unwrap();
        assert_eq!(canli.len(), 1);
        assert_eq!(canli[0], a);

        // İki blob da yerinde.
        assert!(store
            .get_blob(&a.id, BlobRole::Entry)
            .await
            .unwrap()
            .is_some());
        assert!(store
            .get_blob(&a.id, BlobRole::Cancel)
            .await
            .unwrap()
            .is_some());
    });
}

#[test]
fn onchain_alarm_reddediliyor() {
    // OnChain borsaya gider, saklanmaz.
    calistir(|store| async move {
        let mut a = watched_limit("a1");
        a.condition = Condition::MarkCross {
            symbol: Symbol::new("BTC-USD"),
            cross: Cross::Below,
            price: 88_000.0,
        };
        if let AlertAction::Trade(s) = &mut a.action {
            s.entry = Entry::Market; // OnChain limit zaten derlenmiyor
        }
        let body = create_body(&a, Some(blob(SUB, 1)), None);
        let (status, out) = iste(&store, "POST", "/alerts", Some(body)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(out["error"].as_str().unwrap().contains("OnChain"));
        assert!(store.load_live().await.unwrap().is_empty(), "yazılmamalı");
    });
}

#[test]
fn giris_blobu_olmayan_islem_reddediliyor() {
    calistir(|store| async move {
        let a = watched_limit("a1");
        let body = create_body(&a, None, None);
        let (status, _) = iste(&store, "POST", "/alerts", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(store.load_live().await.unwrap().is_empty());
    });
}

#[test]
fn blob_hesabi_uyusmazsa_reddediliyor() {
    // Yanlış sub-account'a blob saklamayı engelle.
    calistir(|store| async move {
        let a = watched_limit("a1");
        let body = create_body(&a, Some(blob("baska-sub", 1)), None);
        let (status, out) = iste(&store, "POST", "/alerts", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(out["error"].as_str().unwrap().contains("uyuşmuyor"));
    });
}

#[test]
fn alarmlar_sahibine_gore_en_yeni_once_listeleniyor() {
    calistir(|store| async move {
        // İki alarm kur (uçtan uca POST yoluyla).
        let a1 = watched_limit("a1");
        iste(
            &store,
            "POST",
            "/alerts",
            Some(create_body(&a1, Some(blob(SUB, 1)), Some(blob(SUB, 2)))),
        )
        .await;

        let mut a2 = watched_limit("a2");
        a2.armed_at_ms = a1.armed_at_ms + 1_000; // daha yeni
        iste(
            &store,
            "POST",
            "/alerts",
            Some(create_body(&a2, Some(blob(SUB, 3)), Some(blob(SUB, 4)))),
        )
        .await;

        let (status, out) = iste(&store, "GET", &format!("/alerts?owner={MASTER}"), None).await;
        assert_eq!(status, StatusCode::OK);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], "a2", "en yeni önce");
        assert_eq!(arr[1]["id"], "a1");
        // İmzalı blob'lar listeye sızmıyor.
        assert!(arr[0].get("entry").is_none());
    });
}

#[test]
fn baska_sahibin_alarmi_listede_gorunmez() {
    calistir(|store| async move {
        let a = watched_limit("a1");
        iste(
            &store,
            "POST",
            "/alerts",
            Some(create_body(&a, Some(blob(SUB, 1)), Some(blob(SUB, 2)))),
        )
        .await;

        let (status, out) = iste(&store, "GET", "/alerts?owner=baskasi", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(out.as_array().unwrap().is_empty());
    });
}

#[test]
fn owner_belirtilmezse_reddediliyor() {
    calistir(|store| async move {
        let (status, _) = iste(&store, "GET", "/alerts", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    });
}

#[test]
fn armed_alarm_iptal_edilip_bloblari_siliniyor() {
    calistir(|store| async move {
        let a = watched_limit("a1");
        iste(
            &store,
            "POST",
            "/alerts",
            Some(create_body(&a, Some(blob(SUB, 1)), Some(blob(SUB, 2)))),
        )
        .await;

        let (status, _) = iste(
            &store,
            "POST",
            &format!("/alerts/a1/cancel?owner={MASTER}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Listede cancelled görünüyor, watcher'ın canlı setinden düştü.
        let (_, list) = iste(&store, "GET", &format!("/alerts?owner={MASTER}"), None).await;
        assert_eq!(list.as_array().unwrap()[0]["state"], "cancelled");
        assert!(store.load_live().await.unwrap().is_empty());

        // Blob'lar bir daha gönderilemesin diye silindi.
        assert!(store
            .get_blob(&a.id, BlobRole::Entry)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get_blob(&a.id, BlobRole::Cancel)
            .await
            .unwrap()
            .is_none());
    });
}

#[test]
fn baskasinin_alarmi_iptal_edilemiyor() {
    calistir(|store| async move {
        let a = watched_limit("a1");
        iste(
            &store,
            "POST",
            "/alerts",
            Some(create_body(&a, Some(blob(SUB, 1)), Some(blob(SUB, 2)))),
        )
        .await;

        let (status, _) = iste(&store, "POST", "/alerts/a1/cancel?owner=baskasi", None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        // Dokunulmadı: hâlâ armed/canlı.
        assert_eq!(store.load_live().await.unwrap().len(), 1);
    });
}

#[test]
fn sonlanmis_alarm_listeden_kaldirilabiliyor() {
    calistir(|store| async move {
        let a = watched_limit("a1");
        iste(
            &store,
            "POST",
            "/alerts",
            Some(create_body(&a, Some(blob(SUB, 1)), Some(blob(SUB, 2)))),
        )
        .await;
        // Önce iptal (armed → cancelled = terminal), sonra kaldır.
        iste(
            &store,
            "POST",
            &format!("/alerts/a1/cancel?owner={MASTER}"),
            None,
        )
        .await;
        let (status, _) = iste(
            &store,
            "DELETE",
            &format!("/alerts/a1?owner={MASTER}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (_, list) = iste(&store, "GET", &format!("/alerts?owner={MASTER}"), None).await;
        assert!(list.as_array().unwrap().is_empty());
    });
}

#[test]
fn aktif_alarm_kaldirilamiyor() {
    // Defterde/beklemede alarm önce iptal edilmeli; doğrudan silinemez.
    calistir(|store| async move {
        let a = watched_limit("a1");
        iste(
            &store,
            "POST",
            "/alerts",
            Some(create_body(&a, Some(blob(SUB, 1)), Some(blob(SUB, 2)))),
        )
        .await;

        let (status, _) = iste(
            &store,
            "DELETE",
            &format!("/alerts/a1?owner={MASTER}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(store.load_live().await.unwrap().len(), 1, "silinmemeli");
    });
}
