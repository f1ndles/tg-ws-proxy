# TG WS Proxy — Magisk модуль

Magisk модуль для ускорения работы Telegram через WebSocket + Cloudflare прокси.

Форк [Flowseal/tg-ws-proxy](https://github.com/Flowseal/tg-ws-proxy), движок на Rust от [valnesfjord/tg-ws-proxy-rs](https://github.com/valnesfjord/tg-ws-proxy-rs).

## Как работает

Telegram → локальный MTProto (127.0.0.1:1443) → tg-ws-proxy → WSS через Cloudflare → Telegram DC

## Установка

1. Скачай ZIP из [Releases](https://github.com/f1ndles/tg-ws-proxy/releases)
2. Установи через Magisk / KernelSU
3. Перезагрузи устройство
4. Открой WebUI модуля и нажми **Запустить**
5. Примени прокси-ссылку в Telegram

## Настройка Cloudflare домена ( для форк креаторов)

Добавь A-записи (Proxied 🔶) в Cloudflare DNS:

| Name | IPv4 |
|------|------|
| kws1, kws1-1 | 149.154.175.50 |
| kws2, kws2-1 | 149.154.167.51 |
| kws3, kws3-1 | 149.154.175.100 |
| kws4, kws4-1 | 149.154.167.91 |
| kws5, kws5-1 | 149.154.171.5 |
| kws203, kws203-1 | 91.105.192.100 |

SSL/TLS → Overview → **Flexible**

## Настройки

| Параметр | Описание |
|----------|----------|
| CF Domain | Твой Cloudflare домен |
| Default Domains | Автозагрузка рабочих CF доменов с GitHub |
| CF Priority | CF прокси идёт до прямого WS |
| CF Balance | Балансировка между CF доменами |
| DC IP | IP датацентров Telegram |

## Credits (отдельная благодарность)

- [Flowseal/tg-ws-proxy](https://github.com/Flowseal/tg-ws-proxy) — оригинал
- [valnesfjord/tg-ws-proxy-rs](https://github.com/valnesfjord/tg-ws-proxy-rs) — Rust движок
- Александр К - многочисленная поддержка проектов
