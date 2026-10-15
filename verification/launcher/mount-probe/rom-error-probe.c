#include "xemu/emutools_basicdefs.h"
#include "xemu/cpu65.h"
#include <assert.h>
static Uint8 mem[65536];static int ok,err;
Uint8 cpu65_read_callback(Uint16 a){return mem[a];}
void cpu65_write_callback(Uint16 a,Uint8 v){mem[a]=v;if(a==0xd640){assert(v==0x40);cpu.pf_c=ok;cpu.a=err;}}
void cpu65_do_aug_callback(void){abort();}void cpu65_do_nop_callback(void){}
int main(void){FILE*f=fopen("work/rom-analysis/MEGA65.ROM","rb");assert(f);fseek(f,0x18000,SEEK_SET);assert(fread(mem+0x8000,1,16384,f)==16384);fclose(f);
int errors[]={0x88,0x8a,0x8b,0xff,0};for(int i=0;i<5;i++){ok=i==4;err=errors[i];memset(&cpu,0,sizeof cpu);cpu.s=255;cpu.sphi=256;cpu.pf_e=1;cpu.pf_i=1;cpu.x=10;cpu.pc=0x7000;mem[0x1181]=8;mem[0x7000]=0x20;mem[0x7001]=0xea;mem[0x7002]=0x9a;
for(int n=0;n<100;n++){if(cpu.pc==0x2ffd||cpu.pc==0x7003)break;cpu65_step();}assert(cpu.pc==(ok?0x7003:0x2ffd));if(!ok)assert(cpu.x==4);printf("Hypervisor carry=%d A=$%02X -> %s\n",ok,err,ok?"normal return":"same BASIC error index $04 (FILE NOT FOUND)");}return 0;}
