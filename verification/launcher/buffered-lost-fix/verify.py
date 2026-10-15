from pathlib import Path
import struct, subprocess
root=Path.cwd(); work=root/'work/launcher-test'; probe=work/'reader-probe';reader=root/'outputs/d65-studio/launcher/reader.bin'
def prg(n,label):
 body=b'\x99"'+label.encode()+b'":\x80\x00'; p=struct.pack('<HHH',0x2001,0x2001+4+len(body),10)+body+b'\0\0'; return p+bytes((i*19+37)%251 for i in range(n-len(p)))
logs=[]
for bank,start,n in [(1,40,16384),(1,41,1000),(2,80,40000),(2,119,509)]:
 payload=prg(n,f'BANK {bank} DIRECT READER OK');p=work/f'probe-{start}.prg';p.write_bytes(payload)
 data=bytearray(5570560); sectors=[(t,s) for t in range(1,81) for s in range(bank*40,(bank+1)*40)]
 sectors=sectors[start-bank*40:]; count=(n+253)//254
 for i,(t,s) in enumerate(sectors[:count]):
  part=payload[i*254:(i+1)*254];o=((t-1)*256+s)*256
  data[o:o+2]=bytes(sectors[i+1] if i+1<count else (0,len(part)+1));data[o+2:o+2+len(part)]=part
 img=work/f'probe-{start}.d65';img.write_bytes(data)
 for mode in (range(16) if start==40 else [0]):
  r=subprocess.run([str(probe),str(img),str(p),'1',str(start),str(mode),str(reader),f'T1/S{start}'],text=True,capture_output=True)
  if r.returncode: print(r.stdout,r.stderr);raise SystemExit(r.returncode)
  print(r.stdout,end='');logs.append(r.stdout)
(work/'probe-results.txt').write_text(''.join(logs))
