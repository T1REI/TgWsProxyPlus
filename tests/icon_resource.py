import ctypes, hashlib, pathlib, struct, sys
from ctypes import wintypes
binary=pathlib.Path(sys.argv[1]).resolve()
icon=pathlib.Path(sys.argv[2] if len(sys.argv)>2 else "icon.ico").read_bytes()
api=ctypes.WinDLL("kernel32",use_last_error=True)
api.LoadLibraryExW.argtypes=[wintypes.LPCWSTR,wintypes.HANDLE,wintypes.DWORD];api.LoadLibraryExW.restype=wintypes.HMODULE
api.FindResourceW.argtypes=[wintypes.HMODULE,ctypes.c_void_p,ctypes.c_void_p];api.FindResourceW.restype=ctypes.c_void_p
api.LoadResource.argtypes=[wintypes.HMODULE,ctypes.c_void_p];api.LoadResource.restype=ctypes.c_void_p
api.LockResource.argtypes=[ctypes.c_void_p];api.LockResource.restype=ctypes.c_void_p
api.SizeofResource.argtypes=[wintypes.HMODULE,ctypes.c_void_p];api.SizeofResource.restype=wintypes.DWORD
api.FreeLibrary.argtypes=[wintypes.HMODULE]
module=api.LoadLibraryExW(str(binary),None,0x2|0x20)
if not module: raise ctypes.WinError(ctypes.get_last_error())
def resource(kind,name):
    info=api.FindResourceW(module,name,kind)
    if not info: raise ctypes.WinError(ctypes.get_last_error())
    return ctypes.string_at(api.LockResource(api.LoadResource(module,info)),api.SizeofResource(module,info))
try:
    group=resource(14,1)
    count=struct.unpack_from("<H",icon,4)[0]
    assert struct.unpack_from("<H",group,4)[0]==count
    for index in range(count):
        entry=6+index*16
        size,offset=struct.unpack_from("<II",icon,entry+8)
        resource_id=struct.unpack_from("<H",group,6+index*14+12)[0]
        assert resource(3,resource_id)==icon[offset:offset+size]
    print("Verified",count,"ICO frames in EXE; SHA256",hashlib.sha256(icon).hexdigest())
finally:
    api.FreeLibrary(module)
