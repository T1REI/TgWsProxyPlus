import argparse, ctypes, json, pathlib, socket, statistics, subprocess, time
from ctypes import wintypes

parser=argparse.ArgumentParser()
parser.add_argument("baseline",type=pathlib.Path)
parser.add_argument("candidate",type=pathlib.Path)
parser.add_argument("--output",type=pathlib.Path,default=pathlib.Path("tests/results/resources.json"))
args=parser.parse_args()
class Memory(ctypes.Structure):
    _fields_=[("cb",wintypes.DWORD),("PageFaultCount",wintypes.DWORD)]+[(name,ctypes.c_size_t) for name in ["PeakWorkingSetSize","WorkingSetSize","QuotaPeakPagedPoolUsage","QuotaPagedPoolUsage","QuotaPeakNonPagedPoolUsage","QuotaNonPagedPoolUsage","PagefileUsage","PeakPagefileUsage","PrivateUsage"]]
class FileTime(ctypes.Structure):
    _fields_=[("low",wintypes.DWORD),("high",wintypes.DWORD)]
psapi=ctypes.WinDLL("psapi",use_last_error=True)
kernel=ctypes.WinDLL("kernel32",use_last_error=True)
psapi.GetProcessMemoryInfo.argtypes=[wintypes.HANDLE,ctypes.POINTER(Memory),wintypes.DWORD]
kernel.GetProcessTimes.argtypes=[wintypes.HANDLE]+[ctypes.POINTER(FileTime)]*4

def sample(process):
    memory=Memory(); memory.cb=ctypes.sizeof(memory)
    if not psapi.GetProcessMemoryInfo(int(process._handle),ctypes.byref(memory),memory.cb): raise ctypes.WinError(ctypes.get_last_error())
    times=[FileTime() for _ in range(4)]
    if not kernel.GetProcessTimes(int(process._handle),*[ctypes.byref(t) for t in times]): raise ctypes.WinError(ctypes.get_last_error())
    cpu=sum((t.high<<32)|t.low for t in times[2:])/10_000_000
    return {"working_set":memory.WorkingSetSize,"private_bytes":memory.PrivateUsage,"cpu_seconds":cpu}

args.output.parent.mkdir(parents=True,exist_ok=True)
results={}
for name,binary in [("baseline",args.baseline),("candidate",args.candidate)]:
    rounds=[]
    for index in range(3):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1",0)); port=listener.getsockname()[1]
        config=args.output.parent/(name+str(index)+"-config.json")
        subprocess.run([str(binary.resolve()),"--init-config",str(config.resolve())],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True)
        value=json.loads(config.read_text(encoding="utf-8"))
        value.update(port=port,host="127.0.0.1",pool_size=0,cfproxy=False,check_updates=False,autostart=False)
        config.write_text(json.dumps(value),encoding="utf-8")
        process=subprocess.Popen([str(binary.resolve()),"--config",str(config.resolve()),"--console"],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            time.sleep(1)
            if process.poll() is not None: raise RuntimeError(name+" exited before measurement")
            before=sample(process); started=time.monotonic(); time.sleep(3); after=sample(process); elapsed=time.monotonic()-started
            after["idle_cpu_percent"]=(after["cpu_seconds"]-before["cpu_seconds"])/elapsed*100
            rounds.append(after)
        finally:
            process.terminate(); process.wait(timeout=5)
            config.unlink(missing_ok=True)
    results[name]={"binary_bytes":binary.stat().st_size,"rounds":rounds,"median":{key:statistics.median(r[key] for r in rounds) for key in rounds[0]}}
results["scenario"]="Windows console, no clients, pool_size=0, cfproxy=false; 1s warmup + 3s observation, median of 3 processes"
args.output.write_text(json.dumps(results,indent=2),encoding="utf-8")
print(json.dumps(results,indent=2))
