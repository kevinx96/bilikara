/* Test-only inclusion exercises pinned AVERROR mappings and the deny callback;
 * these helpers are not extra companion exports. Real success uses Rust tests. */
#include <libavformat/avformat.h>
/* Private compile-time seam: inject only after successful real reads. This is
 * lifecycle/error handling evidence, not proof of real-media rejection. */
static unsigned reads, inject_after;
static int inject_error, inject_corrupt, stop_after;
static unsigned remux_fault;
static int *stop_flag;
static int test_read_frame(AVFormatContext *s, AVPacket *p) {
    if (inject_after && reads == inject_after && inject_error) return inject_error;
    int ret = av_read_frame(s, p);
    if (ret >= 0) {
        reads++;
        if (inject_after && reads == inject_after && inject_corrupt) p->flags |= AV_PKT_FLAG_CORRUPT;
        if (remux_fault == 5 && reads == 3) p->dts = AV_NOPTS_VALUE;
        if (remux_fault == 7 && reads == 3) s->streams[p->stream_index]->nb_frames += 1;
        if (remux_fault == 6 && reads == 3) {
            AVCodecParameters *par = s->streams[p->stream_index]->codecpar;
            uint8_t *side = av_packet_new_side_data(p, AV_PKT_DATA_NEW_EXTRADATA, par->extradata_size);
            if (!side) return AVERROR(ENOMEM);
            par->extradata[par->extradata_size - 1] ^= 1;
            memcpy(side, par->extradata, par->extradata_size);
        }
        if (stop_after && reads == (unsigned)stop_after) *stop_flag = 1;
    }
    return ret;
}
static unsigned remux_writes;
static void (*remux_checkpoint)(void *);
static void *remux_opaque;
static int test_write_frame(AVFormatContext *s, AVPacket *p) {
    int packet = p != NULL;
    int ret = av_interleaved_write_frame(s, p);
    if (ret >= 0 && packet && ++remux_writes == 3 && remux_fault == 1)
        remux_checkpoint(remux_opaque);
    return ret;
}
static int test_write_trailer(AVFormatContext *s) {
    int ret = av_write_trailer(s);
    return ret >= 0 && remux_fault == 2 ? AVERROR(EIO) : ret;
}
#define av_read_frame test_read_frame
#define av_interleaved_write_frame test_write_frame
#define av_write_trailer test_write_trailer
#include "probe.c"
#undef av_read_frame
#undef av_interleaved_write_frame
#undef av_write_trailer
#include <assert.h>
#include <stdio.h>

static int cancelled(void *opaque) { return *(int *)opaque; }

/* Only the private test companion exports this setter. No test flags/env
 * handling are linked into the real companion or its negotiated schema. */
BM_EXPORT void bm_test_remux_fault(uint32_t mode, void (*checkpoint)(void *), void *opaque) {
    remux_fault = mode; remux_writes = 0;
    remux_checkpoint = checkpoint; remux_opaque = opaque;
    reads = 0; inject_after = mode == 3 || mode == 4 ? 3 : 0;
    inject_error = mode == 3 ? AVERROR(EIO) : 0;
    inject_corrupt = mode == 4; stop_after = 0;
}

