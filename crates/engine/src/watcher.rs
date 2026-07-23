//! Watcher döngüsü: besle → değerlendir → gönder → yorumla.
//!
//! Zincire gömülemeyen alarmları bu döngü yaşatıyor. Her tur:
//!
//! 1. Silahlı alarmların ihtiyaç duyduğu feed'ler toplanır (aynı sembol/periyot
//!    yüz alarmda geçse bile bir kez çekilir).
//! 2. Taze mumlar çekilir, devam eden mum elenir, kapanış snapshot'a yazılır.
//! 3. Her alarm kendi kurulma anına göre değerlendirilir.
//! 4. `Some(true)` olanlar gönderilir, yanıt yorumlanır, durum güncellenir.
//!
//! # Hata varsa ateşleme
//!
//! Bir feed çekilemezse o tur atlanıyor — snapshot'a dokunulmuyor. Eski değeri
//! silmek de, yenisini uydurmak da yanlış olurdu: birinde alarm sonsuza dek
//! uyur, diğerinde yanlış fiyatla ateşler. Snapshot olduğu gibi kalıyor ve
//! bir sonraki tur yeniden deneniyor. Ağ hatası alarm kaybettirmemeli.
//!
//! # Geçmişe ateşlememeyi neden feed yapmıyor
//!
//! `pusu-feed`'de bir zamanlar `CandleTracker` vardı; bu korumayı gördüğü
//! **ilk mumu yutarak** yapıyordu. Snapshot'a tazelik kapısı
//! ([`Snapshot::close_after`]) girdikten sonra hem gereksiz hem zararlı hale
//! geldi: watcher 10:30'da ayağa kalkarsa 11:00 kapanışını "ilk gözlem" diye
//! yutar ve kullanıcı bir sonraki saati beklerdi — yani tam da kaçırmak
//! istemediği kapanışı kaçırırdı.
//!
//! Kapı aynı korumayı **alarm başına** ve isabetle veriyor: mum, o alarmın
//! kurulmasından sonra kapandıysa geçerli. Tracker'ın ikinci işi olan
//! monotonluk koruması [`Snapshot::set_close`]'a taşındı.

use crate::outcome::{interpret, Outcome};
use crate::snapshot::{evaluate, evidence, Evidence, Snapshot};
use pusu_core::{Alert, AlertAction, AlertId, AlertState, Condition, Interval, Symbol};
use pusu_feed::{last_closed, KlineSource, MarkSource, OrderSource};
use std::collections::HashSet;

/// Ön-imzalı tx'leri borsaya gönderen taraf.
///
/// Trait olmasının sebebi sadece test değil: watcher'ın **imza yetkisi yok**.
/// Elindeki blob'lar kullanıcının tarayıcıda imzaladığı, değiştirilemez
/// paketler. Watcher yalnızca "şimdi gönder" diyebiliyor.
#[allow(async_fn_in_trait)]
pub trait Dispatch {
    /// Alarmın ön-imzalı giriş tx'ini gönder, borsanın ham yanıtını döndür.
    async fn submit(&self, alert: &Alert) -> Result<serde_json::Value, DispatchError>;

    /// Alarmın ön-imzalı **iptal** tx'ini gönder.
    ///
    /// Bu da kullanıcının imzası. Mümkün olmasının sebebi `oid`'in gönderim
    /// öncesi hesaplanabilmesi (§8.9): `oid = SHA256(seqno ‖ bincode(action) ‖
    /// account ‖ nonce)`. Böylece kullanıcı girişi imzalarken iptalini de
    /// imzalıyor; watcher sadece zamanı geldiğinde postalıyor.
    ///
    /// Alternatifler kabul edilemezdi: sunucuya imza yetkisi vermek custody
    /// olurdu, `cxa` (cancel-all) ise dolmuş bir pozisyonun bracket'ini de
    /// öldürürdü.
    async fn cancel(&self, alert: &Alert) -> Result<serde_json::Value, DispatchError>;
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("gönderim başarısız: {0}")]
    Network(String),
    #[error("bu alarm için ön-imzalı tx yok")]
    NoBlob,
}

/// Bir turda bir alarma ne olduğu.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub id: AlertId,
    pub outcome: Outcome,
}

/// Watcher'ın bir turda yaptıkları.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tick {
    /// Gönderilen alarmlar ve sonuçları.
    pub fired: Vec<Report>,
    /// Kaçırıldı — kullanıcıya "hâlâ istiyor musun?" diye sorulacak.
    ///
    /// İki yoldan doluyor: koşul çok geç görüldü (watcher düşmüştü), ya da
    /// limit giriş bir periyot boyunca dolmadı (retest gelmedi).
    pub missed: Vec<AlertId>,
    /// Limit giriş deftere kondu, dolum bekleniyor.
    pub working: Vec<AlertId>,
    /// İptal koşulu sağlandı — setup bozuldu, alarm düşürüldü.
    pub invalidated: Vec<AlertId>,
    /// Kullanıcı defterdeki girişi iptal etti — ön-imzalı `cx` gönderildi.
    pub cancelled: Vec<AlertId>,
    /// Bildirim alarmı ateşledi — koşul tuttu, kullanıcıya haber verilecek.
    ///
    /// Trade değil: emir yok, imza yok. Engine yalnızca ateşleyeni *raporlar*;
    /// teslimi (in-app/e-posta/Telegram) node yapıyor — engine saf kalıyor.
    pub notified: Vec<AlertId>,
    /// Çekilemeyen feed'ler. Boş değilse bu tur eksik değerlendirildi.
    pub feed_errors: Vec<String>,
}

pub struct Watcher<K, M, O, D> {
    klines: K,
    marks: M,
    orders: O,
    dispatch: D,
    snapshot: Snapshot,
}

impl<K: KlineSource, M: MarkSource, O: OrderSource, D: Dispatch> Watcher<K, M, O, D> {
    pub fn new(klines: K, marks: M, orders: O, dispatch: D) -> Self {
        Self {
            klines,
            marks,
            orders,
            dispatch,
            snapshot: Snapshot::default(),
        }
    }

    /// Bir tur çalıştır. `alerts` yerinde güncellenir.
    pub async fn tick(&mut self, alerts: &mut [Alert], now_ms: u64) -> Tick {
        let mut tick = Tick::default();
        self.refresh(alerts, now_ms, &mut tick).await;
        self.fire(alerts, now_ms, &mut tick).await;
        self.track(alerts, now_ms, &mut tick).await;
        tick
    }

