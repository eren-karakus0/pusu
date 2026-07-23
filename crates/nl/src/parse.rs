//! Cümleyi alarm taslağına çeviren asıl mantık.
//!
//! Yaklaşım tam bir gramer değil — bu alan sınırlı, kelime hazinesi küçük.
//! Cümleyi **anahtar kelimelerle cümleciklere** bölüyoruz (koşul, işlem, giriş,
//! kâr-al, zarar-durdur, iptal), sonra her cümleciği kendi içinde okuyoruz.
//!
//! Bölme, iki sinsi durumu doğru çözecek şekilde tasarlandı:
//!
//! - **Orta-iptal + sonda-çıkış:** "…long 0.5 BTC, cancel if …, TP …, SL …"
//!   İptal sona kadar yutulamaz; her anahtar kelime kendi cümleciğini açar.
//! - **"cancel if" içindeki "if":** `if`/`when` normalde koşul cümleciği açar,
//!   ama hemen bir iptal anahtarından (`cancel`/`invalidate`/`unless`) sonra
//!   geliyorsa yutulur — yoksa iptal boşalır, ikinci bir koşul uydurulurdu.

use crate::{Note, ParseError};
use pusu_core::{
    AlertAction, Condition, Cross, Entry, ExitLeg, Exits, Interval, Side, Symbol, TradeSpec,
};

/// Ayrıştırılmış taslak: alarmın anlamsal çekirdeği. Runtime alanları (id,
/// owner, account, zaman) burada yok — onları [`crate::Draft::into_alert`] koyar.
pub(crate) struct Raw {
    pub condition: Condition,
    pub invalidate: Option<Condition>,
    pub action: AlertAction,
}

/// Cümlecik türü. `Lead`, ilk anahtar kelimeden önceki (etiketlenmemiş) baş kısım.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Lead,
    Condition,
    Buy,
    Sell,
    Notify,
    Entry,
    Tp,
    Sl,
    Invalidate,
}

struct Clause {
    kind: Kind,
    toks: Vec<super::lex::Tok>,
}

use super::lex::Tok;

/// Küçük harf bir kelimeyi, cümlecik açan bir anahtar kelimeyse türüne eşle.
/// `prev`, hemen önceki kelime (iptal-içi `if` yutma kuralı için).
fn starter(word: &str, prev: Option<&str>) -> Option<Kind> {
    let after_inv = matches!(
        prev,
        Some("cancel" | "invalidate" | "invalidated" | "unless" | "void" | "abort" | "kill")
    );
    match word {
        "if" | "when" | "once" | "whenever" | "after" if !after_inv => Some(Kind::Condition),
        "long" | "buy" => Some(Kind::Buy),
        "short" | "sell" => Some(Kind::Sell),
        "notify" | "alert" | "tell" | "ping" => Some(Kind::Notify),
        "limit" | "retest" => Some(Kind::Entry),
        "tp" | "tp1" | "tp2" | "tp3" | "tp4" | "target" | "targets" | "take" | "take-profit"
        | "takeprofit" => Some(Kind::Tp),
        "sl" | "stop" | "stoploss" | "stop-loss" => Some(Kind::Sl),
        "cancel" | "invalidate" | "invalidated" | "unless" | "void" | "abort" | "kill" => {
            Some(Kind::Invalidate)
        }
        _ => None,
    }
}

/// Token'ları anahtar kelimelerle cümleciklere böl. Anahtar kelimenin kendisi
/// cümleciğin token'larına dâhil edilmez — yalnızca türü işaretler.
fn split_clauses(toks: &[Tok]) -> Vec<Clause> {
    let mut clauses = Vec::new();
    let mut cur = Clause {
        kind: Kind::Lead,
        toks: Vec::new(),
    };
    let mut prev_word: Option<String> = None;

    for t in toks {
        if let Tok::Word(w) = t {
            if let Some(kind) = starter(w, prev_word.as_deref()) {
                if !cur.toks.is_empty() || cur.kind != Kind::Lead {
                    clauses.push(cur);
                }
                cur = Clause {
                    kind,
                    toks: Vec::new(),
                };
                prev_word = Some(w.clone());
                continue;
            }
            prev_word = Some(w.clone());
        } else {
            prev_word = None;
        }
        cur.toks.push(t.clone());
    }
    if !cur.toks.is_empty() || cur.kind != Kind::Lead {
        clauses.push(cur);
    }
    clauses
}

