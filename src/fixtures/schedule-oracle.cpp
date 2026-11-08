// SPDX-License-Identifier: GPL-2.0-or-later
// Compile with -DSCENE_SCHEDULE_HEADER=<quoted path to original SceneSchedule.h>.
// Pure original-header oracle: QtCore only, no application, GUI or hardware.
#ifndef SCENE_SCHEDULE_HEADER
#error Define SCENE_SCHEDULE_HEADER to the original header path
#endif
#include SCENE_SCHEDULE_HEADER
#include <cstdint>
#include <fstream>
#include <iostream>
#include <stdexcept>

static void require(bool ok, const char* reason) {
    if (!ok) throw std::runtime_error(reason);
}
static void writeLE(std::ostream& out, std::uint64_t value, int bytes) {
    for (int i = 0; i < bytes; ++i) out.put(static_cast<char>((value >> (8*i)) & 255));
}
int main(int argc, char** argv) {
    if (argc != 2) { std::cerr << "Usage: schedule-oracle output.bin\n"; return 2; }
    try {
        const QTimeZone utcZone(QTimeZone::UTC);
        const auto start = QDateTime(QDate(2025,1,1),QTime(0,0),utcZone).toSecsSinceEpoch();
        const auto end = QDateTime(QDate(2036,1,1),QTime(0,0),utcZone).toSecsSinceEpoch();
        require(SceneSchedule::zone().isValid(), "Europe/Moscow timezone unavailable");
        std::uint32_t seed = 0x317fab21;
        auto random = [&]() { seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5; return seed; };
        std::ofstream out(argv[1], std::ios::binary);
        require(out.good(), "Cannot open output");
        out.write("SCH1",4); writeLE(out,4096,4);
        for (unsigned i=0; i<4096; ++i) {
            SceneSchedule scene;
            require(!scene.enabled && !scene.paused && scene.reconcile && scene.pcSeen==0 && scene.doorSeen==0,
                    "Native defaults changed");
            std::array<unsigned,3> times{600,1020,120};
            if (i>=2048) {
                times[0]=random()%1440;
                do { times[1]=random()%1440; } while (times[1]==times[0]);
                do { times[2]=random()%1440; } while (times[2]==times[0] || times[2]==times[1]);
            }
            for (unsigned n=0;n<3;++n) scene.times[n]=QTime(times[n]/60,times[n]%60);
            require(scene.validTimes(), "Native rejected generated distinct times");
            qint64 now=start+random()%static_cast<std::uint64_t>(end-start);
            // Fixed dates deliberately cover local year, leap-day and month rollover.
            const int year=2025+(i/9)%11;
            QDate date;
            switch ((i/99)%6) {
            case 0: date=QDate(year,1,1); break;
            case 1: date=QDate(year,12,31); break;
            case 2: date=QDate(year,2,QDate::isLeapYear(year)?29:28); break;
            case 3: date=QDate(year,3,1); break;
            case 4: date=QDate(year,6,30); break;
            default: date=QDate(year,1,1).addDays(random()%(QDate::isLeapYear(year)?366:365)); break;
            }
            // 3/4 of samples surround event boundaries with -1/0/+1 second.
            // Every 17th case instead surrounds local midnight.
            if (i%4!=3 || i%17==0) {
                const auto time=i%17==0?QTime(0,0):scene.times[(i/3)%3];
                now=QDateTime(date,time,SceneSchedule::zone()).toSecsSinceEpoch()+static_cast<int>(i%3)-1;
                if (now<start) now+=86400;
                if (now>=end) now-=86400;
            }
            require(now>=start && now<end, "Generated UTC date outside 2025..2035");
            const auto utc=QDateTime::fromSecsSinceEpoch(now,utcZone);
            const auto pc=scene.latest(utc,true);
            const auto door=scene.latest(utc,false);
            const auto next=scene.next(utc);
            require(pc.occurrence>0 && pc.occurrence<=now && door.occurrence>0 && door.occurrence<=now,
                    "Native latest event invalid");
            require(next.isValid() && next.toSecsSinceEpoch()>now && next.toSecsSinceEpoch()<=now+86400,
                    "Native next event invalid");
            require(!scene.due(pc,true) && !scene.due(door,false), "Disabled schedule due");
            scene.enabled=true;
            require(scene.due(pc,true) && scene.due(door,false), "Enabled reconcile missing due");
            scene.paused=true;
            require(!scene.due(pc,true) && !scene.due(door,false), "Paused schedule due");
            scene.paused=false; scene.reconcile=false;
            require(scene.due(pc,true) && scene.due(door,false), "Unclaimed event not due");
            scene.claim(pc,true); scene.claim(door,false);
            require(!scene.due(pc,true) && !scene.due(door,false), "Claimed event still due");
            writeLE(out,static_cast<std::uint64_t>(now),8);
            for (auto time:times) writeLE(out,time,2);
            writeLE(out,static_cast<std::uint64_t>(pc.occurrence),8); writeLE(out,pc.on?1:0,1);
            writeLE(out,static_cast<std::uint64_t>(door.occurrence),8); writeLE(out,door.on?1:0,1);
            writeLE(out,static_cast<std::uint64_t>(next.toSecsSinceEpoch()),8);
        }
        require(out.good(), "Output write failed");
        out.close(); require(out.good(), "Output close failed");
        std::cout << "PASS: 4096 original-header latest/next fixtures; defaults/due/claim checks; UTC2025..2035\n";
        return 0;
    } catch (const std::exception& e) { std::cerr << e.what() << '\n'; return 1; }
}
