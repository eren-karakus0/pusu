//! Alarm koşulları ve **hangi katmanın yürüteceğine karar veren sınıflandırıcı**.
//!
//! Bu modül PUSU'nun kalbi. Ayrım teknik değil, güven ayrımı:
//!
//! - Bazı koşullar BULK'ın `trig` basket'ine derlenebilir → **borsa** yürütür.
//!   Kullanıcı bir kez imzalar, PUSU'nun sunucusu ölse bile emir çalışır.
//! - Bazıları derlenemez (mum kapanışı, indikatör, çok koşullu) → **watcher**
//!   yürütür. Non-custodial ama bizim uptime'ımıza bağlı.
//!
//! Kararı kullanıcıdan gizlemiyoruz; ürünün merkezine koyuyoruz. Bu yüzden
//! sınıflandırıcı yalnızca "hangisi" değil, **"neden"** de döndürüyor —
//! o gerekçe doğrudan kullanıcıya gösterilecek metin.

use crate::market::{Cross, Interval, Symbol};
use serde::{Deserialize, Serialize};

/// Alarmın tetiklenme koşulu.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Condition {
    /// Mark price eşiği kesiyor.
    ///
    /// Tek `OnChain` koşul: BULK'ın `trig` basket'i tam olarak bunu yapıyor.
    MarkCross {
        symbol: Symbol,
        cross: Cross,
        price: f64,
    },

    /// Mum **kapanışı** eşiğin üstünde/altında.
    ///
    /// Zincirde karşılığı yok: `trig` mark price'ın eşiği *kesmesiyle* ateşler,
    /// mumun kapanmasıyla değil. Fiyat eşiğe bir saniye dokunup dönse `trig`
    /// yine ateşler — kullanıcının istediğinin tam tersi. Bu yüzden watcher şart.
    CandleClose {
        symbol: Symbol,
        interval: Interval,
        cross: Cross,
        price: f64,
    },

    /// Tüm alt koşullar sağlanmalı.
    All(Vec<Condition>),

    /// Alt koşullardan en az biri sağlanmalı.
    Any(Vec<Condition>),
}

/// Alarmı kimin yürüteceği.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "layer", rename_all = "snake_case")]
pub enum Execution {
    /// 🔒 Borsada yaşıyor. `trig` basket'ine derlenir; BULK yürütür.
    OnChain,

    /// ⚡ Watcher'da. PUSU değerlendirir, ön-imzalı tx'i gönderir.
    Watched { reason: WatchReason },
}

impl Execution {
    pub const fn is_onchain(&self) -> bool {
        matches!(self, Self::OnChain)
    }

    /// Kullanıcıya gösterilecek rozet metni.
    pub const fn badge(&self) -> &'static str {
        match self {
            Self::OnChain => "Borsada yaşıyor",
            Self::Watched { .. } => "Watcher'da",
        }
    }

    /// Kullanıcıya gösterilecek gerekçe. `OnChain` için gerekçe gerekmiyor —
    /// açıklanması gereken şey, bir şeyin *neden* bize bağımlı olduğu.
    pub const fn explain(&self) -> Option<&'static str> {
        match self {
            Self::OnChain => None,
            Self::Watched { reason } => Some(reason.explain()),
        }
    }
}

/// Bir koşulun neden zincire gömülemediği.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchReason {
    /// Mum kapanışı kavramı zincirde yok.
    CandleClose,
    /// `trig` tek eşik alıyor; çok koşullu mantık kuramıyor.
    MultipleConditions,
    /// Alarmın bir iptal koşulu var.
    Invalidation,
}

impl WatchReason {
    pub const fn explain(&self) -> &'static str {
        match self {
            Self::CandleClose => {
                "Mum kapanışı borsada yok — trigger fiyat eşiğe dokunduğu anda ateşler, \
                 kapanışı beklemez. Bu alarmı PUSU izliyor."
            }
            Self::MultipleConditions => {
                "Borsa tek eşik takip edebiliyor, birden fazla koşulu birlikte değil. \
                 Bu alarmı PUSU izliyor."
            }
            Self::Invalidation => {
                "Borsaya bırakılan bir trigger kendi kendini iptal edemez; setup \
                 bozulduğunda emri geri çekmek gerekiyor. Bu alarmı PUSU izliyor."
            }
        }
    }
}

impl Condition {
    /// Bu koşulu kim yürütecek?
    ///
    /// Tek kural: yalnızca sade bir `MarkCross` zincire gömülebilir.
    /// Kompozisyon (`All`/`Any`) tek elemanlı bile olsa `trig` tek eşik aldığı
    /// için elemanına indirgenerek değerlendirilir.
    pub fn execution(&self) -> Execution {
        match self {
            Self::MarkCross { .. } => Execution::OnChain,

            Self::CandleClose { .. } => Execution::Watched {
                reason: WatchReason::CandleClose,
            },

            Self::All(inner) | Self::Any(inner) => match inner.as_slice() {
                // Tek elemanlı kompozisyon anlamsız; elemanına indirgenir.
                [only] => only.execution(),
                _ => {
                    // İçinde mum kapanışı varsa gerekçe o — kullanıcı için daha
                    // açıklayıcı, çünkü asıl kısıt orada.
                    let reason = if inner.iter().any(Self::contains_candle_close) {
                        WatchReason::CandleClose
                    } else {
                        WatchReason::MultipleConditions
                    };
                    Execution::Watched { reason }
                }
            },
        }
    }

    fn contains_candle_close(&self) -> bool {
        match self {
            Self::CandleClose { .. } => true,
            Self::MarkCross { .. } => false,
            Self::All(inner) | Self::Any(inner) => inner.iter().any(Self::contains_candle_close),
        }
    }

