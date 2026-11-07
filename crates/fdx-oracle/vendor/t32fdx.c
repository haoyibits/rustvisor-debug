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

	The file fdx.h contains the interface to this file.

	(c) Lauterbach GmbH
	http://www.lauterbach.com/

**************************************************************************/


#include "t32fdx.h"

#ifndef  T32_FDX_DOORBELL
# define T32_FDX_DOORBELL()
#endif
#ifndef  T32_FDX_WAITING
# define T32_FDX_WAITING()     T32_Fdx_Waiting()
#endif
#ifndef  T32_FDX_INTDISABLE
# define T32_FDX_INTDISABLE()
#endif
#ifndef  T32_FDX_INTENABLE
# define T32_FDX_INTENABLE()
#endif
#ifndef T32_FDX_COPY
# include "string.h"
# define T32_FDX_COPY(t,s,l)   memcpy((t),(s),(l))
#endif

void * T32_Fdx_SendingChannel;
void * T32_Fdx_ReceivingChannel;

FDX_NO_INSTRUMENT
void T32_Fdx_Waiting()
{
	/* place a breakpoint here to stop debugger for BufferE/BufferC/BufferS polling */
}


/**************************************************************************

	DCC polling functions

**************************************************************************/

#if defined(T32_FDX_DCC)||defined(T32_FDX_DCC3)

#ifdef T32_FDX_DCC

/*imports from t32fdxarm.c*/
extern unsigned long T32_TsMon_SendStatus(void);
extern void          T32_TsMon_SendWord(unsigned long data);
extern unsigned long T32_TsMon_ReceiveStatus(void);
extern unsigned long T32_TsMon_ReceiveWord(void);

# define T32_DCCX_WORDLEN 4
# define T32_DCCX_SENDSTATUS(_TRANSFERCHANNEL_)                T32_TsMon_SendStatus()
# define T32_DCCX_SENDWORD(_TRANSFERCHANNEL_, _SENDWORD_)      T32_TsMon_SendWord(_SENDWORD_)
# define T32_DCCX_RECEIVESTATUS(_TRANSFERCHANNEL_)             T32_TsMon_ReceiveStatus()
# define T32_DCCX_RECEIVEWORD(_TRANSFERCHANNEL_, _RETWORDPTR_) (*(_RETWORDPTR_) = T32_TsMon_ReceiveWord())

#else

/*imports from t32dcc3.c*/
extern unsigned long T32DCC3_SendStatus(int nChannel);
extern void          T32DCC3_SendWord(int nChannel, unsigned int nLength, unsigned int nData3);
extern unsigned long T32DCC3_ReceiveStatus(int DCCChannel);
extern void          T32DCC3_ReceiveWord(int nChannel, unsigned long *pLength, unsigned long *pData3);

# define T32_DCCX_WORDLEN 3
# define T32_DCCX_SENDSTATUS(_TRANSFERCHANNEL_)                T32DCC3_SendStatus(_TRANSFERCHANNEL_)
# define T32_DCCX_SENDWORD(_TRANSFERCHANNEL_, _SENDWORD_)      T32DCC3_SendWord(_TRANSFERCHANNEL_, T32_DCCX_WORDLEN ,_SENDWORD_)
# define T32_DCCX_RECEIVESTATUS(TRANSFERCHANNEL)               T32DCC3_ReceiveStatus(TRANSFERCHANNEL)
# define T32_DCCX_RECEIVEWORD(_TRANSFERCHANNEL_, _RETWORDPTR_) T32DCC3_ReceiveWord(_TRANSFERCHANNEL_, (unsigned long*) 0, _RETWORDPTR_)

#endif

