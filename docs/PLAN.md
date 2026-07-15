# PUSU — Proje Planı

> Sürüm: v1 taslak · 2026-07-15
> Teknik zemin: [`docs/research/01-bulk-technical-findings.md`](research/01-bulk-technical-findings.md)
> Dil: **Rust** (backend + frontend/WASM) · Yüzey: **Web app** (Telegram sonra)
> İsim: **PUSU** — trader "pusuya yatmak" der; seviyeyi belirleyip beklemek tam olarak bu ürün

---

## 1. Ürün tek cümlede

Trader'ın zaten her gün yaptığı şeyi — fiyat alarmı kurmayı — işlemi de yapan bir şeye çevirmek.

**Çözdüğü somut sorun (kullanıcının kendi ifadesi):**
> "Long alacağım ama saatlik kapanışın belirlediğim değerin üzerinde kapaması gerekiyor. Kapanışa daha çok var, uykum geliyor, izleyemiyorum, işlem kaçıyor."

Alarm çalar ama sen yoksundur. Alarm ile işlem arasındaki boşluk, retail trader'ın en çok para kaybettiği yer. BULK, bu boşluğun **borsanın kendi içinde** kapatılabildiği ilk mekan.

---

## 2. Neden bu ürün, neden şimdi

| | |
|---|---|
| **Pazar canlı** | Mainnet $652M/gün hacim, $412M OI (ölçüldü) |
| **Rekabet yok** | Builder codes henüz staging'de; mainnet'te gün bir açılıyor |
| **Model kanıtlı** | Hyperliquid'de $40M+ builder geliri, 100+ takım, DAU'nun %40'ı 3. parti frontend'den |
| **Ekonomi cömert** | Builder 1–15 bps; borsanın kendi taker fee'si 2.2–3.5 bps |
| **Teknik hendek** | `trig`/`of` nested builderCode — dokümandan okunmuyor, kaynak koddan çıktı. Hyperliquid'de karşılığı yok |
| **Kişisel edge** | `priceactiontradebotu` (SMC/price-action sinyal motoru) sonradan seviye önerisi olarak bağlanabilir |

---

## 3. Ürünün belkemiği: alarm iki sınıfa ayrılır

Ayrım teknik değil, **güven** ayrımı — ve gizlenmek yerine ürünün merkezine konuyor.

### 🔒 Sınıf 1 — Borsada yaşayan alarm
- **Koşul:** mark price eşiği kesiyor
- **Derleme:** `trig` basket → içinde builderCode'lu `l`/`m` + `of` bracket'i
- **İmza:** kullanıcı, tarayıcıda, kendi anahtarıyla (`bulk-keychain-wasm`). Sunucumuzda key yok
- **Yürütme:** borsa. **Bizim sunucumuz ölse bile emir çalışır**
- **Agent gerekmiyor**

### ⚡ Sınıf 2 — Watcher'ın değerlendirdiği alarm
- **Koşul:** mum kapanışı, indikatör, çok koşullu, zaman pencereli — mark-price kesişimiyle ifade edilemeyen her şey
- **İmza:** yine kullanıcı, tarayıcıda — **ön-imzalı** (§7). Sunucuda key yok
- **Yürütme:** watcher `/klines`'ı sınır anlarında sorgular, koşul tutunca ön-imzalı tx'i **gönderir**
- **Agent YOK** — Faz 0'da agent'ın para çıkarabildiği görülünce tasarım değişti (§7)
- **Bizim uptime'ımıza bağlı** — kullanıcıya açıkça söylenir

### Derleyicinin routing kararı = ürünün kalbi
Kullanıcı ne yazdığını bilmez; **derleyici hangi sınıfa düştüğüne karar verir ve kullanıcıya söyler.** Her alarm kartında rozet: 🔒 *"Borsada yaşıyor"* / ⚡ *"Watcher'da"*. İmzalamadan önce önizleme.

> ⚠️ **Önemli düzeltme:** Ücretsiz katman (sadece bildirim) da watcher gerektiriyor — native trigger bildirim gönderemez, sadece emir atar. Yani **watcher gün birden itibaren temel altyapı**, sonradan eklenen bir şey değil. Sınıf 1'in vaadi "sunucusuz ürün" değil, **"execution'ın sunucuya bağımlı olmaması"**. Pazarlama bu ayrımı doğru kurmalı.

---

## 4. Etik sınır (pazarlığa kapalı)

