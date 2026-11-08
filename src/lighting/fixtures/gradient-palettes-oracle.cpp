// SPDX-License-Identifier: GPL-2.0-or-later
#include <QImage>
#include <QPainter>
#include <QLinearGradient>
#include <fstream>
#include <cstdint>
int main(){std::ofstream out("gradient-palettes-fixtures.bin",std::ios::binary);uint32_t seed=0x317fab21;auto random=[&](){seed^=seed<<13;seed^=seed>>17;seed^=seed<<5;return seed;};for(int caseID=0;caseID<8192;caseID++){unsigned char n=1+caseID%8;out.write((char*)&n,1);QGradientStops stops;float step=1.f/n;unsigned int same=random();for(int i=0;i<n;i++){unsigned int c=random();switch((caseID/8)%16){case 0:c=same;break;case 1:c=i%2?0xffffff:0;break;case 2:c=0;break;case 3:c=0xffffff;break;case 4:c&=255;break;case 5:c&=0xff00;break;case 6:c&=0xff0000;break;case 7:c=(caseID/128)%256;c|=c<<8|c<<16;break;}unsigned char rgb[]={static_cast<unsigned char>(c),static_cast<unsigned char>(c>>8),static_cast<unsigned char>(c>>16)};out.write((char*)rgb,3);stops<<QGradientStop(i*step,QColor(rgb[0],rgb[1],rgb[2]));}QLinearGradient g(QPointF(0,0),QPointF(100,0));g.setSpread(QGradient::PadSpread);g.setStops(stops);QImage image(100,1,QImage::Format_RGB32);QPainter p(&image);p.fillRect(QRectF(0,0,100,1),QBrush(g));p.end();for(int x=0;x<100;x++){auto c=image.pixelColor(x,0);unsigned char rgb[]={static_cast<unsigned char>(c.red()),static_cast<unsigned char>(c.green()),static_cast<unsigned char>(c.blue())};out.write((char*)rgb,3);}}return out.good()?0:1;}
