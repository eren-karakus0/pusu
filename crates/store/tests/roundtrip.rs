//! Gerçek Postgres'e karşı entegrasyon testleri.
//!
//! `PUSU_TEST_DB` set değilse **atlanıyor** (CI'da DB yok, orada birim
//! mantığı zaten test ediliyor). Lokalde:
//!
//! ```bash
//! PUSU_TEST_DB=postgres://pusu:pusu@localhost:5433/pusu cargo test -p pusu-store
//! ```
//!
//! Testler seri çalışıyor ve her biri öncesinde tabloları boşaltıyor.

use pusu_core::{
    Alert, AlertAction, AlertId, AlertState, Condition, Cross, Entry, Exits, Interval, Side,
    Symbol, TradeSpec,
};
use pusu_store::{BlobRole, Store};
use sqlx::postgres::PgPoolOptions;
use std::sync::{Mutex, OnceLock};

// Tek paylaşımlı runtime + tek havuz, testler seri. Neden hepsi:
//
// 1. **Paylaşımlı runtime.** sqlx havuzu onu yaratan tokio runtime'ına bağlı.
//    `#[tokio::test]` her teste ayrı runtime kuruyor; ilk test havuzu kendi
//    runtime'ında yaratıp bitince o runtime kapanıyor, havuz ölüyor ve
//    sonraki testler ölü havuzdan bağlantı beklerken sonsuza dek asılıyor.
//    Bu yüzden testler düz `#[test]` ve ortak runtime'da `block_on`.
//
// 2. **Tek havuz.** Windows Docker Desktop'ta her yeni TCP bağlantısı pahalı;
//    test başına havuz açmak bağlantı fırtınası yaratıp bazılarını 30 sn
//    timeout'a düşürüyordu.
//
// 3. **Seri + truncate.** İzolasyon şema yerine her test öncesi truncate ile.
//    `std::sync::Mutex` block_on dışında tutuluyor: testler sırayla giriyor.
//
// Migration yalnızca bir kez (havuz kurulurken) çalışıyor.

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

/// Test gövdesini paylaşımlı runtime'da, seri, temiz tablolarla çalıştır.
/// `PUSU_TEST_DB` yoksa gövde hiç çağrılmaz (test atlanır).
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

fn ornek_alarm(id: &str) -> Alert {
    Alert {
        id: AlertId::new(id),
        owner: "master-pk".into(),
        account: "sub-pk".into(),
        condition: Condition::CandleClose {
            symbol: Symbol::new("BTC-USD"),
            interval: Interval::M15,
            cross: Cross::Above,
            price: 90_000.0,
        },
        invalidate: Some(Condition::MarkCross {
            symbol: Symbol::new("BTC-USD"),
            cross: Cross::Below,
            price: 88_000.0,
        }),
        action: AlertAction::Trade(TradeSpec {
            symbol: Symbol::new("BTC-USD"),
            side: Side::Buy,
            size: 0.004,
            entry: Entry::Limit { price: 89_500.0 },
            exits: Some(Exits::simple(87_000.0, 95_000.0)),
        }),
        state: AlertState::Armed,
        armed_at_ms: 1_784_000_000_000,
        entry_oid: None,
        fill_deadline_ms: None,
        cancel_requested: false,
    }
}

#[test]
fn alarm_yazilip_aynen_geri_okunuyor() {
    calistir(|store| async move {
        let a = ornek_alarm("a1");
        store.upsert_user(&a.owner).await.unwrap();
        store.insert_alert(&a).await.unwrap();

        let geri = store.load_live().await.unwrap();
        assert_eq!(geri.len(), 1);
        // Domain nesnesi bit bit aynı dönmeli — JSONB round-trip'i bozmamalı.
        assert_eq!(geri[0], a);
    });
}

#[test]
fn yalnizca_canli_alarmlar_yukleniyor() {
    calistir(|store| async move {
        store.upsert_user("master-pk").await.unwrap();

        let mut armed = ornek_alarm("armed");
        armed.state = AlertState::Armed;
        let mut working = ornek_alarm("working");
        working.state = AlertState::Working;
        let mut fired = ornek_alarm("fired");
        fired.state = AlertState::Fired;
        let mut missed = ornek_alarm("missed");
        missed.state = AlertState::Missed;

        for a in [&armed, &working, &fired, &missed] {
            store.insert_alert(a).await.unwrap();
        }

        let canli = store.load_live().await.unwrap();
        let mut ids: Vec<_> = canli.iter().map(|a| a.id.as_str().to_string()).collect();
        ids.sort();
        // Terminal olanlar (fired/missed) gelmemeli — watcher onlarla ilgilenmiyor.
        assert_eq!(ids, vec!["armed", "working"]);
    });
}

