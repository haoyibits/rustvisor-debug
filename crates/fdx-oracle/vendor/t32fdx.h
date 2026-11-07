/* 
The MIT License

Copyright (c) 1989-2022 Lauterbach GmbH

Permission is hereby granted, free of charge,
to any person obtaining a copy of this software and
associated documentation files (the "Software"), to
deal in the Software without restriction, including
without limitation the rights to use, copy, modify,
merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom
the Software is furnished to do so,
subject to the following conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES
OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR
ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
*/

/**************************************************************************

	TRACE32 FDX target agent

	$Date: 2022-11-21 17:36:06 +0100 (Mon, 21 Nov 2022) $
	$Revision: 20122 $

	This is the current source for this demo.
	The latest source can be found in demo/etc/fdx/target.

	2003/09/12    initial release
	2009/10/28    DCC3 support
	2021/01/11    introduce T32_Fdx_TraceData2 to transport core
	2021/01/11    use 48bit timestamps for FDX Trace. Please update the implementation of T32_Fdx_GetTimebase!

	possible defines:
		T32_FDX_TRACE_COMPRESSED      enable compressed trace
		T32_FDX_TRACE_UNCOMPRESSED    enable uncompressed trace
		T32_FDX_TRACE_BUFSIZE         size of the trace buffer
		T32_FDX_DCC                   set for DCC4 based communication
		T32_FDX_DCC3                  set for DCC3 based communication

	external calls:
		T32_Fdx_GetTimebase()         return timebase (used for trace only)

	(c) Lauterbach GmbH
	http://www.lauterbach.com/

**************************************************************************/

#ifndef __T32_FDX
#define __T32_FDX

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned int  T32_FDX_PARAMETERTYPE;
typedef unsigned char T32_FDX_DATATYPE;

typedef struct {
	volatile T32_FDX_PARAMETERTYPE enable;
	volatile T32_FDX_PARAMETERTYPE size;
	volatile T32_FDX_PARAMETERTYPE hostindex;
	volatile T32_FDX_PARAMETERTYPE targetindex;
	volatile T32_FDX_PARAMETERTYPE stall;
	volatile T32_FDX_PARAMETERTYPE pending;
	volatile T32_FDX_PARAMETERTYPE transferlen;
	volatile T32_FDX_PARAMETERTYPE transferchannel;
} T32_FDX_BUFFER;


#define T32_Fdx_DefineChannel(channel,size)                 struct { T32_FDX_BUFFER header; volatile T32_FDX_DATATYPE data[size]; } channel
#define T32_Fdx_InitChannel(channel)                        ((channel).header.enable = 0, (channel).header.size = 0, (channel).header.hostindex = 0, (channel).header.targetindex = 0, (channel).header.stall = 0, (channel).header.pending = 0, (channel).header.transferchannel = 0x1, (channel).data[0] = 0)

#define T32_Fdx_EnableChannel(channel)                      ((channel).header.size = sizeof((channel).data))
#define T32_Fdx_DisableChannel(channel)                     ((channel).header.size = 0)

#define T32_Fdx_SetTransferChannel(channel,transferchannel) ((channel).header.transferchannel = transferchannel)

#define T32_Fdx_Pending(channel)                            ((channel)->header.hostindex != (channel)->header.targetindex)


/* send data via channel from target to host: */
extern int  T32_Fdx_SendPoll(void * channel, void * buf, int size); /* non-blocking: returns zero when buffer is full */
extern int  T32_Fdx_Send(void * channel, void * buf, int size);     /* blocking: waits when buffer is full */

/* receive data via channel from host to target:*/
extern int  T32_Fdx_ReceivePoll(void * channel, void * buf, int maxsize); /* non-blocking: returns zero when no data is available */
extern int  T32_Fdx_Receive(void * channel, void * buf, int size);        /* blocking: waits until data is available */

extern void T32_Fdx_Poll(void);
extern void T32_Fdx_PollInterrupt(void);


/* FDX trace: */
#if defined(T32_FDX_TRACE_COMPRESSED) || defined(T32_FDX_TRACE_UNCOMPRESSED)

#if !defined(FDX_NO_INSTRUMENT) && defined(__GNUC__)
# define FDX_NO_INSTRUMENT __attribute__((no_instrument_function))
#endif

extern void T32_Fdx_TraceInit(void);
extern void T32_Fdx_TraceData(int cycletype, void* address, unsigned long data);
extern void T32_Fdx_TraceData2(int core, int cycletype, void* address, unsigned long data, unsigned long long time);

#define T32_FDX_PROGFETCH   0x01
#define T32_FDX_DATAREAD    0x02
#define T32_FDX_DATAWRITE   0x03
#define T32_FDX_CYCLETYPE(accesstype,datawidth) (((datawidth) & 0x0F) | ((accesstype) << 4))

#define T32_Fdx_TraceFunction(address)               T32_Fdx_TraceData( T32_FDX_CYCLETYPE(T32_FDX_PROGFETCH,0),    address,0 )
#define T32_Fdx_TraceDataRead(address, width, data)  T32_Fdx_TraceData( T32_FDX_CYCLETYPE(T32_FDX_DATAREAD,width), address,data )
#define T32_Fdx_TraceDataWrite(address, width, data) T32_Fdx_TraceData( T32_FDX_CYCLETYPE(T32_FDX_DATAWRITE,width),address,data )

extern unsigned long long T32_Fdx_GetTimebase(void); /* <- this is a target specific function and must be implemented by the user */

#endif


#if !defined(FDX_NO_INSTRUMENT)
# define FDX_NO_INSTRUMENT
#endif


#ifdef __cplusplus
}
#endif

#endif /* __T32_FDX */