static void T32_Fdx_SendDCC(void * channel)
{
	int             len, headerlen;
	unsigned long   data = 0;
	unsigned int    wordlen;
	volatile T32_FDX_DATATYPE *fdxptr;
	T32_FDX_BUFFER *fdxchannel;

	fdxchannel = channel;
	if (fdxchannel == NULL) {
		return;
	}

	if (T32_DCCX_SENDSTATUS(fdxchannel->transferchannel)) {
		return;
	}

	if (fdxchannel->hostindex == fdxchannel->targetindex) {
		T32_Fdx_SendingChannel = NULL;
		return;
	}
	fdxptr = ((T32_FDX_DATATYPE *)(fdxchannel + 1)) + fdxchannel->hostindex;

	if (!fdxchannel->pending) {

		if (!fdxptr[0])
			return;

		if (*fdxptr == (T32_FDX_DATATYPE)-1) {
			fdxchannel->hostindex = 0;

			fdxptr = ((T32_FDX_DATATYPE *)(fdxchannel + 1)) + fdxchannel->hostindex;

			if (!fdxptr[0])
				return;
		}
		headerlen = 1;
		if (*fdxptr == (T32_FDX_DATATYPE)-2) {
			len = fdxptr[1] | (fdxptr[2] << 8);
			headerlen = 3;
		}
		else if (*fdxptr == (T32_FDX_DATATYPE)-3) {
			len = 0;
		}
		else {
			len = *fdxptr;
		}

		fdxchannel->pending = headerlen + len;
	}
	if (fdxchannel->pending > T32_DCCX_WORDLEN)
		wordlen = T32_DCCX_WORDLEN;
	else
		wordlen = fdxchannel->pending;
	switch (wordlen) {
	case 1:
		data = (unsigned long)fdxptr[0];
		break;
	case 2:
		data = ((unsigned long)fdxptr[0]) | (((unsigned long)fdxptr[1]) << 8);
		break;
	case 3:
		data = ((unsigned long)fdxptr[0]) | (((unsigned long)fdxptr[1]) << 8) | (((unsigned long)fdxptr[2]) << 16);
		break;
	case 4:
		data = ((unsigned long)fdxptr[0]) | (((unsigned long)fdxptr[1]) << 8) | (((unsigned long)fdxptr[2]) << 16) | (((unsigned long)fdxptr[3]) << 24);
		break;
	}

	T32_DCCX_SENDWORD(fdxchannel->transferchannel, data);

	fdxchannel->hostindex += wordlen;
	fdxchannel->pending -= wordlen;
	T32_Fdx_SendingChannel = channel;
}


