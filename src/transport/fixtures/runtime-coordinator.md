# Владелец Runtime вне потока окна

`app/runtime/coordinator.rs::Coordinator` принимает существующий Runtime и продвигает его на отдельном потоке `pc-coordinator`. Самостоятельно backend/discovery/native factory он не создаёт. Ручной Runtime сохраняет прежний API и детерминированное управление временем. Windows App теперь хранит `runtime_client::Client`: обычный запуск и software selftest используют Coordinator; plain snapshot остаётся ручным. Источники: `Coordinator::start`, `ui/windows.rs::App/run_inner/tick`, `ui/windows/runtime_client.rs`.

## Действующая граница

- Один mutex сериализует Runtime tick и команды. `command` получает process timestamp внутри того же lock; borrowed данные не выходят из `read`. Команды не должны ждать устройства, показывать диалог или повторно входить в этот Coordinator. Конкретные transport calls остаются на прежних workers. Источники: `Coordinator::{read,command}`, `Runtime::tick`.
- После tick поток ждёт private Condvar до 50ms либо до команды/stop, без busy loop и глобальной timer policy. Это период надзора за источниками/состоянием, не частота RGB: `start` включает уже существующий автономный RGB clock. Системные монотонные и UTC часы читаются внутри владельца. Это не hard real-time гарантия: длительная запись журнала может задержать следующий tick и команду. Источник: `coordinator.rs::run`.
- Ошибки configuration/persistence остаются в Runtime, по-прежнему требуют явного retry/cancel и не обходят durable barrier. Надзор охлаждения продолжает вызываться до остальных этапов tick. Непригодные часы/паника владельца или команды отмечаются как fatal и останавливают собственные outputs. Источники: `Coordinator::command/run`, `Runtime::tick/configuration_tick`.
- Stop останавливает собственный Runtime и будит владельца. Drained требует завершения и join coordinator thread плюс старые barriers RGB/LCD/capture/sensor/cooling owners. Завершение coordinator не освобождает lease зависшего устройства. Retry возврата охлаждения доступен явно даже при fatal status. Drop останавливает/присоединяет coordinator; он не объявляет заблокированный transport завершённым. Источники: `Coordinator::{stop,drained,retry_cooling_restore,drop}`.

## Программная приёмка

`app/runtime/tests/coordinator_tests.rs` выполняет реальные потоки Coordinator/Runtime/sensor/LCD и запись собственного journal на диске, без окон и UI ticks:

1. Стендовые UTC часы пересекают 10:00 Москвы; PC включается, дверь остаётся выключенной, повторные tick не дублируют durable attempt. Изменения датчика обновляют RAM mean и принятый LCD intent; пропавший участник убирает mean. Это не визуальная приёмка температурного оверлея.
2. Ошибка записи нового профиля сохраняет прежний document; после исправления собственного файла только явный retry завершает применение.
3. Отказ часов и паника команды прекращают продвижение и запрещают дальнейшие обычные команды.
4. Заблокированный !Send sensor остаётся на своём потоке, удерживает lease после завершения coordinator; после возврата poll происходит owner drop/drain, поздняя температура не публикуется.

`app/runtime/coordinator_cooling_stand.rs` использует прежние настоящий GroupPlanner, cooling worker, CohortSafety и DeviceEngine с memory actuator. Потеря источника вызывает revoke/restore; новые 85°C во время удержанного restore не дают команды 85%. После ACK того же владельца управление возобновляется; stop возвращает прежние 37%, один finish/drop и освобождение lease. Factory вызывается один раз. Это не аппаратная проверка FanControl, USB/SMBus/ITE/GPU или межпроцессного recovery.

Точные результаты: `work/coordinator-safety-targeted.{out,exit}` (5 PASS); единый окончательный прогон `work/coordinator-accepted-tests.{out,exit}`: 1054 PASS (985 unit + 69 integration), 6 ignored helpers, exit0; unit117.38s. Clippy all-targets -Dwarnings и build bins: `work/coordinator-final-{clippy,build}.{out,exit}`, exit0. Рабочие охлаждение, подсветка, сеть и power state не менялись. Визуальный интерфейс в этом этапе не изменялся и повторно не принимался.

