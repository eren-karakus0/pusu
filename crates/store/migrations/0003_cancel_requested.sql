-- Kullanıcı, defterde bekleyen (working) bir girişin iptalini isteyebiliyor.
-- api'nin borsaya imza yetkisi yok; iptal isteğini bu bayrağa yazıyor, watcher
-- bir sonraki turda görüp ön-imzalı cx'i gönderiyor (bkz. Watcher::track).
ALTER TABLE alerts
    ADD COLUMN cancel_requested BOOLEAN NOT NULL DEFAULT false;
