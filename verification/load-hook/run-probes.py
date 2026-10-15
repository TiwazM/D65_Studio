from pathlib import Path
import subprocess
w=Path('work/load-hook');f=Path('work/resident-driver/fixtures');logs=[]
for line in (f/'files.txt').read_text().splitlines():
 i,name,t,s=line.split()
 if not name.startswith('GAME'):
  (f/(name+'.prg')).write_bytes(b'' if name=='EMPTY' else b'THE RESIDENT DRIVER READ THIS AFTER LAUNCH')
 modes=([20,21,22]+([23] if name=='MESSAGE' else [])) if not name.startswith('GAME') else (list(range(13))+[25] if name=='GAME00' else [0,25])
 for mode in modes:
  r=subprocess.run([str(w/'probe'),str(f/'RESCAP.D65'),str(f/(name+'.prg')),i,str(mode),'outputs/d65-studio/launcher/reader-compat.bin',str(f/'catalog.bin')],text=True,capture_output=True)
  if r.returncode:print(i,name,mode,r.stdout,r.stderr);raise SystemExit(r.returncode)
  logs.append(r.stdout)
# Exercise the high byte of catalogue count/index and a full name scan.
catalog=(f/'catalog.bin').read_bytes();message=next(int(line.split()[0]) for line in (f/'files.txt').read_text().splitlines() if line.split()[1]=='MESSAGE')
entry=catalog[8+message*24:8+(message+1)*24]
large=bytearray(b'D65R\x01'+(295).to_bytes(2,'little')+b'\0')
for i in range(295):
 e=bytearray(entry)
 if i<294:e[:16]=f'FILE{i:03}'.encode().ljust(16,b'\xa0')
 large+=e
(f/'large-catalog.bin').write_bytes(large)
for mode in [20,24]:
 r=subprocess.run([str(w/'probe'),str(f/'RESCAP.D65'),str(f/'MESSAGE.prg'),'294',str(mode),'outputs/d65-studio/launcher/reader-compat.bin',str(f/'large-catalog.bin')],text=True,capture_output=True)
 if r.returncode:print(r.stdout,r.stderr);raise SystemExit(r.returncode)
 logs.append(r.stdout)
(w/'cpu-results.txt').write_text(''.join(logs));print(f'PASS: {len(logs)} CPU/FDC cases, including resident re-entry after overwriting the old workspace.')
