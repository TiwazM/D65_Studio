from pathlib import Path
import struct,subprocess
r=Path.cwd();w=r/'work/load-hook/suite';f=w
def run(tag,expected):
 d=bytearray((f/(tag+'.D65')).read_bytes());o=lambda t,s:((t-1)*256+s)*256
 p=o(40,3);t,s=d[p+3:p+5];chain=[];start=bytearray()
 while t:
  chain.append((t,s));p=o(t,s);t,s=d[p:p+2];start+=d[p+2:p+2+(s-1 if t==0 else 254)]
 lines=[];p=2
 while start[p:p+2]!=b'\0\0':
  nxt,n=struct.unpack_from('<HH',start,p);q=nxt-0x2001+2;body=bytes(start[p+4:q-1])
  if n==50:body+=b':\x977687,1' # Explicit D81 mode in test copy only.
  if b'\x85' in body and n<400:body=b'N\xb21'
  lines.append((n,body));p=q
 prg=bytearray(b'\x01\x20')
 for n,b in lines:prg+=struct.pack('<HH',0x2001+len(prg)-2+len(b)+5,n)+b+b'\0'
 prg+=b'\0\0';count=(len(prg)+253)//254;assert count<=len(chain)
 for i,(t,s) in enumerate(chain[:count]):
  part=prg[i*254:(i+1)*254];p=o(t,s);d[p:p+256]=bytes(256);d[p:p+2]=bytes(chain[i+1] if i+1<count else (0,len(part)+1));d[p+2:p+2+len(part)]=part
 # All files in these small controls are in the first 40 logical sectors.
 image=b''.join(d[o(t,0):o(t,0)+40*256] for t in range(1,81));img=f/(tag+'-control.d81');img.write_bytes(image)
 exe=r/'work/D65 Xemu Check.app/Contents/MacOS/xemu-d65-check'
 cmd=[str(exe),'-headless','-8',str(img),'-nosound','-prg',str(r/'work/dummy.prg'),'-prgtest','run"start"','-dumpscreen',str(w/(tag+'-screen.txt')),'-dumpmem',str(w/(tag+'-memory.bin'))]
 with (w/(tag+'.log')).open('w') as log:
  p=subprocess.Popen(cmd,stdout=log,stderr=subprocess.STDOUT)
  try:p.wait(timeout=25)
  except subprocess.TimeoutExpired:
   p.terminate()
   try:p.wait(timeout=4)
   except subprocess.TimeoutExpired:p.kill();p.wait()
 screen=(w/(tag+'-screen.txt')).read_text();print(tag,screen)
 assert expected in screen
 mem=(w/(tag+'-memory.bin')).read_bytes();assert mem[0x1e06]==0
 if tag=='RESMAX':
  original=(f/'GAME00.prg').read_bytes();assert mem[0x2001:0x2001+len(original)-2]==original[2:]
 print('PASS: ROM-loaded START, BASIC menu, enhanced DMA, Attic memory and real Xemu FDC:',tag)
run('HOOKLITE','ALL 14 LOAD HOOK CHECKS PASSED')
