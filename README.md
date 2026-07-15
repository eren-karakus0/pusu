<h1 align="center">PUSU</h1>

<p align="center">
  <strong>Alarmı kur, uyu. Fiyat geldiğinde işlem kendi kendine girer.</strong>
</p>

<p align="center">
  <a href="#"><img src="https://img.shields.io/badge/status-in%20development-orange.svg" alt="status" /></a>
  <a href="#"><img src="https://img.shields.io/badge/built%20on-BULK-black.svg" alt="BULK" /></a>
  <a href="#"><img src="https://img.shields.io/badge/rust-1.93+-b7410e.svg" alt="rust" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="license" /></a>
</p>

---

## Sorun

Her trader'ın her gün yaptığı bir şey var: fiyat alarmı kurmak.

Ve bu özellik temelde bozuk. Alarm 03:00'te çalar, sen uyursun, fırsat kaçar. Ya da uyanıp telefonu açana kadar fiyat gitmiştir. **Alarm ile işlem arasındaki boşluk**, retail trader'ın en çok para kaybettiği yer.

## Çözüm

PUSU'da alarm sadece haber vermez — **işlemi de yapar.**

```
Alarm: BTC 88.400'e düşerse
  └─ Alım emri + stop + hedef, tek imzayla hazır
     Fiyat geldiğinde borsa kendisi çalıştırır
```

Kullanıcı bir kez imzalar, telefonu kapatır. Fiyat geldiğinde emir girer, dolduğu anda koruması otomatik kurulur.

## Alarmlar iki sınıfa ayrılır — ve bunu gizlemiyoruz

| | 🔒 Borsada yaşayan | ⚡ Watcher'da |
|---|---|---|
| **Koşul** | Fiyat eşiği kesiyor | Mum kapanışı, indikatör, çok koşullu |
| **Yürüten** | BULK'ın kendisi | PUSU watcher'ı |
| **Sunucumuz ölürse** | Emir yine çalışır | Alarm kaçar |

Her alarm kartında rozeti var. Hangi alarmın neye bağlı olduğunu kullanıcı bilir — çünkü parası söz konusu.

## Güvenlik

**Anahtarınız bizde değil. Hiçbir zaman olmayacak.**

- Emirleri **siz** imzalarsınız, tarayıcınızda. PUSU imzalı mesajı taşır, üretmez.
- PUSU ayrı bir **sub-account**'ta çalışır — ana bakiyenize dokunamaz.
- **Kill switch:** builder onayını geri çekin, bekleyen tüm alarmlar ölür.

## Ekonomi

PUSU, [BULK Builder Codes](https://docs.bulk.trade/bulk-exchange/builder-codes) üzerinden **2 bps** alır — sadece gerçekleşen işlemlerden.

Onayladığınız kadarını keseriz, fazlasını değil. Fee imzaladığınız emrin içinde açıkça görünür ve istediğiniz an iptal edebilirsiniz. Alarm kurmak ücretsiz.

## Durum

Geliştirme aşamasında. [`docs/PLAN.md`](docs/PLAN.md)

## Lisans

MIT
