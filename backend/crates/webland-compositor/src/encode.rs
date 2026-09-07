//! H.264 encode on the GPU, via VA-API.
//!
//! Phase 2 gate 4 asks for 1080p60 inside a sane video bitrate. Deflated pixels
//! cannot get there — a scrolling terminal costs ~47 Mbit/s that way, because
//! deflate has no idea that this frame resembles the last one. H.264 does.
//!
//! Everything here is FFI. ffmpeg's safe Rust wrapper covers no part of the
//! hardware API, so the whole module is `unsafe` and the point of it is to keep
//! that contained: the surface it exposes is [`Encoder::new`] and
//! [`Encoder::encode`], and neither hands a raw pointer back out.
//!
//! The pipeline is `buffer -> hwupload -> scale_vaapi=nv12 -> h264_vaapi`.
//! ponytail: `hwupload` means the caller still hands us CPU pixels, so this
//! closes gate 4 and not gate 1. Feeding the client's dmabuf in as `DRM_PRIME`
//! replaces exactly that one filter and nothing else in this file.
#![allow(unsafe_code)]

use std::ffi::{CString, c_int};

use ffmpeg_next::ffi::{
    AV_BUFFERSRC_FLAG_KEEP_REF, AV_CODEC_FLAG_LOW_DELAY, AVBufferRef, AVCodecContext,
    AVFilterContext, AVFilterGraph, AVFrame, AVHWDeviceType, AVPacket, AVPictureType,
    AVPixelFormat, AVRational, av_buffer_ref, av_buffer_unref, av_buffersink_get_frame,
    av_buffersink_get_hw_frames_ctx, av_buffersrc_add_frame_flags, av_frame_alloc, av_frame_free,
    av_frame_get_buffer, av_frame_make_writable, av_frame_unref, av_hwdevice_ctx_create,
    av_packet_alloc, av_packet_free, av_packet_unref, avcodec_alloc_context3,
    avcodec_find_encoder_by_name, avcodec_free_context, avcodec_open2, avcodec_receive_packet,
    avcodec_send_frame, avfilter_get_by_name, avfilter_graph_alloc, avfilter_graph_alloc_filter,
    avfilter_graph_config, avfilter_graph_free, avfilter_init_str, avfilter_link,
};

