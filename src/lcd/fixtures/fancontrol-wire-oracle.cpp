// SPDX-License-Identifier: GPL-2.0-or-later
// Define FANCONTROL_SENSOR_HEADER; link original TemperatureSensors.cpp compiled
// with TT_SENSOR_TEST and TT_LCD_OFFLINE_TRANSPORT. No IPC path is executed.
#ifndef FANCONTROL_SENSOR_HEADER
#error Define FANCONTROL_SENSOR_HEADER to the original TemperatureSensors.h
#endif
#include FANCONTROL_SENSOR_HEADER
#include <QJsonDocument>
#include <QJsonArray>
#include <QJsonObject>
#include <QFile>
#include <fstream>
#include <iostream>
#include <cstring>
static void put(std::ostream&out,quint64 v,int n){for(int i=0;i<n;i++)out.put(static_cast<char>((v>>(8*i))&255));}
static void bytes(std::ostream&out,const QByteArray&v){put(out,v.size(),4);out.write(v.constData(),v.size());}
int main(int argc,char**argv){if(argc!=3)return 2;QFile in(QString::fromLocal8Bit(argv[1]));if(!in.open(QIODevice::ReadOnly))return 3;const auto cases=QJsonDocument::fromJson(in.readAll()).array();std::ofstream out(argv[2],std::ios::binary);out.write("FWN1",4);put(out,cases.size(),4);for(const auto&value:cases){const auto c=value.toObject();const int op=c["op"].toInt();put(out,op,1);if(op==0){const auto deadline=c["deadline"].toString().toLongLong();put(out,static_cast<quint64>(deadline),8);try{const auto result=tt::sensor_test::request(deadline);put(out,1,1);bytes(out,result);}catch(const std::exception&e){put(out,0,1);bytes(out,QByteArray(e.what()));}continue;}tt::SensorSnapshot snapshot;if(op==1){const auto payload=QByteArray::fromHex(c["payload"].toString().toLatin1());bytes(out,payload);snapshot=tt::sensor_test::parsePayload(payload);}else{QVector<QByteArray>packets;const auto array=c["packets"].toArray();put(out,array.size(),4);for(const auto&packet:array){packets.push_back(QByteArray::fromHex(packet.toString().toLatin1()));bytes(out,packets.back());}snapshot=tt::sensor_test::parsePackets(packets);}put(out,snapshot.error.isEmpty()?1:0,1);bytes(out,snapshot.error.toUtf8());put(out,snapshot.readings.size(),4);for(const auto&r:snapshot.readings){bytes(out,r.id.toUtf8());bytes(out,r.name.toUtf8());bytes(out,r.origin.toUtf8());quint64 bits;static_assert(sizeof(bits)==sizeof(r.celsius));std::memcpy(&bits,&r.celsius,8);put(out,bits,8);}}out.close();if(!out.good())return 1;std::cout<<"PASS: "<<cases.size()<<" original request/payload/packet cases; no IPC\n";}
