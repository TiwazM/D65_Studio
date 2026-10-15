from pathlib import Path
import struct, subprocess, re, shutil, sys
root=Path.cwd(); w=root/'work/launcher-test'; large=len(sys.argv)>1; tag='diag-error' if large else 'diag-success';choice=2
d=(root/'outputs/D65DIAG.D65').read_bytes()
def extract(t,s):
 out=bytearray()
 while t:
  p=((t-1)*256+s)*256;t,s=d[p:p+2];out+=d[p+2:p+2+(s-1 if t==0 else 254)]
 return bytes(out)
o=(39*256+3)*256;starter=extract(d[o+3],d[o+4]);p=2;lines=[]
while starter[p:p+2]!=b'\0\0':
 nxt,line=struct.unpack_from('<HH',starter,p);q=nxt-0x2001+2;body=starter[p+4:q-1]
 if b'\x85' in body and line<400:body=b'N\xb2'+str(choice).encode() # Auto-select for unattended test only.
 if 1000<=line<1100:
  body=re.sub(b'\x974865,(\\d+)',lambda m:b'\x974865,'+str(int(m[1])%40).encode(),body)
  body=body.replace(b'\x974871,0',b'\x974871,0' if large else b'\x974871,1') # Explicit D81 control mode only.
 lines.append((line,body));p=q
out=bytearray(b'\x01\x20')
for n,b in lines:
 nxt=0x2001+len(out)-2+len(b)+5;out+=struct.pack('<HH',nxt,n)+b+b'\0'
out+=b'\0\0';prg=w/f'{tag}-control.prg';prg.write_bytes(out)
source=root/'outputs/d65-studio/Images'/'merge-sources/BANK2.d81'
image=w/f'{tag}-disk.d81';shutil.copy2(source,image)
subprocess.run([str(root/'work/rust-target/debug/d65-studio'),'add',str(image),str(prg),'START'],check=True)
exe=root/'work/D65 Xemu Check.app/Contents/MacOS/xemu-d65-check'
cmd=[str(exe),'-headless','-8',str(image),'-nosound','-prg',str(root/'work/dummy.prg'),'-prgtest','run"start"','-dumpscreen',str(w/f'{tag}-screen.txt'),'-dumpmem',str(w/f'{tag}-memory.bin')]
with (w/f'{tag}.log').open('w') as f:
 p=subprocess.Popen(cmd,stdout=f,stderr=subprocess.STDOUT)
 try:print('Xemu exit',p.wait(timeout=12))
 except subprocess.TimeoutExpired:
  p.terminate()
  try:p.wait(timeout=5)
  except subprocess.TimeoutExpired:p.kill();p.wait()
  print('Xemu timed snapshot')
screen=(w/f'{tag}-screen.txt').read_text();expected='CODE FDC MOUNT FORMAT TRACK SECTOR (HEX)' if large else 'BANK 2 OK'
assert expected in screen,screen
mem=(w/f'{tag}-memory.bin').read_bytes();assert mem[0x1306]==(2 if large else 0)
if large:assert mem[0x1308]==1 and "01 " in screen
print('PASS diagnostic reader Xemu control:',expected)
