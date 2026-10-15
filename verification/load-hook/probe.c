#include "xemu/emutools_basicdefs.h"
#include "xemu/cpu65.h"
#include "xemu/f011_core.h"
#include <assert.h>
static Uint8 attic[65536];
static Uint8 io[0x1000],mem[0x60000], cache[512], disk[5570560], original[65536], prior[65536], code[1792];
static unsigned reads,writes,dmas,code_len;
static int corrupt,timeout,error;
static char output[1024];static unsigned outlen;
int fdc_cb_rd_sec(int which,Uint8*b,Uint8 side,Uint8 track,Uint8 sector){
 assert(which==0);unsigned physical=sector-1+side*10;assert(track<80&&physical<60);
 memcpy(b,disk+(track*128+physical)*512,512);
 if(corrupt){b[8]^=1;b[264]^=1;}reads++;return error;
}
int fdc_cb_wr_sec(int which,Uint8*b,Uint8 side,Uint8 track,Uint8 sector){writes++;abort();}
Uint8 cpu65_read_callback(Uint16 a){if(a>=0xd080&&a<=0xd089){if(timeout&&a==0xd082)return 128;return fdc_read_reg(a-0xd080);}if(a>=0xd000&&a<=0xdfff)return io[a-0xd000];return mem[a];}
void cpu65_write_callback(Uint16 a,Uint8 v){
 if(a>=0xd080&&a<=0xd089){fdc_write_reg(a-0xd080,v);return;}if(a>=0xd000&&a<=0xdfff)io[a-0xd000]=v;else mem[a]=v;
 if(a==0xd705){unsigned p=io[0x705]+256*io[0x701]+65536*io[0x702];
 assert(mem[p]==0x80&&mem[p+2]==0x81&&mem[p+4]==0);unsigned sm=mem[p+1],dm=mem[p+3];p+=5;
 assert(io[0x703]==0&&mem[p]==0);unsigned n=mem[p+1]+256*mem[p+2];assert(n);
 unsigned src=mem[p+3]+256*mem[p+4]+65536*mem[p+5],dst=mem[p+6]+256*mem[p+7]+65536*mem[p+8];
 Uint8 *sp,*dp;if(sm==0x87){assert(src>=0xf0000&&src+n<=0x100000);sp=attic+src-0xf0000;}else{assert(sm==0&&src+n<=sizeof mem);sp=mem+src;}if(dm==0x87){assert(dst>=0xf0000&&dst+n<=0xfe000);dp=attic+dst-0xf0000;}else{assert(dm==0&&dst+n<=sizeof mem);dp=mem+dst;}memmove(dp,sp,n);dmas++;
 }
}
void cpu65_write_rmw_callback(Uint16 a,Uint8 old,Uint8 value){cpu65_write_callback(a,value);}
void cpu65_do_aug_callback(void){abort();}void cpu65_do_nop_callback(void){}
static void call(void){
 memset(&cpu,0,sizeof cpu);cpu.s=255;cpu.sphi=0x100;cpu.pf_e=1;cpu.pf_i=0;cpu.pf_d=1;
 cpu.z=0x73;cpu.bphi=0;mem[0x1000]=0x20;mem[0x1001]=0;mem[0x1002]=0x16;cpu.pc=0x1000;
 for(unsigned n=0;n<100000000&&cpu.pc!=0x1003;n++){
  if(cpu.pc==0xffd2){assert(outlen<sizeof output-1);output[outlen++]=cpu.a;mem[0xffd2]=0x60;}
  cpu65_step();
 }
 if(cpu.pc!=0x1003){fprintf(stderr,"stuck pc=%04x\n",cpu.pc);abort();}
 assert(writes==0&&io[0x703]==1&&io[0x704]==0);
 for(unsigned i=0x40000;i<0x60000;i++)assert(mem[i]==0x55);
 assert(cpu.z==0x73&&cpu.pf_i==0&&cpu.pf_d!=0);
}
int main(int argc,char**argv){assert(argc==7);
 FILE*f=fopen(argv[1],"rb");assert(f);assert(fread(disk,1,sizeof disk,f)==sizeof disk);fclose(f);
 f=fopen(argv[2],"rb");assert(f);unsigned len=fread(original,1,sizeof original,f);fclose(f);
 unsigned index=atoi(argv[3]), mode=atoi(argv[4]);
 memset(mem,0x55,sizeof mem);f=fopen(argv[5],"rb");assert(f);code_len=fread(code,1,sizeof code,f);fclose(f);assert(code_len>0);memcpy(mem+0x1600,code,code_len);
 f=fopen(argv[6],"rb");assert(f);assert(fread(attic+0xe000,1,8192,f)>8);fclose(f);
 memset(mem+0x1e00,0,128);mem[0x1e12]=index;mem[0x1e13]=index>>8;
 io[0x68a]=0;io[0x68b]=0x41;io[0x703]=1;io[0x704]=0;mem[0xd0]=0;
 if(mode==1)corrupt=1;
 if(mode==2)attic[0xe000+8+index*24+16]=81;
 if(mode==3)attic[0xe000+8+index*24+19]=0xe1;
 if(mode==4)timeout=1;
 if(mode==5)error=1;
 if(mode==6)io[0x68b]=1;
 if(mode==7)attic[0xe000+8+index*24+18]--;
 if(mode==8){unsigned c=0xe000+8+index*24;unsigned t=attic[c+16],s=attic[c+17],o=((t-1)*256+s)*256;disk[o]=t;disk[o+1]=s;}
 if(mode==9)io[0x68a]=0x40;
 if(mode==10)attic[0xe000]='X';
 if(mode==11){unsigned c=0xe000+8+index*24;unsigned t=attic[c+16],s=attic[c+17],o=((t-1)*256+s)*256;disk[o+2]=2;}
 if(mode==12){mem[0x1e12]=255;mem[0x1e13]=255;}
 if(mode>=20){mem[0x1e0e]=1;mem[0x1e0f]=0;mem[0x1e10]=0;mem[0x1e11]=0xc0;}
 if(mode==21)mem[0x1e0e]=6;
 if(mode==22)attic[0xe006]=2;
 if(mode==24){mem[0x1e0e]=2;memcpy(mem+0x1e20,attic+0xe000+8+index*24,16);}
 if(mode==25){mem[0x1e0e]=5;memcpy(mem+0x1e20,attic+0xe000+8+index*24,16);}
 if(mode==23){mem[0x1e10]=0xf0;mem[0x1e11]=0xf6;}
 memcpy(prior,mem+0x2000,0xd700);
 fdc_init(cache);fdc_set_disk(0,1,0);call();
 if(mode==25){assert(mem[0x1e06]==0&&!memcmp(attic,original,len));assert(!memcmp(prior,mem+0x2000,0xd700));assert(mem[0xd0]==0);}
 else if(mode==20||mode==24){assert(mem[0x1e06]==0&&!memcmp(mem+0xc000,original,len));assert(mem[0x1e1e]+256*mem[0x1e1f]==len);assert(mem[0xd0]==0);}
 else if(mode){assert(mem[0x1e06]==2);assert(!memcmp(prior,mem+0x2000,0xd700));assert(mem[0xd0]==0);}
 else {
  if(mem[0x1e06]){fprintf(stderr,"failure stage=%02x text=%s\n",mem[0x1e08],output);abort();}
  assert(!memcmp(mem+0x2001,original+2,len-2));assert(mem[0x82]+256*mem[0x83]==0x2001+len-2);assert(mem[0xd0]==4);
  // Simulate the running game's use of the former engine workspace. Every
  // subsequent read must work without reinstallation or re-running START.
  memset(mem+0x6000,0xa6,1024);mem[0xd0]=0;mem[0x1e0e]=len>55040?3:2;mem[0x1e0f]=1;mem[0x1e10]=0;mem[0x1e11]=0x20;
  memcpy(mem+0x1e20,attic+0xe000+8+index*24,16);call();
  assert(mem[0x1e06]==0);if(len>55040){assert(!memcmp(mem+0x12001,original+2,len-2));}else{assert(!memcmp(mem+0x12000,original,len));}assert(mem[0xd0]==0);
  for(int i=0;i<1024;i++)assert(mem[0x6000+i]==0xa6);
  // The service must reject writes into its own code or high resident memory.
  memset(mem+0x12000,0x7b,len);mem[0x1e0f]=5;call();assert(mem[0x1e06]==2);
  for(unsigned i=0;i<len;i++)assert(mem[0x12000+i]==0x7b);
  mem[0x1e0e]=2;mem[0x1e0f]=0;mem[0x1e11]=0x16;call();assert(mem[0x1e06]==2);
  // Missing names don't copy bytes or leave a queued RUN.
  mem[0x1e0f]=1;mem[0x1e11]=0x20;memset(mem+0x1e20,'?',16);call();assert(mem[0x1e06]==2&&mem[0x1e08]==0x13);
 }
 printf("PASS index=%u size=%u mode=%u reads=%u DMA=%u\n",index,len,mode,reads,dmas);
}