    /// Silahlı alarmların ihtiyacı olan veriyi çek, snapshot'ı güncelle.
    async fn refresh(&mut self, alerts: &[Alert], now_ms: u64, tick: &mut Tick) {
        let (candles, marks) = ihtiyaclar(alerts);

        for (symbol, interval) in candles {
            let ks = match self.klines.fresh_klines(&symbol, interval).await {
                Ok(ks) => ks,
                Err(e) => {
                    // Snapshot'a dokunmuyoruz — bkz. modül dokümanı.
                    tick.feed_errors.push(format!(
                        "{} {}: {e}",
                        symbol.as_str(),
                        interval.as_wire()
                    ));
                    continue;
                }
            };

            // Devam eden mumu ele. Snapshot geriye gitmeyi kendisi reddediyor,
            // aynı mumu tekrar yazmak da zararsız.
            if let Some(k) = last_closed(&ks, now_ms) {
                self.snapshot
                    .set_close(&symbol, interval, k.close, k.close_time);
            }
        }

        for symbol in marks {
            match self.marks.mark(&symbol).await {
                Ok(px) => self.snapshot.set_mark(&symbol, px),
                Err(e) => tick
                    .feed_errors
                    .push(format!("{} mark: {e}", symbol.as_str())),
            }
        }
    }

    /// Koşulu tutan alarmları gönder.
    async fn fire(&mut self, alerts: &mut [Alert], now_ms: u64, tick: &mut Tick) {
        for alert in alerts.iter_mut() {
            if alert.state != AlertState::Armed {
                continue;
            }

            // İptal koşulu giriş koşulundan ÖNCE bakılıyor. İkisi aynı turda
            // sağlanırsa iptal kazanır: "10'un üstünde kapatırsa al, 9'un
            // altına düşerse iptal et" diyen kullanıcı için mum 10.5'ten
            // kapanıp fiyat 8.9'a çakıldıysa, setup ölmüş demektir. Kapanışa
            // bakıp long'a girmek, kullanıcının iptal koşuluyla korunmak
            // istediği şeyin ta kendisi olurdu.
            if let Some(inv) = &alert.invalidate {
                if evaluate(inv, &self.snapshot, alert.armed_at_ms) == Some(true) {
                    alert.state = AlertState::Cancelled;
                    tick.invalidated.push(alert.id.clone());
                    continue;
                }
            }

            // Yalnızca Some(true). None ("bilmiyorum") ateşlemez.
            if evaluate(&alert.condition, &self.snapshot, alert.armed_at_ms) != Some(true) {
                continue;
            }

            // Koşul tutuyor ama kanıtı çok mu eski? Watcher saatlerce düşüp
            // geri geldiyse kural hâlâ sağlanıyor olabilir; piyasa çoktan
            // başka yerde. Kullanıcı adına o fiyata girmiyoruz — soruyoruz.
            let bayat = evidence(&alert.condition, &self.snapshot, alert.armed_at_ms)
                .is_some_and(|ev| ev.is_stale(now_ms));
            if bayat {
                alert.state = AlertState::Missed;
                tick.missed.push(alert.id.clone());
                continue;
            }

            // Bildirim alarmı: emir yok, imza yok — koşul tuttu, haber ver ve bitir.
            // Dispatch emir-odaklı (ön-imzalı blob bekler); Notify'ı oraya sokmak
            // NoBlob → Uncertain'a düşürür (kullanıcı kayıp bir işlem sanır). Tek
            // atışlık: Fired terminal, tekrar ateşlemez.
            if matches!(alert.action, AlertAction::Notify) {
                alert.state = AlertState::Fired;
                tick.notified.push(alert.id.clone());
                continue;
            }

            let outcome = match self.dispatch.submit(alert).await {
                Ok(raw) => interpret(&raw),
                // Yanıt gelmedi. Emrin geçmediğini varsayamayız — istek borsaya
                // ulaşıp cevabı yolda kaybolmuş olabilir.
                Err(e) => Outcome::Unknown { raw: e.to_string() },
            };

            // Durum alarmın yaşam döngüsü, işlemin akıbeti değil: emir kabul
            // edildiyse alarm işini yapmıştır. İşlemin ne olduğu (doldu mu,
            // koruma kuruldu mu) rapordaki `outcome`'da duruyor.
            alert.state = match &outcome {
                Outcome::Filled { .. } | Outcome::FilledUnprotected { .. } => AlertState::Fired,

                // Limit giriş deftere kondu ama dolmadı: iş bitmedi.
                // Kullanıcı retest bekliyor; gelmezse haber vereceğiz.
                Outcome::Resting if limit_giris(alert) => {
                    alert.fill_deadline_ms = fill_deadline(alert, &self.snapshot, now_ms);
                    tick.working.push(alert.id.clone());
                    AlertState::Working
                }
                // Market emri "resting" dönerse tuhaf ama emir kabul edilmiş.
                Outcome::Resting => AlertState::Fired,

                Outcome::Rejected { .. } => AlertState::Rejected,
                // Ne olduğunu bilmiyoruz. Fired demek uydurma, Armed bırakmak
                // ise bir sonraki turda aynı emri tekrar gönderir — kullanıcı
                // aynı işleme iki kez girer. Nihai sayıp insana bırakıyoruz.
                Outcome::Unknown { .. } => AlertState::Uncertain,
            };

            tick.fired.push(Report {
                id: alert.id.clone(),
                outcome,
            });
        }
    }

