#include "xemu/emutools_basicdefs.h"
#include "xemu/cpu65.h"
#include "xemu/f011_core.h"
#include <assert.h>
static Uint8 mem[0x60000],cache[512],disk[5570560],original[0xa001],prior[0xa000];
static unsigned reads,writes,max_sector,dmas,mode;
static int corrupt,timeout,error;
static Uint8 lastcmd;
static char output[512];static unsigned outlen;
int fdc_cb_rd_sec(int which,Uint8*b,Uint8 side,Uint8 track,Uint8 sector){
 assert(which==0); unsigned physical=sector-1+side*10;
 assert(track<80 && physical<60); if(physical>max_sector)max_sector=physical;
 unsigned off=(track*128+physical)*512; memcpy(b,disk+off,512);
 if(corrupt && reads==1){b[8]^=1;b[264]^=1;}
 reads++;return error;
}
int fdc_cb_wr_sec(int which,Uint8*b,Uint8 side,Uint8 track,Uint8 sector){writes++;abort();}
Uint8 cpu65_read_callback(Uint16 a){if(a>=0xd080&&a<=0xd089){if((timeout||(mode==10&&lastcmd==0x20))&&a==0xd082)return 128;return fdc_read_reg(a-0xd080);}return mem[a];}
void cpu65_write_callback(Uint16 a,Uint8 v){
 if(a>=0xd080&&a<=0xd089){if(a==0xd081)lastcmd=v;fdc_write_reg(a-0xd080,v);return;}
 mem[a]=v;
 if(a==0xd700){unsigned p=mem[0xd700]+256*mem[0xd701]+65536*mem[0xd702];
 assert(mem[0xd703]==0&&mem[p]==0); unsigned n=mem[p+1]+256*mem[p+2];assert(n);
 unsigned src=mem[p+3]+256*mem[p+4]+65536*mem[p+5],dst=mem[p+6]+256*mem[p+7]+65536*mem[p+8];
 assert(src+n<=sizeof mem&&dst+n<=sizeof mem);memmove(mem+dst,mem+src,n);dmas++;
 }
}
void cpu65_do_aug_callback(void){abort();} void cpu65_do_nop_callback(void){}
int main(int argc,char**argv){assert(argc==8);
 FILE*f=fopen(argv[1],"rb");assert(f);assert(fread(disk,1,sizeof disk,f)==sizeof disk);fclose(f);
 f=fopen(argv[2],"rb");assert(f);unsigned len=fread(original,1,sizeof original,f);fclose(f);assert(len>2);
 unsigned t=atoi(argv[3]),s=atoi(argv[4]);mode=atoi(argv[5]);
 memset(mem,0x55,sizeof mem);f=fopen(argv[6],"rb");assert(f);unsigned code=fread(mem+0x1600,1,512,f);unsigned ext=fread(mem+0x6000,1,1024,f);fclose(f);assert(code);
 unsigned sum=0;for(int i=0;i<len;i++)sum+=original[i];
 mem[0x1300]=t;mem[0x1301]=s;mem[0x1302]=len;mem[0x1303]=len>>8;mem[0x1304]=sum;mem[0x1305]=sum>>8;mem[0x1307]=0;
 mem[0xd68a]=0;mem[0xd68b]=0x41;mem[0xd703]=1;mem[0xd704]=0;mem[0xd0]=0;
 if(mode==1)corrupt=1;
 if(mode==2)mem[0x1300]=81;
 if(mode==3)mem[0x1303]=0xb0;
 if(mode==4)timeout=1;
 if(mode==5)error=1;
 if(mode==6)mem[0xd68b]=1;
 if(mode==7)mem[0x1302]--;
 if(mode==8){unsigned off=((t-1)*256+s)*256;disk[off]=t;disk[off+1]=s;}
 if(mode==9)mem[0xd68a]=0x40;
 if(mode==11){unsigned off=((t-1)*256+s)*256;disk[off+2]=2;}
 memcpy(prior,mem+0x2001,sizeof prior);
 fdc_init(cache);fdc_set_disk(0,1,0);memset(&cpu,0,sizeof cpu);cpu.s=255;cpu.sphi=0x100;cpu.pf_e=1;cpu.pf_i=1;
 mem[0x1000]=0x20;mem[0x1001]=0;mem[0x1002]=0x16;cpu.pc=0x1000;
 unsigned steps; for(steps=0;steps<100000000&&cpu.pc!=0x1003;steps++){
  if(cpu.pc==0xffd2){ // Stub only error-message output; RTs through actual CPU.
   assert(outlen<sizeof output-1);output[outlen++]=cpu.a;mem[0xffd2]=0x60;
  }
  cpu65_step();
 }
 assert(cpu.pc==0x1003);assert(writes==0);assert(mem[0xd703]==1);
 if(mode==0){assert(mem[0x1306]==0);assert(!memcmp(mem+0x2001,original+2,len-2));assert(mem[0x82]+256*mem[0x83]==0x2001+len-2);assert(mem[0xd0]==4);}
 else {unsigned codes[]={0,0x10,6,3,4,0x0a,1,0x0d,0x0c,2,5,0x0f};
 fprintf(stderr,"diagnostic mode=%u code=%02X text=%s\n",mode,mem[0x1308],output);
 assert(mem[0x1308]==codes[mode]);
 char hex[8];snprintf(hex,sizeof hex,"%02X ",codes[mode]);assert(strstr(output,"CODE FDC MOUNT FORMAT TRACK SECTOR (HEX)"));assert(strstr(output,hex));
 assert(mem[0x1306]==2);assert(!memcmp(prior,mem+0x2001,sizeof prior));assert(mem[0xd0]==0);}
 printf("%s: mode=%u, %u bytes, %u sector reads, highest physical sector=%u, %u DMA copies, %u instructions: PASS\n",argv[7],mode,len,reads,max_sector,dmas,steps);
}