static void scan_tests(void) {
    BmScanInfo info;
    assert(bm_scan_info_v1(sizeof(info), &info) == BM_OK);
    assert(bm_scan_info_v1(sizeof(info)-1, &info) == BM_UNAVAILABLE);
    assert(info.schema == 1 && info.result_size == sizeof(BmScanResult));
    BmScanResult r = { .selected_count = 1 };
    r.selected.present = BM_SCAN_BASE;
    AVPacket *p = av_packet_alloc();
    assert(p && av_new_packet(p, 7) == 0);
    p->pts = -5; p->dts = AV_NOPTS_VALUE;
    p->flags = AV_PKT_FLAG_KEY | AV_PKT_FLAG_DISCARD;
    assert(accumulate(&r, p) == BM_OK);
    p->pts = -10; p->dts = -12; p->flags = AV_PKT_FLAG_CORRUPT;
    assert(accumulate(&r, p) == BM_OK);
    assert(r.selected.packet_count == 2 && r.selected.payload_bytes == 14);
    assert(r.selected.corrupt_packets == 1 && r.selected.pts_min == -10 && r.selected.pts_max == -5);
    assert(r.selected.dts_min == -12 && r.selected.dts_max == -12);
    p->stream_index = 1;
    assert(accumulate(&r, p) == BM_OK && r.incidental_corrupt_packets == 1);
    p->stream_index = 0;
    r.selected.payload_bytes = UINT64_MAX - 6;
    assert(accumulate(&r, p) == BM_BACKEND_FAILURE && r.selected.packet_count == 2);
    r.selected.payload_bytes = 14;
    r.selected.packet_count = UINT64_MAX;
    assert(accumulate(&r, p) == BM_BACKEND_FAILURE);
    r.selected.packet_count = 2; r.demuxed_packets = UINT64_MAX;
    assert(accumulate(&r, p) == BM_BACKEND_FAILURE);
    av_packet_free(&p);

    const char *fixture = getenv("BILIKARA_M3_SHIM_INPUT");
    if (!fixture) return; /* build --test runs arithmetic; live suite requires fixture */
    int fd = open(fixture, O_RDONLY);
    assert(fd >= 0);
    int flag = 0;
    stop_flag = &flag;
    BmScanRequest q = { .input = { fd, cancelled, &flag }, .media_type = 2 };
    const int failures[] = {AVERROR_INVALIDDATA, AVERROR(EIO), AVERROR_EXIT, -12345678};
    for (unsigned i = 0; i < sizeof(failures)/sizeof(failures[0]); i++) {
        assert(lseek(fd, 0, SEEK_SET) == 0);
        reads = 0; inject_after = 3; inject_error = failures[i];
        BmScanResult *result = NULL;
        assert(bm_scan_packets_v1(&q, &result) == BM_OK && result);
        assert(reads == 3 && result->selected.packet_count == 3);
        assert(result->status == error_status(failures[i]) && result->terminal == BM_SCAN_INCOMPLETE);
        bm_scan_release_v1(result);
    }
    inject_error = 0; inject_corrupt = 1;
    assert(lseek(fd, 0, SEEK_SET) == 0); reads = 0;
    BmScanResult *result = NULL;
    assert(bm_scan_packets_v1(&q, &result) == BM_OK);
    assert(result->terminal == BM_SCAN_EOF && result->status == BM_INVALID_MEDIA);
    assert(result->selected.corrupt_packets == 1 && result->selected.packet_count > 3);
    bm_scan_release_v1(result);
    inject_corrupt = 0; stop_after = 4;
    assert(lseek(fd, 0, SEEK_SET) == 0); reads = 0;
    assert(bm_scan_packets_v1(&q, &result) == BM_OK);
    assert(reads == 4 && result->status == BM_CANCELLED && result->terminal == BM_SCAN_INCOMPLETE);
    assert(result->selected.packet_count == 3);
    bm_scan_release_v1(result);
    close(fd);
    puts("scan shim: real reads + injected non-EOF errors/corrupt flag/cancellation passed (lifecycle evidence)");
}