// ---- kelime dağarcığı --------------------------------------------------------

/// Açıkça borsa tickerı olan, İngilizce kelimeyle karışma riski düşük semboller.
/// Kelimeyle çakışabilecekler (`near`, `link`, `dot`, `op`, `apt`) bilinçle
/// dışarıda — onlar `LINK-USD` gibi açık yazımla ya da `$` önekiyle girilir.
const KNOWN: &[&str] = &[
    "btc", "eth", "sol", "bnb", "xrp", "doge", "ada", "avax", "matic", "arb", "sui", "sei", "tia",
    "inj", "ltc", "bch", "ftm", "atom", "pepe", "wif", "bonk", "jto", "rndr", "ena", "ordi",
];

/// Bir kelimeyi `BASE-QUOTE` sembolüne çöz. Dönüş: (sembol, quote_varsayıldı_mı).
/// `$` öneki güçlü ticker sinyali — bilinmeyen tabanı bile kabul ederiz.
fn resolve_symbol(word: &str) -> Option<(String, bool)> {
    let had_dollar = word.starts_with('$');
    let w = word.strip_prefix('$').unwrap_or(word);
    if w.is_empty() {
        return None;
    }

    if let Some((base, quote)) = w.split_once('-') {
        if base.is_empty() || quote.is_empty() {
            return None;
        }
        return Some((
            format!("{}-{}", base.to_uppercase(), quote.to_uppercase()),
            false,
        ));
    }

    if had_dollar || KNOWN.contains(&w) {
        return Some((format!("{}-USD", w.to_uppercase()), true));
    }
    None
}

/// Cümlecik token'larından ilk tickerı çöz.
fn first_ticker(toks: &[Tok]) -> Option<(String, bool)> {
    toks.iter()
        .filter_map(|t| t.as_word())
        .find_map(resolve_symbol)
}

fn interval_alias(word: &str) -> Option<Interval> {
    use Interval::*;
    Some(match word {
        "10s" => S10,
        "1m" => M1,
        "3m" => M3,
        "5m" => M5,
        "15m" => M15,
        "30m" => M30,
        "1h" | "hourly" => H1,
        "2h" => H2,
        "4h" => H4,
        "6h" => H6,
        "8h" => H8,
        "12h" => H12,
        "1d" | "daily" => D1,
        "3d" => D3,
        "1w" | "weekly" => W1,
        "1mo" | "1month" | "monthly" => Mo1,
        _ => return None,
    })
}

/// "4 hour" / "15 min" gibi sayı+birim ikilisini interval'e çevirir.
fn unit_norm(word: &str) -> Option<&'static str> {
    Some(match word {
        "s" | "sec" | "secs" | "second" | "seconds" => "s",
        "m" | "min" | "mins" | "minute" | "minutes" => "m",
        "h" | "hr" | "hrs" | "hour" | "hours" => "h",
        "d" | "day" | "days" => "d",
        "w" | "wk" | "week" | "weeks" => "w",
        "mo" | "month" | "months" => "mo",
        _ => return None,
    })
}

/// Cümlecikte interval bul. Dönüş: (interval, tüketilen_sayı_indeksi).
/// Sayı+birim biçiminde ("4 hour") o sayı fiyat sanılmasın diye indeksi döner.
fn find_interval(toks: &[Tok]) -> Option<(Interval, Option<usize>)> {
    for t in toks {
        if let Some(iv) = t.as_word().and_then(interval_alias) {
            return Some((iv, None));
        }
    }
    for i in 0..toks.len().saturating_sub(1) {
        let (Tok::Num { val, .. }, Tok::Word(unit)) = (&toks[i], &toks[i + 1]) else {
            continue;
        };
        if val.fract() != 0.0 {
            continue;
        }
        if let Some(u) = unit_norm(unit) {
            let key = format!("{}{}", *val as i64, u);
            if let Some(iv) = interval_alias(&key) {
                return Some((iv, Some(i)));
            }
        }
    }
    None
}

