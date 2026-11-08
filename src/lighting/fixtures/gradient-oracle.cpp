#include <QImage>
#include <QPainter>
#include <QLinearGradient>
#include <fstream>
int main(){std::ofstream out("gradient-fixtures.bin",std::ios::binary);for(int n=1;n<=8;n++){QGradientStops stops;float step=1.f/n;for(int i=0;i<n;i++)stops<<QGradientStop(i*step,QColor((i*71+255)%256,(i*113+85)%256,(i*197)%256));QLinearGradient g(QPointF(0,0),QPointF(100,0));g.setSpread(QGradient::PadSpread);g.setStops(stops);QImage image(100,1,QImage::Format_RGB32);QPainter p(&image);p.fillRect(QRectF(0,0,100,1),QBrush(g));p.end();for(int x=0;x<100;x++){auto c=image.pixelColor(x,0);unsigned char rgb[]={static_cast<unsigned char>(c.red()),static_cast<unsigned char>(c.green()),static_cast<unsigned char>(c.blue())};out.write((char*)rgb,3);}}return out.good()?0:1;}