static void T32_Fdx_ReceiveDCC(void * channel)
{
	int             len, headerlen;
	unsigned long			data;
	unsigned long			wordlen;
	volatile T32_FDX_DATATYPE *fdxptr;
	T32_FDX_BUFFER *fdxchannel;

	fdxchannel = channel;
	if (fdxchannel == NULL) {
		return;
	}
	T32_Fdx_ReceivingChannel = channel;

	if (fdxchannel->stall) {
		data = fdxchannel->stall;
		fdxchannel->stall = 0;
	}
	else {
		if (!T32_DCCX_RECEIVESTATUS(fdxchannel->transferchannel)) {
			return;
		}
		T32_DCCX_RECEIVEWORD(fdxchannel->transferchannel, &data);
	}

	if (!fdxchannel->pending) {

		headerlen = 1;
		len = data & 0xff;
		if ((T32_FDX_DATATYPE)len == (T32_FDX_DATATYPE)-2) {
			headerlen = 3;
			len = (data >> 8) & 0xffff;
		}
		if (((int)fdxchannel->hostindex) - len - 3 - 1 < 0) {
			if (fdxchannel->targetindex < fdxchannel->hostindex || fdxchannel->targetindex >= fdxchannel->size - 1) {
				fdxchannel->stall = data;
				return;
			}
			fdxptr = ((T32_FDX_DATATYPE *)(fdxchannel + 1)) + fdxchannel->size - 1;
			*fdxptr = 0;
			fdxptr = ((T32_FDX_DATATYPE *)(fdxchannel + 1)) + fdxchannel->hostindex;
			*fdxptr = (T32_FDX_DATATYPE)-1;
			fdxchannel->hostindex = fdxchannel->size - 1;
		}
		if (fdxchannel->targetindex < fdxchannel->hostindex && fdxchannel->hostindex - len - 3 - 1 <= fdxchannel->targetindex) {
			fdxchannel->stall = data;
			return;
		}
		fdxchannel->hostindex = fdxchannel->hostindex - 3 + 1;
		fdxchannel->transferlen = len;
		fdxchannel->pending = len;

		fdxptr = ((T32_FDX_DATATYPE *)(fdxchannel + 1)) + fdxchannel->hostindex - len - 1;
		*fdxptr = 0;

		fdxptr = ((T32_FDX_DATATYPE *)(fdxchannel + 1)) + fdxchannel->hostindex - len;

		if (fdxchannel->pending + headerlen > T32_DCCX_WORDLEN)
			wordlen = T32_DCCX_WORDLEN - headerlen;
		else
			wordlen = fdxchannel->pending;
		data >>= headerlen * 8;
		switch (wordlen) {
		case 1:
			fdxptr[0] = data & 0xff;
			break;
		case 2:
			fdxptr[0] = data & 0xff;
			fdxptr[1] = (data >> 8) & 0xff;
			break;
		case 3:
			fdxptr[0] = data & 0xff;
			fdxptr[1] = (data >> 8) & 0xff;
			fdxptr[2] = (data >> 16) & 0xff;
			break;
		}
		fdxchannel->pending -= wordlen;

	}
	else {
		fdxptr = ((T32_FDX_DATATYPE *)(fdxchannel + 1)) + fdxchannel->hostindex - fdxchannel->pending;
		if (fdxchannel->pending > T32_DCCX_WORDLEN)
			wordlen = T32_DCCX_WORDLEN;
		else
			wordlen = fdxchannel->pending;
		switch (wordlen) {
		case 1:
			fdxptr[0] = data & 0xff;
			break;
		case 2:
			fdxptr[0] = data & 0xff;
			fdxptr[1] = (data >> 8) & 0xff;
			break;
		case 3:
			fdxptr[0] = data & 0xff;
			fdxptr[1] = (data >> 8) & 0xff;
			fdxptr[2] = (data >> 16) & 0xff;
			break;
		case 4:
			fdxptr[0] = data & 0xff;
			fdxptr[1] = (data >> 8) & 0xff;
			fdxptr[2] = (data >> 16) & 0xff;
			fdxptr[3] = (data >> 24) & 0xff;
			break;
		}
		fdxchannel->pending -= wordlen;
	}

	if (fdxchannel->pending == 0) {
		len = fdxchannel->transferlen;
		fdxptr = ((T32_FDX_DATATYPE *)(fdxchannel + 1)) + fdxchannel->hostindex;
		fdxptr[0] = len & 0xff;
		fdxptr[1] = (len >> 8) & 0xff;
		fdxptr[2] = (T32_FDX_DATATYPE)-2;
		fdxchannel->hostindex -= (len + 1);
	}
}

#define T32_FDX_SENDPOLL(channel)    T32_Fdx_SendDCC(channel)
#define T32_FDX_RECEIVEPOLL(channel) T32_Fdx_ReceiveDCC(channel)
#define T32_FDX_POLL()               { if (T32_Fdx_SendingChannel) T32_Fdx_SendDCC(T32_Fdx_SendingChannel); else T32_Fdx_ReceiveDCC(T32_Fdx_ReceivingChannel); }

#endif /* T32_FDX_DCC || T32_FDX_DCC3 */


/**************************************************************************

	generic polling macros (polling is not used)

**************************************************************************/

#ifndef T32_FDX_SENDPOLL
# define T32_FDX_SENDPOLL(channel)
#endif

#ifndef T32_FDX_RECEIVEPOLL
# define T32_FDX_RECEIVEPOLL(channel)
#endif

#ifndef T32_FDX_POLL
# define T32_FDX_POLL()
#endif



/**************************************************************************

	Send packet to ringbuffer or check state of ringbuffer

**************************************************************************/