const ABOVE: &[&str] = &[
    "above", "over", "exceeds", "exceed", "greater", ">", ">=", "up", "out", "rises", "rise",
    "climbs", "climb", "pumps", "pump", "reclaims", "reclaim", "breakout",
];
const BELOW: &[&str] = &[
    "below",
    "under",
    "beneath",
    "less",
    "<",
    "<=",
    "down",
    "drops",
    "drop",
    "falls",
    "fall",
    "dips",
    "dip",
    "loses",
    "lose",
    "breakdown",
];

enum CrossFind {
    None,
    One(Cross),
    Both,
}

fn find_cross(toks: &[Tok]) -> CrossFind {
    let mut above = false;
    let mut below = false;
    for w in toks.iter().filter_map(|t| t.as_word()) {
        if ABOVE.contains(&w) {
            above = true;
        }
        if BELOW.contains(&w) {
            below = true;
        }
    }
    match (above, below) {
        (true, false) => CrossFind::One(Cross::Above),
        (false, true) => CrossFind::One(Cross::Below),
        (false, false) => CrossFind::None,
        (true, true) => CrossFind::Both,
    }
}

fn has_close_word(toks: &[Tok]) -> bool {
    toks.iter()
        .filter_map(|t| t.as_word())
        .any(|w| matches!(w, "close" | "closes" | "closing" | "closed" | "candle"))
}

/// Bir sayı indeksi hariç, cümlecikteki tüm sayı indeksleri.
fn num_indices(toks: &[Tok], skip: Option<usize>) -> Vec<usize> {
    toks.iter()
        .enumerate()
        .filter(|(i, t)| matches!(t, Tok::Num { .. }) && Some(*i) != skip)
        .map(|(i, _)| i)
        .collect()
}

// ---- cümlecik ayrıştırıcılar -------------------------------------------------

/// Koşul (ya da iptal) cümleciğini oku. `force_mark`, iptal için: iptal daima
/// anlık mark price'a bakar (borsaya bırakılan trigger kendini iptal edemez).
fn parse_condition(
    toks: &[Tok],
    fallback_symbol: &str,
    force_mark: bool,
    notes: &mut Vec<Note>,
) -> Result<Condition, ParseError> {
    let cross = match find_cross(toks) {
        CrossFind::One(c) => c,
        CrossFind::None => return Err(ParseError::NoDirection),
        CrossFind::Both => return Err(ParseError::NoDirection),
    };

    let symbol = first_ticker(toks)
        .map(|(s, _)| s)
        .unwrap_or_else(|| fallback_symbol.to_string());

    let iv = find_interval(toks);
    let skip = iv.and_then(|(_, idx)| idx);
    let nums = num_indices(toks, skip);
    let price_idx = match nums.as_slice() {
        [only] => *only,
        [] => return Err(ParseError::NoPrice),
        _ => {
            // Birden çok sayı: yön kelimesinden sonra geleni fiyat say.
            *nums
                .iter()
                .find(|&&i| after_direction(toks, i))
                .ok_or(ParseError::AmbiguousPrice)?
        }
    };
    let price = toks[price_idx]
        .as_num()
        .expect("num_indices sadece Num döndürür");
    let symbol = Symbol::new(symbol);

    if force_mark {
        return Ok(Condition::MarkCross {
            symbol,
            cross,
            price,
        });
    }

    let candle = has_close_word(toks) || iv.is_some();
    if candle {
        let interval = match iv {
            Some((interval, _)) => interval,
            None => {
                notes.push(Note::assumed("Assumed a 1h timeframe"));
                Interval::H1
            }
        };
        Ok(Condition::CandleClose {
            symbol,
            interval,
            cross,
            price,
        })
    } else {
        Ok(Condition::MarkCross {
            symbol,
            cross,
            price,
        })
    }
}

