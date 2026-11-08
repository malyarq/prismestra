# Static decoding only. No execution or emulation of module instructions.
import hashlib, struct, sys
b = open(sys.argv[1], 'rb').read()
expected = '91f9b4b1c39e3d399ce48477a89d8f6bd3e58a2241064a4daec3a1513dff56e5'
assert len(b) == 42404 and hashlib.sha256(b).hexdigest() == expected
base = 516
u32 = lambda o: struct.unpack_from('<I', b, o)[0]
assert struct.unpack_from('<H', b, base + 4)[0] == 0xf1e1
assert b[base+6:base+8] == b'\x0b\x0b'
assert struct.unpack_from('<H', b, base+10)[0] == 8
cod, dat = u32(base+12), u32(base+16)
assert (cod, dat) == (600, 37096) and u32(base) + base == len(b)
cell = lambda o: struct.unpack_from('<q', b, base+cod+o)[0]
print('SHA256', expected, 'payload', base, 'COD', cod, 'DAT', dat)
assert struct.unpack_from('<q', b, base+dat+216)[0] == 0
print('INITIAL_POLL_POLICY', 0, 'DAT+216', 'FILE', hex(base+dat+216))
assert u32(base+28) == 0x8dc8
assert struct.unpack_from('<q', b, base+dat+240)[0] == 0xb00
print('ENTRY', hex(u32(base+28)), 'INITIAL_BASE', '0xb00')
for o in range(u32(base+32), u32(base+36), 8):
    addr, name = struct.unpack_from('<II', b, base+o)
    print('PUBLIC', b[base+name:b.index(0, base+name)].decode('ascii'), hex(addr))
for index, o in enumerate(range(u32(base+36), u32(base+40), 8)):
    _, name = struct.unpack_from('<II', b, base+o)
    print('NATIVE', index, b[base+name:b.index(0, base+name)].decode('ascii'))
names = '''nop load.pri load.alt load.s.pri load.s.alt lref.s.pri
lref.s.alt load.i lodb.i const.pri const.alt addr.pri addr.alt
stor stor.s sref.s stor.i strb.i align.pri lctrl sctrl xchg
push.pri push.alt pushr.pri pop.pri pop.alt pick stack heap
proc ret retn call jump jzer jnz shl shr sshr shl.c.pri
shl.c.alt smul sdiv add sub and or xor not neg invert eq neq
sless sleq sgrtr sgeq inc.pri inc.alt inc.i dec.pri dec.alt
dec.i movs cmps fill halt bounds sysreq switch swap.pri
swap.alt break casetbl'''.split()
counts = [0,1,1,1,1,1,1,0,1,1,1,1,1,1,1,1,
0,1,1,1,1,0,0,0,0,0,0,1,1,1,0,0,
0,1,1,1,1,0,0,0,1,1,0,0,0,0,0,0,
0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
1,1,1,1,1,1,1,0,0,0]
# casetbl is opcode74 with variable data, deliberately not decoded here.
assert (len(names), len(counts)) == (75, 74)
# Complete selector functions, identity entry, block length guard, transfer entry.
for start, end in [(0x0750,0x0788),(0x0d20,0x0d68),(0x1008,0x1348),
                   (0x1358,0x2258),(0x5558,0x6818),(0x6818,0x69c0),
                   (0x45f0,0x4760),(0x75f8,0x8930),(0x8998,0x8dc8),(0x8dc8,0x8e88)]:
    pc = start
    while pc < end:
        op = cell(pc)
        assert 0 <= op < 74, (hex(pc), op)
        n = counts[op]
        operand = cell(pc+8) if n else None
        target = pc+operand if op in (33,34,35,36,70) else None
        print(f'COD+{pc:04x} FILE+{base+cod+pc:04x}',names[op],operand if n else '',f'target={target:x}' if target is not None else '')
        pc += 8*(1+n)
print('CASE_TABLE_RAW')
for pc in range(0x8930, 0x8998, 8):
    print(f'COD+{pc:04x}', cell(pc))
