//! Besteci düğüm: store + feed + engine'i gerçek BULK REST'e bağlar.
//!
//! `pusu-engine` saf mantık — `Dispatch` ve feed trait'lerini soyut bırakıyor
//! ki testler ağa çıkmasın. Bu crate o trait'lerin gerçek ucunu takıyor:
//!
//! - [`HttpDispatch`] — store'daki ön-imzalı blob'u BULK'a postalar. Kritik
//!   sıra (niyeti postalamadan önce yaz) burada.
//! - [`reconcile`] — açılışta borsayla mutabakat. Çökme giriş gönderimi ile
//!   durum yazımı arasına düşmüşse, gerçeği `openOrders`'tan sorgular.
//!
//! İkisi birbirini tamamlıyor: `HttpDispatch` çatlağı **bırakabilir**
//! (niyet yazıldı, POST'tan önce çökme), `reconcile` onu açılışta **kapatır**.

mod dispatch;
mod reconcile;

pub use dispatch::HttpDispatch;
pub use reconcile::{reconcile, ReconcileError, Reconciled};
