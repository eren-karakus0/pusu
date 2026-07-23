//! Doğal dil alarm derleyici.
//!
//! Bir cümleyi PUSU alarm taslağına çevirir. Taslak, mevcut [`pusu_compile`]
//! hattının beklediği anlamsal çekirdektir; oradan imzalı `OrderItem`'lara iner:
//!
//! ```text
//! "if the 1h candle closes above $90k, long 0.5 BTC, SL $88k"
//!         │  pusu_nl::parse
//!         ▼
//!   Draft { condition: CandleClose{1h, >90000}, action: Trade(Long 0.5 BTC),
//!           invalidate: None }
//!         │  Draft::into_alert(ctx)   →   pusu_compile::compile
//!         ▼
//!   Vec<OrderItem>  →  cüzdan imzası  →  blob
//! ```
//!
//! **Bu crate imzalamaz ve borsayla konuşmaz** — yalnızca metni yapıya çevirir.
//! Sürüm 1 İngilizce girdiyi hedefler; kelime tabloları ([`parse`] içinde)
//! tek yerde toplandığı için Türkçe eşanlamlılar sonradan eklenebilir.
//!
//! # Neden not döndürüyoruz
//!
//! Ürünün sözü "imzalamadan önce hangisi olduğunu görürsün." Derleyici de her
//! sessiz varsayımını ([`Note`]) ve alarmın **borsada mı yoksa watcher'da mı**
//! yürüyeceğini açıkça geri veriyor — kullanıcı ne imzaladığını bilsin diye.
//!
//! [`pusu_compile`]: https://docs.rs/pusu-compile

mod lex;
mod parse;

use pusu_core::{Alert, AlertAction, AlertId, AlertState, Condition, Execution, WatchReason};

/// Ayrıştırılmış alarm taslağı: alarmın anlamsal çekirdeği.
///
/// Runtime alanları (id, sahip, hesap, zaman) burada yok — onları
/// [`Draft::into_alert`] bağlamdan koyar.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Draft {
    pub condition: Condition,
    /// Setup'ı geçersiz kılan koşul (varsa). Sağlanırsa alarm düşer, emir girmez.
    pub invalidate: Option<Condition>,
    pub action: AlertAction,
}

/// Bir cümleyi taslağa çevir.
///
/// Başarılıysa taslağı **ve** derleyicinin verdiği notları döndürür (sessiz
/// varsayımlar + yürütme katmanı açıklaması).
///
/// # Örnek
/// ```
/// let parsed = pusu_nl::parse("if the 1h candle closes above $90k, long 0.5 BTC, SL $88k").unwrap();
/// assert!(matches!(parsed.draft.action, pusu_core::AlertAction::Trade(_)));
/// ```
pub fn parse(input: &str) -> Result<Parsed, ParseError> {
    let (raw, mut notes) = parse::run(input)?;
    let draft = Draft {
        condition: raw.condition,
        invalidate: raw.invalidate,
        action: raw.action,
    };
    notes.push(Note::routing(routing_text(&draft)));
    Ok(Parsed { draft, notes })
}

/// [`parse`] sonucu: taslak + insanın okuyacağı notlar.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub draft: Draft,
    pub notes: Vec<Note>,
}

/// Derleyicinin kullanıcıya gösterdiği bir not.
#[derive(Debug, Clone, PartialEq)]
pub enum Note {
    /// Bir girdiyi nasıl yorumladık (örn. "Read \"BTC\" as BTC-USD").
    Interpreted(String),
    /// Söylenmeyeni nasıl varsaydık (örn. "Assumed a market entry").
    Assumed(String),
    /// Alarmı kimin yürüteceği ve nedeni.
    Routing(String),
}

impl Note {
    pub(crate) fn interpreted(s: impl Into<String>) -> Self {
        Self::Interpreted(s.into())
    }
    pub(crate) fn assumed(s: impl Into<String>) -> Self {
        Self::Assumed(s.into())
    }
    pub(crate) fn routing(s: impl Into<String>) -> Self {
        Self::Routing(s.into())
    }