#ifdef _WIN32
static void windows_long_path_tests(void) {
    wchar_t temporary[1024], root[1024], path[1024];
    DWORD length = GetTempPathW(1024, temporary);
    assert(length && length < 1024);
    assert(GetTempFileNameW(temporary, L"bmk", 0, root));
    assert(DeleteFileW(root) && CreateDirectoryW(root, NULL));
    /* Construct independently of bm_wide so this catches its MAX_PATH bug. */
    wcscpy(path, L"\\\\?\\");
    wcscat(path, root);
    size_t root_length = wcslen(path);
    for (int i = 0; i < 6; i++) {
        wcscat(path, L"\\synthetic-012345678901234567890123456789-\x7a7a");
        assert(CreateDirectoryW(path, NULL));
    }
    size_t directory_length = wcslen(path);
    wcscat(path, L"\\output.mp4");
    assert(wcslen(path + 4) > MAX_PATH);
    HANDLE output = CreateFileW(path, GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, NULL, CREATE_NEW, 0, NULL);
    assert(output != INVALID_HANDLE_VALUE);
    char utf8[4096];
    assert(WideCharToMultiByte(CP_UTF8, 0, path + 4, -1, utf8, sizeof(utf8), NULL, NULL));
    int fd = bm_open_read(utf8);
    assert(fd >= 0);
    assert(bm_empty_distinct_output(fd, utf8) == BM_INVALID_REQUEST);
    close(fd);
    /* Use a separate regular input and retain the empty/distinct guard. */
    wcscat(root, L"\\input.bin");
    HANDLE input = CreateFileW(root, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ,
                              NULL, CREATE_NEW, 0, NULL);
    assert(input != INVALID_HANDLE_VALUE);
    fd = bm_fd_from_handle_v1(input);
    assert(fd >= 0 && bm_empty_distinct_output(fd, utf8) == BM_OK);
    /* The Rust Host keeps canonical \\?\ drive paths all the way to remux. */
    char canonical[4096];
    assert(WideCharToMultiByte(CP_UTF8, 0, path, -1, canonical, sizeof(canonical), NULL, NULL));
    assert(bm_absolute_path(canonical));
    int canonical_fd = bm_open_read(canonical);
    assert(canonical_fd >= 0);
    assert(bm_empty_distinct_output(canonical_fd, canonical) == BM_INVALID_REQUEST);
    close(canonical_fd);
    assert(bm_empty_distinct_output(fd, canonical) == BM_OK);
    DWORD written;
    assert(WriteFile(output, "x", 1, &written, NULL) && written == 1);
    assert(bm_empty_distinct_output(fd, utf8) == BM_INVALID_REQUEST);
    assert(bm_empty_distinct_output(fd, canonical) == BM_INVALID_REQUEST);
    close(fd);
    CloseHandle(input);
    CloseHandle(output);
    assert(DeleteFileW(root) && DeleteFileW(path));
    path[directory_length] = 0;
    while (wcslen(path) > root_length) {
        assert(RemoveDirectoryW(path));
        *wcsrchr(path, L'\\') = 0;
    }
    assert(RemoveDirectoryW(path));
    assert(!bm_wide("\\\\server\\share\\file") && !bm_wide("C:relative"));
    assert(!bm_wide("\\\\?\\UNC\\server\\share\\file"));
    assert(!bm_wide("\\\\.\\PhysicalDrive0"));
    assert(!bm_wide("\\\\?\\C:\\file:stream"));
    puts("Windows long-path I/O: Unicode, empty/distinct and nonempty guards passed");
}
#endif

/* Test-only independent demux inspection of finalized cast output. */
typedef struct {
    uint32_t status, streams, first_video_key, audio_rate, audio_channels;
    int64_t duration_us;
    uint64_t packets[2];
} CastCheck;
BM_EXPORT uint32_t bm_test_cast_check_v1(const BmRequest *q,uint32_t size,CastCheck *r) {
    if(!q || !r || size!=sizeof(*r)) return BM_INVALID_REQUEST;
    memset(r,0,sizeof(*r));
    LiveInput in={0};
    /* Auto-probe only for this test reader; production input stays restricted. */
    in.call.request=q;
    in.format=avformat_alloc_context();
    if(!in.format) return BM_BACKEND_FAILURE;
    in.format->opaque=&in.call;in.format->io_open=deny_open;
    in.format->interrupt_callback=(AVIOInterruptCB){interrupted,&in.call};
    in.format->flags|=AVFMT_FLAG_CUSTOM_IO;
    AVDictionary *options=NULL;
    av_dict_set_int(&options,"fd",q->fd,0);
    int ret=avio_open2(&in.io,"fd:",AVIO_FLAG_READ,&in.format->interrupt_callback,&options);
    av_dict_free(&options);
    if(ret<0) goto failed;
    in.format->pb=in.io;
    ret=avformat_open_input(&in.format,"",NULL,NULL);
    if(ret<0) goto failed;
    ret=avformat_find_stream_info(in.format,NULL);
    if(ret<0) goto failed;
    r->streams=in.format->nb_streams;
    if(r->streams!=2 || in.format->streams[0]->codecpar->codec_id!=AV_CODEC_ID_H264 ||
        in.format->streams[1]->codecpar->codec_id!=AV_CODEC_ID_AAC) {r->status=BM_CONTRACT;goto done;}
    r->duration_us=in.format->duration;
    r->audio_rate=in.format->streams[1]->codecpar->sample_rate;
    r->audio_channels=in.format->streams[1]->codecpar->ch_layout.nb_channels;
    in.packet=av_packet_alloc();
    if(!in.packet) {r->status=BM_BACKEND_FAILURE;goto done;}
    for(;;) {
        ret=av_read_frame(in.format,in.packet);
        if(ret==AVERROR_EOF) break;
        if(ret<0) goto failed;
        int n=in.packet->stream_index;
        if(n<0 || n>1 || (in.packet->flags&AV_PKT_FLAG_CORRUPT)) {r->status=BM_INVALID_MEDIA;goto done;}
        if(n==0 && !r->packets[0]) r->first_video_key=!!(in.packet->flags&AV_PKT_FLAG_KEY);
        r->packets[n]++;av_packet_unref(in.packet);
    }
    goto done;
failed:
    r->status=error_status(ret);
done:
    live_close(&in);return BM_OK;
}

