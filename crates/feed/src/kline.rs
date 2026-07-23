//! Mum verisi — tip artık `pusu-core`'da (tarayıcı arayüzü de kullanabilsin
//! diye; bkz. [`pusu_core::kline`]). Feed geriye dönük uyumluluk için re-export
//! ediyor: `pusu_feed::Kline` / `pusu_feed::last_closed` çalışmaya devam eder.

pub use pusu_core::kline::{last_closed, Kline};
