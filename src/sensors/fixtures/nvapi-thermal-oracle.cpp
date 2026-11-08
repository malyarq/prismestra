// CPU-only: actual NVFC header and extracted unchanged thermal constructor.
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <type_traits>
#include "nvapi.h"
#include "thermal-constructor.inc"
int main() {
    NV_GPU_THERMAL_SETTINGS_V2 value;
    using Sensor = std::remove_reference_t<decltype(value.sensor[0])>;
    printf("{\"size\":%zu,\"alignment\":%zu,\"sensor_size\":%zu,\"offset_version\":%zu,\"offset_count\":%zu,\"offset_sensor\":%zu,\"offset_controller\":%zu,\"offset_min\":%zu,\"offset_max\":%zu,\"offset_temperature\":%zu,\"offset_target\":%zu,\"version\":%u,\"gpu_target\":%d,\"memory_target\":%d,\"all_target\":%d,\"bytes_hex\":\"",
        sizeof(value),alignof(decltype(value)),sizeof(Sensor),offsetof(decltype(value),version),offsetof(decltype(value),count),offsetof(decltype(value),sensor),offsetof(Sensor,controller),offsetof(Sensor,default_min),offsetof(Sensor,default_max),offsetof(Sensor,current_temperature),offsetof(Sensor,target),value.version,int(NV_THERMAL_TARGET::GPU),int(NV_THERMAL_TARGET::MEMORY),int(NV_THERMAL_TARGET::ALL));
    for(size_t i=0;i<sizeof(value);++i)printf("%02x",reinterpret_cast<const unsigned char*>(&value)[i]);
    printf("\"}\n");
}