`st` / `tp` / `trl` — koruyucu emirler — **bilinçli olarak builderCode taşımıyor.** Bedavalar.

Bir stop'u `trig` + reduce-only market ile sarıp fee kesmek teknik olarak mümkün, kullanıcı onayı da alınır, fee de görünür. **Yapmıyoruz.** Bedava native alternatifi varken bunu yapmak dark pattern ve ekip kodu okuduğunda görür.

**Fee, native'in yapamadığı yerden kazanılır:** koşullu *giriş*, çok bacaklı basket, enstrümanlar arası mantık, mum-kapanışı değerlendirmesi. Oralarda değer katıyoruz, fee hakkımız.

---

## 5. Mimari (hepsi Rust)

```
crates/
  core/         # domain: Alert, Condition, Action, AlertClass
  compiler/     # Alert → Plan (Native trig basket | Watched rule)
  engine/       # watcher: WS abonelikleri, koşul değerlendirme, agent execution
  api/          # axum: REST + SSE/WS, auth, alarm CRUD
  web/          # Leptos (WASM): frontend + bulk-keychain-wasm ile tarayıcı imzası
  storage/      # sqlx + Postgres: users, alerts, executions, audit log
```

**Dış bağımlılıklar:** *(Faz 0'da netleşti)*

| Katman | Kütüphane | Neden |
|---|---|---|
| **İmzalama** | `bulk-keychain` | Kanonik ve sağlam. `trig`/`of`/`rng` doğru imzalıyor — Faz 0'da doğrulandı |
| Mum kapanışı | **kendi kodumuz** (`pusu-feed`) | REST `/klines` polling. bulk-client'ın candle handler'ı kırık ve WS zaten ağır |
| REST (hesap, emir) | **kendi kodumuz** | `get_account()` açık emir varken çöküyor (§8.8). Kendi ince client'ımızı yazıyoruz |
| Tarayıcı imzası | `bulk-keychain-wasm` | Kullanıcı kendi anahtarıyla imzalar; sunucuda key yok |

⚠️ **`bulk-client` kullanmıyoruz.** v0.1.2'de dört ayrı kırık nokta bulundu (imzalama, candle, hesap sorgusu, hata semantiği). Yalnızca `bulk-keychain`'e (imzalama) bağımlıyız; REST'i kendimiz konuşuyoruz — yüzey zaten dar (`/klines`, `/account`, `/order`). Detay: [`research/02-staging-spike.md`](research/02-staging-spike.md) §6, §8.6, §8.8

⚠️ **`Ok` ≠ başarı.** Borsa reddedilen emirde HTTP 200 dönüyor; sonuç `status` alanında (`rejectedInvalid`). Temel katmanda status kontrolü zorunlu — yoksa reddedilen alarmı "kuruldu" diye gösterir, kullanıcıya yalan söyleriz.

### 🔒 Sınıf 1'in derleme şablonu (Faz 1'de doğrulandı)

```
trig { c, d, tr, actions: [ m{...builderCode}, rng{stop, hedef} ] }
```

Bracket'i `of` ile bağlamak **çalışmıyor**: `of`'un parent'ı trigger olamıyor (`on_fill parent not found`) ve `of` trigger'ın içine de gömülemiyor (`invalid action in trigger order`). Çalışan tek yol `rng`'yi market emrin kardeşi olarak basket'e koymak; market anında dolduğu için on-fill ile eşdeğer.

⚠️ **Trigger içinde yalnızca market giriş.** `trig { actions: [l, rng] }` limit beklerken `rng`'yi hemen kurar — var olmayan pozisyonu korur. v1 kapsamı dışı.

### Domain taslağı

```rust
enum Condition {
    // → Sınıf 1 (native)
    MarkCross { above: bool, price: f64 },

    // → Sınıf 2 (watcher)
    CandleClose { interval: Interval, above: bool, price: f64 },
    Indicator { kind: IndicatorKind, .. },
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Within { window: TimeWindow, inner: Box<Condition> },
}

enum AlertAction {
    NotifyOnly,                       // ücretsiz, builderCode yok
    Trade {
        side, size, leverage,
        bracket: Option<Bracket>,     // of → rng (SL+TP)
        builder_fee_bps: u8,
    },
}

enum Plan {
    Native(Vec<Action>),   // trig basket; kullanıcı tarayıcıda imzalar
    Watched(WatchRule),    // engine değerlendirir; agent imzalar
}
```

---

## 6. Aşamalar

### Faz 0 — Doğrulama spike'ı (staging) · ✅ **TAMAMLANDI** (2026-07-15)

> Sonuçlar: [`research/02-staging-spike.md`](research/02-staging-spike.md) · Kod: `crates/spike` (atılacak)

- [x] Staging'e bağlan, keypair üret, faucet'ten para al → **whitelist gerekmiyor, $1000**
- [x] **Builder onayı sub-account'a yayılıyor mu?** → ✅ **EVET.** Güvenlik + gelir çakışmıyor, mimari korunuyor
- [x] Agent wallet `transfer` imzalayabiliyor mu? → 🚨 **EVET, external transfer geçiyor.** Doküman yanlış → agent'ı bıraktık, ön-imzalı tx'e geçtik (§7)
- [x] `trig` içine builderCode'lu emir gömülüp tetikleniyor mu? → ✅ **EVET.** Uçtan uca doğrulandı, fee birebir kesildi
- [x] Builder fee ne zaman/nasıl ödeniyor? → **Anlık, doğrudan bakiyeye.** Claim/epoch/vesting yok
- [x] Nonce'un ömrü var mı? → **Yok.** Ön-imzalı tasarımı mümkün kılan bulgu
- [x] Doğru endpoint? → `staging-api.bulk.trade` ve `exchange-api.bulk.trade` canlı doğrulandı

**Faz 1'e taşınanlar** (bloklamıyor, ama bilinmeli):
- [ ] `of` bracket'i parent dolunca kuruluyor mu?
- [ ] `rbc` bekleyen blob'ları öldürüyor mu? (kill switch — §7)
- [ ] Rate limit'ler (REST + WS) — ölçek tavanını belirler
- [ ] Trigger max nested action / hesap başına max açık conditional
- [ ] Tetiklenen nested emir yetersiz marjinle reddedilirse ne oluyor? (kullanıcıya ne diyeceğiz?)

**Sürpriz bulgu:** `USD-TRY` (20x), `GOLD-USD`, `EUR-USD`, `USD-JPY`, `USD-KRW` yapılandırılmış ama henüz açılmamış. Türk kitleye hitap eden bir ürün için `USD-TRY` stratejik kart.

### Faz 1 — Temel · ~1 hafta
- [x] Cargo workspace
- [x] `core`: domain model + yürütme katmanı sınıflandırıcısı (20 test)
- [ ] CI: fmt/clippy/test
- [ ] `feed`: mum kapanışı tespiti — **sınır anlarında REST `/klines` polling**
- [ ] `storage`: şema + migration'lar
- [ ] Sağlık/gözlemlenebilirlik iskeleti (watcher güvenilirliği ürünün kredibilitesi — §9)

> **Karar değişikliği:** "candle WS aboneliği" planlanmıştı; ölçüm sonrası **filtresiz REST polling**e
> geçildi. WS abonelik başına ~1,9 MB ilk dump atıyor (11 sembol × 4 timeframe ≈ 85 MB/reconnect) ve
> 1 MB'lık varsayılan frame limitini aşıp bağlantıyı kopartıyor. Ayrıca `bulk-client`'ın candle
> handler'ı zaten kırık (3. bug). Filtresiz REST'te 1h = 23 KB, 4h = 6 KB, 1d = 1 KB — bedava sayılır.
> Detay: [`research/02-staging-spike.md`](research/02-staging-spike.md) §8.6
>
> ⚠️ **İki kural — ikisi de ürünü sessizce bozardı:**
> 1. `/klines` bazen **devam eden** mumu döndürüyor → daima `T <= now` filtrele. Kaçırılırsa
>    "saatlik kapanış" alarmı erken ateşler; kullanıcının kaçınmak istediği şeyin ta kendisi.
> 2. `startTime` filtresi **~60 sn bayat** veri döndürüyor (origin kaynaklı, CDN değil) →
>    canlı tespitte kullanma. Cazip (142 byte) ama alarmı bir dakika geç ateşler.
>
> **v1 kapsamı: 15m ve üstü timeframe'ler.** 1m filtresiz 1,3 MB/poll, `startTime` de bayat.

### Faz 2 — Sınıf 1 uçtan uca · ~1.5 hafta
- [ ] `compiler`: `MarkCross + Trade` → `trig` basket + `of` bracket
- [ ] Tarayıcı imzası (`bulk-keychain-wasm`), cüzdan bağlama
- [ ] Builder code onay akışı — fee şeffaf gösterilir, iptal edilebilir (§8)
- [ ] Alarm CRUD + durum takibi (account stream'den)
- [ ] Minimum çalışan web arayüzü
- [ ] **Milestone: gerçek para, gerçek alarm, gerçek builder fee**

### Faz 3 — Sınıf 2 (watcher) · ~1.5 hafta
- [ ] `engine`: kural değerlendirme döngüsü, mum kapanışı semantiği
- [ ] Sub-account + agent wallet onboarding akışı (§7)
- [ ] Agent key yönetimi: at-rest şifreleme, rotasyon, audit log
- [ ] Execution + retry + idempotency (nonce yönetimi)
- [ ] Bildirimler (web push)
- [ ] Ücretsiz katman: NotifyOnly alarmlar

### Faz 4 — Derleyici / doğal dil · ~1 hafta
Kullanıcının fikri: *"kullanıcı istediği alarmı yazar, arka planda alarm düzenlenir."*
- [ ] Alert DSL (yazılı, denetlenebilir ara temsil)
- [ ] NL → DSL parse (LLM); DSL → Plan derleme
- [ ] **Önizleme + onay ekranı** — kullanıcı ne imzaladığını görmeden imzalamaz
- [ ] Routing kararının açıklanması: *"Bunu borsaya gömebiliyorum"* / *"Bunun için watcher gerekiyor, çünkü mum kapanışı zincirde yok"*

### Faz 5 — Vitrin · ~1 hafta
- [ ] Grafik üstünde sürüklenebilir seviyeler
- [ ] Alarm kartları, 🔒/⚡ rozetleri, tetiklenme geçmişi
- [ ] Landing + demo videosu (X için)
- [ ] Docs / açık kaynak parçalar (grant + görünürlük)

**Toplam: ~6–7 hafta** (tek kişi, gerçekçi tahmin)

---

## 7. Güvenlik duruşu — **Faz 0'da güncellendi** ✅

> Faz 0 sonuçları: [`research/02-staging-spike.md`](research/02-staging-spike.md) §8.5

### Kanıtlanan tehdit
**Agent, sub-account'tan dışarıya para çıkarabiliyor.** Staging'de doğrulandı: sub'a kayıtlı agent, 10 USD'yi keyfi bir hesaba taşıdı ve geçti. Bu, `security.md`'nin *"agents cannot unilaterally move funds"* iddiasıyla çelişiyor.

Ayrıca `agentWalletCreation` payload'ında yetki kapsamı alanı yok — `{"a": pubkey, "d": bool}`, hepsi bu. "Bu agent sadece emir atsın" denemiyor.

### Kararlar

**1. Agent wallet KULLANMIYORUZ. Ön-imzalı tx kullanıyoruz.**
- Kullanıcı emri tarayıcıda **şimdi** imzalar (builderCode + `of` bracket dahil)
- Sunucu imzalı blob'u saklar — **hiçbir imzalama yetkisi yok**
- Koşul gerçekleşince (mum kapanışı vb.) sunucu blob'u POST eder
- Sunucu sızarsa saldırgan **parayı çıkaramaz** — blob tam olarak ne diyorsa onu yapar

Faz 0'da doğrulandı: **nonce'un ömrü yok** (30 gün eski/ileri nonce'lar kabul edildi), yani blob süresiz geçerli. Tasarım çalışıyor.

**2. Master hesapta asla çalışmıyoruz.** Kullanıcı ayrı sub-account açar, riske atacağı miktarı oraya koyar. Faz 0'da doğrulandı: agent/blob master'a dokunamıyor, internal transfer (sub→master) engelli. Zarar tavanı = kullanıcının ayırdığı miktar.

**3. Kill switch: `rbc`.** Builder onayının geri çekilmesi, builderCode taşıyan tüm bekleyen blob'ları öldürür (onaysız builderCode `rejectedInvalid` alıyor). Kullanıcıya net çıkış: *"PUSU'yu durdurmak için onayı geri çek, bekleyen tüm alarmlar ölür."* ⚠️ Faz 1'de doğrudan doğrulanacak.

### Kalan risk
İmzalı blob hiç eskimiyor. Kullanıcı alarmı iptal ederse kopyayı sileriz ama blob teorik olarak gönderilebilir kalır. DB sızarsa eski blob'lar gönderilebilir — zarar sınırlı (kullanıcının istediği emirler, yanlış zamanda), `rbc` ile kapatılabilir. Kullanıcıya dürüstçe anlatılacak.

Sınıf 1'de zaten agent yok — kullanıcı tarayıcıda imzalar, borsa yürütür.

---

## 8. Fee kararı — **2 bps, onay = tahsilat** ✅ karar verildi

Borsanın taker fee'si 3.5 bps (Tier 1). Builder tavanı 15 bps. **PUSU 2 bps kesiyor.**

Gerekçe: 2 bps eklemek kullanıcının işlem maliyetini %57 artırıyor — savunulabilir sınır. 3 bps'te %86 olur, 15 bps'te 5 katına çıkar. Alarm ürününün kullanıcısı fiyat hassas retail; düşük fee → daha çok onay → daha çok hacim.

**Onay = tahsilat.** `abc`'de 2 onaylatıp 2 kesiyoruz; "5 onaylat, 2 kes, sonra sessizce 5'e çık" hukuken serbest ama ürünün tüm güven hikâyesini çöpe atar — ki o hikâye ana farklılaştırıcımız. Sonradan yükseltmek yeniden onay ister; sürtünmeyi bilinçli kabul ediyoruz.

Gerekçe: "5 onaylat, 2 kes, sonra sessizce 5'e çık" hukuken serbest ama ürünün tüm güven hikâyesini çöpe atar — ki o hikâye bizim ana farklılaştırıcımız. Şeffaflığı ürünün merkezine koyuyorsak fee'de de tutarlı olmalıyız.

Karşı argüman: sonradan yükseltmek yeniden onay ister (sürtünme). Bilinçli kabul ediyoruz.

⚠️ 15 bps, kullanıcının işlem maliyetini **5 katına** çıkarır. Alarm ürününün kullanıcısı fiyat hassas retail. Düşük fee → daha çok onay → daha çok hacim.

---

## 9. Riskler

| Risk | Etki | Azaltma |
|---|---|---|
| **Watcher downtime** | Sınıf 2 alarmı kaçar → kullanıcı işlem kaçırır → güven biter. Ürünün en büyük riski | Faz 1'den itibaren gözlemlenebilirlik; kaçırılan tetiklemeler için dürüst raporlama; Sınıf 1'i mümkün olan her yerde tercih et |
| Agent key sızıntısı | Sub-account bakiyesi | §7 izolasyonu; at-rest şifreleme; rotasyon |
| BULK kendi alarm özelliğini çıkarır | Ürün gereksizleşir | Hız + sinyal motoru entegrasyonu (kopyalanamaz) + NL katmanı |
| Dağıtımı olan biri kopyalar | Pazar payı | İlk olmak; ekiple ilişki kurmak |
| Rate limit'ler watcher'ı sınırlar | Ölçek tavanı | Faz 0'da ölç; kullanıcı başına değil sembol başına abone ol |
| Sadece 11 market | Alarm çeşitliliği düşük | `MU-USD` (hisse perp'i) farklılaştırıcı kart; BIP-1 ile market sayısı artacak |
| Aura mekaniği bilinmiyor | Airdrop beklentisi kurgulanamaz | "Coming soon" — üzerine plan kurma, bonus say |

---

## 10. Açık kararlar

- [x] ~~İsim~~ → **PUSU**. Türkçe olması artı: kitle ağırlıklı Türk trading toplulukları, kendi dilindeki ismi sahiplenir; yabancı için telaffuz edilebilir ve merak uyandırıcı
- [x] ~~Fee~~ → **2 bps, onay = tahsilat** (§8)
- [ ] Domain: `pusu.trade` / `pusu.app` müsaitlik kontrolü
- [ ] Ücretsiz katman sınırı: kullanıcı başına kaç NotifyOnly alarm?
- [ ] Leptos mu Dioxus mu (SSR ihtiyacı ve grafik kütüphanesi olgunluğuna göre)
- [ ] Açık kaynak sınırı: hangi parçalar public? (grant/görünürlük ↔ kopyalanma riski)
- [ ] Sinyal motoru (`priceactiontradebotu`) ne zaman bağlanır — Faz 4 sonrası mı, ayrı ürün mü?

---

## 11. Sıradaki adım

**Faz 0 spike'ı.** Plandaki her şey onun cevaplarına dayanıyor — özellikle "builder onayı sub-account'a yayılıyor mu" sorusu, çünkü cevabı hayırsa güvenlik (§7) ile gelir modeli çakışıyor ve mimariyi baştan düşünmek gerekiyor.

Yanlış varsayımla yazılmış plan, plan değil temenni.
