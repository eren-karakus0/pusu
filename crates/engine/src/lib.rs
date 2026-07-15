//! Watcher'ın beyni: koşulu değerlendirir, sonucu yorumlar.
//!
//! Zincire gömülemeyen alarmları (⚡ Watched) bu crate taşıyor. Sınıf ayrımı
//! [`pusu_core::Condition::execution`] tarafından yapılıyor; buraya yalnızca
//! bizim izlememiz gereken alarmlar düşüyor.
//!
//! # İki taraflı dürüstlük
//!
//! Bu crate iki soruya cevap veriyor ve ikisinde de "emin değilim" demeyi
//! bilmesi gerekiyor:
//!
//! 1. **Koşul sağlandı mı?** → [`evaluate`]. Eksik veriyle `None` döner;
//!    ateşleme kararı yalnızca `Some(true)` ile verilir.
//! 2. **Gönderdik, ne oldu?** → [`interpret`]. Borsa reddettiği emirde bile
//!    `{"status":"ok"}` döndüğü için yanıtın içi okunur.
//!
//! İkisinin ortak yanı: belirsizliği başarı sanmamak. Yanlış ateşleme
//! kullanıcının parasını istemediği işleme sokar; yanlış "başarılı" raporu ise
//! korumasız pozisyonu gizler.

mod outcome;
mod snapshot;
mod watcher;

pub use outcome::{interpret, Outcome};
pub use snapshot::{evaluate, Snapshot};
pub use watcher::{Dispatch, DispatchError, Report, Tick, Watcher};