/// `idx`'teki sayıdan önce bir yön kelimesi geçiyor mu?
fn after_direction(toks: &[Tok], idx: usize) -> bool {
    toks[..idx]
        .iter()
        .filter_map(|t| t.as_word())
        .any(|w| ABOVE.contains(&w) || BELOW.contains(&w))
}

/// İşlem cümleciğini oku: sembol, miktar ve (varsa) "at $X" limit girişi.
fn parse_trade(
    toks: &[Tok],
    fallback_symbol: &str,
) -> Result<(Symbol, f64, Option<f64>), ParseError> {
    let symbol = first_ticker(toks)
        .map(|(s, _)| s)
        .unwrap_or_else(|| fallback_symbol.to_string());

    // "at $X" → limit fiyatı; önündeki kelimesi "at"/"@" olan sayı.
    let mut limit = None;
    let mut size = None;
    for (i, t) in toks.iter().enumerate() {
        let Tok::Num { val, .. } = t else { continue };
        let prev = i.checked_sub(1).and_then(|j| toks[j].as_word());
        if matches!(prev, Some("at" | "@")) {
            limit = Some(*val);
        } else if size.is_none() {
            size = Some(*val);
        }
    }

    let size = size.ok_or(ParseError::MissingSize)?;
    if size <= 0.0 {
        return Err(ParseError::MissingSize);
    }
    Ok((Symbol::new(symbol), size, limit))
}

/// Çıkış cümleciğini kademelere çevir. "30% at $95k and 70% at $98k" gibi
/// sıralı (yüzde, fiyat) çiftlerini yakalar.
fn parse_exit_legs(toks: &[Tok]) -> Result<Vec<ExitLeg>, ParseError> {
    let mut prices = Vec::new();
    let mut pcts = Vec::new();
    for t in toks {
        match t {
            Tok::Num { val, .. } => prices.push(*val),
            Tok::Pct(p) => pcts.push(*p),
            Tok::Word(_) => {}
        }
    }
    if prices.is_empty() {
        return Err(ParseError::InvalidExit("a stop/target needs a price"));
    }
    let legs = match (prices.len(), pcts.len()) {
        (1, 0) => vec![ExitLeg::new(prices[0], 100.0)],
        (_, 0) => {
            return Err(ParseError::InvalidExit(
                "give a percentage for each staged exit",
            ));
        }
        (np, npct) if np == npct => prices
            .iter()
            .zip(pcts.iter())
            .map(|(&price, &pct)| ExitLeg::new(price, pct))
            .collect(),
        _ => {
            return Err(ParseError::InvalidExit(
                "each exit price needs one percentage",
            ))
        }
    };
    Ok(legs)
}

/// Bir Entry cümleciğinden ("limit $X" / "retest $X") limit fiyatını al.
fn parse_entry(toks: &[Tok]) -> Option<f64> {
    toks.iter().find_map(|t| t.as_num())
}

// ---- birleştirme -------------------------------------------------------------

