"""Offline signature verification and static AMX CFG decoding, never emulation."""
import sys
# Avoid this directory's standalone inspect.py shadowing Python's stdlib inspect.
sys.path.pop(0)
import hashlib
import hmac
import json
from pathlib import Path
import re
import struct

root = Path(sys.argv[1]).resolve() if len(sys.argv) == 2 else Path(__file__).resolve().parent
b = (root / "AMDFamily17.bin").read_bytes()
assert hashlib.sha256(b).hexdigest() == "dae74615761b78bdf064dfb3e136252ddcc6fc727d88f14738d0e5800d427a91"
source = (root / "vm.cpp").read_bytes()
assert hashlib.sha256(source).hexdigest() == "bb56cb7da7da602da1413cbac27a00a2063cb7707c70d15d0f65a74ff1ae4bb1"
key_text = re.search(rb"k_pubkey_namazso_2023\[\]\s*=\s*\{([^}]+)\}", source).group(1)
key = bytes(int(x, 16) for x in re.findall(rb"0x([0-9A-Fa-f]{2})", key_text))
magic, bits, explen, modlen, p1, p2 = struct.unpack_from("<6I", key)
assert (magic, bits, explen, modlen, p1, p2) == (0x31415352, 4096, 3, 512, 0, 0)
assert len(key) == 24 + explen + modlen
exponent = int.from_bytes(key[24:24 + explen], "big")
modulus = int.from_bytes(key[24 + explen:], "big")
siglen, = struct.unpack_from("<I", b)
assert siglen == modlen
signature, payload = b[4:4 + siglen], b[4 + siglen:]

def verify(sig, data):
    if len(sig) != modlen or int.from_bytes(sig, "big") >= modulus:
        return False
    encoded = pow(int.from_bytes(sig, "big"), exponent, modulus).to_bytes(modlen, "big")
    digest_info = bytes.fromhex("3031300d060960864801650304020105000420") + hashlib.sha256(data).digest()
    expected = b"\x00\x01" + b"\xff" * (modlen - len(digest_info) - 3) + b"\x00" + digest_info
    return hmac.compare_digest(encoded, expected)

positive = verify(signature, payload)
negative = {
    "payload_first_bit": not verify(signature, bytes([payload[0] ^ 1]) + payload[1:]),
    "payload_last_bit": not verify(signature, payload[:-1] + bytes([payload[-1] ^ 1])),
    "signature_first_bit": not verify(bytes([signature[0] ^ 1]) + signature[1:], payload),
    "signature_truncated": not verify(signature[:-1], payload),
    "payload_truncated": not verify(signature, payload[:-1]),
}
rsa = dict(verified=positive, scheme="RSASSA-PKCS1-v1_5 SHA256", exponent=exponent, modulus_bits=bits, key_blob_sha256=hashlib.sha256(key).hexdigest(), key_source_file="vm.cpp", key_source_sha256=hashlib.sha256(source).hexdigest(), signature_bytes=siglen, payload_offset=4 + siglen, negative_tests=negative)
(root / "rsa-proof.json").write_text(json.dumps(rsa, indent=2) + "\n", encoding="utf-8")
assert positive and all(negative.values()), rsa

# Opcode conventions match the read-only reference pawnio-amx-proof.py.
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
cod, dat = struct.unpack_from("<II", payload, 12)
cell = lambda pc: struct.unpack_from("<q", payload, cod + pc)[0]
publics, natives, libraries = struct.unpack_from("<III", payload, 32)

def tables(start, end):
    result = []
    for off in range(start, end, 8):
        addr, name = struct.unpack_from("<II", payload, off)
        result.append((addr, payload[name:payload.index(0, name)].decode("ascii")))
    return result

native_names = [name for _, name in tables(natives, libraries)]
exports = dict((name, addr) for addr, name in tables(publics, natives))
instructions = {}
pc = 0
while pc < dat - cod:
    op = cell(pc)
    assert 0 <= op <= 74, (pc, op, "unexpected opcode: stop static proof")
    if op == 74:
        records = cell(pc + 8)
        assert 0 <= records <= (dat - cod - pc - 24) // 16
        instructions[pc] = ("casetbl", records, pc + 8 * (3 + 2 * records))
        pc += 8 * (3 + 2 * records)
        continue
    count = counts[op]
    arg = cell(pc + 8) if count else None
    instructions[pc] = (names[op], arg, pc + 8 * (1 + count))
    pc += 8 * (1 + count)
assert pc == dat - cod

