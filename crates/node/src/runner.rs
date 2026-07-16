//! Çalışan düğüm: saf `pusu-engine`'i gerçek dünyaya bağlayan olay döngüsü.
//!
//! Engine bilerek saf — store'a, ağa, saate dokunmaz. Bu modül o "kirli
//! kabuk": her turda alarmları store'dan **yükler**, watcher'ı bir kez
//! **döndürür**, değişen durumları geri **yazar**.
//!
//! ```text
//! açılış:  load_live → reconcile   (çökme sonrası borsayla mutabakat)
//! her tur: load_live → tick → diff'i persist → uyu
//! ```
//!
//! # Neden her tur yeniden yükleniyor
//!
//! Watcher'ın kendi mum geçmişi (snapshot) turlar arası korunuyor — o
//! `Watcher` içinde yaşıyor, alarm listesinden bağımsız. Alarm listesini her
//! tur store'dan tazelemek iki işi bedavaya hallediyor: kullanıcının yeni
//! kurduğu alarmlar kendiliğinden giriyor, nihai olanlar (`load_live` yalnızca
//! armed/working döndürdüğü için) kendiliğinden düşüyor.
//!
//! # Kalıcılaştırma neden diff
//!
//! Watcher alarmları yerinde değiştiriyor ama hangisine dokunduğunu tek bir
//! listede toplamıyor (örn. `Working`→`Fired` dolum, `Tick`'te iz bırakmıyor).
//! O yüzden `Tick`'e güvenmek yerine turdan önceki ve sonraki çalışma-zamanı
//! alanlarını karşılaştırıp yalnızca gerçekten değişeni yazıyoruz — bu hem
//! eksiksiz hem de değişmeyen satırların `updated_at`'ini boşuna bumplamıyor.

use pusu_core::{Alert, AlertState};
use pusu_engine::{Dispatch, Tick, Watcher};
use pusu_feed::{
    HttpKlineSource, HttpMarkSource, HttpOrderSource, KlineSource, MarkSource, OrderSource,
};
use pusu_store::{Store, StoreError};
use std::time::Duration;
use tracing::{error, info, warn};

use crate::dispatch::HttpDispatch;
use crate::reconcile::{reconcile, ReconcileError};

/// Düğümün çalışması için gereken her şey.
pub struct Config {
    /// Postgres bağlantı dizesi.
    pub database_url: String,
    /// BULK REST kökü (örn. staging), sonda `/` olmadan.
    pub bulk_url: String,
    /// Turlar arası bekleme.
    pub poll_interval: Duration,
}

impl Config {
    /// Ortam değişkenlerinden oku.
    ///
    /// - `PUSU_DATABASE_URL` — zorunlu
    /// - `PUSU_BULK_URL` — zorunlu (yanlış URL'e körlemesine emir göndermektense
    ///   açıkça istiyoruz)
    /// - `PUSU_POLL_SECS` — opsiyonel, varsayılan 15
    pub fn from_env() -> Result<Self, NodeError> {
        let database_url = zorunlu("PUSU_DATABASE_URL")?;
        let bulk_url = zorunlu("PUSU_BULK_URL")?.trim_end_matches('/').to_string();
        let poll_secs = std::env::var("PUSU_POLL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(15);
        Ok(Self {
            database_url,
            bulk_url,
            poll_interval: Duration::from_secs(poll_secs.max(1)),
        })
    }
}

fn zorunlu(key: &str) -> Result<String, NodeError> {
    std::env::var(key).map_err(|_| NodeError::Config(format!("{key} tanımlı değil")))
}

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("yapılandırma: {0}")]
    Config(String),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Reconcile(#[from] ReconcileError),
}

