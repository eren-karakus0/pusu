//! Cüzdan köprüsü: `window.solana` (Phantom/Backpack) ile konuşur.
//!
//! Güvenlik-kritik nokta burası: cüzdan ham private key'i **asla** vermiyor,
//! yalnızca `signMessage(bytes)` ile imza döndürüyor. `pusu-sign` cüzdanın
//! imzalayacağı `message_bytes`'ı üretiyor; burası onu cüzdana götürüp imzayı
//! (base58) geri getiriyor. Anahtar hiçbir zaman JS'in/wasm'ın eline geçmiyor.
//!
//! Erişim `Reflect` ile runtime'da — böylece cüzdan yoksa (`window.solana`
//! tanımsız) derleme değil, dürüst bir `NoWallet` hatası alıyoruz.

use js_sys::{Function, Promise, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

#[derive(Debug, Clone)]
pub enum WalletError {
    /// `window.solana` yok — kullanıcıda cüzdan eklentisi kurulu değil.
    NoWallet,
    /// Kullanıcı imzayı/bağlantıyı reddetti ya da cüzdan hata verdi.
    Rejected(String),
    /// Beklenmeyen JS şekli (cüzdan API'si değişmiş olabilir).
    Shape(String),
}

impl std::fmt::Display for WalletError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWallet => f.write_str("wallet not found (is Phantom installed?)"),
            Self::Rejected(m) => write!(f, "wallet rejected: {m}"),
            Self::Shape(m) => write!(f, "unexpected wallet response: {m}"),
        }
    }
}

fn js_err(e: JsValue) -> WalletError {
    WalletError::Rejected(
        e.as_string()
            .or_else(|| js_sys::Error::from(e).message().as_string())
            .unwrap_or_else(|| "unknown error".into()),
    )
}

/// `window.solana` sağlayıcısını al.
fn provider() -> Result<JsValue, WalletError> {
    let win = web_sys::window().ok_or(WalletError::NoWallet)?;
    let sol =
        Reflect::get(&win, &JsValue::from_str("solana")).map_err(|_| WalletError::NoWallet)?;
    if sol.is_undefined() || sol.is_null() {
        return Err(WalletError::NoWallet);
    }
    Ok(sol)
}

/// Sağlayıcıdaki bir metodu adıyla çağır (Promise döndürenler için).
async fn call_method(
    target: &JsValue,
    method: &str,
    arg: Option<&JsValue>,
) -> Result<JsValue, WalletError> {
    let f = Reflect::get(target, &JsValue::from_str(method))
        .map_err(|_| WalletError::Shape(format!("no {method}")))?
        .dyn_into::<Function>()
        .map_err(|_| WalletError::Shape(format!("{method} is not a function")))?;
    let ret = match arg {
        Some(a) => f.call1(target, a),
        None => f.call0(target),
    }
    .map_err(js_err)?;
    let promise = ret
        .dyn_into::<Promise>()
        .map_err(|_| WalletError::Shape(format!("{method} did not return a promise")))?;
    JsFuture::from(promise).await.map_err(js_err)
}

/// Cüzdanı bağla, master pubkey'i (base58) döndür.
pub async fn connect() -> Result<String, WalletError> {
    let p = provider()?;
    let res = call_method(&p, "connect", None).await?;
    // Phantom `{ publicKey }` döndürüyor; bazı sürümlerde publicKey sağlayıcıda.
    let pk = Reflect::get(&res, &JsValue::from_str("publicKey"))
        .ok()
        .filter(|v| !v.is_undefined() && !v.is_null())
        .or_else(|| Reflect::get(&p, &JsValue::from_str("publicKey")).ok())
        .ok_or_else(|| WalletError::Shape("no publicKey".into()))?;
    pubkey_to_base58(&pk)
}

/// `message_bytes`'ı cüzdana imzalat, imzayı **base58** döndür (finalize'ın
/// beklediği biçim).
pub async fn sign_message(message: &[u8]) -> Result<String, WalletError> {
    let p = provider()?;
    let arr = Uint8Array::from(message);
    let res = call_method(&p, "signMessage", Some(&arr)).await?;
    let sig = Reflect::get(&res, &JsValue::from_str("signature"))
        .map_err(|_| WalletError::Shape("no signature".into()))?;
    let bytes = Uint8Array::new(&sig).to_vec();
    Ok(bs58::encode(bytes).into_string())
}

/// PublicKey nesnesini base58'e çevir. Phantom'da `.toString()` base58 verir.
fn pubkey_to_base58(pk: &JsValue) -> Result<String, WalletError> {
    if let Some(s) = pk.as_string() {
        return Ok(s);
    }
    let to_string = Reflect::get(pk, &JsValue::from_str("toString"))
        .ok()
        .and_then(|f| f.dyn_into::<Function>().ok())
        .ok_or_else(|| WalletError::Shape("no publicKey.toString".into()))?;
    to_string
        .call0(pk)
        .map_err(js_err)?
        .as_string()
        .ok_or_else(|| WalletError::Shape("publicKey is not a string".into()))
}