def reachable(entry):
    pending, seen, calls, native_calls = [entry], set(), set(), set()
    while pending:
        pc = pending.pop()
        if pc in seen:
            continue
        assert pc in instructions, ("bad target", pc)
        seen.add(pc)
        name, arg, next_pc = instructions[pc]
        if name == "call":
            calls.add(pc + arg)
            pending.append(pc + arg)
        if name == "sysreq":
            assert 0 <= arg < len(native_names)
            native_calls.add(native_names[arg])
        if name in ("ret", "retn", "halt"):
            continue
        assert name not in ("sctrl", "switch", "casetbl"), ("unresolved dynamic branch", pc)
        if name in ("jump", "jzer", "jnz"):
            pending.append(pc + arg)
        if name != "jump":
            pending.append(next_pc)
    return sorted(seen), sorted(calls), sorted(native_calls)

summary = {}
lines = []
for name, entry in [("main", struct.unpack_from("<I", payload, 28)[0]), ("ioctl_read_smn", exports["ioctl_read_smn"])]:
    seen, calls, native_calls = reachable(entry)
    summary[name] = dict(entry=entry, instructions=len(seen), callees=calls, natives=native_calls, unresolved_branches=False)
    lines.append(f"=== {name} entry COD+{entry:x} ===")
    for pc in seen:
        op, arg, _ = instructions[pc]
        extra = f" target={pc + arg:x}" if op in ("call", "jump", "jzer", "jnz") else (" native=" + native_names[arg] if op == "sysreq" else "")
        lines.append(f"COD+{pc:04x} FILE+{4 + siglen + cod + pc:04x} {op} {arg if arg is not None else ''}{extra}")
(root / "amx-focused.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
(root / "cfg-proof.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
assert set(summary["main"]["natives"]) == {"get_arch", "cpuid", "debug_print"}
assert set(summary["ioctl_read_smn"]["natives"]) == {"pci_config_read_dword", "pci_config_write_dword"}

# Fixed-pin instruction assertions document manual static data-flow review.
# They read decoded operands; no virtual register, stack, branch execution or native call.
facts = {
    0x21c8: ("sysreq", 5), 0x21e8: ("const.alt", 1),
    0x2250: ("call", 0x3e0 - 0x2250),
    0x2260: ("const.alt", -4546024346316537082),
    0x22d8: ("call", 0x620 - 0x22d8),
    0x2340: ("call", 0x988 - 0x2340),
    0x23a8: ("call", 0x9e0 - 0x23a8),
    0x2430: ("sysreq", 6), 0x2468: ("const.alt", 0x17),
    0x2478: ("sless", None), 0x2480: ("jnz", 0x24e8 - 0x2480),
    0x24a0: ("const.alt", 0x1a), 0x24b0: ("sgrtr", None),
    0x24b8: ("jnz", 0x24e8 - 0x24b8),
    0x2510: ("const.pri", -1073741637), 0x2540: ("const.pri", 0),
    0x1b08: ("load.s.pri", 32), 0x1b18: ("const.alt", 1),
    0x1ba0: ("load.s.pri", 48), 0x1bb0: ("const.alt", 1),
    0x1c60: ("const.pri", -1073741811),
    0x1d58: ("sysreq", 3), 0x1e38: ("const.alt", 65535),
    0x1e50: ("const.alt", 0x1022),
    0x1e68: ("jzer", 0x1ea8 - 0x1e68),
    0x1e80: ("const.pri", -1073741637),
    0x1eb0: ("load.s.pri", 24), 0x1ec0: ("load.i", None),
    0x1ed0: ("const.pri", 0x60), 0x1f48: ("sysreq", 4),
    0x2018: ("load.s.pri", 40), 0x2030: ("const.pri", 0x64),
    0x20a8: ("sysreq", 3),
}
for pc, expected in facts.items():
    assert instructions[pc][:2] == expected, (pc, expected, instructions[pc])
for pc in [0x1ce0, 0x1cf8, 0x1d10, 0x1d28, 0x1ee8, 0x1f00, 0x1f18, 0x2048, 0x2060, 0x2078]:
    assert instructions[pc][:2] == ("const.pri", 0)
for pc in [0x1dc8, 0x1fb8, 0x2118]:
    assert instructions[pc][:2] == ("const.alt", 0)
    assert instructions[pc + 16][:2] == ("sgeq", None)
    assert instructions[pc + 24][0] == "jnz"
fnv = 14695981039346656037
for byte in b"AuthenticAMD":
    fnv = ((fnv ^ byte) * 1099511628211) & ((1 << 64) - 1)
assert fnv == (-4546024346316537082 & ((1 << 64) - 1))
summary["fixed_pin_operand_assertions"] = len(facts) + 10 + 9
summary["AuthenticAMD_fnv1a64"] = hex(fnv)
summary["scope"] = "all static CFG branches followed for both entries including direct callees; no execution/emulation; native implementation/driver callbacks outside module are not covered"
(root / "cfg-proof.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
print(json.dumps(dict(rsa=rsa, cfg=summary), indent=2))