    /// Defterde bekleyen limit girişleri izle.
    ///
    /// Kullanıcının kuralı: *"retest gelip emrimi alabilir; gelmez de hacimli
    /// giderse 15m sonra bana sor."* Burası o sorunun sorulduğu yer.
    async fn track(&mut self, alerts: &mut [Alert], now_ms: u64, tick: &mut Tick) {
        for alert in alerts.iter_mut() {
            if alert.state != AlertState::Working {
                continue;
            }
            let Some(oid) = alert.entry_oid.clone() else {
                // İzleyemediğimiz bir emri "dolmadı" sayıp iptal edemeyiz.
                tick.feed_errors
                    .push(format!("{}: entry_oid yok, izlenemiyor", alert.id.as_str()));
                continue;
            };

            let acik = match self.orders.open_orders(&alert.account).await {
                Ok(os) => os,
                Err(e) => {
                    // Emirleri göremiyorsak karar veremeyiz. Dolmadı sanıp
                    // iptal etmek, dolmuş bir pozisyonu korumasız bırakırdı.
                    tick.feed_errors
                        .push(format!("{} açık emirler: {e}", alert.account));
                    continue;
                }
            };

            match acik.iter().find(|o| o.oid == oid) {
                // Defterde yok → doldu (ya da kullanıcı kendi iptal etti).
                // Her iki halde de bizim yapacağımız bir şey kalmadı.
                None => alert.state = AlertState::Fired,

                // Kısmen dolmuş: emir "alınmış". Süre dolsa bile iptal
                // etmiyoruz — pozisyon var ve korumaları kurulu.
                Some(o) if !o.untouched() => alert.state = AlertState::Fired,

                // Hiç dolmamış. İptali gerektiren bir sebep var mı?
                Some(_) => {
                    // İki tetik: süre doldu (retest gelmedi) ya da kullanıcı
                    // açıkça iptal istedi. Kullanıcı isteği önceliklidir —
                    // ikisi birden olsa bile sonuç Cancelled, Missed değil.
                    let istendi = alert.cancel_requested;
                    let suresi_doldu = alert.fill_deadline_ms.is_some_and(|d| now_ms > d);
                    if !istendi && !suresi_doldu {
                        continue; // retest hâlâ gelebilir
                    }
                    // Ön-imzalı iptali gönder, sonra durumu yaz. Sıralama önemli:
                    // önce emri geri çekiyoruz ki kullanıcı cevabını düşünürken
                    // beklenmedik bir dolum yaşamasın.
                    match self.dispatch.cancel(alert).await {
                        Ok(_) => {
                            if istendi {
                                alert.state = AlertState::Cancelled;
                                tick.cancelled.push(alert.id.clone());
                            } else {
                                alert.state = AlertState::Missed;
                                tick.missed.push(alert.id.clone());
                            }
                        }
                        Err(e) => {
                            // İptal gitmediyse emir hâlâ canlı. Cancelled/Missed
                            // demek yalan olur: kullanıcı emrin çekildiğini sanır,
                            // oysa dolabilir. Working kalıyor, tekrar denenecek.
                            tick.feed_errors
                                .push(format!("{}: iptal gönderilemedi: {e}", alert.id.as_str()));
                        }
                    }
                }
            }
        }
    }

    /// Snapshot'a dışarıdan bakmak isteyenler için (sağlık ucu, testler).
    pub const fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
}

fn limit_giris(alert: &Alert) -> bool {
    matches!(&alert.action, AlertAction::Trade(s) if s.entry.is_limit())
}

/// Limit girişin dolması için son an.
///
/// Pencere koşulun periyodu: kullanıcı "15m'de kapatınca gir" dediyse 15
/// dakika, "saatlikte" dediyse 1 saat. Kanıtın penceresini yeniden
/// kullanıyoruz — aynı soru: bu kuralın geçerlilik süresi ne?
///
/// Sayaç **gönderim anından** başlıyor, mumun kapanışından değil: kullanıcı
/// emrin girmesinden itibaren bir periyot bekliyor.
///
/// `None` = süresiz. Mark tabanlı koşulda periyot kavramı yok; limit normal
/// bir GTC emri gibi bekler.
fn fill_deadline(alert: &Alert, snap: &Snapshot, now_ms: u64) -> Option<u64> {
    match evidence(&alert.condition, snap, alert.armed_at_ms) {
        Some(Evidence::At { window_ms, .. }) => Some(now_ms + window_ms),
        _ => None,
    }
}

/// Silahlı alarmların hangi feed'lere ihtiyacı var?
///
/// Tekilleştirme burada: yüz kullanıcı "BTC 1h" alarmı kurduysa bir kez
/// çekiyoruz.
fn ihtiyaclar(alerts: &[Alert]) -> (HashSet<(Symbol, Interval)>, HashSet<Symbol>) {
    let mut candles = HashSet::new();
    let mut marks = HashSet::new();
    for a in alerts.iter().filter(|a| a.state.is_live()) {
        topla(&a.condition, &mut candles, &mut marks);
        // İptal koşulunun feed'i unutulursa iptal hiç değerlendirilemez ve
        // sessizce çalışmaz — kullanıcı korunduğunu sanır.
        if let Some(inv) = &a.invalidate {
            topla(inv, &mut candles, &mut marks);
        }
    }
    (candles, marks)
}

