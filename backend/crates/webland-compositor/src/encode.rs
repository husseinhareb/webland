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
//! There are two ways in, picked when the encoder is built:
//!
//! - [`Input::Dmabuf`] imports the client's own GPU buffer as `DRM_PRIME`, maps
//!   it to a VA-API surface and encodes from that. Nothing is copied and no
//!   pixel touches the CPU, which is gate 1.
//! - [`Input::Cpu`] uploads BGRA bytes instead, for `wl_shm` clients that never
//!   had a GPU buffer to begin with.
//!
//! Both converge on `scale_vaapi=nv12 -> h264_vaapi`.
#![allow(unsafe_code)]

use std::ffi::{CString, c_int};

use ffmpeg_next::ffi::{
    AV_BUFFERSRC_FLAG_KEEP_REF, AV_CODEC_FLAG_LOW_DELAY, AV_HWFRAME_MAP_DIRECT,
    AV_HWFRAME_MAP_READ, AVBufferRef, AVCodecContext, AVDRMFrameDescriptor, AVFilterContext,
    AVFilterGraph, AVFrame, AVHWDeviceType, AVPacket, AVPictureType, AVPixelFormat, AVRational,
    av_buffer_create, av_buffer_ref, av_buffer_unref, av_buffersink_get_frame,
    av_buffersink_get_hw_frames_ctx, av_buffersrc_add_frame_flags, av_buffersrc_parameters_alloc,
    av_buffersrc_parameters_set, av_frame_alloc, av_frame_free, av_frame_get_buffer,
    av_frame_make_writable, av_frame_unref, av_free, av_hwdevice_ctx_create,
    av_hwdevice_ctx_create_derived, av_hwframe_ctx_alloc, av_hwframe_ctx_create_derived,
    av_hwframe_ctx_init, av_hwframe_map, av_packet_alloc, av_packet_free, av_packet_unref,
    avcodec_alloc_context3, avcodec_find_encoder_by_name, avcodec_free_context, avcodec_open2,
    avcodec_receive_packet, avcodec_send_frame, avfilter_get_by_name, avfilter_graph_alloc,
    avfilter_graph_alloc_filter, avfilter_graph_config, avfilter_graph_free, avfilter_init_str,
    avfilter_link,
};

