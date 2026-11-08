# Граница завершения Windows I/O

`windows_io::run_for` используется HID feature transfers, PawnIO и guardian pipe. Сигнал manual-reset event сам по себе не подтверждает завершение операции. Теперь перед ожиданием событие сбрасывается, затем `GetOverlappedResult(FALSE)` проверяет результат: завершение между сбросом и запросом видно запросу, более позднее завершение будит ожидание. `ERROR_IO_INCOMPLETE`/`ERROR_IO_PENDING` сохраняют borrowed OVERLAPPED и буферы. Таймаут/отмена вызывают CancelIoEx, затем тот же опрос до окончательного результата. Это только lifetime отдельного запроса, не разрешение native recovery и не доказательство отсутствия поздних аппаратных действий. Источник: `src/transport/windows_io.rs`.

Номинальный deadline сохраняется: default250ms, run_for1..2500ms. Cancel drain может ждать неисправное ядро дольше; hard wall-clock bound не заявляется. Callback обязан вернуть непосредственный OS result, сохранить handle/event и не panic после submission. Источники: unsafe contract `run`/`run_for`, вызовы в `feature_hid_windows.rs`, `pawnio_windows.rs`, `guardian_pipe.rs`.

Новый тест `owned_pipe_early_event_requires_real_completion_or_cancellation_drain` запускает настоящий pending TransactNamedPipe только на собственном локальном pipe и вручную сигналит event до завершения. Проверяются delayed reply (точные65bytes), timeout и активная отмена. Server/thread handles принадлежат тесту, присоединяются до утверждений; не открываются аппаратные устройства. Полный целевой прогон7PASS: `work/io-completion-targeted.out`.

## Статический разбор установленного PawnIO

Исследован файл на диске `C:/Program Files/PawnIO/PawnIO.sys`,85344bytes, SHA256 `e19f274b9f3c917445bff19a80bbe2ee34a54995e62be50073122b20d86af900`. В Craft Anything target`t_e19f274b9f3c`, overview evidence`ev_cef421a146c5d5d287553a9a191a683f04b119a5b8bbfced0dcf00cc9c8400d0`:308functions,118strings,70imports; stripped names и6unresolved indirect jumps ограничивают покрытие. Сам файл не запускался и device не открывался.

Вывод по конкретному просмотренному пути: dispatcher`sub_140001250` вызывает execute wrapper`sub_14000a5e0` до IofCompleteRequest; wrapper берёт fast mutex, отображает буферы, вызывает interpreter`sub_140007160`, освобождает отображения/mutex и возвращается. Это derived static finding`fnd_1`, а не аппаратный runtime test. Evidence: `ev_6c578bef6970494365707002cb2e11aa0218f5504e629e968b5d902aedc42833`, `ev_b64abc3efaede913f8724d0431709069706f8c207934885bf656826f94d630af`.

Свежие исходники закреплены commit`9d52965895588b0ffa3703b72eec75ba19f4ccc0`, URL+SHA каждого файла в `work/pawnio-driver-source-2026-11-08/manifest.json`. Установленный файл отличается: dispatcher обрабатывает0xa1b22004 как чтение DWORD, который CREATE/CLOSE увеличивают/уменьшают. Поэтому считать этот IOCTL version query по свежему source нельзя. Это вывод по тому же dispatcher evidence, не вызов IOCTL.

UNKNOWN: идентичность загруженного в память драйвера и полный контракт отсутствия late I/O во всех native/callback descendants. Дисковая подпись/hash, process exit и completed IRP не заменяют эти доказательства. Craft unknown`unk_90f4a3794b73afc6`. Native coordinator/retirement/new generation и isolated signed-driver acceptance остаются отдельными незавершёнными требованиями полной цели. Рабочее охлаждение не переключалось.