    /// Bu koşulun dokunduğu tüm semboller. Watcher'ın hangi stream'lere
    /// abone olacağını buradan çıkarıyoruz.
    pub fn symbols(&self) -> Vec<&Symbol> {
        let mut out = Vec::new();
        self.collect_symbols(&mut out);
        out.sort();
        out.dedup();
        out
    }

    fn collect_symbols<'a>(&'a self, out: &mut Vec<&'a Symbol>) {
        match self {
            Self::MarkCross { symbol, .. } | Self::CandleClose { symbol, .. } => out.push(symbol),
            Self::All(inner) | Self::Any(inner) => {
                for c in inner {
                    c.collect_symbols(out);
                }
            }
        }
    }

    /// Kısa, insan-okur özet — bildirim metni ve UI için tek satır.
    ///
    /// Örn. `BTC-USD · mark > 90000`, `BTC-USD · 1-hour close < 88000`.
    pub fn summary(&self) -> String {
        fn px(p: f64) -> String {
            if p.fract() == 0.0 {
                format!("{p:.0}")
            } else {
                format!("{p}")
            }
        }
        fn arrow(c: Cross) -> &'static str {
            match c {
                Cross::Above => ">",
                Cross::Below => "<",
            }
        }
        match self {
            Self::MarkCross {
                symbol,
                cross,
                price,
            } => format!(
                "{} · mark {} {}",
                symbol.as_str(),
                arrow(*cross),
                px(*price)
            ),
            Self::CandleClose {
                symbol,
                interval,
                cross,
                price,
            } => format!(
                "{} · {} close {} {}",
                symbol.as_str(),
                interval.label(),
                arrow(*cross),
                px(*price)
            ),
            Self::All(_) => "multi-condition (all must hold)".to_string(),
            Self::Any(_) => "multi-condition (any must hold)".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(price: f64) -> Condition {
        Condition::MarkCross {
            symbol: "BTC-USD".into(),
            cross: Cross::Below,
            price,
        }
    }

    fn candle(interval: Interval) -> Condition {
        Condition::CandleClose {
            symbol: "BTC-USD".into(),
            interval,
            cross: Cross::Above,
            price: 90_000.0,
        }
    }

    #[test]
    fn mark_cross_zincire_gomulur() {
        assert_eq!(mark(88_400.0).execution(), Execution::OnChain);
    }

    #[test]
    fn mum_kapanisi_watcher_gerektirir() {
        // Kullanıcının kendi use case'i: "saatlik kapanış 90 binin üstünde olmalı".
        // Zincirde ifade edilemiyor; gerekçe kullanıcıya bu şekilde gösterilecek.
        let exec = candle(Interval::H1).execution();
        assert_eq!(
            exec,
            Execution::Watched {
                reason: WatchReason::CandleClose
            }
        );
        assert!(exec.explain().unwrap().contains("Mum kapanışı borsada yok"));
    }

    #[test]
    fn cok_kosullu_watcher_gerektirir() {
        let c = Condition::All(vec![mark(88_400.0), mark(95_000.0)]);
        assert_eq!(
            c.execution(),
            Execution::Watched {
                reason: WatchReason::MultipleConditions
            }
        );
    }

    #[test]
    fn tek_elemanli_kompozisyon_elemanina_indirgenir() {
        // All([MarkCross]) ile MarkCross aynı şey; kullanıcıyı gereksiz yere
        // watcher'a düşürmeyelim.
        assert_eq!(
            Condition::All(vec![mark(88_400.0)]).execution(),
            Execution::OnChain
        );
        assert_eq!(
            Condition::Any(vec![mark(88_400.0)]).execution(),
            Execution::OnChain
        );
    }

    #[test]
    fn karisik_kosulda_gerekce_mum_kapanisi_olur() {
        // Hem mark cross hem mum kapanışı varsa asıl kısıt mum kapanışı;
        // kullanıcıya "çok koşullu" demek yanıltıcı olur.
        let c = Condition::All(vec![mark(88_400.0), candle(Interval::H4)]);
        assert_eq!(
            c.execution(),
            Execution::Watched {
                reason: WatchReason::CandleClose
            }
        );
    }

    #[test]
    fn ic_ice_kompozisyonda_da_mum_kapanisi_bulunur() {
        let c = Condition::Any(vec![
            mark(88_400.0),
            Condition::All(vec![mark(90_000.0), candle(Interval::D1)]),
        ]);
        assert_eq!(
            c.execution(),
            Execution::Watched {
                reason: WatchReason::CandleClose
            }
        );
    }

    #[test]
    fn semboller_tekillestirilir() {
        let c = Condition::All(vec![
            mark(88_400.0),
            Condition::CandleClose {
                symbol: "ETH-USD".into(),
                interval: Interval::H1,
                cross: Cross::Above,
                price: 3000.0,
            },
            mark(90_000.0),
        ]);
        let syms = c.symbols();
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].as_str(), "BTC-USD");
        assert_eq!(syms[1].as_str(), "ETH-USD");
    }

    #[test]
    fn onchain_gerekce_dondurmez() {
        // Açıklanması gereken şey, bir alarmın neden BİZE bağımlı olduğu.
        assert_eq!(mark(88_400.0).execution().explain(), None);
    }

    #[test]
    fn ozet_okunur_tek_satir_verir() {
        assert_eq!(mark(88_400.0).summary(), "BTC-USD · mark < 88400");
        assert_eq!(
            candle(Interval::H1).summary(),
            "BTC-USD · hourly close > 90000"
        );
        // Ondalık koru, tam sayıda basma.
        let frac = Condition::MarkCross {
            symbol: "SOL-USD".into(),
            cross: Cross::Above,
            price: 198.5,
        };
        assert_eq!(frac.summary(), "SOL-USD · mark > 198.5");
    }
}