/* Test-only repeated H.264 fixture: original encoded samples, three GOPs.
 * Caller provides a test-owned output path; never used by the product. */
BM_EXPORT uint32_t bm_test_cast_fixture_v1(const BmRequest *q,const char *path) {
    LiveInput in={0};AVFormatContext *out=NULL;uint32_t status=BM_OK;int ret;
    status=live_open(&in,q,AVMEDIA_TYPE_VIDEO,AV_CODEC_ID_H264,"mov");
    if(status) goto done;
    ret=avformat_alloc_output_context2(&out,NULL,"mp4",path);
    if(ret<0 || !out) {status=BM_BACKEND_FAILURE;goto done;}
    AVStream *stream=avformat_new_stream(out,NULL);
    if(!stream) {status=BM_BACKEND_FAILURE;goto done;}
    avcodec_parameters_copy(stream->codecpar,in.config);stream->time_base=in.format->streams[0]->time_base;
    ret=avio_open(&out->pb,path,AVIO_FLAG_WRITE);if(ret<0) goto failed;
    ret=avformat_write_header(out,NULL);if(ret<0) goto failed;
    for(int repeat=0;repeat<3;repeat++) {
        ret=av_seek_frame(in.format,0,0,AVSEEK_FLAG_BACKWARD);if(ret<0) goto failed;
        int64_t origin=AV_NOPTS_VALUE;
        for(;;) {
            ret=av_read_frame(in.format,in.packet);if(ret==AVERROR_EOF) break;if(ret<0) goto failed;
            AVPacket *p=in.packet;
            if(origin==AV_NOPTS_VALUE) origin=p->pts;
            p->pts-=origin;p->dts-=origin;
            av_packet_rescale_ts(p,in.format->streams[0]->time_base,stream->time_base);
            int64_t offset=av_rescale_q((int64_t)repeat*1001000,AV_TIME_BASE_Q,stream->time_base);
            p->pts+=offset;p->dts+=offset;p->pos=-1;
            ret=av_write_frame(out,p);av_packet_unref(p);if(ret<0) goto failed;
        }
    }
    ret=av_write_trailer(out);if(ret<0) goto failed;goto done;
failed: status=error_status(ret);
done:
    if(out) {if(out->pb) avio_closep(&out->pb);avformat_free_context(out);}
    live_close(&in);return status;
}