FDX_NO_INSTRUMENT
static int T32_Fdx_SendToBuffer(void * channel, void * buf, int size)
{
	int                        hsize;
	T32_FDX_PARAMETERTYPE      targetindex;
	T32_FDX_BUFFER            *fdxchannel;
	volatile T32_FDX_DATATYPE *fdxptr, *fdxptr2;

	fdxchannel = channel;

	if ((T32_FDX_PARAMETERTYPE)size > fdxchannel->size - 4 || size <= 0)
		return -1;

	hsize = 1;
	if ((unsigned int)size > (unsigned int)((T32_FDX_DATATYPE)-5))
		hsize = 3;
	targetindex = fdxchannel->targetindex;
	if (targetindex + hsize + size + 2 > fdxchannel->size) {	/* wrap around ? */
		if (fdxchannel->hostindex > targetindex || fdxchannel->hostindex <= 1)
			goto stall;
		fdxptr2 = (T32_FDX_DATATYPE *)(fdxchannel + 1);
		*fdxptr2 = (T32_FDX_DATATYPE)0;
		fdxptr = ((T32_FDX_DATATYPE *)(fdxchannel + 1)) + targetindex;
		*fdxptr = (T32_FDX_DATATYPE)-1;
		targetindex = 0;
		fdxchannel->targetindex = targetindex;
	}
	if (fdxchannel->hostindex > targetindex && targetindex + hsize + size + 2 > fdxchannel->hostindex)
		goto stall;

	if (buf) {			/* just polling state ? */

		fdxptr = ((T32_FDX_DATATYPE *)(fdxchannel + 1)) + targetindex;
		T32_FDX_COPY((void *)(fdxptr + hsize), buf, size);	/* copy payload data */

		fdxptr2 = fdxptr + hsize + size;
		*fdxptr2 = 0;		/* mark next packet as not ready */

		if (hsize == 3) {	/* large packet size ? */
			fdxptr[2] = (T32_FDX_DATATYPE)(size >> 8);
			fdxptr[1] = (T32_FDX_DATATYPE)size;
			fdxptr[0] = (T32_FDX_DATATYPE)-2;
		}
		else {
			fdxptr[0] = (T32_FDX_DATATYPE)size;
		}

		fdxchannel->targetindex = targetindex + hsize + size;
		fdxchannel->stall = 0;

		T32_FDX_DOORBELL();
	}
	return size;

stall:
	if (buf && !fdxchannel->stall) {
		fdxptr = ((T32_FDX_DATATYPE *)(fdxchannel + 1)) + targetindex;
		fdxptr2 = fdxptr + 1;
		*fdxptr2 = (T32_FDX_DATATYPE)0;
		*fdxptr = (T32_FDX_DATATYPE)-3;
		fdxchannel->targetindex = targetindex + 1;
		fdxchannel->stall = 1;
	}
	return 0;
}



/**************************************************************************

	Receive packet from ringbuffer or check state of ringbuffer

**************************************************************************/

static int T32_Fdx_ReceiveFromBuffer(void * channel, void * buf, int maxsize)
{
	int             size, hsize, copysize;
	T32_FDX_PARAMETERTYPE targetindex;
	T32_FDX_BUFFER *fdxchannel;
	T32_FDX_DATATYPE *fdxptr;

	fdxchannel = channel;

	targetindex = fdxchannel->targetindex;
	fdxptr = ((T32_FDX_DATATYPE *)(fdxchannel + 1)) + targetindex;

	size = *fdxptr;
	if (size == (T32_FDX_DATATYPE)-1) {
		targetindex = fdxchannel->size - 1;
		fdxptr = ((T32_FDX_DATATYPE *)(fdxchannel + 1)) + targetindex;
		size = *fdxptr;
	}
	if (buf == 0 || size == 0) {/* just return if nothing is ready */
		return size;
	}
	hsize = 1;
	if (size == (T32_FDX_DATATYPE)-2) {
		size = fdxptr[-2] | (fdxptr[-1] << 8);
		hsize = 3;
	}
	targetindex -= size + hsize;

	copysize = (size > maxsize) ? maxsize : size;
	T32_FDX_COPY(buf, (void *)(((T32_FDX_DATATYPE *)(fdxchannel + 1)) + targetindex + 1), copysize);

	fdxchannel->targetindex = targetindex;

	T32_FDX_DOORBELL();

	return copysize;
}


/**************************************************************************

	Send packet to ringbuffer or poll state of ringbuffer

**************************************************************************/

int T32_Fdx_SendPoll(void * channel, void * buf, int size)
{
	int             result;

	T32_FDX_INTDISABLE();

	result = T32_Fdx_SendToBuffer(channel, buf, size);

	T32_FDX_SENDPOLL(channel);

	T32_FDX_INTENABLE();

	return result;
}


