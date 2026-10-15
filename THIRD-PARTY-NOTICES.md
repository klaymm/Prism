# Сторонние компоненты

Prism распространяется по лицензии GPL-3.0-or-later (см. `LICENSE`). В сборку входят следующие сторонние компоненты.

## Встроено в prism.exe и prism-setup.exe

| Компонент | Лицензия | Что используется |
|---|---|---|
| [Slint](https://slint.dev) 1.18 | GPL-3.0-only (из вариантов GPL-3.0 / Royalty-free / Software) | Интерфейс, рендеринг окна и трея |
| [Inter](https://github.com/rsms/inter) 4.1 | SIL Open Font License 1.1, © The Inter Project Authors - [`licenses/Inter-OFL-1.1.txt`](licenses/Inter-OFL-1.1.txt) | Шрифт. Изменённая копия под именем "Prism Inter": включены варианты cv11, ss01 (и tnum у "Prism Inter Tabular"), оставлены латиница, кириллица и знаки |
| [Lucide](https://lucide.dev) 1.48 | ISC, © Lucide Icons and Contributors - [`licenses/Lucide-ISC.txt`](licenses/Lucide-ISC.txt) | Иконки интерфейса (`crates/prism-ui/ui/icons`) |
| [flag-icons](https://github.com/lipis/flag-icons) 7.5 | MIT, © Panayiotis Lipiridis - [`licenses/flag-icons-MIT.txt`](licenses/flag-icons-MIT.txt) | Флаги стран, обрезанные в круг (`app/assets/flags`) |
| Библиотеки Rust (tokio, reqwest, serde, winit и др.) | MIT и/или Apache-2.0 | Список и лицензии - `cargo tree`, тексты в исходниках пакетов |

## Поставляется вместе с Prism (отдельные программы)

| Компонент | Лицензия | Исходный код |
|---|---|---|
| [Xray-core](https://github.com/XTLS/Xray-core) | MPL-2.0 | https://github.com/XTLS/Xray-core |
| [sing-box](https://github.com/SagerNet/sing-box) | GPL-3.0-or-later | https://github.com/SagerNet/sing-box |
| Правила роутинга [roscomvpn](https://github.com/hydraponique/roscomvpn-routing) (geoip.dat, geosite.dat, DEFAULT.DEEPLINK) | по условиям репозитория-источника | https://github.com/hydraponique/roscomvpn-routing |
