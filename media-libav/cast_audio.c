/* AAC/resampler portions adapted from bilikara-next live_audio.c (MIT).
 * Copyright (c) 2026 bilikara contributors; see ../LICENSE.
 * Single selected track only: no L1 DSP, pitch or offset control. */
#include <libswresample/swresample.h>
#define CAST_AUDIO_RATE 48000
#define CAST_AUDIO_CACHE (CAST_AUDIO_RATE*16)
#define CAST_AUDIO_CONVERT 400000

typedef struct {
    LiveInput input;
    AVCodecContext *decoder;
    SwrContext *swr;
    AVFrame *frame;
    float *cache,*converted;
    int64_t base;
    int count,head,draining,done;
} CastAudioSource;
typedef struct {
    CastAudioSource sources[1];
    AVCodecContext *encoder;
    AVFrame *frame;
    int64_t source_frame,frames,sent;
    int draining;
    float pcm[1024*2];
} CastAudio;

static void cast_audio_source_close(CastAudioSource *s) {
    avcodec_free_context(&s->decoder);swr_free(&s->swr);av_frame_free(&s->frame);
    av_freep(&s->cache);av_freep(&s->converted);live_close(&s->input);
}
static uint32_t cast_input_audio(LiveInput *in,const BmRequest *q) {
    uint8_t magic[4];
    /* Descriptor-only format detection. live_open independently checks scope. */
    if(q->fd<0 || !q->cancelled) return BM_INVALID_REQUEST;
    if(interrupted(&(Call){.request=q})) return BM_CANCELLED;
#ifdef _WIN32
    if(!bm_regular_fd(q->fd) || _lseeki64(q->fd,0,SEEK_SET)<0 || _read(q->fd,magic,4)!=4 || _lseeki64(q->fd,0,SEEK_SET)<0) return BM_IO;
#else
    struct stat st;
    if(fstat(q->fd,&st)||!S_ISREG(st.st_mode)) return BM_INVALID_REQUEST;
    if(lseek(q->fd,0,SEEK_SET)<0||read(q->fd,magic,4)!=4||lseek(q->fd,0,SEEK_SET)<0) return BM_IO;
#endif
    uint32_t status=live_open(in,q,AVMEDIA_TYPE_AUDIO,AV_CODEC_ID_NONE,!memcmp(magic,"fLaC",4)?"flac":"mov");
    return status;
}
static uint32_t cast_audio_source_open(CastAudioSource *s,const BmRequest *q) {
    uint32_t status=cast_input_audio(&s->input,q);
    if(status) return status;
    AVCodecParameters *p=s->input.format->streams[0]->codecpar;
    if(p->codec_id!=AV_CODEC_ID_AAC && p->codec_id!=AV_CODEC_ID_FLAC && p->codec_id!=AV_CODEC_ID_EAC3 &&
       p->codec_id!=AV_CODEC_ID_AC3 && p->codec_id!=AV_CODEC_ID_MP3 && p->codec_id!=AV_CODEC_ID_OPUS) return BM_UNSUPPORTED_CODEC;
    if(p->sample_rate<8000||p->sample_rate>192000||p->ch_layout.nb_channels<1||p->ch_layout.nb_channels>8||
       !av_channel_layout_check(&p->ch_layout)) return BM_UNSUPPORTED_LAYOUT;
    const AVCodec *codec=avcodec_find_decoder(p->codec_id);
    if(!codec) return BM_UNSUPPORTED_CODEC;
    s->decoder=avcodec_alloc_context3(codec);s->frame=av_frame_alloc();
    s->cache=av_malloc_array(CAST_AUDIO_CACHE*2,sizeof(float));s->converted=av_malloc_array(CAST_AUDIO_CONVERT*2,sizeof(float));
    if(!s->decoder||!s->frame||!s->cache||!s->converted) return BM_BACKEND_FAILURE;
    int ret=avcodec_parameters_to_context(s->decoder,p);
    s->decoder->thread_count=1;s->decoder->pkt_timebase=s->input.format->streams[0]->time_base;
    s->decoder->err_recognition=AV_EF_CRCCHECK|AV_EF_BITSTREAM|AV_EF_BUFFER|AV_EF_EXPLODE;
    if(ret>=0) ret=avcodec_open2(s->decoder,codec,NULL);
    if(ret<0) return error_status(ret);
    AVChannelLayout stereo=AV_CHANNEL_LAYOUT_STEREO;
    ret=swr_alloc_set_opts2(&s->swr,&stereo,AV_SAMPLE_FMT_FLT,CAST_AUDIO_RATE,&p->ch_layout,s->decoder->sample_fmt,p->sample_rate,0,NULL);
    if(ret>=0) ret=swr_init(s->swr);
    return ret<0?error_status(ret):BM_OK;
}
static int cast_audio_append(CastAudioSource *s,int64_t start,int count) {
    if(start < -INT64_C(604800)*CAST_AUDIO_RATE || start > INT64_C(604800)*CAST_AUDIO_RATE || count<0 || count>CAST_AUDIO_CONVERT) return AVERROR_INVALIDDATA;
    if(!count) return 0;
    if(!s->count) s->base=start;
    int64_t gap=start-(s->base+s->count);
    /* Accept only sample-clock quantization, never silently bridge bad media. */
    if(gap < -48 || gap > 48) return AVERROR_INVALIDDATA;
    int skip=gap<0?(int)FFMIN(-gap,count):0;
    int zeros=gap>0?(int)gap:0;
    int add=count-skip+zeros;
    if(s->count+add>CAST_AUDIO_CACHE) {
        int drop=s->count+add-CAST_AUDIO_CACHE;
        s->head=(s->head+drop)%CAST_AUDIO_CACHE;s->base+=drop;s->count-=drop;
    }
    for(int n=0;n<zeros;n++) {int index=(s->head+s->count+n)%CAST_AUDIO_CACHE;s->cache[index*2]=s->cache[index*2+1]=0;}
    s->count+=zeros;
    for(int n=skip*2;n<count*2;n++) if(!isfinite(s->converted[n])) return AVERROR_INVALIDDATA;
    int position=(s->head+s->count)%CAST_AUDIO_CACHE;
    int first=FFMIN(count-skip,CAST_AUDIO_CACHE-position);
    memcpy(s->cache+position*2,s->converted+skip*2,first*2*sizeof(float));
    memcpy(s->cache,s->converted+(skip+first)*2,(count-skip-first)*2*sizeof(float));
    s->count+=count-skip;return 0;
}
static int cast_audio_decode(CastAudioSource *s) {
    for(;;) {
        if(interrupted(&s->input.call)) return AVERROR_EXIT;
        int ret=avcodec_receive_frame(s->decoder,s->frame);
        if(ret==AVERROR_EOF) {
            int64_t start=s->base+s->count;
            uint8_t *out=(uint8_t *)s->converted;
            int count=swr_convert(s->swr,&out,CAST_AUDIO_CONVERT,NULL,0);
            if(count<0) return count;
            ret=cast_audio_append(s,start,count);s->done=1;return ret;
        }
        if(ret>=0) {
            AVFrame *f=s->frame;
            if(f->nb_samples<=0||f->nb_samples>65536||f->pts==AV_NOPTS_VALUE||f->decode_error_flags||
               f->sample_rate!=s->decoder->sample_rate||f->format!=s->decoder->sample_fmt||
               av_channel_layout_compare(&f->ch_layout,&s->decoder->ch_layout)) {av_frame_unref(f);return AVERROR_INVALIDDATA;}
            int64_t position=av_rescale_q(f->pts,s->input.format->streams[0]->time_base,(AVRational){1,CAST_AUDIO_RATE});
            int64_t delay=swr_get_delay(s->swr,CAST_AUDIO_RATE);
            if(position < -INT64_C(604800)*CAST_AUDIO_RATE || position > INT64_C(604800)*CAST_AUDIO_RATE || delay<0 || delay>CAST_AUDIO_CONVERT) {av_frame_unref(f);return AVERROR_INVALIDDATA;}
            int64_t start=position-delay;
            uint8_t *out=(uint8_t *)s->converted;
            int count=swr_convert(s->swr,&out,CAST_AUDIO_CONVERT,(const uint8_t **)f->extended_data,f->nb_samples);
            av_frame_unref(f);
            if(count<0) return count;
            return cast_audio_append(s,start,count);
        }
        if(ret!=AVERROR(EAGAIN)||s->draining) return ret;
        ret=live_read(&s->input);
        if(ret<0) return ret;
        if(!ret) {s->draining=1;ret=avcodec_send_packet(s->decoder,NULL);}
        else ret=avcodec_send_packet(s->decoder,s->input.packet);
        if(ret<0) return ret;
    }
}
static int cast_audio_seek(CastAudioSource *s,int64_t target) {
    int64_t timestamp=av_rescale_q(FFMAX(0,target-CAST_AUDIO_RATE/4),(AVRational){1,CAST_AUDIO_RATE},s->input.format->streams[0]->time_base);
    int ret=av_seek_frame(s->input.format,0,timestamp,AVSEEK_FLAG_BACKWARD);
    if(ret<0) return ret;
    avcodec_flush_buffers(s->decoder);swr_close(s->swr);ret=swr_init(s->swr);
    s->count=s->head=0;s->draining=s->done=s->input.eof=0;return ret;
}
static int32_t cast_audio_read(void *opaque,uint32_t track,int64_t start,uint32_t frames,float *out) {
    CastAudio *a=opaque;
    if(track!=0||!frames||frames>16384||!out||
       start < -INT64_C(604800)*CAST_AUDIO_RATE||start>INT64_C(604800)*CAST_AUDIO_RATE) return BM_INVALID_REQUEST;
    CastAudioSource *s=&a->sources[track];
    memset(out,0,frames*2*sizeof(float));
    int64_t end=start+frames;
    if(end<=0) return BM_OK;
    int64_t wanted=FFMAX(0,start);
    if((s->count && wanted<s->base) || (!s->done && wanted>s->base+s->count+CAST_AUDIO_CACHE-CAST_AUDIO_RATE)) {
        int ret=cast_audio_seek(s,wanted);if(ret<0) return error_status(ret);
    }
    while(!s->done && s->base+s->count<end) {
        int ret=cast_audio_decode(s);if(ret<0) return interrupted(&s->input.call)?BM_CANCELLED:error_status(ret);
    }
    /* A final partial AAC frame may be padded; a truncated source must not
     * become seconds/minutes of fabricated silence from container duration. */
    if(s->done && end>s->base+s->count+1024) return BM_INVALID_MEDIA;
    if(wanted<s->base) {
        // Only leading silence before an actual positive stream start is valid.
        AVStream *stream=s->input.format->streams[0];
        int64_t declared=stream->start_time==AV_NOPTS_VALUE?0:av_rescale_q(stream->start_time,stream->time_base,(AVRational){1,CAST_AUDIO_RATE});
        if(wanted>=declared) return BM_INVALID_MEDIA;
    }
    int64_t begin=FFMAX(wanted,s->base),limit=FFMIN(end,s->base+s->count);
    if(limit>begin) {
        int position=(s->head+(int)(begin-s->base))%CAST_AUDIO_CACHE;
        int first=(int)FFMIN(limit-begin,CAST_AUDIO_CACHE-position);
        memcpy(out+(begin-start)*2,s->cache+position*2,first*2*sizeof(float));
        memcpy(out+(begin-start+first)*2,s->cache,(size_t)(limit-begin-first)*2*sizeof(float));
    }
    return BM_OK;
}
static void cast_audio_release(LiveInput *in) {
    CastAudio *a=in->state;if(!a) return;
    for(int n=0;n<1;n++) cast_audio_source_close(&a->sources[n]);
    avcodec_free_context(&a->encoder);av_frame_free(&a->frame);av_free(a);in->state=NULL;
}
static int cast_audio_next(LiveInput *in) {
    CastAudio *a=in->state;
    for(;;) {
        if(interrupted(&in->call)) return AVERROR_EXIT;
        int ret=avcodec_receive_packet(a->encoder,in->packet);
        if(ret==AVERROR_EOF) {in->eof=1;return 0;}
        if(ret>=0) return 1;
        if(ret!=AVERROR(EAGAIN)||a->draining) return ret;
        if(a->sent>=a->frames) {a->draining=1;ret=avcodec_send_frame(a->encoder,NULL);}
        else {
            int32_t status=cast_audio_read(a,0,a->source_frame+a->sent,1024,a->pcm);
            if(status) return status==BM_CANCELLED?AVERROR_EXIT:AVERROR_INVALIDDATA;
            ret=av_frame_make_writable(a->frame);if(ret<0) return ret;
            int count=(int)FFMIN(1024,a->frames-a->sent);
            for(int n=0;n<1024;n++) for(int c=0;c<2;c++) {
                float value=n<count?a->pcm[n*2+c]:0;
                if(!isfinite(value)) return AVERROR_INVALIDDATA;
                ((float *)a->frame->data[c])[n]=value;
            }
            a->frame->pts=a->source_frame+a->sent;a->sent+=count;
            ret=avcodec_send_frame(a->encoder,a->frame);
        }
        if(ret<0) return ret;
    }
}
static uint32_t cast_audio_open(LiveInput *in,const BmRequest *q,int64_t source_us,uint32_t *transcoded) {
    /* Inspect once to preserve AAC bytes and use the same source validation. */
    uint32_t status=cast_input_audio(in,q);
    if(status) return status;
    if(in->config->codec_id==AV_CODEC_ID_AAC) {
        const AVCodecParameters *p=in->config;
        if(!p->extradata || p->extradata_size<=0 || p->ch_layout.nb_channels<1 ||
            p->ch_layout.nb_channels>8 || p->sample_rate<8000 || p->sample_rate>192000) return BM_UNSUPPORTED_CODEC;
        if(source_us>0) {
            int ret=av_seek_frame(in->format,0,av_rescale_q(source_us,AV_TIME_BASE_Q,in->format->streams[0]->time_base),AVSEEK_FLAG_BACKWARD);
            if(ret<0) return error_status(ret);
        }
        return BM_OK;
    }
    AVStream *src=in->format->streams[0];
    int64_t end_us=src->duration==AV_NOPTS_VALUE ? 0 : av_rescale_q(src->duration,src->time_base,AV_TIME_BASE_Q);
    if(end_us<=0 || end_us>INT64_C(21600000000)) return BM_UNSUPPORTED_LAYOUT;
    if(src->start_time!=AV_NOPTS_VALUE) {
        int64_t start=av_rescale_q(src->start_time,src->time_base,AV_TIME_BASE_Q);
        if(start < -INT64_C(604800000000) || start > INT64_C(604800000000)) return BM_INVALID_MEDIA;
        end_us+=start;
    }
    live_close(in);memset(in,0,sizeof(*in));
    *transcoded=1;
    CastAudio *a=av_mallocz(sizeof(*a));
    if(!a) return BM_BACKEND_FAILURE;
    in->state=a;in->release=cast_audio_release;in->next=cast_audio_next;in->call.request=q;
    a->source_frame=av_rescale_q(source_us,AV_TIME_BASE_Q,(AVRational){1,CAST_AUDIO_RATE});
    a->frames=av_rescale_q(end_us-source_us,AV_TIME_BASE_Q,(AVRational){1,CAST_AUDIO_RATE});
    if(a->frames<=0||a->frames>INT64_C(21600)*CAST_AUDIO_RATE) return BM_UNSUPPORTED_LAYOUT;
    status=cast_audio_source_open(&a->sources[0],q);if(status) return status;
    const AVCodec *codec=avcodec_find_encoder(AV_CODEC_ID_AAC);
    a->encoder=avcodec_alloc_context3(codec);a->frame=av_frame_alloc();in->packet=av_packet_alloc();in->format=avformat_alloc_context();
    if(!a->encoder||!a->frame||!in->packet||!in->format) return BM_BACKEND_FAILURE;
    AVCodecContext *e=a->encoder;
    e->sample_rate=CAST_AUDIO_RATE;e->sample_fmt=AV_SAMPLE_FMT_FLTP;e->ch_layout=(AVChannelLayout)AV_CHANNEL_LAYOUT_STEREO;
    e->bit_rate=192000;e->profile=AV_PROFILE_AAC_LOW;e->time_base=(AVRational){1,CAST_AUDIO_RATE};e->thread_count=1;e->flags|=AV_CODEC_FLAG_GLOBAL_HEADER;
    int ret=avcodec_open2(e,codec,NULL);if(ret<0) return error_status(ret);
    if(e->frame_size!=1024||e->sample_fmt!=AV_SAMPLE_FMT_FLTP) return BM_UNSUPPORTED_LAYOUT;
    a->frame->nb_samples=1024;a->frame->sample_rate=CAST_AUDIO_RATE;a->frame->format=e->sample_fmt;
    ret=av_channel_layout_copy(&a->frame->ch_layout,&e->ch_layout);
    if(ret>=0) ret=av_frame_get_buffer(a->frame,0);
    if(ret<0) return error_status(ret);
    AVStream *stream=avformat_new_stream(in->format,NULL);if(!stream) return BM_BACKEND_FAILURE;
    stream->time_base=e->time_base;in->format->bit_rate=e->bit_rate;
    ret=avcodec_parameters_from_context(stream->codecpar,e);
    return ret<0?error_status(ret):BM_OK;
}
