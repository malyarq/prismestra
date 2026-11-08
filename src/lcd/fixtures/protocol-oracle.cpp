// SPDX-License-Identifier: GPL-2.0-or-later
// Define LCD_PROTOCOL_HEADER to the original header; link original Protocol.cpp.
#ifndef LCD_PROTOCOL_HEADER
#error Define LCD_PROTOCOL_HEADER to the original Protocol.h
#endif
#include LCD_PROTOCOL_HEADER
#include <cstdint>
#include <fstream>
#include <iostream>
#include <vector>
static void put(std::ostream& out,std::uint64_t v,int n){for(int i=0;i<n;i++)out.put(static_cast<char>((v>>(8*i))&255));}
static QByteArray structural(){return QByteArray::fromHex("ffd8ffc000110801e001e003011100021100031100ffda0002ffd9");}
int main(int argc,char**argv){if(argc!=2)return 2;std::ofstream out(argv[1],std::ios::binary);out.write("LCP1",4);put(out,512,4);std::uint32_t state=0x317fab21;auto random=[&](){state^=state<<13;state^=state>>17;state^=state<<5;return state;};
for(int n=0;n<512;n++){std::uint32_t length,index,seed=random();if(n<255){length=tt::MaxFrame;index=n;}else{const std::uint32_t edge[]={0,1,1019,1020,1021,tt::MaxFrame,tt::MaxFrame+1};length=n<262?edge[n-255]:random()%(tt::MaxFrame+2);index=n<262?0:random()%300;}QByteArray frame(length,'\0');for(int i=0;i<frame.size();i++)frame[i]=static_cast<char>((i*31u+seed)&255);auto report=tt::packet(frame,index);put(out,length,4);put(out,index,4);put(out,seed,4);put(out,report.size(),2);out.write(report.constData(),report.size());}
std::vector<QByteArray> cases;auto base=structural();cases.push_back(base);cases.push_back(QByteArray());cases.push_back(base.left(base.size()-1));auto padded=base; padded.insert(padded.size()-2,QByteArray(tt::MaxFrame-base.size(),'x'));cases.push_back(padded);padded.insert(padded.size()-2,'x');cases.push_back(padded);cases.push_back(base.left(21)+base.mid(2));cases.push_back(QByteArray::fromHex("ffd8ffda0002ffd9"));cases.push_back(base.left(21)+QByteArray::fromHex("ffd9"));cases.push_back(QByteArray::fromHex("ffd8ffffffffffffd9"));
for(int i=0;i<160;i++){auto value=base;if(i<base.size())value[i]=static_cast<char>(quint8(value[i])^1);else if(i<2*base.size())value[i-base.size()]=static_cast<char>(random());else{value.insert(value.size()-2,QByteArray(i,'x'));if(i%3==0)value[13]=0x22;}cases.push_back(value);}
put(out,cases.size(),4);for(const auto& value:cases){QString error;const bool ok=tt::validateJpeg(value,error);const auto message=error.toUtf8();put(out,value.size(),4);out.write(value.constData(),value.size());put(out,ok?1:0,1);put(out,message.size(),2);out.write(message.constData(),message.size());}out.close();if(!out.good())return 1;std::cout<<"PASS:512 original packet reports and "<<cases.size()<<" original structural JPEG outcomes\n";return 0;}
