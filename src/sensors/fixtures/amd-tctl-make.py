"""Compile only extracted pinned LHM arithmetic. Never execute the module/DLL."""
from pathlib import Path
import hashlib,json,os,re,subprocess,tempfile
HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
SOURCE=ROOT/'work/amd-cpu-source/Amd17Cpu.cs'
PIN='8a6dba06cd584c91043f13bc4555fb43ec095534c80ceb00afa31e62d2b082e3'
assert hashlib.sha256(SOURCE.read_bytes()).hexdigest()==PIN,'Pinned LHM source drift'
source=SOURCE.read_text(encoding='utf-8-sig')
flag=re.search(r'bool tempOffsetFlag = .*?temperature = \(temperature >> 21\) \* 125;',source,re.S)
conversion=re.search(r'float t = temperature \* 0\.001f;\s*if \(tempOffsetFlag\)\s*t \+= -49\.0f;',source)
assert flag and conversion,'Pinned arithmetic not found'
constants=[]
for key in ['F17H_TEMP_RANGE_SEL_MASK','F17H_TEMP_TJ_SEL_MASK']:
    m=re.search(r'private const uint '+key+r' = (0x[0-9A-Fa-f]+);',source)
    assert m,key
    constants.append(f'constexpr std::uint32_t {key} = {m.group(1)};')
extracted='\n'.join(constants)+'\nstatic float source_tctl(std::uint32_t temperature) {\n'+flag.group(0)+'\n'+conversion.group(0)+'\nreturn t;\n}\n'
with tempfile.TemporaryDirectory(prefix='amd-tctl-build-',dir=HERE) as directory:
    tmp=Path(directory).resolve()
    assert tmp.parent==HERE.resolve() and tmp.name.startswith('amd-tctl-build-')
    (tmp/'amd-tctl-expressions.inc').write_text(extracted,encoding='utf-8')
    build=tmp/'amd-tctl-build.cmd'
    build.write_text(f'@echo off\nchcp 65001 >nul\ncall "C:/Program Files/Microsoft Visual Studio/18/Community/VC/Auxiliary/Build/vcvars64.bat" >nul\nif errorlevel 1 exit /b 1\ncl /nologo /std:c++17 /EHsc /fp:strict /utf-8 /W4 /I"{tmp}" "{HERE / "amd-tctl-oracle.cpp"}" /Fe:"{tmp / "amd-tctl-oracle.exe"}" /Fo:"{tmp / "amd-tctl-oracle.obj"}"\n',encoding='utf-8')
    env=dict(os.environ)
    env['PATH']=os.environ['SystemRoot']+'/System32;'+os.environ['SystemRoot']
    subprocess.run(['cmd.exe','/d','/c',str(build)],env=env,cwd=tmp,check=True)
    # Binary stdout: set CRT stdout mode so 0x0A bytes are not CRLF translated.
    raw=subprocess.run([str(tmp/'amd-tctl-oracle.exe')],capture_output=True,cwd=tmp,check=True).stdout
assert len(raw)==2048*8*4,len(raw)
fixture=HERE/'amd-tctl-f32-bits.bin'
if fixture.exists() and fixture.read_bytes()!=raw:raise RuntimeError('Fixture drift; old fixture preserved')
fixture.write_bytes(raw)
report={
    'lhm_commit':'2808d80564b6761e1b01f42fabfd966ed695fcbc',
    'source_url':'https://raw.githubusercontent.com/LibreHardwareMonitor/LibreHardwareMonitor/2808d80564b6761e1b01f42fabfd966ed695fcbc/LibreHardwareMonitorLib/Hardware/Cpu/Amd17Cpu.cs',
    'source_sha256':PIN,'source_lines':'287-289,303-305,925-926',
    'extracted_arithmetic_sha256':hashlib.sha256(extracted.encode()).hexdigest(),
    'oracle_sha256':hashlib.sha256((HERE/'amd-tctl-oracle.cpp').read_bytes()).hexdigest(),
    'compiler':'MSVC C++17 /fp:strict; no vendor code, DLL or module executed',
    'fixture_sha256':hashlib.sha256(raw).hexdigest(),'fixture_bytes':len(raw),
    'order':'code 0..2047 outer; flags 0..7 inner; raw=(code<<21)|((flags&1)<<19)|(((flags>>1)&3)<<16); little-endian f32 bits',
    'scope':'Tctl expression only; excludes legacy Tdie model offsets, CCDs and native acceptance',
    'module_bytes':10652,'module_sha256':'dae74615761b78bdf064dfb3e136252ddcc6fc727d88f14738d0e5800d427a91',
    'publisher_signature_cryptographically_verified':False,
}
(HERE/'amd-tctl-provenance.json').write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
print(json.dumps(report))
