import hashlib
import json
import pathlib
import struct
import sys

path = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/TgWsProxyPlus.exe")
data = path.read_bytes()
pe = struct.unpack_from("<I", data, 0x3C)[0]
assert data[:2] == b"MZ" and data[pe:pe+4] == b"PE\0\0"
machine, sections = struct.unpack_from("<HH", data, pe + 4)
optional_size = struct.unpack_from("<H", data, pe + 20)[0]
optional = pe + 24
assert struct.unpack_from("<H", data, optional)[0] == 0x20B
table = optional + optional_size

def offset(rva):
    for index in range(sections):
        section = table + index * 40
        size, address, raw_size, raw = struct.unpack_from("<IIII", data, section + 8)
        if address <= rva < address + max(size, raw_size):
            return raw + rva - address
    raise ValueError(f"Unmapped RVA {rva}")

imports_rva = struct.unpack_from("<I", data, optional + 112 + 8)[0]
descriptor = offset(imports_rva)
imports = []
while any(data[descriptor:descriptor+20]):
    name = offset(struct.unpack_from("<I", data, descriptor + 12)[0])
    imports.append(data[name:data.index(b"\0", name)].decode("ascii"))
    descriptor += 20
assert len(data) <= 2 * 1024 * 1024, "EXE exceeds 2 MiB"
assert not any(any(token in name.lower() for token in ("python", "libssl", "libcrypto", "vcruntime", "msvcp")) for name in imports)
print(json.dumps({"file":path.name,"bytes":len(data),"MiB":round(len(data)/1024/1024,4),
                  "sha256":hashlib.sha256(data).hexdigest(),"machine":hex(machine),"imports":sorted(set(imports))},indent=2))