pub(crate) fn run(input: &str) -> Result<(Raw, Vec<Note>), ParseError> {
    let toks = super::lex::tokenize(input);
    if toks.is_empty() {
        return Err(ParseError::Empty);
    }
    let mut notes = Vec::new();
    let mut clauses = split_clauses(&toks);

    // `Lead` içinde yön+fiyat varsa (if'siz koşul) onu koşul say.
    if let Some(lead) = clauses.iter_mut().find(|c| c.kind == Kind::Lead) {
        if matches!(find_cross(&lead.toks), CrossFind::One(_)) {
            lead.kind = Kind::Condition;
        }
    }

    // Herhangi bir cümlecikte geçen ilk ticker — koşul/işlem birbirinin
    // sembolünü ödünç alsın diye (koşulda "BTC" yazmayabilir).
    let fallback = clauses
        .iter()
        .find_map(|c| first_ticker(&c.toks))
        .ok_or(ParseError::NoSymbol)?
        .0;

    // Aksiyon.
    let sides: Vec<Kind> = clauses
        .iter()
        .map(|c| c.kind)
        .filter(|k| matches!(k, Kind::Buy | Kind::Sell | Kind::Notify))
        .collect();
    let has_buy = sides.contains(&Kind::Buy);
    let has_sell = sides.contains(&Kind::Sell);
    if has_buy && has_sell {
        return Err(ParseError::ConflictingSides);
    }

    // Koşul.
    let cond_clause = clauses
        .iter()
        .find(|c| c.kind == Kind::Condition)
        .ok_or(ParseError::NoCondition)?;
    let condition = parse_condition(&cond_clause.toks, &fallback, false, &mut notes)?;

    // İptal.
    let invalidate = match clauses.iter().find(|c| c.kind == Kind::Invalidate) {
        Some(c) => Some(parse_condition(&c.toks, &fallback, true, &mut notes)?),
        None => None,
    };

    // Aksiyonu kur.
    let action = if has_buy || has_sell {
        let side = if has_buy { Side::Buy } else { Side::Sell };
        let action_clause = clauses
            .iter()
            .find(|c| c.kind == Kind::Buy || c.kind == Kind::Sell)
            .expect("side var");
        let (symbol, size, at_limit) = parse_trade(&action_clause.toks, &fallback)?;

        // Giriş: ayrı Entry cümleciği > işlem içi "at $X" > market.
        let entry_price = clauses
            .iter()
            .find(|c| c.kind == Kind::Entry)
            .and_then(|c| parse_entry(&c.toks))
            .or(at_limit);
        let entry = match entry_price {
            Some(p) if p > 0.0 => Entry::Limit { price: p },
            _ => {
                notes.push(Note::assumed("Assumed a market entry"));
                Entry::Market
            }
        };

        let exits = build_exits(&clauses, side)?;
        AlertAction::Trade(TradeSpec {
            symbol,
            side,
            size,
            entry,
            exits,
        })
    } else if sides.contains(&Kind::Notify) {
        AlertAction::Notify
    } else {
        return Err(ParseError::NoAction);
    };

    check_invalidate_side(&condition, invalidate.as_ref())?;

    // Quote'u varsaydığımız (çıplak ticker) semboller için not — sembol hangi
    // cümlecikte geçerse geçsin, kullanılan sonuç üzerinden tek yerden.
    let mut used = vec![condition_symbol(&condition).to_string()];
    if let AlertAction::Trade(spec) = &action {
        used.push(spec.symbol.as_str().to_string());
    }
    if let Some(inv) = &invalidate {
        used.push(condition_symbol(inv).to_string());
    }
    let interp = symbol_notes(&toks, &used);
    notes.splice(0..0, interp);

    Ok((
        Raw {
            condition,
            invalidate,
            action,
        },
        notes,
    ))
}

/// Bir koşulun taşıdığı sembol (kompozisyonda ilk alt koşulunki).
fn condition_symbol(c: &Condition) -> &str {
    match c {
        Condition::MarkCross { symbol, .. } | Condition::CandleClose { symbol, .. } => {
            symbol.as_str()
        }
        Condition::All(v) | Condition::Any(v) => v.first().map_or("", condition_symbol),
    }
}

/// Çıplak ticker'dan (quote varsayılarak) çözülen ve gerçekten kullanılan her
/// sembol için bir "Read X as Y" notu. Aynı kelime bir kez notlanır.
fn symbol_notes(toks: &[Tok], used: &[String]) -> Vec<Note> {
    let mut out = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for w in toks.iter().filter_map(|t| t.as_word()) {
        if let Some((sym, true)) = resolve_symbol(w) {
            if used.iter().any(|u| u == &sym) && !seen.iter().any(|s| s == w) {
                seen.push(w.to_string());
                out.push(Note::interpreted(format!("Read \"{w}\" as {sym}")));
            }
        }
    }
    out
}

