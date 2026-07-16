//! `Dispatch`'in gerçek implementasyonu: store'daki blob'u BULK'a postalar.
//!
//! Watcher `submit`/`cancel` derken bu katman devreye giriyor. Kritik olan
//! **sıra**: niyet önce yazılır, sonra postalanır.
//!
//! ```text
//! 1. blob'u store'dan al
//! 2. mark_dispatched  ← ağ çağrısından ÖNCE
//! 3. POST /order
//! ```
//!
//! Çökme 2 ile 3 arasında olursa, açılışta `dispatched_at` dolu görünür ve
//! [`crate::reconcile`] emri `openOrders`'tan sorgulayarak gerçeği öğrenir —
//! körlemesine tekrar göndermek yerine (§8.11). Niyeti postalamadan sonra
//! yazsaydık, çökme sonrası "gönderdim mi?" sorusuna "hayır" derdik ve emri
//! ikinci kez postalardık; nonce bunu yakalar ama 504 döndürdüğü için sonucu
//! da okuyamazdık.

use pusu_core::Alert;
use pusu_engine::{Dispatch, DispatchError};
use pusu_store::{BlobRole, Store};

/// BULK REST'e ön-imzalı blob gönderen dispatcher.
#[derive(Clone)]
pub struct HttpDispatch {
    store: Store,
    client: reqwest::Client,
    base_url: String,
}

impl HttpDispatch {
    pub fn new(store: Store, base_url: impl Into<String>) -> Self {
        Self {
            store,
            client: reqwest::Client::new(),
            base_url: base_url.into(),
        }
    }

    /// Bir rolün blob'unu al, niyeti işaretle, postala.
    async fn gonder(
        &self,
        alert: &Alert,
        role: BlobRole,
    ) -> Result<serde_json::Value, DispatchError> {
        let payload = self
            .store
            .get_blob(&alert.id, role)
            .await
            .map_err(|e| DispatchError::Network(format!("blob okunamadı: {e}")))?
            .ok_or(DispatchError::NoBlob)?;

        // Niyet önce. Bu satır ile POST arasında çökersek reconcile toparlar.
        self.store
            .mark_dispatched(&alert.id, role)
            .await
            .map_err(|e| DispatchError::Network(format!("niyet yazılamadı: {e}")))?;

        let resp = self
            .client
            .post(format!("{}/order", self.base_url))
            .json(&payload)
            .send()
            .await
            .map_err(|e| DispatchError::Network(e.to_string()))?;

        // HTTP hatası da transport hatası: emrin geçip geçmediği belirsiz.
        // Watcher bunu Outcome::Unknown'a çevirip Uncertain işaretliyor.
        // (§8.11: tekrar gönderim 504 dönüyor; gerçek durum openOrders'ta.)
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| DispatchError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(DispatchError::Network(format!("HTTP {status}: {text}")));
        }

        serde_json::from_str(&text)
            .map_err(|e| DispatchError::Network(format!("yanıt çözülemedi: {e} — {text}")))
    }
}

impl Dispatch for HttpDispatch {
    async fn submit(&self, alert: &Alert) -> Result<serde_json::Value, DispatchError> {
        self.gonder(alert, BlobRole::Entry).await
    }

    async fn cancel(&self, alert: &Alert) -> Result<serde_json::Value, DispatchError> {
        self.gonder(alert, BlobRole::Cancel).await
    }
}
