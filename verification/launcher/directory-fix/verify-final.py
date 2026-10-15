from pathlib import Path
import subprocess,json
r=Path.cwd();w=r/'work/directory-fix';images=r/'outputs/d65-studio/Images'
for tag in ['DEMO','CAPACITY']:
 d=(images/f'LAUNCH-{tag}.d65').read_bytes();p=(39*256+3)*256;t,s=d[p+3:p+5];prg=bytearray()
 while t:
  p=((t-1)*256+s)*256;t,s=d[p:p+2];prg+=d[p+2:p+2+(s-1 if t==0 else 254)]
 mem=(w/f'new-{tag}-load.bin').read_bytes()
 assert mem[0x2001:0x2001+len(prg)-2]==prg[2:]
 print('PASS ROM loaded exact original START bytes:',tag,len(prg))
assert 'NUMBER (0 NEXT PAGE, -1 QUIT)?' in (w/'new-CAPACITY-run.txt').read_text()
print('PASS ROM RUN START reaches interactive menu with unchanged first-bank bytes.')
with (w/'capacity-execution.txt').open('w') as f:
 for e in json.loads((r/'work/launcher-test/capacity-files.json').read_text()):
  if e['name']=='START':continue
  subprocess.run([str(r/'work/launcher-test/reader-probe'),str(images/'D65FULL.D65'),str(r/'work/launcher-test'/f"{e['name']}.prg"),*map(str,e['start']),'0',str(r/'outputs/d65-studio/launcher/reader.bin'),e['name']],stdout=f,check=True)
print('PASS all 38 revised D65 payloads through CPU/FDC harness; modeled D65/DMA backend.')