/// Düğümü çalıştır. `Ctrl-C`'ye kadar döner.
///
/// Açılışta bir kez mutabakat yapıyor (çökme sonrası borsayla uzlaşma), sonra
/// poll döngüsüne giriyor. Döngü içi hatalar (DB gelip gitmesi gibi) günlüğe
/// yazılıp geçiliyor — geçici bir aksaklık düğümü düşürmemeli. Açılış
/// mutabakatının hatası ise yayılıyor: temiz bir başlangıç yapamadıysak
/// devam etmek tehlikeli.
pub async fn run(cfg: Config) -> Result<(), NodeError> {
    // Blob'ları çözebilmemiz için anahtar şart (api ile AYNI PUSU_BLOB_KEY).
    // Yoksa hata ilk dispatch'te gecelerce sonra çıkardı; başlangıca çekiyoruz.
    pusu_store::check_key()?;

    let store = Store::connect(&cfg.database_url).await?;
    let klines = HttpKlineSource::new(&cfg.bulk_url);
    let marks = HttpMarkSource::new(&cfg.bulk_url);
    let orders = HttpOrderSource::new(&cfg.bulk_url);
    let dispatch = HttpDispatch::new(store.clone(), &cfg.bulk_url);

    // Açılış mutabakatı: "gönderdim ama durumu yazamadım" çatlağını kapat.
    // orders yalnızca ödünç alınıyor; hemen ardından watcher'a taşınıyor.
    let mut acilis = store.load_live().await?;
    let rec = reconcile(&store, &orders, &mut acilis, now_ms()).await?;
    info!(
        fired = rec.fired.len(),
        working = rec.working.len(),
        "açılış mutabakatı tamam"
    );

    let mut watcher = Watcher::new(klines, marks, orders, dispatch);
    let mut ticker = tokio::time::interval(cfg.poll_interval);
    info!(
        poll_secs = cfg.poll_interval.as_secs(),
        "watcher döngüsü başlıyor"
    );

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("kapatma sinyali — döngü sonlandırılıyor");
                return Ok(());
            }
            _ = ticker.tick() => {
                match tur(&store, &mut watcher, now_ms()).await {
                    Ok(t) => ozetle(&t),
                    // Tek turun DB hatası ölümcül değil; bir sonraki tur yeniden dener.
                    Err(e) => error!("tur atlandı: {e}"),
                }
            }
        }
    }
}

/// Tek bir poll turu: yükle → watcher'ı döndür → değişen durumları yaz.
///
/// Döngüden ayrı ve generic olması bilerek: fake feed'lerle gerçek store'a
/// karşı test edilebilsin, gerekirse başka bir zamanlayıcıya gömülebilsin.
/// `ozetle`'yi çağırmıyor — turun sonucunu (`Tick`) döndürüp çağırana bırakıyor.
pub async fn tur<K: KlineSource, M: MarkSource, O: OrderSource, D: Dispatch>(
    store: &Store,
    watcher: &mut Watcher<K, M, O, D>,
    now_ms: u64,
) -> Result<Tick, StoreError> {
    let mut alerts = store.load_live().await?;
    let onceki: Vec<Calisma> = alerts.iter().map(calisma).collect();

    let tick = watcher.tick(&mut alerts, now_ms).await;

    persist(store, &alerts, &onceki).await;
    Ok(tick)
}

/// Bir alarmın watcher'ın dokunabileceği tek alanları.
type Calisma = (AlertState, Option<String>, Option<u64>);

fn calisma(a: &Alert) -> Calisma {
    (a.state, a.entry_oid.clone(), a.fill_deadline_ms)
}

/// Turdan sonra değişen çalışma-zamanı alanlarını yaz.
///
/// `alerts` yerinde değiştirildi ama sırası/uzunluğu korundu (watcher eleman
/// ekleyip çıkarmıyor), o yüzden indeks indeks eşleştirmek güvenli.
async fn persist(store: &Store, alerts: &[Alert], onceki: &[Calisma]) {
    for (a, eski) in alerts.iter().zip(onceki) {
        if calisma(a) == *eski {
            continue; // dokunulmadı
        }
        if let Err(e) = store.update_runtime(a).await {
            // Kullanıcı aynı anda iptal edip silmiş olabilir; ölümcül değil.
            warn!(id = a.id.as_str(), "durum yazılamadı: {e}");
            continue;
        }
        let _ = store
            .audit(
                &a.id,
                "state",
                &serde_json::json!({
                    "state": a.state,
                    "entry_oid": a.entry_oid,
                    "fill_deadline_ms": a.fill_deadline_ms,
                }),
            )
            .await;
    }
}

/// Turun sonucunu günlüğe düş. Feed hataları ayrıca uyarı seviyesinde —
/// boş değilse o tur eksik değerlendirilmiş demektir.
fn ozetle(tick: &Tick) {
    if !tick.fired.is_empty()
        || !tick.missed.is_empty()
        || !tick.working.is_empty()
        || !tick.invalidated.is_empty()
    {
        info!(
            fired = tick.fired.len(),
            missed = tick.missed.len(),
            working = tick.working.len(),
            invalidated = tick.invalidated.len(),
            "tur tamam"
        );
    }
    for e in &tick.feed_errors {
        warn!("feed hatası: {e}");
    }
}

/// Şimdi (unix ms).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
