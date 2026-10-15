# Сборка Prism

Нужны Rust (stable, MSVC), Visual Studio Build Tools и Node.js 20 или новее для скриптов сборки (npm-зависимостей нет).

```sh
node scripts/fetch-cores.mjs          # скачать xray.exe, sing-box.exe и geo-файлы в resources/ со сверкой sha256
cargo run -p prism                    # клиент для разработки
node scripts/build-installer.mjs --payload-only   # release/prism-dev.pkg для установщика
cargo run -p prism-setup              # установщик для разработки (PRISM_PAYLOAD=release/prism-dev.pkg)
node scripts/build-installer.mjs      # release/Prism-Setup-<версия>.exe со свежими ядрами и правилами
```

`PRISM_PROFILE=dev` запускает клиент и установщик в отдельном профиле "Prism Dev": свои данные (`com.klaymm.prism.dev`), своя папка установки и записи в реестре. Системный прокси и TUN в этом профиле не трогаются, поэтому такая сборка работает рядом с установленным Prism.

Xray и sing-box отдельно от Prism не обновляются, поэтому полная сборка установщика каждый раз скачивает их последние версии. Обновление самого Prism можно проверить в dev-профиле: переменная `PRISM_UPDATE_FEED` подменяет адрес `releases/latest` на свой JSON в том же формате, что отдаёт GitHub.

Тесты:

```sh
cargo test --workspace                 # модульные тесты
cargo test -p prism-core -- --ignored  # сквозной тест: локальный Reality-сервер и настоящие ядра
```
