-- Ön-imzalı blob payload'ları artık uygulama katmanında AES-256-GCM ile
-- şifreleniyor (bkz. pusu_store::crypto). Kolon JSONB'den opak ikili veriye
-- (BYTEA) dönüyor: `nonce(12) ‖ ciphertext`.
--
-- Şifreleme öncesi yazılmış düz metin satırlar çözülemez; staging'de kalıntı
-- oldukları için önce siliniyorlar. Bu satırlara bağlı alarmlar blob'suz kalır
-- (watcher dispatch edemez) ama staging'de blob'lar zaten geçici.
DELETE FROM presigned_blobs;

ALTER TABLE presigned_blobs
    ALTER COLUMN payload TYPE BYTEA USING convert_to(payload::text, 'UTF8');