/**************************************************************************

	Send packet to ringbuffer, wait till packet can be saved

**************************************************************************/

FDX_NO_INSTRUMENT
int T32_Fdx_Send(void * channel, void * buf, int size)
{
	int             result;

	T32_FDX_INTDISABLE();

	while ((result = T32_Fdx_SendToBuffer(channel, buf, size)) == 0) {
		T32_FDX_WAITING();
		T32_FDX_POLL();
	}

	T32_FDX_SENDPOLL(channel);

	T32_FDX_INTENABLE();

	return result;
}



/**************************************************************************

	Receive packet from ringbuffer or poll state of ringbuffer

**************************************************************************/

int T32_Fdx_ReceivePoll(void * channel, void * buf, int size)
{
	int             result;

	T32_FDX_INTDISABLE();

	T32_FDX_RECEIVEPOLL(channel);

	result = T32_Fdx_ReceiveFromBuffer(channel, buf, size);

	T32_FDX_INTENABLE();

	return result;
}


/**************************************************************************

	Receive packet from ringbuffer, wait till packet arrives

**************************************************************************/

int T32_Fdx_Receive(void * channel, void * buf, int size)
{
	int             result;

	T32_FDX_INTDISABLE();

	T32_FDX_RECEIVEPOLL(channel);

	while ((result = T32_Fdx_ReceiveFromBuffer(channel, buf, size)) == 0) {
		T32_FDX_WAITING();
		T32_FDX_POLL();
	}

	T32_FDX_INTENABLE();

	return result;
}



/**************************************************************************

	Macro variant for fast data transmit

**************************************************************************/

#define T32_FDX_SEND(channel,buf,siz) \
{ \
    int                        hsize, stalled;   \
    T32_FDX_PARAMETERTYPE      targetindex;      \
    T32_FDX_BUFFER            *fdxchannel;       \
    volatile T32_FDX_DATATYPE *fdxptr, *fdxptr2; \
 \
    fdxchannel = (T32_FDX_BUFFER *)(void*)(channel); \
 \
    for (stalled = 0;; stalled = 1) { \
 \
	if (stalled) { \
	    if (!fdxchannel->stall) { \
		fdxptr = ((T32_FDX_DATATYPE *) (fdxchannel + 1)) + targetindex; \
		fdxptr2 = fdxptr + 1; \
		*fdxptr2 = (T32_FDX_DATATYPE) 0; \
		*fdxptr = (T32_FDX_DATATYPE) - 3; \
		fdxchannel->targetindex = targetindex + 1; \
		fdxchannel->stall = 1; \
	    } \
	    T32_FDX_WAITING(); \
	    T32_FDX_POLL(); \
	} \
	hsize = 1; \
	if ((unsigned int) (siz) > (unsigned int) ((T32_FDX_DATATYPE) - 5)) \
	    hsize = 3; \
	targetindex = fdxchannel->targetindex; \
	if (targetindex + hsize + (siz) + 2 > fdxchannel->size) {	/* wrap around ? */ \
	    if (fdxchannel->hostindex > targetindex || fdxchannel->hostindex <= 1) \
		continue; \
	    fdxptr2 = (T32_FDX_DATATYPE *) (fdxchannel + 1); \
	    *fdxptr2 = (T32_FDX_DATATYPE) 0; \
	    fdxptr = ((T32_FDX_DATATYPE *) (fdxchannel + 1)) + targetindex; \
	    *fdxptr = (T32_FDX_DATATYPE) - 1; \
	    targetindex = 0; \
	    fdxchannel->targetindex = targetindex; \
	} \
	if (fdxchannel->hostindex > targetindex && targetindex + hsize + (siz) + 2 > fdxchannel->hostindex) \
	    continue; \
 \
	fdxptr = ((T32_FDX_DATATYPE *) (fdxchannel + 1)) + targetindex; \
	T32_FDX_COPY((void *) (fdxptr + hsize), (buf), (siz));	/* copy payload data */ \
 \
	fdxptr2 = fdxptr + hsize + (siz); \
	*fdxptr2 = 0;		/* mark next packet as not ready */ \
 \
	if (hsize == 3) {	/* large packet (siz) ? */ \
	    fdxptr[2] = (T32_FDX_DATATYPE)((siz) >> 8); \
	    fdxptr[1] = (T32_FDX_DATATYPE)(siz); \
	    fdxptr[0] = (T32_FDX_DATATYPE) - 2; \
	} else { \
	    fdxptr[0] = (T32_FDX_DATATYPE)(siz); \
	} \
 \
	fdxchannel->targetindex = targetindex + hsize + (siz); \
	fdxchannel->stall = 0; \
 \
	T32_FDX_DOORBELL(); \
	break; \
    } \
}



