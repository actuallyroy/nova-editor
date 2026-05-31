// README image rendering. Unlike the icon atlas (one shared 96px-capped atlas),
// README images can be large and varied, so each gets its own wgpu texture +
// bind group. Decodes PNG/JPG and the first frame of GIFs via the `image` crate
// (animation is layered on later). Reuses the textured-quad shader (icon.wgsl):
// instance = screen rect + full-image UV. Draws one instance per visible image,
// switching the bind group between them.

use std::collections::HashMap;

use wgpu::{
    AddressMode, BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout,
    BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingType, BlendState, Buffer,
    BufferDescriptor, BufferUsages, ColorTargetState, ColorWrites, Device, Extent3d, FilterMode,
    FragmentState, ImageCopyTexture, ImageDataLayout, MultisampleState, PipelineCompilationOptions,
    PipelineLayoutDescriptor, PrimitiveState, Queue, RenderPass, RenderPipeline,
    RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages,
    TextureFormat, TextureSampleType, TextureViewDimension, VertexAttribute, VertexBufferLayout,
    VertexFormat, VertexState, VertexStepMode,
};

use crate::icon::IconInstance;

struct Decoded {
    frames: Vec<BindGroup>, // one per GIF frame (length 1 for static images)
    delays_ms: Vec<u32>,
    total_ms: u32,
    w: u32,
    h: u32,
}

impl Decoded {
    /// The frame index to show at `now_ms` (looping). Frame 0 for static images.
    fn frame_at(&self, now_ms: u64) -> usize {
        if self.total_ms == 0 || self.frames.len() <= 1 {
            return 0;
        }
        let t = (now_ms % self.total_ms as u64) as u32;
        let mut acc = 0u32;
        for (i, &d) in self.delays_ms.iter().enumerate() {
            acc += d;
            if t < acc {
                return i;
            }
        }
        self.frames.len() - 1
    }
}

pub struct Media {
    pipeline: RenderPipeline,
    bgl: BindGroupLayout,
    sampler: Sampler,
    uniform_buf: Buffer,
    instance_buf: Buffer,
    capacity_bytes: u64,
    images: HashMap<String, Decoded>,
    // Draw order built by `prepare`: (key, frame index) parallel to instance buffer.
    order: Vec<(String, usize)>,
}