fn topla(c: &Condition, candles: &mut HashSet<(Symbol, Interval)>, marks: &mut HashSet<Symbol>) {
    match c {
        Condition::CandleClose {
            symbol, interval, ..
        } => {
            candles.insert((symbol.clone(), *interval));
        }
        Condition::MarkCross { symbol, .. } => {
            marks.insert(symbol.clone());
        }
        Condition::All(inner) | Condition::Any(inner) => {
            for c in inner {
                topla(c, candles, marks);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pusu_core::{AlertAction, Cross, Entry, Side, TradeSpec};
    use std::cell::RefCell;

    // -- sahte altyapı ------------------------------------------------------

    struct SahteKline(RefCell<Vec<pusu_feed::Kline>>);
    impl KlineSource for SahteKline {
        async fn klines(
            &self,
            _s: &Symbol,
            _i: Interval,
            _since: Option<u64>,
        ) -> Result<Vec<pusu_feed::Kline>, pusu_feed::FeedError> {
            Ok(self.0.borrow().clone())
        }
    }

    struct PatlayanKline;
    impl KlineSource for PatlayanKline {
        async fn klines(
            &self,
            _s: &Symbol,
            _i: Interval,
            _since: Option<u64>,
        ) -> Result<Vec<pusu_feed::Kline>, pusu_feed::FeedError> {
            Err(pusu_feed::FeedError::Decode("ağ öldü".into()))
        }
    }

    struct SahteMark(f64);
    impl MarkSource for SahteMark {
        async fn mark(&self, _s: &Symbol) -> Result<f64, pusu_feed::FeedError> {
            Ok(self.0)
        }
    }

    /// Gönderilenleri sayan sahte borsa.
    struct SahteBorsa {
        yanit: serde_json::Value,
        gonderilen: RefCell<Vec<AlertId>>,
        iptaller: RefCell<Vec<AlertId>>,
        iptal_patlar: bool,
    }
    impl SahteBorsa {
        fn new(yanit: serde_json::Value) -> Self {
            Self {
                yanit,
                gonderilen: RefCell::new(vec![]),
                iptaller: RefCell::new(vec![]),
                iptal_patlar: false,
            }
        }
        fn iptal_patlayan(yanit: serde_json::Value) -> Self {
            Self {
                iptal_patlar: true,
                ..Self::new(yanit)
            }
        }
        fn sayi(&self) -> usize {
            self.gonderilen.borrow().len()
        }
        fn iptal_sayisi(&self) -> usize {
            self.iptaller.borrow().len()
        }
    }
    impl Dispatch for SahteBorsa {
        async fn submit(&self, alert: &Alert) -> Result<serde_json::Value, DispatchError> {
            self.gonderilen.borrow_mut().push(alert.id.clone());
            Ok(self.yanit.clone())
        }
        async fn cancel(&self, alert: &Alert) -> Result<serde_json::Value, DispatchError> {
            if self.iptal_patlar {
                return Err(DispatchError::Network("iptal gitmedi".into()));
            }
            self.iptaller.borrow_mut().push(alert.id.clone());
            Ok(
                serde_json::json!({"status":"ok","response":{"data":{"statuses":[
                    {"cancelled":{"oid":"x"}}
                ]}}}),
            )
        }
    }

    /// Defterde duran emirleri taklit eder.
    #[derive(Default)]
    struct SahteEmirler(RefCell<Vec<pusu_feed::OpenOrder>>);
    impl SahteEmirler {
        fn duran(oid: &str, filled: f64) -> Self {
            Self(RefCell::new(vec![pusu_feed::OpenOrder {
                oid: oid.into(),
                symbol: "BTC-USD".into(),
                size: 0.01,
                filled,
                order_type: "limit".into(),
                reduce_only: false,
            }]))
        }
        /// Defter boş = emir doldu (ya da kullanıcı iptal etti).
        fn bos() -> Self {
            Self(RefCell::new(vec![]))
        }
    }
    impl OrderSource for SahteEmirler {
        async fn open_orders(
            &self,
            _a: &str,
        ) -> Result<Vec<pusu_feed::OpenOrder>, pusu_feed::FeedError> {
            Ok(self.0.borrow().clone())
        }
    }

    struct PatlayanEmirler;
    impl OrderSource for PatlayanEmirler {
        async fn open_orders(
            &self,
            _a: &str,
        ) -> Result<Vec<pusu_feed::OpenOrder>, pusu_feed::FeedError> {
            Err(pusu_feed::FeedError::Decode("hesap okunamadi".into()))
        }
    }

    fn dolu_yanit() -> serde_json::Value {
        serde_json::json!({"status":"ok","response":{"data":{"statuses":[
            {"filled":{"totalSz":0.01,"avgPx":90_500.0,"oid":"x"}}
        ]}}})
    }

    fn ret_yaniti() -> serde_json::Value {
        serde_json::json!({"status":"ok","response":{"data":{"statuses":[
            {"rejectedInvalid":{"oid":"x","reason":"insufficient margin"}}
        ]}}})
    }

    fn mum(close_time: u64, close: f64) -> pusu_feed::Kline {
        pusu_feed::Kline {
            open_time: close_time - 3_600_000,
            close_time,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
            num_trades: 1,
        }
    }

    fn alarm(armed_at_ms: u64) -> Alert {
        Alert {
            id: AlertId::new("a1"),
            owner: "master".into(),
            account: "sub".into(),
            invalidate: None,
            condition: Condition::CandleClose {
                symbol: "BTC-USD".into(),
                interval: Interval::H1,
                cross: Cross::Above,
                price: 90_000.0,
            },
            action: AlertAction::Trade(TradeSpec {
                symbol: "BTC-USD".into(),
                side: Side::Buy,
                size: 0.01,
                entry: Entry::Market,
                exits: None,
            }),
            state: AlertState::Armed,
            armed_at_ms,
            entry_oid: None,
            fill_deadline_ms: None,
            cancel_requested: false,
        }
    }

    // -- testler ------------------------------------------------------------

    #[tokio::test]
    async fn bildirim_alarmi_notified_uretir_emir_gondermez() {
        // Notify: koşul tutunca HABER VER, emir gönderme. Eski bug: Notify
        // dispatch'e düşüp NoBlob → Uncertain oluyordu (kullanıcı kayıp işlem sanır).
        let armed = 10_800_000;
        let kapanis = armed + 1_800_000; // alarmdan sonra kapanan mum
        let kaynak = SahteKline(RefCell::new(vec![mum(kapanis, 90_500.0)]));
        let borsa = SahteBorsa::new(dolu_yanit());
        let mut w = Watcher::new(kaynak, SahteMark(90_500.0), SahteEmirler::bos(), borsa);

        let mut alerts = vec![alarm(armed)];
        alerts[0].action = AlertAction::Notify;

        let t = w.tick(&mut alerts, kapanis + 60_000).await;

        assert_eq!(t.notified, vec![AlertId::new("a1")], "notified'a düşmeli");
        assert!(t.fired.is_empty(), "bildirim emir raporu üretmez");
        assert_eq!(alerts[0].state, AlertState::Fired, "tek atışlık, terminal");
    }

    #[tokio::test]
    async fn kullanicinin_senaryosu_uctan_uca() {
        // "Long alacağım ama saatlik kapanış 90 binin üstünde olmalı.
        //  Uykum var, izleyemiyorum." — alarm 10:30'da kuruldu.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![]));
        let borsa = SahteBorsa::new(dolu_yanit());
        let mut w = Watcher::new(kaynak, SahteMark(90_000.0), SahteEmirler::bos(), borsa);
        let mut alerts = vec![alarm(armed)];

        // 11:00 kapanışı henüz olmadı → hiçbir şey.
        let t = w.tick(&mut alerts, armed + 60_000).await;
        assert!(t.fired.is_empty());
        assert_eq!(alerts[0].state, AlertState::Armed);

        // 11:00'da mum 90.500'den kapandı → alarm ateşlemeli.
        w.klines
            .0
            .borrow_mut()
            .push(mum(armed + 1_800_000, 90_500.0));
        let t = w.tick(&mut alerts, armed + 1_900_000).await;

        assert_eq!(t.fired.len(), 1);
        assert!(t.fired[0].outcome.entered(), "işlem girmeliydi");
        assert_eq!(alerts[0].state, AlertState::Fired);
    }

    #[tokio::test]
    async fn ates_edilen_alarm_bir_daha_gonderilmez() {
        // En pahalı hata: aynı işleme iki kez girmek.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 90_500.0)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(90_000.0),
            SahteEmirler::bos(),
            SahteBorsa::new(dolu_yanit()),
        );
        let mut alerts = vec![alarm(armed)];

        w.tick(&mut alerts, armed + 2_000).await;
        assert_eq!(alerts[0].state, AlertState::Fired);

        // Koşul hâlâ sağlanıyor, mum hâlâ orada — ama alarm silahlı değil.
        let t = w.tick(&mut alerts, armed + 3_000).await;
        assert!(t.fired.is_empty());
        assert_eq!(w.dispatch.sayi(), 1, "yalnızca bir kez gönderilmeliydi");
    }

    #[tokio::test]
    async fn kurulmadan_once_kapanan_mumla_ateslenmez() {
        // Watcher saatlerdir çalışıyor ve elinde eşiği çoktan geçmiş bir
        // kapanış var. Kullanıcı şimdi alarm kuruyor: ateşlememeli, çünkü
        // kullanıcı bir SONRAKİ kapanışı bekliyor.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed - 1_800_000, 95_000.0)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::bos(),
            SahteBorsa::new(dolu_yanit()),
        );

        let mut alerts = vec![alarm(armed)];
        let t = w.tick(&mut alerts, armed + 1_000).await;
        assert!(t.fired.is_empty(), "bayat mumla ateşlendi");
        assert_eq!(alerts[0].state, AlertState::Armed);
    }

    #[tokio::test]
    async fn watcher_yeni_ayaga_kalktiysa_ilk_kapanisi_kacirmaz() {
        // Tracker'ın priming'i tam burada zarar veriyordu: watcher 10:30'da
        // başlar, 11:00 kapanışı gelir ve "ilk gözlem" diye yutulurdu.
        // Kullanıcı bir saat kaybederdi.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::bos(),
            SahteBorsa::new(dolu_yanit()),
        );
        let mut alerts = vec![alarm(armed)];

        // İlk tur: henüz kapanış yok.
        w.tick(&mut alerts, armed + 1_000).await;

        // İlk kapanış geldi — bu, watcher'ın gördüğü İLK mum. Ateşlemeli.
        w.klines
            .0
            .borrow_mut()
            .push(mum(armed + 1_800_000, 90_500.0));
        let t = w.tick(&mut alerts, armed + 1_900_000).await;
        assert_eq!(t.fired.len(), 1, "ilk gözlemdeki kapanış yutuldu");
        assert_eq!(alerts[0].state, AlertState::Fired);
    }

    #[tokio::test]
    async fn gecikmis_yanit_yanlis_ateslemez() {
        // Sunucu ara sıra eski mumu döndürüyor. 11:00 89k ile kapandı;
        // ardından gecikmiş yanıt 10:00'ı 95k olarak getiriyor.
        let armed = 9_000_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(11_000_000, 89_000.0)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::bos(),
            SahteBorsa::new(dolu_yanit()),
        );
        let mut alerts = vec![alarm(armed)];

        let t = w.tick(&mut alerts, 11_100_000).await;
        assert!(t.fired.is_empty(), "89k eşiği geçmiyor");

        // Gecikmiş yanıt: 10:00 kapanışı, eşiğin üstünde.
        *w.klines.0.borrow_mut() = vec![mum(10_000_000, 95_000.0)];
        let t = w.tick(&mut alerts, 11_200_000).await;
        assert!(t.fired.is_empty(), "gecikmiş mumla ateşlendi");
        assert_eq!(alerts[0].state, AlertState::Armed);
    }

    #[tokio::test]
    async fn feed_patlarsa_ateslenmez_ama_alarm_olmez() {
        let armed = 10_800_000;
        let mut w = Watcher::new(
            PatlayanKline,
            SahteMark(0.0),
            SahteEmirler::bos(),
            SahteBorsa::new(dolu_yanit()),
        );
        let mut alerts = vec![alarm(armed)];

        let t = w.tick(&mut alerts, armed + 1_000).await;
        assert!(t.fired.is_empty());
        assert_eq!(t.feed_errors.len(), 1, "hata rapor edilmeli");
        assert_eq!(alerts[0].state, AlertState::Armed, "alarm silahlı kalmalı");
    }

    // -- limit giriş + dolum deadline ---------------------------------------

    /// Kullanıcının kurgusu: "15m'de 10'un üstünde kapatırsa limit emrimi gir
    /// (retest'te dolsun); retest gelmez de dolmazsa 15 dk sonra bana sor."
    fn retest_alarmi(armed: u64) -> Alert {
        let mut a = alarm(armed);
        a.condition = Condition::CandleClose {
            symbol: "BTC-USD".into(),
            interval: Interval::M15,
            cross: Cross::Above,
            price: 10.0,
        };
        a.entry_oid = Some("giris-oid".into());
        if let AlertAction::Trade(s) = &mut a.action {
            s.entry = Entry::Limit { price: 9.8 };
        }
        a
    }

    fn resting_yanit() -> serde_json::Value {
        serde_json::json!({"status":"ok","response":{"data":{"statuses":[
            {"resting":{"oid":"giris-oid"}}
        ]}}})
    }

    #[tokio::test]
    async fn limit_giris_deftere_konunca_working_olur() {
        // Market emri gibi "bitti" demiyoruz: retest bekleniyor.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 10.5)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::duran("giris-oid", 0.0),
            SahteBorsa::new(resting_yanit()),
        );
        let mut alerts = vec![retest_alarmi(armed)];

        let t = w.tick(&mut alerts, armed + 2_000).await;
        assert_eq!(alerts[0].state, AlertState::Working);
        assert_eq!(t.working, vec![AlertId::new("a1")]);
        assert!(t.missed.is_empty(), "daha vakit var");
    }

    #[tokio::test]
    async fn deadline_periyottan_geliyor() {
        // 15m alarm → 15 dakika. Kullanıcının kuralı bu.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 10.5)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::duran("giris-oid", 0.0),
            SahteBorsa::new(resting_yanit()),
        );
        let mut alerts = vec![retest_alarmi(armed)];

        let simdi = armed + 2_000;
        w.tick(&mut alerts, simdi).await;
        assert_eq!(
            alerts[0].fill_deadline_ms,
            Some(simdi + 900_000),
            "gönderimden 15 dk sonrası"
        );
    }

    #[tokio::test]
    async fn retest_gelmezse_iptal_edilip_kullaniciya_soruluyor() {
        // Senaryonun asıl yarısı: hacimli gitti, emir dolmadı.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 10.5)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::duran("giris-oid", 0.0),
            SahteBorsa::new(resting_yanit()),
        );
        let mut alerts = vec![retest_alarmi(armed)];

        w.tick(&mut alerts, armed + 2_000).await;
        assert_eq!(alerts[0].state, AlertState::Working);

        // 15 dakika geçti, emir hâlâ dolmadı.
        let t = w.tick(&mut alerts, armed + 2_000 + 900_001).await;
        assert_eq!(alerts[0].state, AlertState::Missed);
        assert_eq!(t.missed, vec![AlertId::new("a1")]);
        assert_eq!(
            w.dispatch.iptal_sayisi(),
            1,
            "ön-imzalı iptal gönderilmeliydi"
        );
    }

    #[tokio::test]
    async fn retest_gelirse_islem_girer() {
        // Emir defterden kayboldu → doldu.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 10.5)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::duran("giris-oid", 0.0),
            SahteBorsa::new(resting_yanit()),
        );
        let mut alerts = vec![retest_alarmi(armed)];

        w.tick(&mut alerts, armed + 2_000).await;
        assert_eq!(alerts[0].state, AlertState::Working);

        *w.orders.0.borrow_mut() = vec![]; // retest geldi, emir doldu
        let t = w.tick(&mut alerts, armed + 3_000).await;
        assert_eq!(alerts[0].state, AlertState::Fired);
        assert!(t.missed.is_empty());
        assert_eq!(w.dispatch.iptal_sayisi(), 0, "dolan emir iptal edilmemeli");
    }

    #[tokio::test]
    async fn kismen_dolan_emir_sure_dolsa_bile_iptal_edilmez() {
        // Emir "alınmış": pozisyon var, korumaları kurulu. İptal etmek
        // korumasız bırakmaz ama kullanıcıyı boşuna rahatsız eder.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 10.5)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::duran("giris-oid", 0.004),
            SahteBorsa::new(resting_yanit()),
        );
        let mut alerts = vec![retest_alarmi(armed)];

        w.tick(&mut alerts, armed + 2_000).await;
        let t = w.tick(&mut alerts, armed + 2_000 + 900_001).await;

        assert_eq!(alerts[0].state, AlertState::Fired);
        assert!(t.missed.is_empty());
        assert_eq!(w.dispatch.iptal_sayisi(), 0);
    }

    #[tokio::test]
    async fn emirler_okunamazsa_karar_verilmez() {
        // Defteri göremiyorsak "dolmadı" sayıp iptal edemeyiz — dolmuş bir
        // pozisyonu korumasız bırakırdık.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 10.5)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            PatlayanEmirler,
            SahteBorsa::new(resting_yanit()),
        );
        let mut alerts = vec![retest_alarmi(armed)];

        w.tick(&mut alerts, armed + 2_000).await;
        let t = w.tick(&mut alerts, armed + 2_000 + 900_001).await;

        assert_eq!(alerts[0].state, AlertState::Working, "izlemeye devam");
        assert_eq!(w.dispatch.iptal_sayisi(), 0);
        assert!(!t.feed_errors.is_empty(), "hata rapor edilmeli");
    }

    #[tokio::test]
    async fn iptal_gonderilemezse_missed_denmez() {
        // İptal gitmediyse emir hâlâ canlı. "Kaçırdın" demek yalan olur:
        // kullanıcı emrin çekildiğini sanır, oysa dolabilir.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 10.5)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::duran("giris-oid", 0.0),
            SahteBorsa::iptal_patlayan(resting_yanit()),
        );
        let mut alerts = vec![retest_alarmi(armed)];

        w.tick(&mut alerts, armed + 2_000).await;
        let t = w.tick(&mut alerts, armed + 2_000 + 900_001).await;

        assert_eq!(alerts[0].state, AlertState::Working, "hâlâ canlı");
        assert!(t.missed.is_empty(), "iptal edilemedi ama kaçırıldı denildi");
        assert!(!t.feed_errors.is_empty());
    }

    #[tokio::test]
    async fn market_giris_working_olmaz() {
        // Market emri ya dolar ya reddedilir; beklemez.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 90_500.0)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::bos(),
            SahteBorsa::new(dolu_yanit()),
        );
        let mut alerts = vec![alarm(armed)];

        let t = w.tick(&mut alerts, armed + 2_000).await;
        assert_eq!(alerts[0].state, AlertState::Fired);
        assert!(t.working.is_empty());
    }

    // -- kullanıcı-tetikli iptal (working) ----------------------------------

    #[tokio::test]
    async fn kullanici_working_alarmi_iptal_edebiliyor() {
        // Kullanıcı defterde bekleyen girişi iptal etti: süre dolmasa da watcher
        // ön-imzalı cx'i gönderip Cancelled yapmalı — Missed değil.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 10.5)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::duran("giris-oid", 0.0),
            SahteBorsa::new(resting_yanit()),
        );
        let mut alerts = vec![retest_alarmi(armed)];

        w.tick(&mut alerts, armed + 2_000).await;
        assert_eq!(alerts[0].state, AlertState::Working);

        // Kullanıcı iptal istedi; süre daha dolmadı.
        alerts[0].cancel_requested = true;
        let t = w.tick(&mut alerts, armed + 3_000).await;

        assert_eq!(alerts[0].state, AlertState::Cancelled);
        assert_eq!(t.cancelled, vec![AlertId::new("a1")]);
        assert!(t.missed.is_empty(), "kullanıcı iptali Missed değil");
        assert_eq!(w.dispatch.iptal_sayisi(), 1, "ön-imzalı iptal gitmeliydi");
    }

    #[tokio::test]
    async fn iptal_istegi_gelse_de_dolan_emir_kazanir() {
        // Kullanıcı iptal istedi ama emir bu arada doldu: dolum kazanır.
        // Dolmuş bir pozisyonu "iptal edildi" saymak korumasız bırakırdı.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 10.5)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::duran("giris-oid", 0.0),
            SahteBorsa::new(resting_yanit()),
        );
        let mut alerts = vec![retest_alarmi(armed)];
        w.tick(&mut alerts, armed + 2_000).await;
        assert_eq!(alerts[0].state, AlertState::Working);

        // İptal istendi AMA emir defterden kayboldu (doldu).
        alerts[0].cancel_requested = true;
        *w.orders.0.borrow_mut() = vec![];
        let t = w.tick(&mut alerts, armed + 3_000).await;

        assert_eq!(
            alerts[0].state,
            AlertState::Fired,
            "dolum iptalden önce geldi"
        );
        assert!(t.cancelled.is_empty());
        assert_eq!(w.dispatch.iptal_sayisi(), 0, "dolan emir iptal edilmemeli");
    }

    // -- iptal koşulu -------------------------------------------------------

    /// Kullanıcının kendi kurgusu: "saatlik 10'un üstünde kapatırsa al;
    /// kıramazsa ve 9'un altına düşerse iptal et."
    fn setup_alarmi(armed: u64) -> Alert {
        let mut a = alarm(armed);
        a.condition = Condition::CandleClose {
            symbol: "BTC-USD".into(),
            interval: Interval::H1,
            cross: Cross::Above,
            price: 10.0,
        };
        a.invalidate = Some(Condition::MarkCross {
            symbol: "BTC-USD".into(),
            cross: Cross::Below,
            price: 9.0,
        });
        a
    }

    #[tokio::test]
    async fn setup_bozulursa_alarm_iptal_olur() {
        // Fiyat 9'un altına düştü: setup öldü, alarm düşmeli.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 9.5)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(8.9),
            SahteEmirler::bos(),
            SahteBorsa::new(dolu_yanit()),
        );
        let mut alerts = vec![setup_alarmi(armed)];

        let t = w.tick(&mut alerts, armed + 2_000).await;
        assert_eq!(t.invalidated, vec![AlertId::new("a1")]);
        assert_eq!(alerts[0].state, AlertState::Cancelled);
        assert_eq!(w.dispatch.sayi(), 0);
    }

    #[tokio::test]
    async fn iptal_kosulu_giris_kosulunu_yener() {
        // En ince hal: mum 10.5'ten kapandı (giriş sağlandı) AMA fiyat 8.9'a
        // çakılmış (iptal de sağlandı). Kapanışa bakıp long'a girmek,
        // kullanıcının iptal koşuluyla korunmak istediği şeyin ta kendisi.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 10.5)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(8.9),
            SahteEmirler::bos(),
            SahteBorsa::new(dolu_yanit()),
        );
        let mut alerts = vec![setup_alarmi(armed)];

        let t = w.tick(&mut alerts, armed + 2_000).await;
        assert!(t.fired.is_empty(), "setup ölmüşken işleme girildi");
        assert_eq!(alerts[0].state, AlertState::Cancelled);
        assert_eq!(w.dispatch.sayi(), 0, "borsaya gitmemeliydi");
    }

    #[tokio::test]
    async fn setup_ayaktayken_kosul_tutunca_normal_atesler() {
        // Fiyat 9'un üstünde kaldı, mum 10.5'ten kapandı → işlem girmeli.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 10.5)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(10.4),
            SahteEmirler::bos(),
            SahteBorsa::new(dolu_yanit()),
        );
        let mut alerts = vec![setup_alarmi(armed)];

        let t = w.tick(&mut alerts, armed + 2_000).await;
        assert_eq!(t.fired.len(), 1);
        assert!(t.invalidated.is_empty());
        assert_eq!(alerts[0].state, AlertState::Fired);
    }

    #[tokio::test]
    async fn iptal_kosulunun_feedi_de_cekiliyor() {
        // İptal koşulu mark istiyor ama giriş koşulu istemiyor. Feed toplama
        // iptali unutursa iptal sessizce hiç çalışmaz — kullanıcı korunduğunu
        // sanar.
        let a = setup_alarmi(0);
        let (candles, marks) = ihtiyaclar(&[a]);
        assert_eq!(candles.len(), 1, "giriş koşulunun mumu");
        assert_eq!(marks.len(), 1, "iptal koşulunun mark'ı çekilmiyor");
    }

    #[test]
    fn iptal_kosulu_olan_alarm_zincire_gomulemez() {
        // Koşul tek başına MarkCross olsa bile: borsaya bırakılan trigger
        // kendi kendini iptal edemez.
        let mut a = alarm(0);
        a.condition = Condition::MarkCross {
            symbol: "BTC-USD".into(),
            cross: Cross::Above,
            price: 10.0,
        };
        assert!(a.execution().is_onchain(), "iptalsiz hali zincirde");

        a.invalidate = Some(Condition::MarkCross {
            symbol: "BTC-USD".into(),
            cross: Cross::Below,
            price: 9.0,
        });
        assert!(!a.execution().is_onchain(), "iptalli hali watcher'da");
    }

    // -- bayatlık -----------------------------------------------------------

    #[tokio::test]
    async fn watcher_uzun_sure_dustuyse_gonderme_kullaniciya_sor() {
        // Kullanıcının kuralı: saatlik alarmda pencere 1 saat. Watcher 6 saat
        // düşüp geri geldiğinde kural hâlâ sağlanıyor ama piyasa çoktan başka
        // yerde — o fiyata market emriyle girmek kullanıcının alarmının değil
        // bizim gecikmemizin sonucu olurdu.
        let armed = 9 * 3_600_000;
        let kapanis = 10 * 3_600_000; // 10:00'da 90.500'den kapandı
        let kaynak = SahteKline(RefCell::new(vec![mum(kapanis, 90_500.0)]));
        let borsa = SahteBorsa::new(dolu_yanit());
        let mut w = Watcher::new(kaynak, SahteMark(0.0), SahteEmirler::bos(), borsa);
        let mut alerts = vec![alarm(armed)];

        // Watcher 16:00'da geri geliyor — kapanışın üstünden 6 saat geçmiş.
        let t = w.tick(&mut alerts, 16 * 3_600_000).await;

        assert!(t.fired.is_empty(), "bayat kapanışla emir gönderildi");
        assert_eq!(w.dispatch.sayi(), 0, "borsaya hiç gitmemeliydi");
        assert_eq!(t.missed, vec![AlertId::new("a1")]);
        assert_eq!(alerts[0].state, AlertState::Missed);
    }

    #[tokio::test]
    async fn pencere_icinde_gecikme_normal_ateseler() {
        // Watcher birkaç dakika gecikti — bu normal, alarm çalışmalı.
        let armed = 9 * 3_600_000;
        let kapanis = 10 * 3_600_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(kapanis, 90_500.0)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::bos(),
            SahteBorsa::new(dolu_yanit()),
        );
        let mut alerts = vec![alarm(armed)];

        let t = w.tick(&mut alerts, kapanis + 300_000).await; // 5 dk sonra
        assert_eq!(t.fired.len(), 1);
        assert!(t.missed.is_empty());
        assert_eq!(alerts[0].state, AlertState::Fired);
    }

    #[tokio::test]
    async fn kacirilan_alarm_tekrar_denenmez() {
        let armed = 9 * 3_600_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(10 * 3_600_000, 90_500.0)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::bos(),
            SahteBorsa::new(dolu_yanit()),
        );
        let mut alerts = vec![alarm(armed)];

        w.tick(&mut alerts, 16 * 3_600_000).await;
        assert_eq!(alerts[0].state, AlertState::Missed);

        // Kullanıcı cevap verene kadar alarm sessiz kalmalı.
        let t = w.tick(&mut alerts, 17 * 3_600_000).await;
        assert!(t.missed.is_empty(), "aynı alarm iki kez bildirildi");
        assert_eq!(w.dispatch.sayi(), 0);
    }

    #[tokio::test]
    async fn yanit_kaybolursa_alarm_belirsiz_isaretlenir_ve_tekrar_gonderilmez() {
        // Ürünün en pahalı hatası: aynı işleme iki kez girmek. İstek borsaya
        // ulaşıp yanıtı yolda kaybolmuş olabilir; "olmadı" varsayıp tekrar
        // göndermek kullanıcıyı çift pozisyona sokar.
        struct Kayip;
        impl Dispatch for Kayip {
            async fn submit(&self, _a: &Alert) -> Result<serde_json::Value, DispatchError> {
                Err(DispatchError::Network("timeout".into()))
            }
            async fn cancel(&self, _a: &Alert) -> Result<serde_json::Value, DispatchError> {
                Err(DispatchError::Network("timeout".into()))
            }
        }

        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 90_500.0)]));
        let mut w = Watcher::new(kaynak, SahteMark(0.0), SahteEmirler::bos(), Kayip);
        let mut alerts = vec![alarm(armed)];

        let t = w.tick(&mut alerts, armed + 2_000).await;
        assert_eq!(alerts[0].state, AlertState::Uncertain);
        assert!(t.fired[0].outcome.needs_attention(), "insan bakmalı");

        // Koşul hâlâ sağlanıyor ama alarm silahlı değil → ikinci gönderim yok.
        let t = w.tick(&mut alerts, armed + 3_000).await;
        assert!(t.fired.is_empty(), "belirsiz alarm tekrar gönderildi");
    }

    #[tokio::test]
    async fn korumasiz_kalan_pozisyon_raporlaniyor() {
        // Giriş doldu, bracket reddedildi. Alarm çalıştı (Fired) ama
        // kullanıcının korumasız pozisyonu var — rapor bunu taşımalı.
        let yanit = serde_json::json!({"status":"ok","response":{"data":{"statuses":[
            {"ack":{"ok":false,"message":"on_fill parent not found"}},
            {"filled":{"totalSz":0.01,"avgPx":90_500.0,"oid":"x"}}
        ]}}});
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 90_500.0)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::bos(),
            SahteBorsa::new(yanit),
        );
        let mut alerts = vec![alarm(armed)];

        let t = w.tick(&mut alerts, armed + 2_000).await;
        assert_eq!(alerts[0].state, AlertState::Fired, "pozisyon açıldı");
        assert!(
            t.fired[0].outcome.needs_attention(),
            "korumasız — haber ver"
        );
    }

    #[tokio::test]
    async fn borsa_reddederse_durum_rejected_olur() {
        // Alarm çalıştı ama işlem girmedi. "Fired" demek yalan olurdu.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 90_500.0)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::bos(),
            SahteBorsa::new(ret_yaniti()),
        );
        let mut alerts = vec![alarm(armed)];

        let t = w.tick(&mut alerts, armed + 2_000).await;
        assert_eq!(alerts[0].state, AlertState::Rejected);
        assert!(!t.fired[0].outcome.entered());
    }

    #[tokio::test]
    async fn esik_gecilmezse_ateslenmez() {
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 1_000, 89_900.0)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::bos(),
            SahteBorsa::new(dolu_yanit()),
        );
        let mut alerts = vec![alarm(armed)];

        let t = w.tick(&mut alerts, armed + 2_000).await;
        assert!(t.fired.is_empty());
        assert_eq!(
            alerts[0].state,
            AlertState::Armed,
            "sonraki kapanışı bekler"
        );
    }

    #[tokio::test]
    async fn devam_eden_mum_kapanmis_sayilmaz() {
        // close_time gelecekte → mum hâlâ oluşuyor. Erken ateşleme = kullanıcının
        // tam olarak kaçınmak istediği şey.
        let armed = 10_800_000;
        let kaynak = SahteKline(RefCell::new(vec![mum(armed + 3_600_000, 95_000.0)]));
        let mut w = Watcher::new(
            kaynak,
            SahteMark(0.0),
            SahteEmirler::bos(),
            SahteBorsa::new(dolu_yanit()),
        );
        let mut alerts = vec![alarm(armed)];

        let t = w.tick(&mut alerts, armed + 60_000).await;
        assert!(t.fired.is_empty(), "kapanmamış mumla ateşlendi");
    }

    #[test]
    fn ayni_feed_tekillestiriliyor() {
        // Yüz kullanıcı "BTC 1h" kurduysa bir kez çekilmeli.
        let alerts: Vec<Alert> = (0..100).map(|_| alarm(0)).collect();
        let (candles, marks) = ihtiyaclar(&alerts);
        assert_eq!(candles.len(), 1);
        assert!(marks.is_empty());
    }

    #[test]
    fn silahli_olmayan_alarm_feed_istemez() {
        let mut a = alarm(0);
        a.state = AlertState::Cancelled;
        let (candles, _) = ihtiyaclar(&[a]);
        assert!(
            candles.is_empty(),
            "iptal edilmiş alarm için veri çekilmemeli"
        );
    }

    #[test]
    fn bilesik_kosul_hem_mum_hem_mark_ister() {
        let mut a = alarm(0);
        a.condition = Condition::All(vec![
            Condition::CandleClose {
                symbol: "BTC-USD".into(),
                interval: Interval::H1,
                cross: Cross::Above,
                price: 90_000.0,
            },
            Condition::MarkCross {
                symbol: "ETH-USD".into(),
                cross: Cross::Below,
                price: 3_000.0,
            },
        ]);
        let (candles, marks) = ihtiyaclar(&[a]);
        assert_eq!(candles.len(), 1);
        assert_eq!(marks.len(), 1, "mark bacağı beslenmezse alarm sonsuza uyur");
    }
}