/**************************************************************************

	Execute any interface polling

	Only needed for special communication variants.

**************************************************************************/

void T32_Fdx_Poll(void)
{
	T32_FDX_INTDISABLE();

	T32_FDX_POLL();

	T32_FDX_INTENABLE();
}


void T32_Fdx_PollInterrupt(void)
{
	T32_FDX_POLL();
}


#if defined(T32_FDX_TRACE_COMPRESSED) || defined(T32_FDX_TRACE_UNCOMPRESSED)

/**************************************************************************

	FDX Tracing functions

**************************************************************************/

#ifndef T32_FDX_TRACE_BUFSIZE
# define T32_FDX_TRACE_BUFSIZE 512
#endif

T32_Fdx_DefineChannel(FdxTraceSendBuffer, T32_FDX_TRACE_BUFSIZE);

FDX_NO_INSTRUMENT
void T32_Fdx_TraceData(int cycletype, void* address, unsigned long data)
{
	unsigned long long timestamp = T32_Fdx_GetTimebase();
	T32_Fdx_TraceData2(/*core*/ 0, cycletype, address, data, timestamp);
}

#ifdef T32_FDX_TRACE_COMPRESSED

static int T32_Fdx_LastCycle;
static unsigned long long T32_Fdx_LastTimestamp;
static unsigned long T32_Fdx_LastAddress;
static unsigned long T32_Fdx_LastData;
static int T32_Fdx_LastCore;

FDX_NO_INSTRUMENT
void T32_Fdx_TraceInit(void)
{
	T32_Fdx_LastCycle = 0;
	T32_Fdx_LastTimestamp = 0;
	T32_Fdx_LastAddress = 0;
	T32_Fdx_LastData = 0;
	T32_Fdx_LastCore = 0;

	T32_Fdx_InitChannel(FdxTraceSendBuffer);
	T32_Fdx_EnableChannel(FdxTraceSendBuffer);
}

#if defined(C55X)

