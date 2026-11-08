// CPU-only float expression oracle. No CPUID, vendor DLL or module calls.
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <fcntl.h>
#include <io.h>
#include "amd-tctl-expressions.inc"
int main() {
    if(_setmode(_fileno(stdout),_O_BINARY)==-1) return 1;
    static_assert(sizeof(float)==4);
    for(std::uint32_t code=0;code<2048;++code) {
        for(std::uint32_t flags=0;flags<8;++flags) {
            std::uint32_t raw=(code<<21)|((flags&1)<<19)|(((flags>>1)&3)<<16);
            float value=source_tctl(raw);
            std::uint32_t bits;
            std::memcpy(&bits,&value,4);
            for(unsigned shift=0;shift<32;shift+=8) if(std::putchar((bits>>shift)&255)==EOF) return 1;
        }
    }
    return std::fflush(stdout)==0 ? 0 : 1;
}
