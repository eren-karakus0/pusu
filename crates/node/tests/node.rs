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
use pusu_engine::{Dispatch, DispatchError, Watcher};
use pusu_feed::{FeedError, Kline, KlineSource, MarkSource, OpenOrder, OrderSource};
use pusu_node::{reconcile, tur, HttpDispatch};
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

// ── tur: yükle → tick → persist ──────────────────────────────────────────────

struct SahteKline(Vec<Kline>);
impl KlineSource for SahteKline {
    async fn klines(
        &self,
        _s: &Symbol,
        _i: Interval,
        _since: Option<u64>,
    ) -> Result<Vec<Kline>, FeedError> {
        Ok(self.0.clone())
    }
}

struct SahteMark(f64);
impl MarkSource for SahteMark {
    async fn mark(&self, _s: &Symbol) -> Result<f64, FeedError> {
        Ok(self.0)
    }
}

/// Her gönderimi "doldu" sayan sahte borsa.
struct DolduranBorsa;
impl Dispatch for DolduranBorsa {
    async fn submit(&self, _a: &Alert) -> Result<serde_json::Value, DispatchError> {
        Ok(
            serde_json::json!({"status":"ok","response":{"data":{"statuses":[
                {"filled":{"totalSz":0.004,"avgPx":90_500.0,"oid":"x"}}
            ]}}}),
        )
    }
    async fn cancel(&self, _a: &Alert) -> Result<serde_json::Value, DispatchError> {
        Ok(serde_json::json!({"status":"ok"}))
    }
}

fn market_alarm(id: &str) -> Alert {
    let mut a = limit_alarm(id);
    if let AlertAction::Trade(s) = &mut a.action {
        s.entry = Entry::Market;
    }
    a
}

fn kapanan_mum(close_time: u64, close: f64) -> Kline {
    Kline {
        open_time: close_time - Interval::M15.duration_ms(),
        close_time,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
        num_trades: 1,
    }
}

#[test]
fn tur_ateslenen_alarmi_store_a_yaziyor() {
    // Uçtan uca kablolama: store'dan yükle → watcher ateşlesin → sonucu geri yaz.
    calistir(|store| async move {
        let a = market_alarm("uctan-uca");
        store.upsert_user(&a.owner).await.unwrap();
        store.insert_alert(&a).await.unwrap();

        // M15 mumu ARM_MS+1sn'de 90.500'den kapandı (eşik 90.000, üstünde).
        let mum = kapanan_mum(ARM_MS + 1_000, 90_500.0);
        let mut watcher = Watcher::new(
            SahteKline(vec![mum]),
            SahteMark(90_500.0),
            SahteEmirler(vec![]),
            DolduranBorsa,
        );

        let now = ARM_MS + 2_000;
        let tick = tur(&store, &mut watcher, now).await.unwrap();

        assert_eq!(tick.fired.len(), 1, "koşul tuttu, ateşlemeliydi");
        // Fired terminal → artık canlı değil, store'a yazılmış olmalı.
        assert!(store.load_live().await.unwrap().is_empty());
        assert!(
            store.audit_count(&a.id).await.unwrap() >= 1,
            "durum değişikliği denetime düşmeli"
        );
    });
}

#[test]
fn tur_kosul_tutmazsa_armed_birakiyor_ve_yazmiyor() {
    // Eşik geçilmedi: alarm silahlı kalmalı, gereksiz yazım olmamalı.
    calistir(|store| async move {
        let a = market_alarm("tutmayan");
        store.upsert_user(&a.owner).await.unwrap();
        store.insert_alert(&a).await.unwrap();

        // 89.900 < 90.000 eşiği → ateşlemez.
        let mum = kapanan_mum(ARM_MS + 1_000, 89_900.0);
        let mut watcher = Watcher::new(
            SahteKline(vec![mum]),
            SahteMark(89_900.0),
            SahteEmirler(vec![]),
            DolduranBorsa,
        );

        let tick = tur(&store, &mut watcher, ARM_MS + 2_000).await.unwrap();

        assert!(tick.fired.is_empty());
        let canli = store.load_live().await.unwrap();
        assert_eq!(canli.len(), 1);
        assert_eq!(canli[0].state, AlertState::Armed);
        assert_eq!(store.audit_count(&a.id).await.unwrap(), 0, "yazım olmamalı");
    });
}
