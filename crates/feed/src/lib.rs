//! PUSU mum kapanışı feed'i.
//!
//! Watcher'ın yakıtı: "saatlik mum 90 binin üstünde kapandı" bilgisini
//! **doğru zamanda ve bir kez** üretmek.
//!
//! # Neden REST, neden WS değil
//!
//! WS candle aboneliği abonelik başına ~1,9 MB'lık 5.000 mumluk geçmiş
//! döküyor ve 1 MB'lık varsayılan frame limitini aşıp bağlantıyı kopartıyor.
//! Saatlik mumun kapanışını 2 saniye geç öğrenmek hiçbir şeyi değiştirmediği
//! için latency argümanı da yok. REST'te 1h serisi filtresiz 23 KB — filtreli
//! yanıt daha küçük ama **bayat** (~60 sn geriden geliyor), o yüzden
//! kullanmıyoruz. Ölçümler: [`source`] modülü.
//!
//! # İki sessiz hata kaynağı — ikisi de burada kapatılıyor
//!
//! 1. **Devam eden mum.** `/klines` bazen henüz kapanmamış mumu da döndürüyor.
//!    Onu kapanmış saymak, "saatlik kapanış" alarmını erken ateşler — yani
//!    kullanıcının kaçınmak istediği şeyin ta kendisi. → [`kline::last_closed`]
//!
//! 2. **Bayat veri.** `startTime` filtresi ucuz ama donuk yanıt veriyor
//!    (~60 sn geriden). → [`source::KlineSource::fresh_klines`] filtre kullanmaz.
//!
//! Üçüncü bir tehlike — **alarm kurulmadan önce kapanmış mumla ateşlemek** —
//! bir zamanlar burada bir `CandleTracker` ile çözülüyordu: gördüğü ilk mumu
//! yutuyordu. Bu, yeni ayağa kalkan bir watcher'ın ilk gerçek kapanışı
//! kaçırması demekti. Koruma `pusu-engine`'e, alarm başına çalışan bir tazelik
//! kapısına taşındı; burada artık öyle bir durum tutulmuyor.

pub mod kline;
pub mod mark;
pub mod orders;
pub mod source;

pub use kline::{last_closed, Kline};
pub use mark::{HttpMarkSource, MarkSource};
pub use orders::{HttpOrderSource, OpenOrder, OrderSource};
pub use source::{FeedError, HttpKlineSource, KlineSource};
