# Расписание: native differential fixtures

`schedule-native.bin`: original `SceneSchedule.h` из соседнего C++ проекта, QtCore6.8.3,4096syntheticcases. Генератор `schedule-oracle.cpp` компилируется с `SCENE_SCHEDULE_HEADER`, указывающим на original header, и Qt6Core; installed RGB приложения не нужны и не запускаются.

SHA256 `1316cd9d0f7a885c0b5f1c6110556cc76494cf3dc84b03f4af2eb4f28408387f`. ФорматLE: magicSCH1,u32count, затем40bytes/record: UTCi64,minutes[3]u16,PCoccurrencei64,PConu8,doorOccurrencei64,doorOnu8,nextUTCi64.

Coverage:2048defaulttimes10:00/17:00/02:00 и2048randomdistincttimes,2025..2035, ±1secondaroundevents, midnight/year/leap rollover. `schedule::tests::native_qt_schedule_differential_4096_cases` сравнивает latest PC/door и next с actual Qt outputs.

Это finite coverage дляEurope/Moscow. DST в других зонах, failure/backoff/persistence и observeclockrollback проверяются отдельно вRust, этимfixtureне доказываются. Формат не содержит userIDs/profiles/paths. Подробные sourcehashes, compilationcommands и manifest сохраняются локально в `work/schedule-oracle`.
