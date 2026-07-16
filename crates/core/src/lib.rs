//! PUSU domain modeli.
//!
//! Alarmı kur, uyu. Fiyat geldiğinde işlem kendi kendine girer.
//!
//! # Ürünün ana ayrımı
//!
//! Alarmlar iki sınıfa ayrılır ve ayrım **güven** ayrımıdır:
//!
//! - 🔒 **Borsada yaşayan** — koşul mark price eşiğini kesiyor. BULK'ın `trig`
//!   basket'ine derlenir; kullanıcı bir kez imzalar, borsa yürütür. PUSU'nun
//!   sunucusu ölse bile emir çalışır.
//! - ⚡ **Watcher'da** — mum kapanışı, indikatör, çok koşullu. Zincirde
//!   ifade edilemez; PUSU izler ve ön-imzalı tx'i gönderir.
//!
//! Kararı [`Condition::execution`] veriyor ve yalnızca "hangisi" değil
//! **"neden"** de döndürüyor — o gerekçe kullanıcıya gösterilir. Ayrımı
//! gizlemek yerine ürünün merkezine koyuyoruz: kullanıcı hangi alarmın
//! neye bağlı olduğunu bilmeli, çünkü parası söz konusu.
//!
//! # Güvenlik duruşu
//!
//! PUSU **imzalama yetkisi tutmaz**. Kullanıcı emri tarayıcısında imzalar;
//! biz imzalı mesajı taşırız, üretmeyiz. İşlemler daima ayrı bir
//! sub-account'ta yapılır — sunucu sızsa bile zarar tavanı, kullanıcının
//! o hesaba ayırdığı miktardır.
//!
//! [`Condition::execution`]: condition::Condition::execution

pub mod alert;
pub mod condition;
pub mod market;

pub use alert::{
    Alert, AlertAction, AlertId, AlertState, Entry, ExitLeg, Exits, TradeSpec, BUILDER_FEE_BPS,
};
pub use condition::{Condition, Execution, WatchReason};
pub use market::{Cross, Interval, Side, Symbol};
