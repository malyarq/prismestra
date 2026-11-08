#include "../../../../rgb-openrgb-2026-11-05/effects-source/OpenRGB/qt/hsv.h"
#include "../../../../rgb-openrgb-2026-11-05/effects-source/Dependencies/SimplexNoise/src/SimplexNoise.h"
#include <fstream>
int main(){std::ofstream out("math-fixtures.bin",std::ios::binary);SimplexNoise noise(.12f,3.9f,.75f,.5f);for(int i=0;i<4096;++i){unsigned int c=((i*71)%256)|(((i*113)%256)<<8)|(((i*197)%256)<<16);hsv_t hsv;rgb2hsv(c,&hsv);auto back=hsv2rgb(&hsv);float x=(i%71)*.3f-10.f,y=(i%43)*.2f-5.f,z=i*.01f;float n=noise.fractal(2,x,y,z);out.write((char*)&hsv.hue,4);out.write((char*)&hsv.saturation,1);out.write((char*)&hsv.value,1);out.write((char*)&back,4);out.write((char*)&x,4);out.write((char*)&y,4);out.write((char*)&z,4);out.write((char*)&n,4);}return out.good()?0:1;}
