# Автономный RGB-холст

`runtime::Host` сохраняет прежний ручной `tick` для детерминированного стенда. Явный `enable_autonomous` запускает поток `rgb-canvas`, который вызывает тот же `Core::tick` с общими monotonic timestamps. Окно больше не должно вызывать tick для продвижения анимации и отправки кадров RGB. I/O остаётся в прежних отдельных Output workers, с их generation/sequence, bounded latest frame, cancellation и endpoint leases. Источники: `src/runtime/{mod.rs,autonomous.rs,output.rs}`.

## Потоки и состояние

- Setup и последний общий кадр передаются через Arc. Host передаёт GUI согласованный Arc snapshot холста и canvas frames без очереди и дополнительного deep clone самого FrameView. Последующая маска Runtime для экранного предпросмотра по-прежнему обновляет свой буфер. Некоторое старое изображение может оставаться в UI до следующего чтения; драйверы продолжают получать новые кадры.
- Core mutex сериализует короткие команды и один расчёт кадра; device I/O внутри него не выполняется. Изменение сцены сохраняет прежние правила Renderer::configure. Большой CPU-кадр всё ещё может задержать команду GUI на длительность расчёта; это не hard real-time гарантия.
- Ожидание использует ближайший deadline scene/device cadences. Прежняя логика пропуска опоздавших периодов сохранена, очередь пропущенных кадров не создаётся. При отсутствии живых выходов поток ждёт сигнала, а ручной UI tick может обновлять только предпросмотр.
- Stop отменяет clock, будит его и останавливает собственные outputs. Drained учитывает завершение render thread и I/O owners. Заблокированный factory/driver не освобождает endpoint lease от одного завершения clock. После drain явное повторное attach создаёт новый clock.
- Ошибка рендера/ожидания видна через ClockStatus и Host::tick, outputs останавливаются. Последующий исправленный setup/explicit attach может восстановить clock. Источники: `Host::{enable_autonomous,tick,stop,reap,drained}` и tests.

Windows: private auto-reset event и one-shot waitable timer с HIGH_RESOLUTION flag. Если флаг не принят, используется обычный waitable timer; ClockStatus.high_resolution=false, высокая частота не обещается. Аргументы объектов без имён, нет вызовов timeBeginPeriod/NtSetTimerResolution и глобальной смены timer policy. HIGH_RESOLUTION поддерживается с Windows10 1803 согласно [CreateWaitableTimerExW](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createwaitabletimerexw); относительные отрицательные due times задаются в 100ns согласно [SetWaitableTimerEx](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-setwaitabletimerex). Реализация: `runtime/autonomous/wait.rs`. Ветка fallback на старой Windows в данном прогоне не проверялась.

## Интеграция и пределы

`Runtime::enable_autonomous_rgb` включает режим. Он сохраняется при смене Document/output plan и simulated power refresh; обычный интерактивный UI и software selftest включают его при запуске, plain snapshot сохраняет ручной режим. Открываемый UI profile также наследует режим. Флаг не включает native devices, discovery или cooling. Источники: `app/runtime.rs`, `ui/windows.rs`.

Общий RGB-холст и прямой capture->RGB feed работают без тиков окна; bounded slot и freshness guards описаны в `ambient-frame-feed.md`. Runtime::tick теперь продвигает Coordinator вне окна, включая расписание и публикацию температур LCD (`runtime-coordinator.md`). UI ingress общего Registry и child supervisor ещё не перенесены; полная независимость всех источников от окна не достигнута.

## Проверки

Actual software tests проверяют продвижение без UI ticks, точные цвета из прежнего Renderer, mute, stop/no extra writes, удержание занятого I/O/lease, восстановление после ошибки, Runtime configuration и simulated resume без OS power API. Actual memory cooling/sensor test оставляет прежними epoch/generation, owner/reader counts и restore/drop counts при lighting replacement.

`examples/rgb_cadence_stand.rs` запускает только memory sink20LED, без Host/Runtime/window ticks. `work/autonomous-cadence-final/report.json`: два измеряемых секунды на настройку,25→25,49→49,60→60,144→144 submitted frames/s; high-resolution timer=true. P95 интервалов:40.272/20.593/17.023/7.058ms соответственно. Native BLE и остальные transports НЕ участвовали, их реальные ограничения не изменились. Это проверка cadence на данном ПК, не сравнение CPU/RAM, не тест тяжёлого холста и не гарантия физической частоты/сохранения результата под любой нагрузкой.