/// Where the encoder's frames come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// The client's own GPU buffer, imported as `DRM_PRIME`. No copy.
    Dmabuf,
    /// BGRA bytes uploaded from system memory, for `wl_shm` clients.
    Cpu,
}

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
    input: Input,
    // Dmabuf input only: the DRM side of the import and the frames it maps through.
    drm_device: *mut AVBufferRef,
    drm_frames: *mut AVBufferRef,
    vaapi_frames: *mut AVBufferRef,
    drm_frame: *mut AVFrame,
    mapped: *mut AVFrame,
    // The descriptor the DRM frame points at. Owned here so it outlives the
    // frame that references it, and reused rather than reallocated per frame.
    descriptor: Box<AVDRMFrameDescriptor>,
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
    pub fn new(
        node: &str,
        width: u32,
        height: u32,
        bitrate: i64,
        input: Input,
        format: u32,
        modifier: u64,
    ) -> Result<Self, Error> {
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
            input,
            drm_device: std::ptr::null_mut(),
            drm_frames: std::ptr::null_mut(),
            vaapi_frames: std::ptr::null_mut(),
            drm_frame: std::ptr::null_mut(),
            mapped: std::ptr::null_mut(),
            // SAFETY: an all-zero descriptor is a valid empty one; `encode_dmabuf`
            // fills it before anything reads it.
            descriptor: Box::new(unsafe { std::mem::zeroed() }),
        };
        // SAFETY: every pointer starts null and `Drop` frees whatever got set,
        // so an error at any step below still unwinds cleanly.
        unsafe { enc.build(node, bitrate, format, modifier) }?;
        Ok(enc)
    }

    /// # Safety
    /// Called once, on a freshly zeroed `Encoder`, before any encoding.
    unsafe fn build(
        &mut self,
        node: &str,
        bitrate: i64,
        format: u32,
        modifier: u64,
    ) -> Result<(), Error> {
        let node_c = CString::new(node).map_err(|_| Error("render node path has a NUL"))?;
        let node = node_c;
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
        // Dmabuf input needs the DRM side built first: the graph's source is fed
        // already-VA-API frames, so it has to be told which frame pool they come
        // from before it will configure.
        if self.input == Input::Dmabuf {
            unsafe { self.build_import(&node, format, modifier) }?;
        }

        let pix_fmt = match self.input {
            Input::Dmabuf => AVPixelFormat::AV_PIX_FMT_VAAPI,
            Input::Cpu => AVPixelFormat::AV_PIX_FMT_BGRA,
        };
        let args = format!(
            "video_size={}x{}:pix_fmt={}:time_base=1/1000:pixel_aspect=1/1",
            self.width, self.height, pix_fmt as c_int,
        );
        // The source is allocated and configured before it is initialised: with a
        // hardware pix_fmt it validates its frame pool during init, so a pool
        // attached afterwards is one it has already refused to start without.
        let src = unsafe { self.alloc_filter("buffer", "in") }?;
        if self.input == Input::Dmabuf {
            let params = unsafe { av_buffersrc_parameters_alloc() };
            if params.is_null() {
                return Err(Error("buffersrc parameter alloc failed"));
            }
            // SAFETY: freshly allocated, and freed on both paths out.
            let set = unsafe {
                (*params).hw_frames_ctx = self.vaapi_frames;
                let set = av_buffersrc_parameters_set(src, params);
                av_free(params.cast());
                set
            };
            if set < 0 {
                return Err(Error("buffersrc will not take the frame pool"));
            }
        }
        unsafe { init_filter(src, Some(&args)) }?;

        let scale = unsafe { self.filter("scale_vaapi", "scale", Some("format=nv12"), false) }?;
        let sink = unsafe { self.filter("buffersink", "out", None, false) }?;
        let links: Vec<(*mut AVFilterContext, *mut AVFilterContext)> = match self.input {
            Input::Dmabuf => vec![(src, scale), (scale, sink)],
            Input::Cpu => {
                let upload = unsafe { self.filter("hwupload", "upload", None, true) }?;
                vec![(src, upload), (upload, scale), (scale, sink)]
            }
        };
        for (from, to) in links {
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

    /// Build the DRM device and the two frame pools the import maps between.
    ///
    /// # Safety
    /// Called once from `build`, before any encoding.
    unsafe fn build_import(
        &mut self,
        node: &CString,
        format: u32,
        modifier: u64,
    ) -> Result<(), Error> {
        if unsafe {
            av_hwdevice_ctx_create(
                &raw mut self.drm_device,
                AVHWDeviceType::AV_HWDEVICE_TYPE_DRM,
                node.as_ptr(),
                std::ptr::null_mut(),
                0,
            )
        } < 0
        {
            return Err(Error("no DRM device"));
        }
        // Derive VA-API from that same DRM device rather than opening it twice:
        // a mapping between unrelated devices is a copy, which is the thing we
        // are here to avoid.
        let mut derived: *mut AVBufferRef = std::ptr::null_mut();
        if unsafe {
            av_hwdevice_ctx_create_derived(
                &raw mut derived,
                AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                self.drm_device,
                0,
            )
        } < 0
        {
            return Err(Error("VA-API will not derive from the DRM device"));
        }
        // SAFETY: `self.device` holds a VA-API device from `build`; swap in the
        // derived one and release the original.
        unsafe {
            av_buffer_unref(&raw mut self.device);
            self.device = derived;
        }

        self.drm_frames = unsafe { av_hwframe_ctx_alloc(self.drm_device) };
        if self.drm_frames.is_null() {
            return Err(Error("DRM frame pool alloc failed"));
        }
        // SAFETY: freshly allocated; `data` is an AVHWFramesContext by contract.
        unsafe {
            #[allow(clippy::cast_ptr_alignment)] // ffmpeg allocates this aligned.
            let frames = (*self.drm_frames)
                .data
                .cast::<ffmpeg_next::ffi::AVHWFramesContext>();
            (*frames).format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME;
            (*frames).sw_format = drm_to_pixel_format(format)?;
            (*frames).width = i32::try_from(self.width).map_err(|_| Error("surface too wide"))?;
            (*frames).height = i32::try_from(self.height).map_err(|_| Error("surface too tall"))?;
            // Frames are supplied by the client, not allocated from this pool.
            (*frames).initial_pool_size = 0;
            if av_hwframe_ctx_init(self.drm_frames) < 0 {
                return Err(Error("DRM frame pool will not initialise"));
            }
        }

        if unsafe {
            av_hwframe_ctx_create_derived(
                &raw mut self.vaapi_frames,
                AVPixelFormat::AV_PIX_FMT_VAAPI,
                self.device,
                self.drm_frames,
                AV_HWFRAME_MAP_DIRECT as c_int,
            )
        } < 0
        {
            return Err(Error("VA-API frames will not derive from DRM frames"));
        }

        self.drm_frame = unsafe { av_frame_alloc() };
        self.mapped = unsafe { av_frame_alloc() };
        if self.drm_frame.is_null() || self.mapped.is_null() {
            return Err(Error("import frame alloc failed"));
        }
        let _ = modifier;
        Ok(())
    }

    /// Encode straight from a client's GPU buffer.
    ///
    /// `planes` is one `(fd, offset, stride)` per plane, `modifier` the buffer's
    /// DRM format modifier. Nothing here reads the pixels: the fds are handed to
    /// VA-API, which encodes from the memory the client already rendered into.
    ///
    /// Returns `None` on any import or encode failure; the caller falls back to
    /// the copying path rather than dropping the surface.
    #[must_use]
    pub fn encode_dmabuf(
        &mut self,
        planes: &[(i32, u32, u32)],
        format: u32,
        modifier: u64,
        keyframe: bool,
    ) -> Option<Vec<u8>> {
        if self.input != Input::Dmabuf || planes.is_empty() || planes.len() > 4 {
            return None;
        }
        // Rebuild the descriptor in place. Objects are distinct fds, not planes:
        // a compressed AMD buffer arrives as two planes — pixels and the DCC
        // metadata — that share one buffer object at different offsets, and
        // VA-API refuses to map a frame made of more than one object.
        let descriptor = &mut *self.descriptor;
        *descriptor = unsafe { std::mem::zeroed() };
        descriptor.nb_layers = 1;
        descriptor.layers[0].format = format;
        descriptor.layers[0].nb_planes = i32::try_from(planes.len()).ok()?;
        let mut objects: Vec<i32> = Vec::with_capacity(planes.len());
        for (i, &(fd, offset, stride)) in planes.iter().enumerate() {
            let object = if let Some(existing) = objects.iter().position(|&seen| seen == fd) {
                existing
            } else {
                objects.push(fd);
                let index = objects.len() - 1;
                descriptor.objects[index].fd = fd;
                descriptor.objects[index].size = 0;
                descriptor.objects[index].format_modifier = modifier;
                index
            };
            descriptor.layers[0].planes[i].object_index = i32::try_from(object).ok()?;
            descriptor.layers[0].planes[i].offset = isize::try_from(offset).ok()?;
            descriptor.layers[0].planes[i].pitch = isize::try_from(stride).ok()?;
        }
        descriptor.nb_objects = i32::try_from(objects.len()).ok()?;

        // SAFETY: the frames and pools live as long as `self`, and the
        // descriptor is owned by `self` so it outlives the frame pointing at it.
        unsafe {
            av_frame_unref(self.drm_frame);
            let f = &mut *self.drm_frame;
            f.format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as c_int;
            f.width = i32::try_from(self.width).ok()?;
            f.height = i32::try_from(self.height).ok()?;
            f.hw_frames_ctx = av_buffer_ref(self.drm_frames);
            if f.hw_frames_ctx.is_null() {
                return None;
            }
            let bytes = std::ptr::from_mut(descriptor).cast::<u8>();
            f.data[0] = bytes;
            // ffmpeg wants a buf[0] to consider the frame reference-counted. The
            // descriptor is ours, so the free callback deliberately does nothing.
            f.buf[0] = av_buffer_create(
                bytes,
                std::mem::size_of::<AVDRMFrameDescriptor>(),
                Some(no_free),
                std::ptr::null_mut(),
                0,
            );
            if f.buf[0].is_null() {
                return None;
            }

            av_frame_unref(self.mapped);
            (*self.mapped).format = AVPixelFormat::AV_PIX_FMT_VAAPI as c_int;
            (*self.mapped).hw_frames_ctx = av_buffer_ref(self.vaapi_frames);
            if (*self.mapped).hw_frames_ctx.is_null() {
                return None;
            }
            if av_hwframe_map(
                self.mapped,
                self.drm_frame,
                (AV_HWFRAME_MAP_DIRECT as c_int) | (AV_HWFRAME_MAP_READ as c_int),
            ) < 0
            {
                return None;
            }
            (*self.mapped).pts = self.pts;
            self.pts += 1;
        }

        self.drain(self.mapped, keyframe)
    }

    /// Create one filter, optionally attaching the VA-API device to it.
    unsafe fn filter(
        &mut self,
        kind: &str,
        name: &str,
        args: Option<&str>,
        needs_device: bool,
    ) -> Result<*mut AVFilterContext, Error> {
        // Allocate and initialise as two steps: `hwupload` reads its device
        // reference during init, so a filter created and initialised in one call
        // has already failed by the time we could attach one.
        let ctx = unsafe { self.alloc_filter(kind, name) }?;
        if needs_device {
            // SAFETY: `ctx` was just allocated in our graph and is uninitialised.
            unsafe {
                (*ctx).hw_device_ctx = av_buffer_ref(self.device);
                if (*ctx).hw_device_ctx.is_null() {
                    return Err(Error("could not reference the VA-API device"));
                }
            }
        }
        unsafe { init_filter(ctx, args) }?;
        Ok(ctx)
    }

    /// Allocate a filter without initialising it, so whatever it validates at
    /// init time can be attached first.
    unsafe fn alloc_filter(
        &mut self,
        kind: &str,
        name: &str,
    ) -> Result<*mut AVFilterContext, Error> {
        let kind_c = CString::new(kind).map_err(|_| Error("bad filter name"))?;
        let name_c = CString::new(name).map_err(|_| Error("bad filter name"))?;
        let filter = unsafe { avfilter_get_by_name(kind_c.as_ptr()) };
        if filter.is_null() {
            return Err(Error("ffmpeg is missing a filter we need"));
        }
        let ctx = unsafe { avfilter_graph_alloc_filter(self.graph, filter, name_c.as_ptr()) };
        if ctx.is_null() {
            return Err(Error("filter alloc failed"));
        }
        Ok(ctx)
    }

    /// Which kind of frame this encoder accepts.
    #[must_use]
    pub fn input(&self) -> Input {
        self.input
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

        self.drain(self.frame, keyframe)
    }

    /// Push one frame through the graph and pull the encoded packet back out.
    fn drain(&mut self, frame: *mut AVFrame, keyframe: bool) -> Option<Vec<u8>> {
        // SAFETY: all four pointers are live for the lifetime of `self`.
        unsafe {
            // KEEP_REF, or the call takes our buffer and leaves the frame
            // empty — the next frame would then copy into a null plane.
            if av_buffersrc_add_frame_flags(self.src, frame, AV_BUFFERSRC_FLAG_KEEP_REF as c_int)
                < 0
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
            av_frame_free(&raw mut self.mapped);
            av_frame_free(&raw mut self.drm_frame);
            avfilter_graph_free(&raw mut self.graph);
            av_buffer_unref(&raw mut self.vaapi_frames);
            av_buffer_unref(&raw mut self.drm_frames);
            av_buffer_unref(&raw mut self.device);
            av_buffer_unref(&raw mut self.drm_device);
        }
    }
}

/// `av_buffer_create` insists on a free callback; the descriptor it wraps is
/// owned by the [`Encoder`], so there is nothing here to free.
unsafe extern "C" fn no_free(_opaque: *mut std::ffi::c_void, _data: *mut u8) {}

/// The software format behind a DRM fourcc, which is what the DRM frame pool
/// wants to know.
///
/// Only the formats a client is plausibly going to hand us are listed; anything
/// else falls back to the copying path rather than guessing wrong.
fn drm_to_pixel_format(fourcc: u32) -> Result<AVPixelFormat, Error> {
    // Fourccs are little-endian packed ASCII, so these read backwards.
    const AR24: u32 = u32::from_le_bytes(*b"AR24");
    const XR24: u32 = u32::from_le_bytes(*b"XR24");
    const AB24: u32 = u32::from_le_bytes(*b"AB24");
    const XB24: u32 = u32::from_le_bytes(*b"XB24");
    match fourcc {
        AR24 => Ok(AVPixelFormat::AV_PIX_FMT_BGRA),
        XR24 => Ok(AVPixelFormat::AV_PIX_FMT_BGR0),
        AB24 => Ok(AVPixelFormat::AV_PIX_FMT_RGBA),
        XB24 => Ok(AVPixelFormat::AV_PIX_FMT_RGB0),
        _ => Err(Error("client buffer format is not one we import")),
    }
}

/// Initialise a filter that has already been allocated and configured.
unsafe fn init_filter(ctx: *mut AVFilterContext, args: Option<&str>) -> Result<(), Error> {
    let args_c = args
        .map(|a| CString::new(a).map_err(|_| Error("bad filter args")))
        .transpose()?;
    if unsafe {
        avfilter_init_str(
            ctx,
            args_c.as_ref().map_or(std::ptr::null(), |a| a.as_ptr()),
        )
    } < 0
    {
        return Err(Error("filter creation failed"));
    }
    Ok(())
}
