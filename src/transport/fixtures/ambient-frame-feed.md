# Экранная подсветка без тиков окна

Capture worker передаёт готовые RGB-массивы непосредственно в автономный Host через ограниченный latest-slot. Runtime больше не копирует кадры на каждом UI tick. Источники: `app/capture_worker.rs`, `app/runtime.rs`, `runtime/frame_feed.rs`, `runtime/autonomous.rs`.

## Границы и отказы

- Единственный Publisher имеет фиксированное поколение; Reader не вызывает provider и не выполняет аппаратный I/O. Последний immutable snapshot и пиксели передаются через Arc, очереди кадров нет. Чужие маршруты, неверные размеры, будущие и обратные timestamps отклоняются до замены принятого состояния. Источник: `runtime/frame_feed.rs` и его tests.
- Host проверяет роль Ambient и число LED до подключения. Замена feed отключает прежний Reader; поздняя публикация прежнего поколения не достигает нового выхода. Отсутствующий, закрытый или просроченный маршрут становится чёрным отдельно от остальных. Источники: `Core::{bind_feed,refresh_feed,feed_deadline}` и `runtime/frame_feed/tests.rs`.
- Свежесть ограничена TTL источника, включая случай зависшего poll. Истечение проверяет RGB clock независимо от capture и окна. Stop/terminal error закрывает slot; это не доказательство завершения зависшего provider или освобождения его lease: drain по-прежнему ждёт владельца. Источники: `capture_worker::Worker::stop/run`, `runtime/autonomous.rs`, `app/runtime/ambient_feed_tests.rs`.
- Быстрые off/on сохраняют барьер неактивности: кадр от poll, начатого до выключения, не возвращается из старого cache. Явные команды меняют capture intent сразу через Runtime::bump. Runtime::tick теперь выполняется фоновым Coordinator (`runtime-coordinator.md`); UI ingress общего Registry и child supervisor ещё не перенесены. Источник: `app/runtime.rs` и `ambient_feed_tests.rs`.
- Capture ждёт реальные deadlines Controller для источников, выходов и TTL. Вторая независимая периодическая сетка удалена: она приводила к пропускам опросов при фазовом дрейфе. Ожидание использует существующий private event/waitable timer; фактический high-resolution/fallback отражён в snapshot. Источники: `Controller::next_due_us`, `capture_worker::run`, `runtime/autonomous/wait.rs`.

TTL короче периода источника намеренно может давать чёрный промежуток. Latest-slot может объединять промежуточные кадры; количество отправок не равно числу уникальных изображений. Это не hard real-time гарантия.

## Принятые проверки

`work/frame-feed-final-tests.{out,exit}`: 1049 PASS (980 unit + 69 integration), 6 ignored helpers, exit0. Clippy all-targets с -Dwarnings и build bins: `work/frame-feed-final-{clippy,build}.{out,exit}`, exit0. Нативный GUI на программных фабриках, offscreen/noactivate, 96/144 DPI: `work/frame-feed-ui-accepted/results.json`, оба exit0. Аппаратные factories/discovery выключены.

`examples/ambient_cadence_stand.rs` запускает синтетические изображения и memory sink150LED без Runtime/Host/window ticks. В `work/ambient-cadence-accepted/report.json` за окна по 2 секунды:

| Запрошено, Гц | Опросы источника/с | Отправки RGB/с | Изменения цвета за окно |
|---|---|---|---|
| 25 | 25 | 25 | 49 |
| 49 | 49 | 49 | 97 |
| 60 | 60 | 60 | 111 |
| 144 | 144 | 144 | 287 |

Оба private timers high-resolution=true; после drain clock остановлен, native writes0. Ранние `ambient-cadence-first` и `ambient-cadence-final` относятся к отвергнутой двойной cadence и не являются финальным результатом. Это не замер DXGI/USB/BLE, тяжёлой сцены или экономии CPU/RAM; рабочие OpenRGB/FanControl/подсветка не переключались.