FDX_NO_INSTRUMENT
void T32_Fdx_TraceData2(int core, int cycletype, void* address, unsigned long data, unsigned long long timestamp)
{
	//transfer of core is not implemented so far

	int                len;
	unsigned long      addr;
	unsigned int       ctrl;
	unsigned int       buf[8];

	ctrl = 0x00;
	len = 1;
	if (cycletype != T32_Fdx_LastCycle) {
		ctrl |= 0x80;
		ctrl |= (cycletype << 8);
		T32_Fdx_LastCycle = cycletype;
	}
	if (timestamp != T32_Fdx_LastTimestamp) {
		if ((timestamp & 0xffffffffffffff00ull) == (T32_Fdx_LastTimestamp & 0xffffffffffffff00ull) && !(ctrl & 0x80)) {
			ctrl |= 0x20;
			ctrl |= (timestamp & 0xff) << 8;
		}
		else if ((timestamp & 0xffffffffffff0000ull) == (T32_Fdx_LastTimestamp & 0xffffffffffff0000ull)) {
			buf[len] = timestamp & 0xffff;
			len += 1;
			ctrl |= 0x40;
		}
		else {
			buf[len] = timestamp & 0xffff;
			buf[len + 1] = (timestamp >> 16) & 0xffff;
			buf[len + 2] = (timestamp >> 32) & 0xffff;
			len += 3;
			ctrl |= 0x60;
		}
		T32_Fdx_LastTimestamp = timestamp;
	}
	addr = (unsigned long)address;
	if (addr != T32_Fdx_LastAddress) {
		if ((addr & 0xffffff00) == (T32_Fdx_LastAddress & 0xffffff00)) {
			switch ((int)(addr - T32_Fdx_LastAddress)) {
			case 0:
				break;
			case 1:
				ctrl |= 0x04;
				break;
			case 2:
				ctrl |= 0x08;
				break;
			case 4:
				ctrl |= 0x0c;
				break;
			case 8:
				ctrl |= 0x10;
				break;
			default:
				buf[len] = addr & 0xffff;
				len += 1;
				ctrl |= 0x14;
			}
		}
		else if ((addr & 0xffff0000) == (T32_Fdx_LastAddress & 0xffff0000)) {
			buf[len] = addr & 0xffff;
			len += 1;
			ctrl |= 0x18;
		}
		else {
			buf[len] = addr & 0xffff;
			buf[len + 1] = (addr >> 16) & 0xffff;
			len += 2;
			ctrl |= 0x1c;
		}

		T32_Fdx_LastAddress = addr;
	}
	if (data != T32_Fdx_LastData) {
		if ((data & 0xffffff00) == (T32_Fdx_LastData & 0xffffff00) && !(ctrl & 0xa0)) {
			ctrl |= 0x01;
			ctrl |= (data & 0xff) << 8;
		}
		else if ((data & 0xffff0000) == (T32_Fdx_LastData & 0xffff0000)) {
			buf[len] = data & 0xffff;
			len += 1;
			ctrl |= 0x02;
		}
		else {
			buf[len] = data & 0xffff;
			buf[len + 1] = (data >> 16) & 0xffff;
			len += 2;
			ctrl |= 0x03;
		}

		T32_Fdx_LastData = data;
	}
	buf[0] = ctrl;
	T32_Fdx_Send(&FdxTraceSendBuffer, buf, len);
}

#else

