# Installation-bound journal entry

`app::cooling_process::InstallationJournalSession` получает `Arc<NativeState>`, уже открытый через штатную проверку машинного хранилища, и валидированный `ResolvedOwner`. Scope берётся из реального installation ID, а не из caller-selected JSON. Перед транспортом и созданием журнала проверяется known/current reported boot stamp. Это согласованность метаданных, не boot UUID, quiescence или аппаратное разрешение. Источник: `owner_session/installation.rs`, `NativeState`, `resource_catalog::validate_entry`.

Parent `accept` проверяет exact authenticated child, Hello role/owner/target/channel order/scope/boot и удерживает lifetime handles. Имя журнала выводится из connection fingerprint; caller не задаёт произвольный путь. Журнал является непосредственным файлом в retained root. Существующий resource catalog запрещает второй файл/подмножество каналов для уже claimed device. Claim, marker и initial snapshot сохраняются ДО Accepted. При ошибке ACK/последующей связи файлы остаются, автоматического retry/clear нет. Источники: `OwnerSession::receive/into_guardian/acknowledge`, `InstallationJournalSession::accept` и прежние contextual writers.

Host удерживает NativeState через durable writer/resource claim до собственного Drop. `connect` - child-side metadata entry с независимо открытым NativeState; он использует общий Hello/Accepted protocol7. Production owner launcher/аппаратные factories этим API не подключаются автоматически. Wire/context/claim форматы не менялись: role7/context3/claim4. Старый injected путь сохраняется. Источники: новые entry methods и прежний `Expected::new`.

## Проверки и пределы

Четыре новых tests используют настоящие собственные hidden child processes/local pipes и файлы. Проверка security descriptor/установки root в этих fixtures INJECTED, это не UAC/cross-user/machine security acceptance:

- NVIDIA: child получает Accepted, отправляет simulated capture через настоящий Client/Host, получает ACK после durable write, затем закрывает канал. Parent сохраняет original37%, capture/context/marker и наблюдение exact child exit. Root жив до Drop Host, затем handles освобождаются, файлы остаются.
- ITE: выбранные header IDs0/3/5 и точный target доходят до contextual snapshot без bus opener.
- Injected вместо installation scope отклоняется до файлов. Повторная installation session для retained device не получает Accepted; старые claim/snapshot bytes не меняются.
- Empty/duplicate selection и unavailable/changed expected boot stamp отклоняются до transport/files.

Источники: `owner_session/namespace_tests.rs`, `installation.rs::tests`. Целевой окончательный ACK-прогон: `work/installation-entry-ack-tests.out`,4PASS. Child-side `connect` отдельно с независимо OS-открытым privileged root здесь не испытывался; проверен общий wire handshake в child и parent installation entry. Native device ownership/quiescence, recovery actor/coordinator, retirement/new generation и real signed-driver acceptance остаются незавершёнными.

## Длинные пути Windows

Первый capture test обнаружил os error3 при durable replacement: destination253chars был создан Rust API, но имя временного файла `.save-PID-counter` превышало MAX_PATH для сырого MoveFileExW. Предыдущий raw wide path передавался без extended prefix. Логи: `work/installation-entry-targeted-2.out`; после исправления тот же длинный fixture проходит `installation-entry-targeted-3.out`/`installation-entry-ack-tests.out`.

`storage::replace` теперь приводит путь через `std::path::absolute` и extended disk/UNC prefix, сохраняет уже verbatim paths. Нет canonicalize/symlink traversal для подготовки пути, изменения registry/manifest/global policy или copy/reboot flags. Поведение absolute описано [Rust](https://doc.rust-lang.org/std/path/fn.absolute.html), длинный путь MoveFileExW - [Microsoft](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw). Это исправление длины, не новый security boundary для произвольных небезопасных папок.

Отдельный test `long_windows_destination_and_temporary_paths_replace_without_global_policy` проверяет новый и существующий destination>260chars, уже verbatim canonical path, отказ locked destination, сохранение старых bytes и удаление временного файла. Network/UNC share, volume GUID mount и filesystem failure после kernel commit в этом тесте не проверялись. Источник: `storage.rs::tests`. Причины прежнего отдельного Access5 не установлены этим исправлением и не объявляются закрытыми.
