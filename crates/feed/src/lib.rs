//! PUSU mum kapanışı feed'i.
//!
//! Watcher'ın yakıtı: "saatlik mum 90 binin üstünde kapandı" bilgisini
//! **doğru zamanda ve bir kez** üretmek.
//!
//! # Neden REST, neden WS değil
//!
//! WS candle aboneliği abonelik başına ~1,9 MB'lık 5.000 mumluk geçmiş
//! döküyor ve 1 MB'lık varsayılan frame limitini aşıp bağlantıyı kopartıyor.
//! REST'te aynı bilgi `startTime` filtresiyle **142 byte**. Saatlik mumun
//! kapanışını 2 saniye geç öğrenmek hiçbir şeyi değiştirmediği için latency
//! argümanı yok. Detay: [`source`] modülü.
//!
//! # İki sessiz hata kaynağı — ikisi de burada kapatılıyor
//!
//! 1. **Devam eden mum.** `/klines` bazen henüz kapanmamış mumu da döndürüyor.
//!    Onu kapanmış saymak, "saatlik kapanış" alarmını erken ateşler — yani
//!    kullanıcının kaçınmak istediği şeyin ta kendisi. → [`kline::last_closed`]
//!
//! 2. **Geçmişe ateşleme.** İlk polling'de gelen "son kapanmış mum", alarm
//!    kurulmadan önce kapanmış olabilir. → [`tracker::CandleTracker`] ilk
//!    gözlemde yalnızca hizalanır, ateşlemez.

pub mod kline;
pub mod source;
pub mod tracker;

pub use kline::{last_closed, Kline};
pub use source::{FeedError, HttpKlineSource, KlineSource};
pub use tracker::CandleTracker;
