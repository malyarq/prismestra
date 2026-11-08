"""Compile a CPU-only NVFC thermal ABI oracle; no vendor DLL is linked or loaded."""
from pathlib import Path
import hashlib,json,os,re,subprocess,tempfile
HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
SOURCE=ROOT.parent/'rgb-openrgb-2026-11-05/core-source-1.1/dependencies/NVFC'
PINS={'nvapi.cpp':'250c9a02509cf9e6387a8ff22ec9c12e806ed4b6b77f555a05c6244055636f13','nvapi.h':'1cd791d34799fcea408bc12bf4e1cc7662fa792c7a37fa3e91470e48597a2f41'}
for name,expected in PINS.items():
    assert hashlib.sha256((SOURCE/name).read_bytes()).hexdigest()==expected,f'Source drift: {name}'
source=(SOURCE/'nvapi.cpp').read_text()
constructor=re.search(r'NV_GPU_THERMAL_SETTINGS_V2::NV_GPU_THERMAL_SETTINGS_V2\(\)\s*\{.*?\n\}',source,re.S)
assert constructor,'Missing actual constructor'
with tempfile.TemporaryDirectory(prefix='.nvapi-thermal-build-',dir=HERE) as temporary:
    tmp=Path(temporary).resolve()
    assert tmp.parent==HERE.resolve()
    (tmp/'thermal-constructor.inc').write_text(constructor.group(0)+'\n')
    build=tmp/'build.cmd'
    build.write_text(f'@echo off\nchcp 65001 >nul\ncall "C:/Program Files/Microsoft Visual Studio/18/Community/VC/Auxiliary/Build/vcvars64.bat" >nul\nif errorlevel 1 exit /b 1\ncl /nologo /std:c++17 /EHsc /utf-8 /W4 /I"{SOURCE}" /I"{tmp}" "{HERE / "nvapi-thermal-oracle.cpp"}" /Fe:"{tmp / "oracle.exe"}" /Fo:"{tmp / "oracle.obj"}"\n',encoding='utf-8')
    env=dict(os.environ);env['PATH']=os.environ['SystemRoot']+'/System32;'+os.environ['SystemRoot']
    subprocess.run(['cmd.exe','/d','/c',str(build)],env=env,cwd=tmp,check=True)
    raw=subprocess.run([str(tmp/'oracle.exe')],capture_output=True,cwd=tmp,check=True).stdout.replace(b'\r\n',b'\n')
data=json.loads(raw)
assert data['size']==68 and data['alignment']==4 and data['version']==0x20044
fixture=HERE/'nvapi-thermal-native.json'
if fixture.exists() and fixture.read_bytes()!=raw:raise RuntimeError('Fixture drift; preserving current fixture')
fixture.write_bytes(raw)
print(json.dumps({'status':'PASS','source_sha256':PINS,'fixture_sha256':hashlib.sha256(raw).hexdigest(),'abi':data}))