/// Tüm Tp/Sl cümleciklerini toplayıp doğrulanmış `Exits` kur.
fn build_exits(clauses: &[Clause], side: Side) -> Result<Option<Exits>, ParseError> {
    let mut take_profits = Vec::new();
    let mut stops = Vec::new();
    for c in clauses {
        match c.kind {
            Kind::Tp => take_profits.extend(parse_exit_legs(&c.toks)?),
            Kind::Sl => stops.extend(parse_exit_legs(&c.toks)?),
            _ => {}
        }
    }
    if take_profits.is_empty() && stops.is_empty() {
        return Ok(None);
    }
    let e = Exits {
        take_profits,
        stops,
    };
    // core'un kurallarıyla aynı: yüzdeler geçerli, stop'lar hedeflerin doğru
    // tarafında (yoksa emir dolar dolmaz kendini tetikler).
    if !e.pcts_ok() {
        return Err(ParseError::InvalidPercentages);
    }
    if !e.is_coherent(side) {
        return Err(ParseError::IncoherentExits);
    }
    Ok(Some(e))
}

/// İptal seviyesi, tetik eşiğinin yanlış tarafındaysa alarm koşul tutmadan
/// anında iptal olurdu — kullanıcı sessizce kaybeder. `web::build_alert`'teki
/// kontrolün aynısı; kaynak orada, mantık burada tekrar ediyor.
fn check_invalidate_side(
    condition: &Condition,
    invalidate: Option<&Condition>,
) -> Result<(), ParseError> {
    let Some(inv) = invalidate else {
        return Ok(());
    };
    let (Some((cc, cp)), Some((ic, ip))) = (cross_price(condition), cross_price(inv)) else {
        return Ok(());
    };
    let opposite = cc != ic;
    let wrong_side = match cc {
        Cross::Above => ip >= cp,
        Cross::Below => ip <= cp,
    };
    if opposite && wrong_side {
        return Err(ParseError::InvalidateWrongSide);
    }
    Ok(())
}

fn cross_price(c: &Condition) -> Option<(Cross, f64)> {
    match c {
        Condition::MarkCross { cross, price, .. } | Condition::CandleClose { cross, price, .. } => {
            Some((*cross, *price))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sembol_cozumu() {
        assert_eq!(resolve_symbol("btc"), Some(("BTC-USD".into(), true)));
        assert_eq!(resolve_symbol("$sol"), Some(("SOL-USD".into(), true)));
        assert_eq!(resolve_symbol("eth-usd"), Some(("ETH-USD".into(), false)));
        assert_eq!(resolve_symbol("$foo"), Some(("FOO-USD".into(), true)));
        // İngilizce kelimeyle karışanlar açık yazım ister.
        assert_eq!(resolve_symbol("near"), None);
        assert_eq!(resolve_symbol("link"), None);
    }

    #[test]
    fn interval_tek_token_ve_sayi_birim() {
        assert_eq!(interval_alias("4h"), Some(Interval::H4));
        assert_eq!(interval_alias("hourly"), Some(Interval::H1));
        let toks = super::super::lex::tokenize("the 4 hour candle");
        assert_eq!(find_interval(&toks).map(|(iv, _)| iv), Some(Interval::H4));
    }

    #[test]
    fn cancel_if_icindeki_if_yutulur() {
        let toks = super::super::lex::tokenize(
            "if the 1h candle closes above $90k, long 0.5 btc, cancel if price drops below $88k",
        );
        let clauses = split_clauses(&toks);
        let kinds: Vec<Kind> = clauses.iter().map(|c| c.kind).collect();
        // İki koşul olmamalı: cancel'ın if'i yutuldu.
        assert_eq!(kinds.iter().filter(|k| **k == Kind::Condition).count(), 1);
        assert_eq!(kinds.iter().filter(|k| **k == Kind::Invalidate).count(), 1);
    }
}