FDX_NO_INSTRUMENT
void T32_Fdx_TraceData2(int core, int cycletype, void* address, unsigned long data, unsigned long long timestamp)
{
	int             len;
	int             ctrlidx;
	unsigned long   addr;
	unsigned char   ctrl;
	unsigned char   buf[16+5];

	len = 0;
	ctrlidx = 0;
	
	if (core != T32_Fdx_LastCore) {
		//core information prefix
		buf[len] = 0x80; len++; //ctrl
		buf[len] = 0x4 << 4; //access type = 0x4
		if (core <= 0xFF) {
			buf[len] |= 0x1; len++; //width = 1
			buf[len] = core & 0xFF; len++;
		} else if (core <= 0xFFFF) {
			buf[len] |= 0x2; len++; //width = 2
			buf[len] = core & 0xFF; len++;
			buf[len] = (core >> 8) & 0xFF; len++;
		} else {
			buf[len] |= 0x4; len++; //width = 4
			buf[len] = core & 0xFF; len++;
			buf[len] = (core >> 8) & 0xFF; len++;
			buf[len] = (core >> 16) & 0xFF; len++;
			buf[len] = (core >> 24) & 0xFF; len++;
		}
		T32_Fdx_LastCore = core;
		ctrlidx = len;
	}
	
	ctrl = 0x00;
	len += 1; //space for ctrl
	if (cycletype != T32_Fdx_LastCycle) {
		buf[len] = cycletype;
		len++;
		ctrl |= 0x80;
		T32_Fdx_LastCycle = cycletype;
	}

	if (timestamp != T32_Fdx_LastTimestamp) {
		if ((timestamp & 0xffffffffffffff00ull) == (T32_Fdx_LastTimestamp & 0xffffffffffffff00ull)) {
			buf[len] = timestamp & 0xff;
			len += 1;
			ctrl |= 0x20;
		}
		else if ((timestamp & 0xffffffffffff0000ull) == (T32_Fdx_LastTimestamp & 0xffffffffffff0000ull)) {
			buf[len] = timestamp & 0xff;
			buf[len + 1] = (timestamp >> 8) & 0xff;
			len += 2;
			ctrl |= 0x40;
		}
		else {
			buf[len] = timestamp & 0xff;
			buf[len + 1] = (timestamp >> 8) & 0xff;
			buf[len + 2] = (timestamp >> 16) & 0xff;
			buf[len + 3] = (timestamp >> 24) & 0xff;
			buf[len + 4] = (timestamp >> 32) & 0xff;
			buf[len + 5] = (timestamp >> 40) & 0xff;;
			len += 6;
			ctrl |= 0x60;
		}
		T32_Fdx_LastTimestamp = timestamp;
	}

	addr = (unsigned long)address;
	if (addr != T32_Fdx_LastAddress) {
		if ((addr & 0xffffff00) == (T32_Fdx_LastAddress & 0xffffff00)) {
			switch ((int)(addr - T32_Fdx_LastAddress)) {
			case 0:
				break;
			case 1:
				ctrl |= 0x04;
				break;
			case 2:
				ctrl |= 0x08;
				break;
			case 4:
				ctrl |= 0x0c;
				break;
			case 8:
				ctrl |= 0x10;
				break;
			default:
				buf[len] = addr & 0xff;
				len += 1;
				ctrl |= 0x14;
			}
		}
		else if ((addr & 0xffff0000) == (T32_Fdx_LastAddress & 0xffff0000)) {
			buf[len] = addr & 0xff;
			buf[len + 1] = (addr >> 8) & 0xff;
			len += 2;
			ctrl |= 0x18;
		}
		else {
			buf[len] = addr & 0xff;
			buf[len + 1] = (addr >> 8) & 0xff;
			buf[len + 2] = (addr >> 16) & 0xff;
			buf[len + 3] = (addr >> 24) & 0xff;
			len += 4;
			ctrl |= 0x1c;
		}

		T32_Fdx_LastAddress = addr;
	}

	if (data != T32_Fdx_LastData) {
		if ((data & 0xffffff00) == (T32_Fdx_LastData & 0xffffff00)) {
			buf[len] = data & 0xff;
			len += 1;
			ctrl |= 0x01;
		}
		else if ((data & 0xffff0000) == (T32_Fdx_LastData & 0xffff0000)) {
			buf[len] = data & 0xff;
			buf[len + 1] = (data >> 8) & 0xff;
			len += 2;
			ctrl |= 0x02;
		}
		else {
			buf[len] = data & 0xff;
			buf[len + 1] = (data >> 8) & 0xff;
			buf[len + 2] = (data >> 16) & 0xff;
			buf[len + 3] = (data >> 24) & 0xff;
			len += 4;
			ctrl |= 0x03;
		}

		T32_Fdx_LastData = data;
	}

	buf[ctrlidx] = ctrl;
	T32_Fdx_Send(&FdxTraceSendBuffer, buf, len);
}

#endif
#endif /* T32_FDX_TRACE_COMPRESSED */

#ifdef T32_FDX_TRACE_UNCOMPRESSED

FDX_NO_INSTRUMENT
void T32_Fdx_TraceInit(void)
{
	T32_Fdx_InitChannel(FdxTraceSendBuffer);
	T32_Fdx_EnableChannel(FdxTraceSendBuffer);
}

FDX_NO_INSTRUMENT
void T32_Fdx_TraceData2(int core, int cycletype, void* address, unsigned long data, unsigned long long timestamp)
{
	struct {
#ifndef C55X
		unsigned char   align;
#endif
		unsigned char   cycletype;
		unsigned short  tsupper;
		unsigned long   tslower;
		unsigned long   address;
		unsigned long   data;
		unsigned long   core;
	} traceframe;

	int  len = sizeof(traceframe);

#ifndef C55X
	len--;
#endif

	T32_FDX_INTDISABLE();

	traceframe.cycletype = (unsigned char)cycletype;
	traceframe.tsupper = (timestamp >> 32) & 0xFFFF;
	traceframe.tslower = timestamp & 0xFFFFFFFF;
	traceframe.address = (unsigned long)address;
	traceframe.data = data;
	traceframe.core = (unsigned long)core;

	T32_FDX_SEND(&FdxTraceSendBuffer, &traceframe.cycletype, len);

	T32_FDX_SENDPOLL(&FdxTraceSendBuffer);

	T32_FDX_INTENABLE();
}

#endif /* T32_FDX_TRACE_UNCOMPRESSED */

#endif /* T32_FDX_TRACE_COMPRESSED || T32_FDX_TRACE_UNCOMPRESSED */
