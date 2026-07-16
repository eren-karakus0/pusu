//! `pusu-node`'un gerçek Postgres'e karşı entegrasyon testleri.
//!
//! `PUSU_TEST_DB` set değilse **atlanıyor** (bkz. `pusu-store` testleri).
//! Lokalde:
//!
//! ```bash
//! PUSU_TEST_DB=postgres://pusu:pusu@localhost:5433/pusu cargo test -p pusu-node
//! ```
//!
//! Harness `pusu-store`'unkiyle aynı: tek paylaşımlı runtime + tek havuz,
//! testler seri, her biri öncesinde truncate. Gerekçesi orada uzun uzun yazılı.

use pusu_core::{
    Alert, AlertAction, AlertId, AlertState, Condition, Cross, Entry, Exits, Interval, Side,
    Symbol, TradeSpec,
};
use pusu_engine::{Dispatch, DispatchError};
use pusu_feed::{FeedError, OpenOrder, OrderSource};
use pusu_node::{reconcile, HttpDispatch};
use pusu_store::{BlobRole, Store};
use sqlx::postgres::PgPoolOptions;
use std::sync::{Mutex, OnceLock};

struct Ortam {
    rt: tokio::runtime::Runtime,
    pool: Option<sqlx::PgPool>,
}

fn ortam() -> &'static Ortam {
    static ORTAM: OnceLock<Ortam> = OnceLock::new();
    ORTAM.get_or_init(|| {
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

/// Sabit bir emir listesi döndüren sahte kaynak — testler ağa çıkmasın.
struct SahteEmirler(Vec<OpenOrder>);

impl OrderSource for SahteEmirler {
    async fn open_orders(&self, _account: &str) -> Result<Vec<OpenOrder>, FeedError> {
        Ok(self.0.clone())
    }
}

/// Her okumada hata veren kaynak — "emirler okunamazsa karar verme" için.
struct KirikEmirler;

impl OrderSource for KirikEmirler {
    async fn open_orders(&self, _account: &str) -> Result<Vec<OpenOrder>, FeedError> {
        Err(FeedError::Decode("test: erişilemedi".into()))
    }
}

const ARM_MS: u64 = 1_784_000_000_000;

fn limit_alarm(id: &str) -> Alert {
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
        invalidate: None,
        action: AlertAction::Trade(TradeSpec {
            symbol: Symbol::new("BTC-USD"),
            side: Side::Buy,
            size: 0.004,
            entry: Entry::Limit { price: 89_500.0 },
            exits: Some(Exits::simple(87_000.0, 95_000.0)),
        }),
        state: AlertState::Armed,
        armed_at_ms: ARM_MS,
        entry_oid: None,
        fill_deadline_ms: None,
    }
}

fn resting_order(oid: &str) -> OpenOrder {
    OpenOrder {
        oid: oid.into(),
        symbol: "BTC-USD".into(),
        size: 0.004,
        filled: 0.0,
        order_type: "limit".into(),
        reduce_only: false,
    }
}

/// Alarmı "giriş gönderilmiş ama durum yazılamamış" çatlağına sok:
/// blob koy, dispatched işaretle, oid'i alarma yaz — ama state Armed kalsın.
async fn catlaga_sok(store: &Store, alert: &mut Alert, oid: &str) {
    store.upsert_user(&alert.owner).await.unwrap();
    store.insert_alert(alert).await.unwrap();
    store
        .put_blob(
            &alert.id,
            BlobRole::Entry,
            1,
            &serde_json::json!({"nonce": 1}),
        )
        .await
        .unwrap();
    store
        .mark_dispatched(&alert.id, BlobRole::Entry)
        .await
        .unwrap();
    alert.entry_oid = Some(oid.into());
    store.update_runtime(alert).await.unwrap(); // hâlâ Armed
}

// ── reconcile ────────────────────────────────────────────────────────────

#[test]
fn temiz_armed_dokunulmuyor() {
    // Gönderilmemiş bir Armed alarm mutabakatın konusu değil — watcher'a ait.
    calistir(|store| async move {
        let a = limit_alarm("temiz");
        store.upsert_user(&a.owner).await.unwrap();
        store.insert_alert(&a).await.unwrap();

        let mut alerts = vec![a];
        let orders = SahteEmirler(vec![]);
        let sonuc = reconcile(&store, &orders, &mut alerts, ARM_MS)
            .await
            .unwrap();

        assert_eq!(sonuc.fired.len(), 0);
        assert_eq!(sonuc.working.len(), 0);
        assert_eq!(alerts[0].state, AlertState::Armed, "temiz Armed kalmalı");
    });
}

#[test]
fn gonderilmis_ama_defterde_yok_fired() {
    // Giriş postalandı, oid defterde değil → doldu ya da gitti. İşlem
    // girmiştir; körlemesine tekrar göndermek yerine Fired yazıyoruz.
    calistir(|store| async move {
        let mut a = limit_alarm("dolmus");
        catlaga_sok(&store, &mut a, "oid-yok").await;

        let mut alerts = vec![a];
        let orders = SahteEmirler(vec![]); // defter boş
        let sonuc = reconcile(&store, &orders, &mut alerts, ARM_MS)
            .await
            .unwrap();

        assert_eq!(sonuc.fired, vec!["dolmus".to_string()]);
        assert_eq!(alerts[0].state, AlertState::Fired);
        // Kalıcı da olmalı — sadece bellekte değil.
        let geri = store.load_live().await.unwrap();
        assert!(geri.is_empty(), "Fired artık canlı değil");
    });
}

#[test]
fn gonderilmis_ve_defterde_bekliyor_working() {
    // Limit defterde, hiç dolmamış → retest bekliyor. Working + deadline.
    calistir(|store| async move {
        let mut a = limit_alarm("bekliyor");
        catlaga_sok(&store, &mut a, "oid-bekler").await;

        let mut alerts = vec![a];
        let orders = SahteEmirler(vec![resting_order("oid-bekler")]);
        let now = ARM_MS + 5_000;
        let sonuc = reconcile(&store, &orders, &mut alerts, now).await.unwrap();

        assert_eq!(sonuc.working, vec!["bekliyor".to_string()]);
        assert_eq!(alerts[0].state, AlertState::Working);
        // Deadline = now + koşulun periyodu (M15).
        assert_eq!(
            alerts[0].fill_deadline_ms,
            Some(now + Interval::M15.duration_ms())
        );
    });
}

#[test]
fn gonderilmis_ve_kismen_dolmus_fired() {
    // Defterde ama kısmen dolmuş → emir alınmış, pozisyon var → Fired.
    calistir(|store| async move {
        let mut a = limit_alarm("kismen");
        catlaga_sok(&store, &mut a, "oid-kismi").await;

        let mut order = resting_order("oid-kismi");
        order.filled = 0.001;
        let mut alerts = vec![a];
        let orders = SahteEmirler(vec![order]);
        let sonuc = reconcile(&store, &orders, &mut alerts, ARM_MS)
            .await
            .unwrap();

        assert_eq!(sonuc.fired, vec!["kismen".to_string()]);
        assert_eq!(alerts[0].state, AlertState::Fired);
    });
}

#[test]
fn oidsiz_gonderim_watchera_birakiliyor() {
    // Gönderildi işaretli ama entry_oid yok → izleyemeyiz, dokunmuyoruz.
    calistir(|store| async move {
        let a = limit_alarm("oidsiz");
        store.upsert_user(&a.owner).await.unwrap();
        store.insert_alert(&a).await.unwrap();
        store
            .put_blob(&a.id, BlobRole::Entry, 1, &serde_json::json!({}))
            .await
            .unwrap();
        store.mark_dispatched(&a.id, BlobRole::Entry).await.unwrap();
        // entry_oid bilerek None.

        let mut alerts = vec![a];
        let orders = SahteEmirler(vec![]);
        let sonuc = reconcile(&store, &orders, &mut alerts, ARM_MS)
            .await
            .unwrap();

        assert_eq!(sonuc.fired.len() + sonuc.working.len(), 0);
        assert_eq!(alerts[0].state, AlertState::Armed);
    });
}

#[test]
fn zaten_working_reconcile_disinda() {
    // Working alarm normal `track` döngüsünün işi; mutabakat yalnızca
    // Armed görünüp gönderilmiş olan çatlağa bakar.
    calistir(|store| async move {
        let mut a = limit_alarm("working");
        a.state = AlertState::Working;
        a.entry_oid = Some("oid-w".into());
        store.upsert_user(&a.owner).await.unwrap();
        store.insert_alert(&a).await.unwrap();

        let mut alerts = vec![a];
        let orders = SahteEmirler(vec![]); // defterde olmasa bile dokunma
        let sonuc = reconcile(&store, &orders, &mut alerts, ARM_MS)
            .await
            .unwrap();

        assert_eq!(sonuc.fired.len() + sonuc.working.len(), 0);
        assert_eq!(alerts[0].state, AlertState::Working);
    });
}

#[test]
fn emirler_okunamazsa_hata_veriyor_durumu_bozmuyor() {
    // "Emri okuyamadıysan karar verme." Mutabakat hata döndürür, alarm Armed
    // kalır — yanlış Fired/Working yazmaktansa açılışı geri çevirmek doğru.
    calistir(|store| async move {
        let mut a = limit_alarm("kirik");
        catlaga_sok(&store, &mut a, "oid-x").await;

        let mut alerts = vec![a];
        let sonuc = reconcile(&store, &KirikEmirler, &mut alerts, ARM_MS).await;

        assert!(sonuc.is_err());
        assert_eq!(alerts[0].state, AlertState::Armed);
    });
}

// ── HttpDispatch ───────────────────────────────────────────────────────────

#[test]
fn blob_yoksa_noblob() {
    calistir(|store| async move {
        let a = limit_alarm("blobsuz");
        store.upsert_user(&a.owner).await.unwrap();
        store.insert_alert(&a).await.unwrap();

        let disp = HttpDispatch::new(store, "http://127.0.0.1:1");
        let r = disp.submit(&a).await;
        assert!(matches!(r, Err(DispatchError::NoBlob)));
    });
}

#[test]
fn niyet_post_basarisiz_olsa_bile_isaretleniyor() {
    // Kritik değişmez: mark_dispatched, POST'tan ÖNCE. Ulaşılamaz bir adrese
    // gönderip POST'u düşürüyoruz; yine de was_dispatched=true olmalı — çökme
    // burada olsaydı reconcile'ın toparlayabilmesi buna bağlı.
    calistir(|store| async move {
        let a = limit_alarm("niyet");
        store.upsert_user(&a.owner).await.unwrap();
        store.insert_alert(&a).await.unwrap();
        store
            .put_blob(&a.id, BlobRole::Entry, 1, &serde_json::json!({"nonce": 1}))
            .await
            .unwrap();

        // Port 1 → bağlantı reddi; POST kesin başarısız.
        let disp = HttpDispatch::new(store.clone(), "http://127.0.0.1:1");
        let r = disp.submit(&a).await;
        assert!(matches!(r, Err(DispatchError::Network(_))), "POST düşmeli");

        assert!(
            store.was_dispatched(&a.id, BlobRole::Entry).await.unwrap(),
            "niyet POST'tan önce yazılmalıydı"
        );
    });
}
