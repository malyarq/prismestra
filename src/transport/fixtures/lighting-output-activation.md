# Связь интерфейса с аппаратным выводом

Новый явный путь запуска: `pc-control-ui.exe --project <portable-profile> --native-lighting <private-local.json> --state-dir <private-state-root>`. Это инструкция для изолированного аппаратного стенда. Такой запуск на рабочем ПК в этой итерации НЕ выполнялся; live hardware acceptance отсутствует. Без `--native-lighting` приложение стартует в программном предпросмотре; явная кнопка «Применить» на странице подключений теперь может выбрать аппаратные фабрики, см. `lighting-binding-editor.md`. Загрузка проекта сама по себе не даёт аппаратных фабрик. Источники: `ui/windows.rs::options/run_inner/attach_outputs`.

`app/lighting_outputs.rs::Local` хранится отдельно от переносимого профиля:

```json
{
  "version": 1,
  "devices": {
    "door0": {"type": "ble", "address": 73014444033}
  },
  "monitors": {"monitors": []},
  "door_brightness": 15
}
```

Это пример структуры с вымышленным BLE-адресом, не готовые привязки пользователя. ID`door0` должен существовать в логическом Setup с совместимым driver; HID/serial/ENE bindings используют существующую строгую схему `devices::Binding`. `Local::read` ограничивает размер4MiB, отвергает повторные JSON-поля, неизвестные поля/version/brightness, прямой UNC/device path и reparse leaf. Вложенные mount points/сетевые mapped drives этим чтением отдельно не аттестованы; не считать path-prefix доказательством физического локального диска.

## Действующий маршрут кода

- Полная document/binding/monitor validation и создание отложенных фабрик выполняются до первого worker. Неполная/несовместимая конфигурация не подменяется фиктивным устройством. Ошибки старта отдельных I/O workers отражаются их статусами; это не обещание аппаратной транзакции all-or-nothing.
- RGB: `runtime::native::bound_factory` возвращает повторно вызываемую фабрику+canonical endpoint keys; Runtime сохраняет её для retry/resume/reconfiguration. В ней используются прежние Skydimo serial, Door BLE, Fusion2 HID, Valkyrie HID и ENE PIIX4 adapters. Сами native constructors вызываются только на owning worker thread. Старый `attach_bound` использует ту же фабрику.
- LCD: Runtime получает новый Controller из текущих настроек и asset store, затем точный TT display HID через прежний `open_display`. Cancellation проверяется до/после открытия; transport сохраняет собственные bounded request deadlines. Жидкостный MI00 interface не подменяет display interface.
- Ambient: logical source keys сопоставляются explicit Windows monitor names и попадают в `ambient_windows::Native` на отдельном capture worker. Синтетический Feed в этом режиме не подключается. Сохранение профиля/карты не сериализует физические адреса.
- В UI меняются заголовок, подписи групп/расписания и статусы. Hardware-lighting caption явно оставляет охлаждение на стенде. Тестовые40°C в этом режиме отклоняются. Invalid native profile/binding не останавливает текущий Runtime при Apply/Open: preflight выполняется прежде замены. Ошибки интерактивного запуска видны в MessageBox, а diagnostic/internal paths не получают этот modal.
- При старте с `--native-lighting` состояние/asset store размещаются в дочернем `Lighting` каталоге выбранного state root. Применение кнопкой в уже открытом окне сохраняет текущий state/assets/journal root и намерения групп; оно не переносит и не удаляет файлы. Нативные датчики и охлаждение не включаются флагом подсветки.

Источники: `app/lighting_outputs.rs`, `runtime/native.rs`, `ui/windows.rs`, `ui/paint.rs`. Условие режима проверено до открытия проекта/локального файла. `--snapshot`/`--selftest` запрещают native lighting и native metadata discovery. Значение опции не может поглотить следующий `--flag`; повторный native-lighting flag запрещён.

## Доказательства и незакрытое

Три новых tests в `lighting_outputs/tests.rs`: explicit local plan→real Runtime→scene renderer→own RGB worker→20LED submissions→stop/drain с memory sink; ошибка второй deferred factory/missing binding до любого открытия; strict private file decode/duplicate rejection/no binding leakage в portable document. Native BLE factory строится, но не вызывается. Новый options test проверяет opt-in, недостающий/повторный flag, diagnostic запреты; реальные устройства не перечисляет. Это подтверждение glue/worker path, не физического протокола.

Нативный Windows UI selftest запускается offscreen/noactivate с программными factories. Результаты окончательного запуска: `work/lighting-activation-ui-accepted/`, helpers в `work/lighting-activation-ui-test.py`. Actual binary guard checks подтверждают отказ snapshot+native и swallowed-selftest value до запуска; отсутствующий private файл специально исключает hardware даже при ошибке guard. Финальные исходники и проверки указаны в верхнем HANDOFF. Режим аппаратного вывода на рабочем ПК не запускался. Per-process endpoint leases и existing vendor process guards не являются доказательством machine-wide exclusivity со всеми сторонними RGB-программами; отдельная cross-process/native приёмка остаётся. GUI/внешние ingress обслуживаются WM_TIMER16ms, Runtime::tick теперь на отдельном Coordinator (`runtime-coordinator.md`), а общий RGB canvas имеет отдельный real-clock render thread (`autonomous-rgb-clock.md`). Software sink достиг25/49/60/144frames/s без UI ticks; это не гарантия частоты настоящего транспорта. Ambient frames передаются прямо в Host через bounded slot (`ambient-frame-feed.md`); scheduler/source/cooling coordinator и публикация температур LCD выполняются фоновым Runtime::tick; UI Registry/child-supervisor ingress пока остаются на окне.

Остаются: live acceptance каждого поддерживаемого adapter/monitor/LCD, сенсоры CPU/RAM и end-to-end temperature bindings, аппаратное охлаждение и его native coordinator/recovery, unsupported protocols из PARITY, полноценная упаковка/обновление/лицензии и matched CPU/RAM после полноты. Графическое редактирование/применение/возврат реализовано отдельно; остальные пункты этим не закрыты. Предварительный режим не является готовым публичным релизом.