impl Media {
    pub fn new(device: &Device, format: TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("media-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("icon.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("media-bgl"),
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::VERTEX,
                    ty: BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        sample_type: TextureSampleType::Float { filterable: true },
                        view_dimension: TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 2,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let uniform_buf = device.create_buffer(&BufferDescriptor {
            label: Some("media-uniform"),
            size: 16,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("media-sampler"),
            address_mode_u: AddressMode::ClampToEdge,
            address_mode_v: AddressMode::ClampToEdge,
            address_mode_w: AddressMode::ClampToEdge,
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            ..Default::default()
        });
        let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("media-pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("media-pipeline"),
            layout: Some(&layout),
            vertex: VertexState {
                module: &shader,
                entry_point: "vs_main",
                compilation_options: PipelineCompilationOptions::default(),
                buffers: &[VertexBufferLayout {
                    array_stride: std::mem::size_of::<IconInstance>() as u64,
                    step_mode: VertexStepMode::Instance,
                    attributes: &[
                        VertexAttribute { offset: 0, shader_location: 0, format: VertexFormat::Float32x4 },
                        VertexAttribute { offset: 16, shader_location: 1, format: VertexFormat::Float32x4 },
                    ],
                }],
            },
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: "fs_main",
                compilation_options: PipelineCompilationOptions::default(),
                targets: &[Some(ColorTargetState {
                    format,
                    blend: Some(BlendState::ALPHA_BLENDING),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let capacity_bytes = 64 * std::mem::size_of::<IconInstance>() as u64;
        let instance_buf = device.create_buffer(&BufferDescriptor {
            label: Some("media-instances"),
            size: capacity_bytes,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            bgl,
            sampler,
            uniform_buf,
            instance_buf,
            capacity_bytes,
            images: HashMap::new(),
            order: Vec::new(),
        }
    }

    pub fn has(&self, key: &str) -> bool {
        self.images.contains_key(key)
    }

    /// Natural (width, height) of a decoded image, if loaded.
    pub fn size(&self, key: &str) -> Option<(f32, f32)> {
        self.images.get(key).map(|d| (d.w as f32, d.h as f32))
    }

    /// Whether a loaded key is an animated (multi-frame) GIF.
    pub fn is_animated(&self, key: &str) -> bool {
        self.images.get(key).map(|d| d.frames.len() > 1).unwrap_or(false)
    }

    /// Upload one RGBA frame (raw bytes + dims) as a texture + bind group.
    fn upload(&self, device: &Device, queue: &Queue, rgba: &[u8], w: u32, h: u32) -> BindGroup {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("media-tex"),
            size: Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            ImageDataLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
            Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        device.create_bind_group(&BindGroupDescriptor {
            label: Some("media-bg"),
            layout: &self.bgl,
            entries: &[
                BindGroupEntry { binding: 0, resource: self.uniform_buf.as_entire_binding() },
                BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    /// Upload already-decoded frames (cheap GPU work) as a media entry. The heavy
    /// decode happens off-thread in `decode`; this just creates textures.
    pub fn upload_frames(&mut self, device: &Device, queue: &Queue, key: &str, frames: Vec<DecodedFrame>) {
        if frames.is_empty() || self.images.contains_key(key) {
            return;
        }
        let (w, h) = (frames[0].w, frames[0].h);
        let mut bgs = Vec::with_capacity(frames.len());
        let mut delays = Vec::with_capacity(frames.len());
        let mut total = 0u32;
        for f in &frames {
            bgs.push(self.upload(device, queue, &f.rgba, f.w, f.h));
            delays.push(f.delay_ms);
            total += f.delay_ms;
        }
        self.images.insert(key.to_string(), Decoded { frames: bgs, delays_ms: delays, total_ms: total, w, h });
    }

    /// Write the per-image instance quads (full-image UV) for `items` that are
    /// loaded, recording draw order + the current animation frame per item. Call
    /// once per frame before `render`. `now_ms` drives GIF playback.
    pub fn prepare(
        &mut self,
        device: &Device,
        queue: &Queue,
        items: &[(String, crate::widgets::Rect)],
        res: (u32, u32),
        now_ms: u64,
    ) {
        self.order.clear();
        let mut inst: Vec<IconInstance> = Vec::new();
        for (key, rect) in items {
            if let Some(d) = self.images.get(key) {
                inst.push(IconInstance { rect: [rect.x, rect.y, rect.w, rect.h], uv: [0.0, 0.0, 1.0, 1.0] });
                self.order.push((key.clone(), d.frame_at(now_ms)));
            }
        }
        let bytes: &[u8] = bytemuck::cast_slice(&inst);
        let needed = bytes.len() as u64;
        if needed > self.capacity_bytes {
            self.capacity_bytes = needed.next_power_of_two().max(64 * 32);
            self.instance_buf = device.create_buffer(&BufferDescriptor {
                label: Some("media-instances"),
                size: self.capacity_bytes,
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.instance_buf, 0, bytes);
        }
        let uniform = [res.0 as f32, res.1 as f32, 0.0, 0.0];
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&uniform));
    }

    pub fn render<'a>(&'a self, pass: &mut RenderPass<'a>) {
        if self.order.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.instance_buf.slice(..));
        for (i, (key, frame)) in self.order.iter().enumerate() {
            if let Some(img) = self.images.get(key) {
                if let Some(bg) = img.frames.get(*frame) {
                    pass.set_bind_group(0, bg, &[]);
                    let n = i as u32;
                    pass.draw(0..6, n..n + 1);
                }
            }
        }
    }
}

/// A decoded image frame (raw RGBA + dims + delay). `Send`, so decoding happens on
/// a worker thread and frames are shipped to the main thread for cheap upload.
pub struct DecodedFrame {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
    pub delay_ms: u32,
}

/// Decode a still image at (near-)native resolution for the image viewer, so
/// zooming in stays sharp. Only clamps to the GPU's max texture dimension. GIFs
/// fall back to the downscaled `decode` (animation matters more than crispness).
pub fn decode_full(bytes: &[u8]) -> Vec<DecodedFrame> {
    const MAX_TEX: u32 = 8192; // safe across GPUs
    if bytes.starts_with(b"GIF8") {
        return decode(bytes);
    }
    if let Ok(img) = image::load_from_memory(bytes) {
        let mut rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        if w > MAX_TEX || h > MAX_TEX {
            let scale = (MAX_TEX as f32 / w.max(h) as f32).min(1.0);
            let (nw, nh) = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
            rgba = image::imageops::resize(&rgba, nw.max(1), nh.max(1), image::imageops::FilterType::Triangle);
        }
        let (w, h) = rgba.dimensions();
        if w > 0 && h > 0 {
            return vec![DecodedFrame { rgba: rgba.into_raw(), w, h, delay_ms: 0 }];
        }
    }
    Vec::new()
}

/// Decode image bytes into frames OFF the main thread. Animated GIFs yield all
/// frames (capped) with delays; PNG/JPG yield one. Large frames are downscaled so
/// a multi-frame GIF doesn't blow up GPU memory or upload time.
pub fn decode(bytes: &[u8]) -> Vec<DecodedFrame> {
    const MAX_FRAMES: usize = 120;
    const MAX_DIM: u32 = 900;

    let fit = |img: image::RgbaImage| -> (Vec<u8>, u32, u32) {
        let (w, h) = img.dimensions();
        if w > MAX_DIM {
            let nh = (h * MAX_DIM / w).max(1);
            let r = image::imageops::resize(&img, MAX_DIM, nh, image::imageops::FilterType::Triangle);
            (r.into_raw(), MAX_DIM, nh)
        } else {
            (img.into_raw(), w, h)
        }
    };

    if bytes.starts_with(b"GIF8") {
        use image::codecs::gif::GifDecoder;
        use image::AnimationDecoder;
        if let Ok(dec) = GifDecoder::new(std::io::Cursor::new(bytes)) {
            if let Ok(frames) = dec.into_frames().collect_frames() {
                let mut out = Vec::new();
                for f in frames.into_iter().take(MAX_FRAMES) {
                    let (n, d) = f.delay().numer_denom_ms();
                    let ms = if d == 0 { 100 } else { (n / d).max(20) };
                    let (rgba, w, h) = fit(f.into_buffer());
                    out.push(DecodedFrame { rgba, w, h, delay_ms: ms });
                }
                if !out.is_empty() {
                    return out;
                }
            }
        }
    }
    if let Ok(img) = image::load_from_memory(bytes) {
        let (rgba, w, h) = fit(img.to_rgba8());
        if w > 0 && h > 0 {
            return vec![DecodedFrame { rgba, w, h, delay_ms: 0 }];
        }
    }
    Vec::new()
}