#[test]
fn runtime_guncellemesi_state_ve_oidi_yaziyor() {
    calistir(|store| async move {
        let mut a = ornek_alarm("a1");
        store.upsert_user(&a.owner).await.unwrap();
        store.insert_alert(&a).await.unwrap();

        // Limit giriş deftere kondu: Working + oid + deadline.
        a.state = AlertState::Working;
        a.entry_oid = Some("giris-oid-123".into());
        a.fill_deadline_ms = Some(1_784_000_900_000);
        store.update_runtime(&a).await.unwrap();

        let geri = &store.load_live().await.unwrap()[0];
        assert_eq!(geri.state, AlertState::Working);
        assert_eq!(geri.entry_oid.as_deref(), Some("giris-oid-123"));
        assert_eq!(geri.fill_deadline_ms, Some(1_784_000_900_000));
    });
}

#[test]
fn iki_blob_ayri_rollerde_saklaniyor() {
    // Limit girişte alarm başına İKİ blob: giriş + ön-imzalı iptal (§8.9).
    calistir(|store| async move {
        let a = ornek_alarm("a1");
        store.upsert_user(&a.owner).await.unwrap();
        store.insert_alert(&a).await.unwrap();

        let giris = serde_json::json!({"nonce": 111, "actions": ["m"]});
        let iptal = serde_json::json!({"nonce": 222, "actions": ["cx"]});
        store
            .put_blob(&a.id, BlobRole::Entry, 111, &giris)
            .await
            .unwrap();
        store
            .put_blob(&a.id, BlobRole::Cancel, 222, &iptal)
            .await
            .unwrap();

        assert_eq!(
            store.get_blob(&a.id, BlobRole::Entry).await.unwrap(),
            Some(giris)
        );
        assert_eq!(
            store.get_blob(&a.id, BlobRole::Cancel).await.unwrap(),
            Some(iptal)
        );
    });
}

#[test]
fn gonderim_niyeti_cokme_sonrasi_okunabiliyor() {
    // §8.11 mutabakatının temeli: "bu blob'u postaladım mı?"
    calistir(|store| async move {
        let a = ornek_alarm("a1");
        store.upsert_user(&a.owner).await.unwrap();
        store.insert_alert(&a).await.unwrap();
        let blob = serde_json::json!({"nonce": 111});
        store
            .put_blob(&a.id, BlobRole::Entry, 111, &blob)
            .await
            .unwrap();

        // Henüz gönderilmedi.
        assert!(!store.was_dispatched(&a.id, BlobRole::Entry).await.unwrap());

        // Postalamadan ÖNCE niyet işaretlenir.
        store.mark_dispatched(&a.id, BlobRole::Entry).await.unwrap();
        assert!(store.was_dispatched(&a.id, BlobRole::Entry).await.unwrap());
    });
}

#[test]
fn alarm_silininca_bloblari_da_gidiyor_ama_audit_kaliyor() {
    calistir(|store| async move {
        let a = ornek_alarm("a1");
        store.upsert_user(&a.owner).await.unwrap();
        store.insert_alert(&a).await.unwrap();
        store
            .put_blob(&a.id, BlobRole::Entry, 111, &serde_json::json!({}))
            .await
            .unwrap();
        store
            .audit(&a.id, "state_change", &serde_json::json!({"to": "armed"}))
            .await
            .unwrap();

        store.delete_alert(&a.id).await.unwrap();

        // Blob CASCADE ile gitti.
        assert_eq!(store.get_blob(&a.id, BlobRole::Entry).await.unwrap(), None);
        // Denetim izi kaldı — "alarmım neden vardı" sorusunun kaynağı.
        assert_eq!(store.audit_count(&a.id).await.unwrap(), 1);
    });
}

#[test]
fn audit_yalnizca_ekliyor() {
    calistir(|store| async move {
        let a = ornek_alarm("a1");
        store.upsert_user(&a.owner).await.unwrap();
        store.insert_alert(&a).await.unwrap();

        for i in 0..3 {
            store
                .audit(&a.id, "state_change", &serde_json::json!({"seq": i}))
                .await
                .unwrap();
        }
        assert_eq!(store.audit_count(&a.id).await.unwrap(), 3);
    });
}
