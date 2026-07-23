//! Genel API'nin gerçek cümlelerle uçtan uca davranışı: farklı ifadeler,
//! notlar ve hata yolları.

use pusu_core::{AlertAction, Condition, Cross, Entry, Interval, Side};
use pusu_nl::{parse, Note, ParseError};

fn trade(input: &str) -> (Condition, pusu_core::TradeSpec, Vec<Note>) {
    let p = parse(input).unwrap();
    let AlertAction::Trade(spec) = p.draft.action.clone() else {
        panic!("trade bekleniyordu: {input}");
    };
    (p.draft.condition, spec, p.notes)
}

#[test]
fn hourly_kelimesi_saatlik_intervale_coz() {
    let (cond, _, _) = trade("long 1 SOL when the hourly candle closes above $150");
    assert_eq!(
        cond,
        Condition::CandleClose {
            symbol: "SOL-USD".into(),
            interval: Interval::H1,
            cross: Cross::Above,
            price: 150.0,
        }
    );
}

#[test]
fn sayi_birim_intervali_fiyatla_karistirmaz() {
    // "4 hour" içindeki 4, fiyat sanılmamalı; fiyat 2800.
    let (cond, _, _) = trade("short 3 ETH when the 4 hour candle closes below 2800");
    assert_eq!(
        cond,
        Condition::CandleClose {
            symbol: "ETH-USD".into(),
            interval: Interval::H4,
            cross: Cross::Below,
            price: 2_800.0,
        }
    );
}

#[test]
fn limit_girisi_at_ile() {
    let (_, spec, _) = trade("buy 0.1 BTC at $89,500 when price crosses above $90k");
    assert_eq!(spec.entry, Entry::Limit { price: 89_500.0 });
    assert_eq!(spec.side, Side::Buy);
    assert_eq!(spec.size, 0.1);
}

#[test]
fn limit_girisi_ayri_cumlecikle() {
    let (_, spec, _) = trade("long 0.2 BTC, limit $89,000, when it closes above $90k");
    assert_eq!(spec.entry, Entry::Limit { price: 89_000.0 });
}

#[test]
fn market_giris_varsayimi_not_birakir() {
    let (_, spec, notes) = trade("long 0.5 BTC when the 1h candle closes above $90k");
    assert_eq!(spec.entry, Entry::Market);
    assert!(notes
        .iter()
        .any(|n| n.kind() == "assumed" && n.text().contains("market")));
}

#[test]
fn bare_ticker_quote_notu_birakir() {
    let (_, _, notes) = trade("long 0.5 BTC when it closes above $90k");
    assert!(notes
        .iter()
        .any(|n| n.kind() == "interpreted" && n.text().contains("BTC-USD")));
}

#[test]
fn acik_sembol_quote_notu_birakmaz() {
    let (_, spec, notes) = trade("long 0.5 BTC-USD when it closes above $90k");
    assert_eq!(spec.symbol.as_str(), "BTC-USD");
    assert!(!notes.iter().any(|n| n.kind() == "interpreted"));
}

#[test]
fn dolar_prefixli_bilinmeyen_ticker_kabul() {
    let (cond, spec, _) = trade("long 100 $WIF when $WIF breaks above $4");
    assert_eq!(spec.symbol.as_str(), "WIF-USD");
    let Condition::MarkCross { symbol, .. } = cond else {
        panic!();
    };
    assert_eq!(symbol.as_str(), "WIF-USD");
}

#[test]
fn capraz_varlik_kosul_ve_islem_farkli_sembol() {
    // Koşul BTC, işlem ETH — tipler bunu destekliyor.
    let (cond, spec, _) = trade("long 2 ETH-USD when BTC-USD breaks above $100k");
    let Condition::MarkCross { symbol, .. } = cond else {
        panic!();
    };
    assert_eq!(symbol.as_str(), "BTC-USD");
    assert_eq!(spec.symbol.as_str(), "ETH-USD");
}

#[test]
fn kademeli_tp_farkli_kelime_sirasi() {
    let (_, spec, _) =
        trade("long 1 BTC when it closes above $90k, take profit 50% at $95k and 50% at $100k");
    let e = spec.exits.unwrap();
    assert_eq!(e.take_profits.len(), 2);
    assert_eq!(e.take_profits[0].pct, 50.0);
    assert_eq!(e.take_profits[1].price, 100_000.0);
}

#[test]
fn hem_tp_hem_sl_ayri_cumleciklerde() {
    let (_, spec, _) = trade("long 1 BTC when it closes above $90k, TP $95k, SL $88k");
    let e = spec.exits.unwrap();
    assert_eq!(e.take_profits.len(), 1);
    assert_eq!(e.stops.len(), 1);
}

// ---- hata yolları ------------------------------------------------------------

#[test]
fn bos_girdi() {
    assert_eq!(parse("   "), Err(ParseError::Empty));
}

#[test]
fn sembol_yok() {
    assert_eq!(
        parse("long 0.5 when it closes above $90k"),
        Err(ParseError::NoSymbol)
    );
}

#[test]
fn kosul_yok() {
    assert_eq!(parse("long 0.5 BTC"), Err(ParseError::NoCondition));
}

#[test]
fn celiskili_yon() {
    assert_eq!(
        parse("buy and sell 1 BTC when it closes above $90k"),
        Err(ParseError::ConflictingSides)
    );
}

#[test]
fn tutarsiz_cikis_reddedilir() {
    // Long'da stop (95k) hedefin (92k) üstünde → emir kendini tetikler.
    assert_eq!(
        parse("long 1 BTC when it closes above $90k, TP $92k, SL $95k"),
        Err(ParseError::IncoherentExits)
    );
}

#[test]
fn yuzde_toplami_asan_reddedilir() {
    assert_eq!(
        parse("long 1 BTC when it closes above $90k, TP 70% at $95k and 70% at $98k"),
        Err(ParseError::InvalidPercentages)
    );
}

#[test]
fn iptal_yanlis_tarafta_reddedilir() {
    // Tetik yukarı (>90k) ama iptal ters yönde ve eşiğin üstünde (<95k) —
    // 90k civarı fiyat zaten 95k'nın altında, yani alarm anında iptal olurdu.
    assert_eq!(
        parse("long 1 BTC when it closes above $90k, cancel if price drops below $95k"),
        Err(ParseError::InvalidateWrongSide)
    );
}

#[test]
fn staged_tp_yuzdesiz_reddedilir() {
    assert!(matches!(
        parse("long 1 BTC when it closes above $90k, TP at $95k and $98k"),
        Err(ParseError::InvalidExit(_))
    ));
}
