//! Kelime ayrıştırma (tokenizer).
//!
//! Cümleyi boşluklardan bölüp her parçayı sınıflandırır: sayı/fiyat, yüzde ya
//! da kelime. Asıl incelik sayıda: kullanıcı `$90,000`, `90k`, `88.4k`, `0.5`
//! ve `3,000` gibi çok biçim yazıyor; hepsi tek bir `f64`'e inmeli.
//!
//! **`m` eki bilinçli olarak yalnızca `$` ile milyon sayılıyor.** `$1m` bir
//! milyon; ama `15m`/`1m`/`30m` birer zaman dilimi (dakika) — bunları sayı
//! yapıp milyona çevirmek alarmı sessizce bozardı. Bu yüzden `$`'siz `m` eki
//! sayı değil kelime kalır ve interval çözümü [`crate::parse`]'a bırakılır.

/// Tek bir token.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// Bir sayı. `dollar`, önünde `$` olup olmadığını taşır (fiyat/size
    /// ayrımında ipucu).
    Num { val: f64, dollar: bool },
    /// Yüzde, örn. `30%` → `30.0`.
    Pct(f64),
    /// Küçük harfe indirgenmiş kelime. Interval (`1h`, `15m`), sembol (`btc`),
    /// yön (`above`) ve anahtar kelimeler burada.
    Word(String),
}

impl Tok {
    pub fn as_word(&self) -> Option<&str> {
        match self {
            Self::Word(w) => Some(w),
            _ => None,
        }
    }

    pub fn as_num(&self) -> Option<f64> {
        match self {
            Self::Num { val, .. } => Some(*val),
            _ => None,
        }
    }
}

/// Cümleyi token dizisine çevir.
pub fn tokenize(input: &str) -> Vec<Tok> {
    input.split_whitespace().filter_map(classify).collect()
}

/// Bir boşluksuz parçayı tek token'a sınıflandır. Anlam taşımayan noktalama
/// (`,` `.` `;` `:` `!` `?` ve saran parantez/tırnak) baştan/sondan soyulur —
/// ama sayının içindeki virgül/nokta korunur, çünkü yalnızca uçlar soyuluyor.
fn classify(piece: &str) -> Option<Tok> {
    let trimmed = piece.trim_matches(|c| {
        matches!(
            c,
            ',' | '.' | ';' | ':' | '!' | '?' | '(' | ')' | '"' | '\''
        )
    });
    if trimmed.is_empty() {
        return None;
    }

    // Yüzde: sondaki % işaretini soyup kalanı sayı olarak dene.
    if let Some(body) = trimmed.strip_suffix('%') {
        if let Some((val, _)) = parse_number(body) {
            return Some(Tok::Pct(val));
        }
    }

    if let Some((val, dollar)) = parse_number(trimmed) {
        return Some(Tok::Num { val, dollar });
    }

    Some(Tok::Word(trimmed.to_ascii_lowercase()))
}

/// Bir parçayı sayıya çevir. Başarısızsa `None` (çağıran onu kelime sayar).
///
/// Kabul edilen biçimler: `$` öneki (opsiyonel), binlik `,`/`_` ayraçları,
/// ondalık `.`, ve `k`/`b` ölçek ekleri. `m` ölçeği **yalnızca `$` varsa**
/// (milyon) — yoksa dakika interval'i sanılır ve kelime kalır.
fn parse_number(s: &str) -> Option<(f64, bool)> {
    let dollar = s.starts_with('$');
    let body = if dollar { &s[1..] } else { s };
    if body.is_empty() {
        return None;
    }

    // Ölçek eki.
    let (digits, scale) = match body.chars().last() {
        Some('k' | 'K') => (&body[..body.len() - 1], 1_000.0),
        Some('b' | 'B') => (&body[..body.len() - 1], 1_000_000_000.0),
        // `m`/`M` yalnızca `$` ile milyon; yoksa dakika interval'i → kelime.
        Some('m' | 'M') if dollar => (&body[..body.len() - 1], 1_000_000.0),
        _ => (body, 1.0),
    };

    let cleaned: String = digits.chars().filter(|&c| c != ',' && c != '_').collect();
    if cleaned.is_empty() {
        return None;
    }
    // Sadece rakam ve en fazla bir ondalık nokta.
    if !cleaned.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    if cleaned.chars().filter(|&c| c == '.').count() > 1 {
        return None;
    }

    let val: f64 = cleaned.parse().ok()?;
    Some((val * scale, dollar))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(s: &str) -> Option<(f64, bool)> {
        parse_number(s)
    }

    #[test]
    fn dolar_ve_virgul() {
        assert_eq!(num("$90,000"), Some((90_000.0, true)));
        assert_eq!(num("90,000"), Some((90_000.0, false)));
        assert_eq!(num("3,000"), Some((3_000.0, false)));
    }

    #[test]
    fn k_ve_b_olcegi_dolarsiz_da_calisir() {
        assert_eq!(num("90k"), Some((90_000.0, false)));
        assert_eq!(num("88.4k"), Some((88_400.0, false)));
        assert_eq!(num("$1.5k"), Some((1_500.0, true)));
        assert_eq!(num("2b"), Some((2_000_000_000.0, false)));
    }

    #[test]
    fn m_eki_yalnizca_dolarla_milyon() {
        // "$1m" milyon; ama "15m"/"1m" interval (dakika) → sayı değil.
        assert_eq!(num("$1m"), Some((1_000_000.0, true)));
        assert_eq!(num("15m"), None);
        assert_eq!(num("1m"), None);
        assert_eq!(num("30m"), None);
    }

    #[test]
    fn intervaller_kelime_kalir() {
        // Rakamla başlasalar bile sayı lexer'ı bunları reddetmeli.
        for iv in ["1h", "4h", "10s", "1d", "1w", "12h"] {
            assert_eq!(num(iv), None, "{iv} sayı olmamalı");
        }
    }

    #[test]
    fn ondalik_ve_sade() {
        assert_eq!(num("0.5"), Some((0.5, false)));
        assert_eq!(num("88400"), Some((88_400.0, false)));
    }

    #[test]
    fn yuzde_tokenlenir() {
        assert_eq!(classify("30%"), Some(Tok::Pct(30.0)));
        assert_eq!(classify("70%,"), Some(Tok::Pct(70.0)));
    }

    #[test]
    fn sondaki_noktalama_soyulur_sayi_ici_korunur() {
        assert_eq!(
            classify("$90k,"),
            Some(Tok::Num {
                val: 90_000.0,
                dollar: true
            })
        );
        assert_eq!(classify("BTC,"), Some(Tok::Word("btc".into())));
        assert_eq!(
            classify("$90,000"),
            Some(Tok::Num {
                val: 90_000.0,
                dollar: true
            })
        );
    }

    #[test]
    fn tam_cumle_tokenlenir() {
        let t = tokenize("if the 1H candle closes above $90k, long 0.5 BTC");
        assert_eq!(
            t,
            vec![
                Tok::Word("if".into()),
                Tok::Word("the".into()),
                Tok::Word("1h".into()),
                Tok::Word("candle".into()),
                Tok::Word("closes".into()),
                Tok::Word("above".into()),
                Tok::Num {
                    val: 90_000.0,
                    dollar: true
                },
                Tok::Word("long".into()),
                Tok::Num {
                    val: 0.5,
                    dollar: false
                },
                Tok::Word("btc".into()),
            ]
        );
    }
}
