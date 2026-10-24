[Русский](README.md) | [English](README.en.md)

# Prism

![Release](https://img.shields.io/github/v/release/klaymm/Prism) ![License](https://img.shields.io/github/license/klaymm/Prism) ![Rust](https://img.shields.io/badge/language-Rust-orange) ![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-blue)

A VPN client for Windows for VLESS + Reality. Russian sites open directly in it, and everything else goes through the VPN. The Xray and sing-box cores run inside, and the window is drawn with Slint, without an embedded browser.

## Features

- **Subscriptions and links** - subscriptions in base64, link list and Xray JSON formats, and `vless://` links (just press Ctrl+V in the window)
- **Two modes** - "Всё, кроме РФ" (all except Russia) sends Russian sites directly, "Глобальный" (global) routes all traffic through the VPN except the local network
- **Custom rules** - lists of sites and addresses that always go through the VPN or always go directly
- **Programs outside the VPN** - in TUN mode the chosen programs go directly, which games that connect to servers by IP need
- **Proxy or TUN** - the system proxy works without administrator rights, TUN sends all system traffic through the VPN
- **Server checks** - ping in two ways and a speed test
- **Updates** - Prism checks for new versions itself and installs them, this can be switched off in the settings

## Installation

Download `Prism-Setup-<version>.exe` from the [Releases](https://github.com/klaymm/Prism/releases) page and run it. Administrator rights are not needed: Prism is installed to `%LOCALAPPDATA%\Programs\Prism`. If a previous version is installed, the installer updates it, and servers, subscriptions and settings are kept.

How to build Prism yourself is described in [docs/build.md](docs/build.md).

## Usage

Add a subscription or a `vless://` link, choose a server and press the connect button.

In the "Всё, кроме РФ" mode Russian sites go directly by the [roscomvpn-routing](https://github.com/hydraponique/roscomvpn-routing) profile. The switch on the "Роутинг" (Routing) page, which is on by default, adds all sites in the .ru, .su and .рф zones and Russian clouds to them. Custom rules on the "Роутинг" page decide which sites and addresses always go through the VPN and which go directly, and with TUN also which programs.

Prism normally turns on the Windows system proxy, which needs no administrator rights. TUN mode asks for administrator rights.

More about what the client can do and how it works is in [docs/features.md](docs/features.md).

## Updating and removal

Prism checks the latest release at startup and every 6 hours. A new version is installed by itself at startup or on a button press. Prism can be removed through "Apps" in Windows or with `uninstall.exe`, together with the settings if you wish.

## Limitations

Works only on Windows 10 and 11 (x64). Understands only VLESS: `vmess://`, `trojan://`, `ss://` and other links are not accepted.

## License

GPL-3.0-or-later, the text is in [LICENSE](LICENSE). Third-party components are listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
