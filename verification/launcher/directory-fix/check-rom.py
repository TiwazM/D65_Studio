from pathlib import Path
import subprocess
r=Path.cwd();w=r/'work/directory-fix';exe=r/'work/D65 Xemu Check.app/Contents/MacOS/xemu-d65-check'
for tag in ['DEMO','CAPACITY']:
 d=(r/f'outputs/d65-studio/Images/LAUNCH-{tag}.d65').read_bytes()
 (w/f'new-{tag}.d81').write_bytes(b''.join(d[t*65536:t*65536+10240] for t in range(80)))
for variant,tag,cmd in [('old','DEMO','dir'),('old','DEMO','load"start",8'),('new','DEMO','dir'),('new','DEMO','load"start",8'),('new','CAPACITY','dir'),('new','CAPACITY','load"start",8'),('new','CAPACITY','run"start"')]:
 name=f'{variant}-{tag}-'+cmd.split('"')[0];args=[str(exe),'-headless','-8',str(w/f'{variant}-{tag}.d81'),'-nosound','-prg',str(r/'work/dummy.prg'),'-prgtest',cmd,'-dumpscreen',str(w/f'{name}.txt'),'-dumpmem',str(w/f'{name}.bin')]
 with (w/f'{name}.log').open('w') as f:
  p=subprocess.Popen(args,stdout=f,stderr=subprocess.STDOUT)
  try:p.wait(timeout=8)
  except subprocess.TimeoutExpired:
   p.terminate()
   try:p.wait(timeout=3)
   except subprocess.TimeoutExpired:p.kill();p.wait()
 screen=(w/f'{name}.txt').read_text()
 print(name,screen,flush=True)
 if variant=='old':assert 'ILLEGAL TRACK OR SECTOR' in screen
 else:
  assert 'ILLEGAL' not in screen and 'ERROR' not in screen
  if cmd=='dir':assert 'START' in screen or 'EXTRA38' in screen
  if cmd.startswith('load'):
   mem=(w/f'{name}.bin').read_bytes();assert mem[0x2001:0x2003]!=b'\0\0';assert 'INJECTED' not in screen
