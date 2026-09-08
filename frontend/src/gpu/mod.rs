//! WebGPU render path (via `wgpu`).
//!
//! Uploads each frame's BGRA pixels straight into a GPU texture and draws a
//! fullscreen triangle, so scaling and the pixel blit run on the GPU instead of
//! the CPU. `Bgra8Unorm` matches `wl_shm`'s byte order, so there is no CPU
//! swizzle either — the only per-frame CPU cost is `inflate`.
//!
//! Falls back to the 2D canvas (see [`crate::compositor`]) when WebGPU is
//! unavailable; construction returns `Err` in that case.
//!
//! ponytail: nothing constructs this at the moment. The Phase 4 scene builds a
//! renderer per surface, and doing that here wants one `wgpu::Device` shared
//! between surfaces rather than a device each — a refactor worth writing when it
//! can be run. WebGPU has never actually executed on the development machine
//! (Chromium there has no Vulkan, so `request_adapter` finds nothing), and
//! rewriting an untested path blind is how it stays untested. Enable
//! `chrome://flags/#enable-vulkan`, then wire this into `scene::Scene::ensure`.
#![allow(dead_code)]

use web_sys::{HtmlCanvasElement, VideoFrame};
use webland_protocol::{Codec, ServerMessage, SurfaceFrame, inflate};

const SHADER: &str = r"
struct VSOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VSOut {
    var corners = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    let xy = corners[i];
    var out: VSOut;
    out.pos = vec4(xy, 0.0, 1.0);
    out.uv = vec2((xy.x + 1.0) * 0.5, (1.0 - xy.y) * 0.5);
    return out;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs(in: VSOut) -> @location(0) vec4<f32> {
    return vec4(textureSample(tex, samp, in.uv).rgb, 1.0);
}
";

struct Target {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

/// A GPU-backed surface renderer.
pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    canvas: HtmlCanvasElement,
    target: Option<Target>,
}

impl GpuRenderer {
    /// Initialise WebGPU on `canvas`.
    ///
    /// # Errors
    /// Returns a message if WebGPU is unavailable or a device cannot be created,
    /// so the caller can fall back to the 2D canvas.
    pub async fn new(canvas: HtmlCanvasElement) -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        // Get a device *before* touching the canvas: `create_surface` claims the
        // canvas's context, and a canvas claimed for WebGPU can never return a 2D
        // context again — so failing after it would take the fallback down too.
        // (`compatible_surface` is ignored on the WebGPU backend anyway.)
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .map_err(|err| format!("no WebGPU adapter: {err}"))?;
        let limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                memory_hints: wgpu::MemoryHints::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|err| format!("request_device: {err}"))?;
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|err| format!("create_surface: {err}"))?;

        let caps = surface.get_capabilities(&adapter);
        // Non-sRGB so the raw bytes pass through as the 2D path renders them.
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
            .unwrap_or(caps.formats[0]);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(surface_format.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Ok(Self {
            device,
            queue,
            surface,
            surface_format,
            pipeline,
            bind_group_layout,
            sampler,
            canvas,
            target: None,
        })
    }

    /// Apply a server message: resize on `SurfaceCreated`, draw on `SurfaceFrame`.
    pub fn handle(&mut self, message: ServerMessage) {
        match message {
            ServerMessage::SurfaceCreated(created) => {
                self.resize(created.size.width, created.size.height);
            }
            ServerMessage::SurfaceFrame(frame) => self.draw(&frame),
            // The scene owns surface lifetime and chrome; a renderer only draws.
            ServerMessage::SurfaceDestroyed { .. }
            | ServerMessage::SurfaceTitle { .. }
            | ServerMessage::Applications(_) => {}
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if self
            .target
            .as_ref()
            .is_some_and(|t| t.width == width && t.height == height)
        {
            return;
        }
        self.canvas.set_width(width);
        self.canvas.set_height(height);
        self.surface.configure(
            &self.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.surface_format,
                color_space: wgpu::SurfaceColorSpace::Auto,
                width,
                height,
                present_mode: wgpu::PresentMode::Fifo,
                alpha_mode: wgpu::CompositeAlphaMode::Opaque,
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            },
        );
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.target = Some(Target {
            texture,
            bind_group,
            width,
            height,
        });
    }

    /// Copy a decoded video frame into the surface texture and present it.
    ///
    /// `copy_external_image_to_texture` is a GPU-to-GPU copy: the decoded frame
    /// never leaves the GPU, which is the whole point of Decision 2.
    pub fn draw_video_frame(&mut self, frame: &VideoFrame) {
        let Some(target) = self.target.as_ref() else {
            return;
        };
        let width = frame.display_width().min(target.width);
        let height = frame.display_height().min(target.height);
        if width == 0 || height == 0 {
            return;
        }
        self.queue.copy_external_image_to_texture(
            &wgpu::CopyExternalImageSourceInfo {
                // `Clone::clone`, spelled out: VideoFrame has its own JS `clone()`
                // method that returns a Result and would be picked up instead.
                source: wgpu::ExternalImageSource::VideoFrame(Clone::clone(frame)),
                origin: wgpu::Origin2d::ZERO,
                flip_y: false,
            },
            wgpu::CopyExternalImageDestInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
                color_space: wgpu::PredefinedColorSpace::Srgb,
                premultiplied_alpha: false,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.present();
    }

    fn draw(&mut self, frame: &SurfaceFrame) {
        let pixels = match frame.codec {
            Codec::Raw => frame.payload.clone(),
            Codec::Deflate => match inflate(&frame.payload) {
                Ok(bytes) => bytes,
                Err(_) => return,
            },
            Codec::H264 => return,
        };
        let Some(target) = self.target.as_ref() else {
            return;
        };
        // Frames carry only what changed; the texture holds the rest, so this
        // uploads into a sub-rectangle rather than replacing the surface.
        let Some(region) = frame.damage.first().copied() else {
            return;
        };
        let (Ok(x), Ok(y)) = (u32::try_from(region.x), u32::try_from(region.y)) else {
            return;
        };
        if region.width == 0
            || region.height == 0
            || x + region.width > target.width
            || y + region.height > target.height
        {
            return;
        }
        let expected = (region.width as usize) * (region.height as usize) * 4;
        if pixels.len() < expected {
            return;
        }

        // BGRA bytes upload directly into a Bgra8Unorm texture — no CPU swizzle.
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &pixels[..expected],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(region.width * 4),
                rows_per_image: Some(region.height),
            },
            wgpu::Extent3d {
                width: region.width,
                height: region.height,
                depth_or_array_layers: 1,
            },
        );

        self.present();
    }

    /// Blit the surface texture to the canvas.
    fn present(&mut self) {
        let Some(target) = self.target.as_ref() else {
            return;
        };
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            // Skip this frame; the next `SurfaceCreated` reconfigures the surface.
            _ => return,
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &target.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(output);
    }
}