/// Why the encoder could not be built. Deliberately coarse: every one of these
/// means the same thing to the caller, which is to stay on the deflate path.
#[derive(Debug)]
pub struct Error(&'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

/// A VA-API H.264 encoder bound to one surface size.
///
/// A resize needs a new one: the filter graph and the encoder both bake the
/// dimensions in, and a stream whose size changes mid-flight is not something a
/// browser's `VideoDecoder` will thank us for.
pub struct Encoder {
    device: *mut AVBufferRef,
    graph: *mut AVFilterGraph,
    src: *mut AVFilterContext,
    sink: *mut AVFilterContext,
    codec: *mut AVCodecContext,
    frame: *mut AVFrame,
    hw_frame: *mut AVFrame,
    packet: *mut AVPacket,
    width: u32,
    height: u32,
    pts: i64,
}

impl std::fmt::Debug for Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encoder")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl Encoder {
    /// Build an encoder for `width`x`height` BGRA input on the given render node.
    ///
    /// # Errors
    /// Returns an error if VA-API is unavailable, `h264_vaapi` is missing, or
    /// the filter graph will not configure — all of which mean "use deflate".
    pub fn new(node: &str, width: u32, height: u32, bitrate: i64) -> Result<Self, Error> {
        // Odd dimensions have no NV12 representation; the chroma plane is half
        // size in both directions.
        if width == 0 || height == 0 || width % 2 == 1 || height % 2 == 1 {
            return Err(Error("surface size is not encodable as NV12"));
        }
        let mut enc = Encoder {
            device: std::ptr::null_mut(),
            graph: std::ptr::null_mut(),
            src: std::ptr::null_mut(),
            sink: std::ptr::null_mut(),
            codec: std::ptr::null_mut(),
            frame: std::ptr::null_mut(),
            hw_frame: std::ptr::null_mut(),
            packet: std::ptr::null_mut(),
            width,
            height,
            pts: 0,
        };
        // SAFETY: every pointer starts null and `Drop` frees whatever got set,
        // so an error at any step below still unwinds cleanly.
        unsafe { enc.build(node, bitrate) }?;
        Ok(enc)
    }

    /// # Safety
    /// Called once, on a freshly zeroed `Encoder`, before any encoding.
    unsafe fn build(&mut self, node: &str, bitrate: i64) -> Result<(), Error> {
        let node = CString::new(node).map_err(|_| Error("render node path has a NUL"))?;
        if unsafe {
            av_hwdevice_ctx_create(
                &raw mut self.device,
                AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                node.as_ptr(),
                std::ptr::null_mut(),
                0,
            )
        } < 0
        {
            return Err(Error("no VA-API device"));
        }

        self.graph = unsafe { avfilter_graph_alloc() };
        if self.graph.is_null() {
            return Err(Error("filter graph alloc failed"));
        }

        // buffer -> hwupload -> scale_vaapi -> buffersink, linked by hand. The
        // parser would do this too, but it wants the hw device attached to a
        // filter we can only reach after parsing, so linking directly is less
        // code than parsing and then going looking.
        let args = format!(
            "video_size={}x{}:pix_fmt={}:time_base=1/1000:pixel_aspect=1/1",
            self.width,
            self.height,
            AVPixelFormat::AV_PIX_FMT_BGRA as c_int,
        );
        let src = unsafe { self.filter("buffer", "in", Some(&args), false) }?;
        let upload = unsafe { self.filter("hwupload", "upload", None, true) }?;
        let scale = unsafe { self.filter("scale_vaapi", "scale", Some("format=nv12"), false) }?;
        let sink = unsafe { self.filter("buffersink", "out", None, false) }?;
        for (from, to) in [(src, upload), (upload, scale), (scale, sink)] {
            if unsafe { avfilter_link(from, 0, to, 0) } < 0 {
                return Err(Error("filter link failed"));
            }
        }
        if unsafe { avfilter_graph_config(self.graph, std::ptr::null_mut()) } < 0 {
            return Err(Error("filter graph will not configure"));
        }
        self.src = src;
        self.sink = sink;

        // The encoder has to share the frame pool the graph ends up allocating
        // from, or it has nowhere to read the surfaces it is handed.
        let frames = unsafe { av_buffersink_get_hw_frames_ctx(self.sink) };
        if frames.is_null() {
            return Err(Error("filter graph produced no hardware frames"));
        }

        let name = CString::new("h264_vaapi").map_err(|_| Error("bad encoder name"))?;
        let codec = unsafe { avcodec_find_encoder_by_name(name.as_ptr()) };
        if codec.is_null() {
            return Err(Error("ffmpeg has no h264_vaapi encoder"));
        }
        self.codec = unsafe { avcodec_alloc_context3(codec) };
        if self.codec.is_null() {
            return Err(Error("encoder alloc failed"));
        }
        // SAFETY: `self.codec` was just allocated and is ours alone.
        unsafe {
            let c = &mut *self.codec;
            c.width = i32::try_from(self.width).map_err(|_| Error("surface too wide"))?;
            c.height = i32::try_from(self.height).map_err(|_| Error("surface too tall"))?;
            c.pix_fmt = AVPixelFormat::AV_PIX_FMT_VAAPI;
            c.time_base = AVRational { num: 1, den: 1000 };
            c.framerate = AVRational { num: 60, den: 1 };
            c.bit_rate = bitrate;
            // No B-frames and no reordering: this is a desktop, and a frame the
            // browser cannot show until the next one arrives is a frame of
            // added latency on every interaction.
            c.max_b_frames = 0;
            c.gop_size = 120;
            c.flags |= AV_CODEC_FLAG_LOW_DELAY.cast_signed();
            c.hw_frames_ctx = av_buffer_ref(frames);
            if c.hw_frames_ctx.is_null() {
                return Err(Error("could not reference the frame pool"));
            }
        }
        if unsafe { avcodec_open2(self.codec, codec, std::ptr::null_mut()) } < 0 {
            return Err(Error("h264_vaapi will not open"));
        }

        self.frame = unsafe { av_frame_alloc() };
        self.hw_frame = unsafe { av_frame_alloc() };
        self.packet = unsafe { av_packet_alloc() };
        if self.frame.is_null() || self.hw_frame.is_null() || self.packet.is_null() {
            return Err(Error("frame alloc failed"));
        }
        // SAFETY: freshly allocated, and the dimensions are the ones the graph
        // was configured with.
        unsafe {
            let f = &mut *self.frame;
            f.format = AVPixelFormat::AV_PIX_FMT_BGRA as c_int;
            f.width = i32::try_from(self.width).map_err(|_| Error("surface too wide"))?;
            f.height = i32::try_from(self.height).map_err(|_| Error("surface too tall"))?;
            if av_frame_get_buffer(self.frame, 0) < 0 {
                return Err(Error("frame buffer alloc failed"));
            }
        }
        Ok(())
    }

    /// Create one filter, optionally attaching the VA-API device to it.
    unsafe fn filter(
        &mut self,
        kind: &str,
        name: &str,
        args: Option<&str>,
        needs_device: bool,
    ) -> Result<*mut AVFilterContext, Error> {
        let kind_c = CString::new(kind).map_err(|_| Error("bad filter name"))?;
        let name_c = CString::new(name).map_err(|_| Error("bad filter name"))?;
        let args_c = args
            .map(|a| CString::new(a).map_err(|_| Error("bad filter args")))
            .transpose()?;
        let filter = unsafe { avfilter_get_by_name(kind_c.as_ptr()) };
        if filter.is_null() {
            return Err(Error("ffmpeg is missing a filter we need"));
        }
        // Allocate and initialise as two steps: `hwupload` reads its device
        // reference during init, so a filter created and initialised in one call
        // has already failed by the time we could attach one.
        let ctx = unsafe { avfilter_graph_alloc_filter(self.graph, filter, name_c.as_ptr()) };
        if ctx.is_null() {
            return Err(Error("filter alloc failed"));
        }
        if needs_device {
            // SAFETY: `ctx` was just allocated in our graph and is uninitialised.
            unsafe {
                (*ctx).hw_device_ctx = av_buffer_ref(self.device);
                if (*ctx).hw_device_ctx.is_null() {
                    return Err(Error("could not reference the VA-API device"));
                }
            }
        }
        if unsafe {
            avfilter_init_str(
                ctx,
                args_c.as_ref().map_or(std::ptr::null(), |a| a.as_ptr()),
            )
        } < 0
        {
            return Err(Error("filter creation failed"));
        }
        Ok(ctx)
    }

    /// The surface size this encoder was built for.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Encode one tightly-packed BGRA frame, returning an Annex B access unit.
    ///
    /// `keyframe` forces an IDR, which is what a browser joining mid-stream
    /// needs before it can decode anything at all.
    ///
    /// Returns `None` when the encoder accepted the frame but has no packet yet,
    /// and on any encode error — a dropped frame is recoverable, and the next
    /// keyframe resynchronises the browser.
    #[must_use]
    pub fn encode(&mut self, bgra: &[u8], keyframe: bool) -> Option<Vec<u8>> {
        let stride = self.width as usize * 4;
        if bgra.len() < stride * self.height as usize {
            return None;
        }
        // SAFETY: the frame was allocated with these dimensions and BGRA format,
        // and we copy row by row using ffmpeg's own stride, never our own.
        unsafe {
            // The graph still holds a reference to the last frame we sent, so
            // this may hand us fresh storage rather than let us scribble on a
            // buffer something downstream is reading.
            if av_frame_make_writable(self.frame) < 0 {
                return None;
            }
            let f = &mut *self.frame;
            let dst_stride = usize::try_from(f.linesize[0]).ok()?;
            for row in 0..self.height as usize {
                let src = bgra.get(row * stride..(row + 1) * stride)?;
                let dst = f.data[0].add(row * dst_stride);
                std::ptr::copy_nonoverlapping(src.as_ptr(), dst, stride);
            }
            f.pts = self.pts;
            self.pts += 1;
        }

        // SAFETY: all four pointers are live for the lifetime of `self`.
        unsafe {
            // KEEP_REF, or the call takes our buffer and leaves the frame
            // empty — the next frame would then copy into a null plane.
            if av_buffersrc_add_frame_flags(
                self.src,
                self.frame,
                AV_BUFFERSRC_FLAG_KEEP_REF as c_int,
            ) < 0
            {
                return None;
            }
            av_frame_unref(self.hw_frame);
            if av_buffersink_get_frame(self.sink, self.hw_frame) < 0 {
                return None;
            }
            (*self.hw_frame).pict_type = if keyframe {
                AVPictureType::AV_PICTURE_TYPE_I
            } else {
                AVPictureType::AV_PICTURE_TYPE_NONE
            };
            if avcodec_send_frame(self.codec, self.hw_frame) < 0 {
                return None;
            }
            av_packet_unref(self.packet);
            if avcodec_receive_packet(self.codec, self.packet) < 0 {
                return None;
            }
            let p = &*self.packet;
            let len = usize::try_from(p.size).ok()?;
            let bytes = std::slice::from_raw_parts(p.data, len).to_vec();
            av_packet_unref(self.packet);
            Some(bytes)
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: each of these frees a pointer this type allocated, and ffmpeg's
        // free functions all accept null.
        unsafe {
            av_packet_free(&raw mut self.packet);
            av_frame_free(&raw mut self.hw_frame);
            av_frame_free(&raw mut self.frame);
            avcodec_free_context(&raw mut self.codec);
            avfilter_graph_free(&raw mut self.graph);
            av_buffer_unref(&raw mut self.device);
        }
    }
}