int main(void) {
    BmCastInfo cast_info;
    assert(bm_cast_info_v1(sizeof(cast_info),&cast_info)==BM_OK);
    assert(bm_cast_info_v1(sizeof(cast_info)-1,&cast_info)==BM_UNAVAILABLE);
    assert(cast_info.schema==1 && cast_info.request_size==sizeof(BmCastRequest) && cast_info.result_size==sizeof(BmCastResult));
    assert(flac_time_matches(95856, (AVRational){1, 96000}, 96000, 96000));
    assert(flac_time_matches(95712, (AVRational){1, 96000}, 96000, 96000));
    /* Both sides of the inclusive 200 ms boundary, in microseconds. */
    assert(flac_time_matches(1199999, (AVRational){1, 1000000}, 96000, 96000));
    assert(flac_time_matches(1200000, (AVRational){1, 1000000}, 96000, 96000));
    assert(!flac_time_matches(1200001, (AVRational){1, 1000000}, 96000, 96000));
    assert(flac_time_matches(800001, (AVRational){1, 1000000}, 96000, 96000));
    assert(flac_time_matches(800000, (AVRational){1, 1000000}, 96000, 96000));
    assert(!flac_time_matches(799999, (AVRational){1, 1000000}, 96000, 96000));
    assert(!flac_time_matches(-1, (AVRational){1, 1000000}, 96000, 96000));
    assert(!flac_time_matches(1000000, (AVRational){1, 1000000}, -1, 96000));
    assert(!flac_time_matches(INT64_MAX, (AVRational){1, 1}, 96000, 96000));
    assert(!flac_time_matches(0, (AVRational){1, 1}, INT64_MAX, 1));
#ifdef _WIN32
    windows_long_path_tests();
#endif
    scan_tests();
    BmRemuxInfo remux_info;
    assert(bm_remux_info_v1(sizeof(remux_info), &remux_info) == BM_OK);
    assert(bm_remux_info_v1(sizeof(remux_info) - 1, &remux_info) == BM_UNAVAILABLE);
    assert(remux_info.schema == 1 && remux_info.result_size == sizeof(BmRemuxResult));
    assert(bm_flac_info_v1(sizeof(remux_info), &remux_info) == BM_OK);
    assert(bm_flac_info_v1(sizeof(remux_info) - 1, &remux_info) == BM_UNAVAILABLE);
    AVPacket *timestamp = av_packet_alloc();
    assert(timestamp);
    timestamp->pts = -1024; timestamp->dts = AV_NOPTS_VALUE; timestamp->duration = 512;
    av_packet_rescale_ts(timestamp, (AVRational){1, 48000}, (AVRational){1, 96000});
    assert(timestamp->pts == -2048 && timestamp->dts == AV_NOPTS_VALUE && timestamp->duration == 1024);
    av_packet_free(&timestamp);
    assert(error_status(AVERROR_INVALIDDATA) == BM_INVALID_MEDIA);
    assert(error_status(AVERROR_EOF) == BM_INVALID_MEDIA);
    assert(error_status(AVERROR_DECODER_NOT_FOUND) == BM_UNSUPPORTED_CODEC);
    assert(error_status(AVERROR(EIO)) == BM_IO);
    assert(error_status(AVERROR(EACCES)) == BM_IO);
    assert(error_status(AVERROR(ENOMEM)) == BM_BACKEND_FAILURE);
    assert(error_status(AVERROR(EINVAL)) == BM_BACKEND_FAILURE);
    assert(error_status(AVERROR_EXIT) == BM_BACKEND_FAILURE);
    assert(error_status(AVERROR_PATCHWELCOME) == BM_BACKEND_FAILURE);
    assert(error_status(-12345678) == BM_BACKEND_FAILURE);
    BmInfo info;
    assert(bm_get_info(sizeof(info), &info) == BM_OK);
    assert(bm_get_info(sizeof(info) - 1, &info) == BM_UNAVAILABLE);
    int flag = 1;
    BmRequest request = { .fd = -1, .cancelled = cancelled, .opaque = &flag };
    BmResult *result = NULL;
    assert(bm_probe_metadata(&request, &result) == BM_OK);
    assert(result && result->status == BM_CANCELLED);
    bm_release(result);
    flag = 0;
    assert(bm_probe_metadata(&request, &result) == BM_OK);
    assert(result && result->status == BM_IO);
    bm_release(result);
    Call call = { .request = &request };
    AVFormatContext *context = avformat_alloc_context();
    assert(context);
    context->opaque = &call;
    assert(deny_open(context, NULL, "/another/local/file", AVIO_FLAG_READ, NULL) == AVERROR(EACCES));
    assert(call.denied_open);
    RemuxOutput output = { .input = &call, .path = "/owned/private/output.mp4" };
    context->opaque = &output;
    assert(reopen_staging(context, NULL, "/another/local/file", AVIO_FLAG_READ, NULL) == AVERROR(EACCES));
    assert(output.denied_open);
    output.denied_open = 0;
    assert(reopen_staging(context, NULL, output.path, AVIO_FLAG_WRITE, NULL) == AVERROR(EACCES));
    assert(output.denied_open);
    avformat_free_context(context);
    puts("shim: 4 groups passed (error mapping, negotiation, cancellation/cleanup, subordinate-open denial)");
    return 0;
}