## Связь с окном и оставшаяся граница

`ui/windows/runtime_client.rs` передаёт короткие команды владельцу и получает Arc<View>; GUI рисует owned данные, без engine guard во время native UI/диалогов. Нужные transport/status snapshots читаются через ограниченные read closures. Версия документа меняется при commit/configure_scene/configure_schedule отдельно от температурных revisions; неизменённые Document/запрошенный Document и samples переиспользуются через Arc. Старый View не меняется после новых команд. Источники: `coordinator::View::capture`, `Runtime::document_revision`, `runtime_client::Client` и новый coordinator view test.

В первой GUI проверке найдена и исправлена неверная связь document cache с intent revision: set_scene/configure_schedule меняют view revision, а не LCD intent revision. `work/coordinator-ui-first/wide/stdout.log` сохраняет actual scene failure; новый отдельный document revision и regression test проверяют точный новый Scene и неизменность старого snapshot. Никакие scene controls/ожидания не ослаблены.

Client сохраняет ошибки фонового Runtime и сообщает новую ошибку окну при poll; активный configuration fault остаётся в View, повторные UI poll не блокируют retry/cancel. `window_client_reports_a_background_persistence_failure_once` сначала воспроизвёл потерю ошибки (`work/coordinator-ui-error-before.out`), затем прошёл после исправления. Fatal owner допускает только stopped-engine maintenance/recovery; обычные команды отвергаются. Tests также различают manual snapshot и background clock (UI timestamps не вводятся в background owner).

Open ждёт drain старого Client, потом создаёт новый с тем же режимом. Apply/Return сохраняют прежний Runtime barrier. Close требует прежние Shutdown/owner/scan/registry/child barriers плюс join coordinator; повторное Close не обходит restore, explicit retry доступен после fatal. Источники: `ui/windows.rs::{open_profile,tick,request_close,close_ready}`, `runtime_client::{request_shutdown,poll_shutdown,retry_shutdown}`, actual close stand.

Конкретный остаток: `sources::Editor::poll` (общий Registry -> injected readings) и UI child-supervisor ingress пока запускает App tick. Собственные attached sensor workers -> Runtime -> LCD/cooling уже продвигаются на фоне, но внешние UI-маршруты при долгой блокировке окна могут перестать обновляться; freshness/leases должны отозвать устаревшие данные. Полная независимость источников/межпроцессного управления ещё не доказана. NEXT перенос этих конкретных ingress с сохранением scope/epoch/stop/restore/lease правил, затем весь исходный native hardware/UI/distribution/benchmark scope. Реальные cooling/native factories на рабочем ПК не запускались.

## Окончательная проверка UI-интеграции

`work/coordinator-ui-verified-tests.{out,exit}`:1058 PASS (989 unit+69 integration),6 ignored helpers,exit0,unit99.83s. `coordinator-ui-verified-{clippy,build}.{out,exit}`: all-targets -Dwarnings/build bins, оба0. `work/coordinator-ui-verified/results.json`:96 DPI manual и144 DPI background/selftest, оба0; native outputs/discovery=false. Wide stdout фактически подтверждает progression160ms без App tick/message pump, затем прежние metadata/save/apply/journal-failure/cancel/open-failure/return/source/LCD/close tests. Итоговые флаги background_runtime=true/autonomous_rgb=true; narrow обаfalse. GUI layout не менялся, проверены generated view и close-blocked image; это software acceptance, не физическая совместимость и не CPU/RAM сравнение.

Первый общий прогон `coordinator-ui-final-tests` имел987PASS и один отказ bundle font-size test: прежний helper create_dir_all принимал старую PID/counter папку. `work/coordinator-ui-fixture-reuse.json` фиксирует старый font.zip29296-5 от16:11, существовавший до упавшего прогона около16:35. Helper теперь резервирует новую папку через atomic create_dir с nonce/counter и ограниченной обработкой collision; прежние данные не удалялись, production bundle export/import не менялись. После исправления полный1057 прогон + GUI0 (`coordinator-ui-accepted-*`), после новой проверки фоновых ошибок окончательный1058+GUI0 (`coordinator-ui-verified-*`).