    /// Notun metni.
    pub fn text(&self) -> &str {
        match self {
            Self::Interpreted(s) | Self::Assumed(s) | Self::Routing(s) => s,
        }
    }

    /// UI'ın stillendirmesi için not türü.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Interpreted(_) => "interpreted",
            Self::Assumed(_) => "assumed",
            Self::Routing(_) => "routing",
        }
    }
}

/// Taslağı tam [`Alert`]'e çevirmek için runtime bağlamı.
pub struct AlertCtx {
    /// Çakışmayan alarm kimliği.
    pub id: String,
    /// Kullanıcının ana hesabı (builder onayı burada).
    pub owner: String,
    /// İşlemin gireceği sub-account (asla master).
    pub account: String,
    /// Alarmın kurulduğu an (unix ms).
    pub now_ms: u64,
}

impl Draft {
    /// Bu alarmı kim yürütecek? Kural [`Alert::execution`]'da; burada geçici bir
    /// alarm kurup ona soruyoruz — tek kaynaktan, kopyalamadan.
    pub fn classify(&self) -> Execution {
        self.as_alert(AlertCtx {
            id: "draft".into(),
            owner: String::new(),
            account: String::new(),
            now_ms: 0,
        })
        .execution()
    }

    /// Runtime bağlamıyla tam [`Alert`]'e çevir.
    pub fn into_alert(self, ctx: AlertCtx) -> Alert {
        self.as_alert(ctx)
    }

    fn as_alert(&self, ctx: AlertCtx) -> Alert {
        Alert {
            id: AlertId::new(ctx.id),
            owner: ctx.owner,
            account: ctx.account,
            condition: self.condition.clone(),
            invalidate: self.invalidate.clone(),
            action: self.action.clone(),
            state: AlertState::Armed,
            armed_at_ms: ctx.now_ms,
            entry_oid: None,
            fill_deadline_ms: None,
            cancel_requested: false,
        }
    }
}

/// Alarmın nerede yürüyeceğini İngilizce, kullanıcıya dönük tek cümleyle anlat.
fn routing_text(draft: &Draft) -> String {
    if matches!(draft.action, AlertAction::Notify) {
        return "PUSU watches this one — a heads-up isn't an order, so it can't live on the exchange."
            .into();
    }
    match draft.classify() {
        Execution::OnChain => {
            "Lives on the exchange — you sign once and BULK runs it, even if PUSU is offline."
        }
        Execution::Watched { reason } => match reason {
            WatchReason::CandleClose => {
                "PUSU watches this one — a candle close doesn't exist on-chain, so the watcher \
                 posts your pre-signed order at the close."
            }
            WatchReason::MultipleConditions => {
                "PUSU watches this one — the exchange can't track more than one condition together."
            }
            WatchReason::Invalidation => {
                "PUSU watches this one — an on-chain trigger can't cancel itself if your setup breaks."
            }
        },
    }
    .into()
}

