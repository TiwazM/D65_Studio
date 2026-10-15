#include "xemu/emutools_basicdefs.h"
#include "xemu/cpu65.h"
#include <assert.h>
static Uint8 mem[65536];static int mode,last_error;static char output[8192];static unsigned outlen;
Uint8 cpu65_read_callback(Uint16 a){return mem[a];}
void cpu65_write_callback(Uint16 a,Uint8 v){mem[a]=v;if(a>=0xd000&&a<0xe000){assert(a==0xd02f||a==0xd640);if(a!=0xd640)return;
 if(v==0x38){cpu.a=last_error;cpu.pf_c=1;return;}
 assert(v==2||v==0x3c||v==0x2e||v==0x34||v==0x40||v==0x4a);
 int e=0;char*name=(char*)mem+0x400;
 if(v==0x34&&(!strcmp(name,"LAUNCH.D65")||(mode==2&&!strcmp(name,"GAMES.D65"))))e=0x88;
 if((v==0x40||v==0x4a)&&!strcmp(name,"GAMES.D65")){if(mode==1)e=0x8b;if(mode==3)e=0xff;}
 cpu.pf_c=e==0;cpu.a=e;last_error=e;
}}
void cpu65_do_aug_callback(void){abort();}void cpu65_do_nop_callback(void){}
int main(void){for(mode=0;mode<4;mode++){memset(mem,0,sizeof mem);FILE*f=fopen("work/mount-probe/MNTCHECK.PRG","rb");assert(f);int lo=fgetc(f),hi=fgetc(f);assert((lo|(hi<<8))==0x2001);fread(mem+0x2001,1,16384,f);fclose(f);mem[0xffd2]=0x60;mem[0x7000]=0x20;mem[0x7001]=0;mem[0x7002]=0x21;
 memset(&cpu,0,sizeof cpu);cpu.s=255;cpu.sphi=256;cpu.pf_e=1;cpu.pc=0x7000;last_error=0;outlen=0;
 for(int n=0;n<100000;n++){if(cpu.pc==0x7003)break;if(cpu.pc==0xffd2){assert(outlen<sizeof output-1);output[outlen++]=cpu.a==13?'\n':cpu.a;}cpu65_step();}output[outlen]=0;assert(cpu.pc==0x7003);assert(strstr(output,"DONE - NO DISK FILES WRITTEN"));assert(strstr(output,"TRYFIX.D65\nSET    C=1"));
 if(mode==0)assert(strstr(output,"OLD40  C=1 A=00 E=00"));if(mode==1)assert(strstr(output,"OLD40  C=0 A=8B E=8B"));if(mode==2)assert(strstr(output,"GAMES.D65\nSET    C=1 A=00 E=00\nFIND   C=0 A=88 E=88"));if(mode==3)assert(strstr(output,"NEW4A  C=0 A=FF E=FF"));printf("PASS modeled hypervisor scenario %d\n",mode);if(mode==1)puts(output);
}return 0;}
