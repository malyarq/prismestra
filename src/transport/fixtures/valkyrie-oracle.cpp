// SPDX-License-Identifier: GPL-2.0-or-later
// Links unchanged OpenRGB Valkyrie controller/wrapper against memory-only HID mocks.
#include "RGBController_ValkyrieKeyboard.h"
#include <fstream>
#include <array>
#include <cstring>
#include <iostream>
#include <stdexcept>
#include <cstdlib>
struct Action{unsigned char kind;std::array<unsigned char,65> packet;};
static std::vector<Action> actions;
extern "C" int hid_send_feature_report(hid_device*,const unsigned char* data,size_t n){if(n!=65)std::abort();Action a{'S',{}};std::memcpy(a.packet.data(),data,n);actions.push_back(a);return int(n);}
extern "C" int hid_get_feature_report(hid_device*,unsigned char* data,size_t n){if(n!=65)std::abort();Action a{'G',{}};std::memcpy(a.packet.data(),data,n);actions.push_back(a);std::memset(data,0xa5,n);data[0]=0;return int(n);}
extern "C" int hid_get_serial_number_string(hid_device*,wchar_t*,size_t){return -1;}
extern "C" void hid_close(hid_device* d){delete d;}
static uint32_t state=0x2b6c7d91;
static uint32_t random32(){state^=state<<13;state^=state>>17;state^=state<<5;return state;}
static void byte(std::ofstream& o,uint8_t n){o.put(char(n));}
static void u16(std::ofstream& o,uint16_t n){byte(o,n&255);byte(o,n>>8);}
static void u32(std::ofstream& o,uint32_t n){u16(o,n&65535);u16(o,n>>16);}
int main(int argc,char** argv){
    if(argc!=2)return 2;std::ofstream out(argv[1],std::ios::binary);if(!out)return 3;out.write("VK990001",8);
    for(int model=0;model<2;model++){
        RGBController_ValkyrieKeyboard keyboard(new ValkyrieKeyboardController(new hid_device,"SYNTHETIC",0x024f,model?3:2,"SYNTHETIC"));
        byte(out,model);u16(out,uint16_t(keyboard.colors.size()));const auto& m=keyboard.zones.at(0).matrix_map;u16(out,m.width);u16(out,m.height);
        for(unsigned int p:m.data)u16(out,p==0xffffffff?0xffff:uint16_t(p));
    }
    u32(out,64);
    for(int model=0;model<2;model++)for(int pattern=0;pattern<32;pattern++){
        RGBController_ValkyrieKeyboard keyboard(new ValkyrieKeyboardController(new hid_device,"SYNTHETIC",0x024f,model?3:2,"SYNTHETIC"));
        for(size_t i=0;i<keyboard.colors.size();i++){
            uint32_t c=pattern==0?0:pattern==1?0xffffff:pattern==2?0xff:pattern==3?0xff00:pattern==4?0xff0000:
                pattern==5?uint32_t(i*255/(keyboard.colors.size()-1))*0x010101:
                pattern==6?uint32_t(i)|(uint32_t(255-i)<<8)|(uint32_t(i*7%256)<<16):
                pattern==7?(i%2?0xffffff:0):pattern==8?(i==0?0xffffff:0):pattern==9?(i+1==keyboard.colors.size()?0xffffff:0):random32()&0xffffff;
            keyboard.colors[i]=c;
        }
        byte(out,model);u16(out,uint16_t(keyboard.colors.size()));for(auto c:keyboard.colors){byte(out,RGBGetRValue(c));byte(out,RGBGetGValue(c));byte(out,RGBGetBValue(c));}
        actions.clear();keyboard.DeviceUpdateLEDs();if(actions.size()!=12)throw std::runtime_error("unexpected transaction size");
        byte(out,uint8_t(actions.size()));for(const auto& a:actions){byte(out,a.kind);out.write(reinterpret_cast<const char*>(a.packet.data()),65);}
    }
    if(!out)return 4;std::cout<<"PASS unchanged OpenRGB source oracle: 2 models, 64 complete RGB transactions, memory HID only\n";
}