/// Bir cümle taslağa çevrilemediğinde. Mesajlar İngilizce ve kullanıcıya dönük —
/// girdi İngilizce olduğu için hata da aynı dilde, düzeltme yolunu göstererek.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("Type an alert to get started.")]
    Empty,
    #[error("I couldn't find a trigger — try \"when BTC closes above $90k\".")]
    NoCondition,
    #[error("Say which way the price goes — \"above\" or \"below\".")]
    NoDirection,
    #[error("Your trigger needs a price, like \"above $90k\".")]
    NoPrice,
    #[error("More than one number could be the trigger price — try rephrasing.")]
    AmbiguousPrice,
    #[error("Which market? Add a symbol like BTC or ETH-USD.")]
    NoSymbol,
    #[error("Tell me what to do — buy, sell, or notify.")]
    NoAction,
    #[error("Pick one side — you wrote both buy and sell.")]
    ConflictingSides,
    #[error("How much? Add a size, like \"0.5 BTC\".")]
    MissingSize,
    #[error("I couldn't read the exit: {0}.")]
    InvalidExit(&'static str),
    #[error("Your stop and target are on the wrong sides — the order would trigger itself.")]
    IncoherentExits,
    #[error("Exit percentages must each be 0–100 and can't add up past 100%.")]
    InvalidPercentages,
    #[error(
        "Your cancel level is on the wrong side of the trigger — the alert would cancel instantly."
    )]
    InvalidateWrongSide,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pusu_core::{Cross, Interval, Side};

    #[test]
    fn tam_ornek_mum_kapanisi_long_sl() {
        let p = parse("if the 1H candle closes above $90k, long 0.5 BTC, SL $88k").unwrap();
        assert_eq!(
            p.draft.condition,
            Condition::CandleClose {
                symbol: "BTC-USD".into(),
                interval: Interval::H1,
                cross: Cross::Above,
                price: 90_000.0,
            }
        );
        let AlertAction::Trade(spec) = &p.draft.action else {
            panic!("trade bekleniyordu");
        };
        assert_eq!(spec.side, Side::Buy);
        assert_eq!(spec.size, 0.5);
        assert_eq!(spec.symbol.as_str(), "BTC-USD");
        let e = spec.exits.as_ref().unwrap();
        assert_eq!(e.stops.len(), 1);
        assert_eq!(e.stops[0].price, 88_000.0);
        // Mum kapanışı → watcher.
        assert!(matches!(p.draft.classify(), Execution::Watched { .. }));
    }

    #[test]
    fn mark_cross_onchain_yonlenir() {
        let p = parse("short 2 ETH when ETH breaks below $3,000").unwrap();
        assert_eq!(
            p.draft.condition,
            Condition::MarkCross {
                symbol: "ETH-USD".into(),
                cross: Cross::Below,
                price: 3_000.0,
            }
        );
        assert!(matches!(p.draft.classify(), Execution::OnChain));
    }

    #[test]
    fn iptalli_kademeli_cikis() {
        let p = parse(
            "if the 1h candle closes above $90,000, long 0.5 BTC, \
             cancel if price drops below $88,000, TP 30% at $95k and 70% at $98k, SL $88k",
        )
        .unwrap();
        assert_eq!(
            p.draft.invalidate,
            Some(Condition::MarkCross {
                symbol: "BTC-USD".into(),
                cross: Cross::Below,
                price: 88_000.0,
            })
        );
        let AlertAction::Trade(spec) = &p.draft.action else {
            panic!();
        };
        let e = spec.exits.as_ref().unwrap();
        assert_eq!(e.take_profits.len(), 2);
        assert_eq!(e.take_profits[0].pct, 30.0);
        assert_eq!(e.take_profits[1].price, 98_000.0);
        // İptal koşulu var → watcher (borsa kendini iptal edemez).
        assert!(matches!(
            p.draft.classify(),
            Execution::Watched {
                reason: WatchReason::Invalidation
            }
        ));
    }

    #[test]
    fn notify_watchera_duser() {
        let p = parse("notify me when the 4h candle closes below $2,800 on ETH").unwrap();
        assert_eq!(p.draft.action, AlertAction::Notify);
        assert!(p.notes.iter().any(|n| n.kind() == "routing"));
    }

    #[test]
    fn eksik_miktar_hatasi() {
        assert_eq!(
            parse("long BTC when it closes above $90k"),
            Err(ParseError::MissingSize)
        );
    }

    #[test]
    fn yonsuz_kosul_hatasi() {
        assert_eq!(
            parse("long 0.5 BTC when BTC hits $90k"),
            Err(ParseError::NoDirection)
        );
    }

    #[test]
    fn into_alert_runtime_alanlarini_koyar() {
        let p = parse("short 2 ETH when ETH breaks below $3,000").unwrap();
        let alert = p.draft.into_alert(AlertCtx {
            id: "a1".into(),
            owner: "master".into(),
            account: "sub".into(),
            now_ms: 1234,
        });
        assert_eq!(alert.id.as_str(), "a1");
        assert_eq!(alert.armed_at_ms, 1234);
        assert_eq!(alert.state, AlertState::Armed);
    }
}
