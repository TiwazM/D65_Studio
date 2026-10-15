from pathlib import Path
import struct
w=Path('work/resident-driver/fixtures');w.mkdir(exist_ok=True)
words={'PRINT':b'\x99','POKE':b'\x97','PEEK':b'\xc2','SYS':b'\x9e','BANK':b'\xfe\x02','FOR':b'\x81','TO':b'\xa4','NEXT':b'\x82','IF':b'\x8b','THEN':b'\xa7','END':b'\x80','CHR$':b'\xc7','=':b'\xb2','+':b'\xaa','<>':b'\xb3\xb1'}
def tok(s):
 out=b'';quote=False
 while s:
  c=s[0]
  if c=='"':quote=not quote
  match=next((k for k in words if s.startswith(k)),None) if not quote else None
  if match:out+=words[match];s=s[len(match):]
  else:out+=c.encode();s=s[1:]
 return out
def prg(lines):
 p=bytearray(b'\x01\x20')
 for n,body in lines:
  b=tok(body);p+=struct.pack('<HH',0x2001+len(p)-2+len(b)+5,n)+b+b'\0'
 return p+b'\0\0'
lines=[(10,'PRINT"RESIDENT PROGRAM STARTED":BANK0'),(20,'FORI=0TO15:POKE7712+I,160:NEXT')]
for i,c in enumerate(b'MESSAGE'):lines.append((30+i,f'POKE{7712+i},{c}'))
lines += [(50,'POKE7694,2:POKE7695,0:POKE7696,0:POKE7697,192:SYS5632'),(60,'PRINT"STATUS";PEEK(7686)'),(70,'FORI=0TO39:PRINTCHR$(PEEK(49152+I));:NEXT:PRINT'),(80,'IFPEEK(7686)=0THENPRINT"RESIDENT SECOND READ OK"'),(90,'END')]
p=prg(lines);p+=b'\x55'*(40000-len(p));(w/'READDEMO.prg').write_bytes(p)
