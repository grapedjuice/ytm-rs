//! Animated album-art background, a port of Kawarp (MIT © 2026 Better Lyrics), the
//! effect behind better-lyrics-shaders: the blurred cover is domain-warped by two
//! octaves of simplex noise, zoomed, vignetted, saturated and dithered. Tracks
//! crossfade. Everything runs in one fragment pass over a 64×64 texture. The same
//! shader is written twice: GLSL for OpenGL, WGSL for wgpu (see renderer.rs).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use eframe::glow::{self, HasContext};

use crate::art::{Art, SIZE};

const FRAGMENT: &str = r#"
precision highp float;
uniform sampler2D u_a;
uniform sampler2D u_b;
uniform float u_blend;
uniform float u_time;
uniform float u_intensity;
uniform float u_scale;
uniform float u_saturation;
uniform float u_dim;
uniform float u_contrast;
uniform float u_dither;
uniform float u_rotate;
uniform float u_alpha;
uniform vec2 u_resolution;
IN vec2 v_uv;
#if NEW_SHADER_INTERFACE
out vec4 frag_color;
#define FRAG frag_color
#define TEX texture
#else
#define FRAG gl_FragColor
#define TEX texture2D
#endif

vec3 mod289(vec3 x) { return x - floor(x * (1.0 / 289.0)) * 289.0; }
vec2 mod289(vec2 x) { return x - floor(x * (1.0 / 289.0)) * 289.0; }
vec3 permute(vec3 x) { return mod289(((x * 34.0) + 1.0) * x); }
float snoise(vec2 v) {
  const vec4 C = vec4(0.211324865405187, 0.366025403784439, -0.577350269189626, 0.024390243902439);
  vec2 i = floor(v + dot(v, C.yy));
  vec2 x0 = v - i + dot(i, C.xx);
  vec2 i1 = (x0.x > x0.y) ? vec2(1.0, 0.0) : vec2(0.0, 1.0);
  vec4 x12 = x0.xyxy + C.xxzz;
  x12.xy -= i1;
  i = mod289(i);
  vec3 p = permute(permute(i.y + vec3(0.0, i1.y, 1.0)) + i.x + vec3(0.0, i1.x, 1.0));
  vec3 m = max(0.5 - vec3(dot(x0, x0), dot(x12.xy, x12.xy), dot(x12.zw, x12.zw)), 0.0);
  m = m * m; m = m * m;
  vec3 x = 2.0 * fract(p * C.www) - 1.0;
  vec3 h = abs(x) - 0.5;
  vec3 ox = floor(x + 0.5);
  vec3 a0 = x - ox;
  m *= 1.79284291400159 - 0.85373472095314 * (a0 * a0 + h * h);
  vec3 g;
  g.x = a0.x * x0.x + h.x * x0.y;
  g.yz = a0.yz * x12.xz + h.yz * x12.yw;
  return 130.0 * dot(m, g);
}
float hash(vec3 p) {
  p = fract(p * 0.1031);
  p += dot(p, p.zyx + 31.32);
  return fract((p.x + p.y) * p.z);
}

void main() {
  // Zoom in so warped edges never show the clamp.
  vec2 uv = (v_uv - 0.5) / u_scale + 0.5;
  if (u_rotate != 0.0) {
    float s = sin(u_rotate), k = cos(u_rotate);
    uv = mat2(k, s, -s, k) * (uv - 0.5) + 0.5;
  }
  // A still background (intensity 0) skips the noise: that's most of this shader's cost.
  vec2 w = clamp(uv, 0.0, 1.0);
  if (u_intensity != 0.0) {
    float t = u_time * 0.05;
    float centerWeight = 1.0 - smoothstep(0.0, 0.7, length(uv - 0.5));
    float n1 = snoise(uv * 0.35 + vec2(t, t * 0.7));
    float n2 = snoise(uv * 0.35 + vec2(-t * 0.8, t * 0.5) + vec2(50.0, 50.0));
    float n3 = snoise(uv * 0.9 + vec2(t * 1.2, -t) + vec2(100.0, 0.0));
    float n4 = snoise(uv * 0.9 + vec2(-t, t * 1.1) + vec2(0.0, 100.0));
    vec2 warp = vec2(n1 * 0.65 + n3 * 0.35, n2 * 0.65 + n4 * 0.35) * centerWeight;
    w = clamp(uv + warp * u_intensity, 0.0, 1.0);
  }

  vec3 color = mix(TEX(u_b, w).rgb, TEX(u_a, w).rgb, u_blend);

  vec2 c = v_uv - 0.5;
  color *= 1.0 - dot(c, c) * 0.3;
  float gray = dot(color, vec3(0.299, 0.587, 0.114));
  color = mix(vec3(gray), color, u_saturation);
  color = (color - 0.5) * u_contrast + 0.5;
  color *= u_dim;
  // Dither to hide banding in the smooth gradients.
  color += (hash(vec3(floor(v_uv * u_resolution), floor(u_time * 30.0))) - 0.5) * u_dither;
  // Premultiplied, as egui blends: under 1 it shows what was painted before it.
  FRAG = vec4(color * u_alpha, u_alpha);
}
"#;

const VERTEX: &str = r#"
OUT vec2 v_uv;
void main() {
  // Full-screen triangle from the vertex index; no vertex buffers needed.
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  // Texture row 0 is the image's top edge, drawn at the top of the rect.
  v_uv = vec2(p.x, 1.0 - p.y);
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

/// [`VERTEX`] and [`FRAGMENT`] in WGSL, for wgpu; keep the two in step.
const WGSL: &str = r#"
struct U {
  blend: f32,
  time: f32,
  intensity: f32,
  scale: f32,
  saturation: f32,
  dim: f32,
  resolution: vec2<f32>,
  contrast: f32,
  dither: f32,
  rotate: f32,
  alpha: f32,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex_a: texture_2d<f32>;
@group(0) @binding(2) var tex_b: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

struct VOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
  // Full-screen triangle from the vertex index, as in VERTEX.
  let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
  var o: VOut;
  o.uv = vec2<f32>(p.x, 1.0 - p.y);
  o.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
  return o;
}

fn mod289_3(x: vec3<f32>) -> vec3<f32> { return x - floor(x * (1.0 / 289.0)) * 289.0; }
fn mod289_2(x: vec2<f32>) -> vec2<f32> { return x - floor(x * (1.0 / 289.0)) * 289.0; }
fn permute(x: vec3<f32>) -> vec3<f32> { return mod289_3(((x * 34.0) + 1.0) * x); }
fn snoise(v: vec2<f32>) -> f32 {
  let C = vec4<f32>(0.211324865405187, 0.366025403784439, -0.577350269189626, 0.024390243902439);
  var i = floor(v + dot(v, C.yy));
  let x0 = v - i + dot(i, C.xx);
  var i1 = vec2<f32>(0.0, 1.0);
  if (x0.x > x0.y) { i1 = vec2<f32>(1.0, 0.0); }
  var x12 = x0.xyxy + C.xxzz;
  x12 = vec4<f32>(x12.xy - i1, x12.zw);
  i = mod289_2(i);
  let p = permute(permute(i.y + vec3<f32>(0.0, i1.y, 1.0)) + i.x + vec3<f32>(0.0, i1.x, 1.0));
  var m = max(0.5 - vec3<f32>(dot(x0, x0), dot(x12.xy, x12.xy), dot(x12.zw, x12.zw)), vec3<f32>(0.0));
  m = m * m;
  m = m * m;
  let x = 2.0 * fract(p * C.www) - 1.0;
  let h = abs(x) - 0.5;
  let ox = floor(x + 0.5);
  let a0 = x - ox;
  m = m * (1.79284291400159 - 0.85373472095314 * (a0 * a0 + h * h));
  let g = vec3<f32>(a0.x * x0.x + h.x * x0.y, a0.yz * x12.xz + h.yz * x12.yw);
  return 130.0 * dot(m, g);
}
fn hash(q: vec3<f32>) -> f32 {
  var p = fract(q * 0.1031);
  p = p + dot(p, p.zyx + 31.32);
  return fract((p.x + p.y) * p.z);
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
  var uv = (in.uv - 0.5) / u.scale + 0.5;
  if (u.rotate != 0.0) {
    let s = sin(u.rotate);
    let k = cos(u.rotate);
    uv = mat2x2<f32>(k, s, -s, k) * (uv - 0.5) + 0.5;
  }
  var w = clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0));
  if (u.intensity != 0.0) {
    let t = u.time * 0.05;
    let centerWeight = 1.0 - smoothstep(0.0, 0.7, length(uv - 0.5));
    let n1 = snoise(uv * 0.35 + vec2<f32>(t, t * 0.7));
    let n2 = snoise(uv * 0.35 + vec2<f32>(-t * 0.8, t * 0.5) + vec2<f32>(50.0, 50.0));
    let n3 = snoise(uv * 0.9 + vec2<f32>(t * 1.2, -t) + vec2<f32>(100.0, 0.0));
    let n4 = snoise(uv * 0.9 + vec2<f32>(-t, t * 1.1) + vec2<f32>(0.0, 100.0));
    let warp = vec2<f32>(n1 * 0.65 + n3 * 0.35, n2 * 0.65 + n4 * 0.35) * centerWeight;
    w = clamp(uv + warp * u.intensity, vec2<f32>(0.0), vec2<f32>(1.0));
  }

  var color = mix(textureSample(tex_b, samp, w).rgb, textureSample(tex_a, samp, w).rgb, u.blend);

  let c = in.uv - 0.5;
  color = color * (1.0 - dot(c, c) * 0.3);
  let gray = dot(color, vec3<f32>(0.299, 0.587, 0.114));
  color = mix(vec3<f32>(gray), color, u.saturation);
  color = (color - 0.5) * u.contrast + 0.5;
  color = color * u.dim;
  color = color + (hash(vec3<f32>(floor(in.uv * u.resolution), floor(u.time * 30.0))) - 0.5) * u.dither;
  return vec4<f32>(color * u.alpha, u.alpha);
}
"#;

pub struct Params {
    pub time: f32,
    /// 0..1 crossfade from the previous texture to the current one.
    pub blend: f32,
    pub intensity: f32,
    pub saturation: f32,
    /// Brightness multiplier (lower behind text-heavy pages).
    pub dim: f32,
    /// Extra zoom (audio-reactive pulse rides on this).
    pub scale: f32,
    pub contrast: f32,
    /// Noise against banding, fractions of full brightness.
    pub dither: f32,
    /// Radians.
    pub rotate: f32,
    /// Under 1, what was painted before shows through (1 replaces it).
    pub alpha: f32,
}

struct Gl {
    program: glow::Program,
    vao: glow::VertexArray,
    tex: [glow::Texture; 2],
    /// Index of the texture holding the current art.
    cur: usize,
    /// Uniform locations, looked up once: u_a, u_b, u_blend, u_time, u_intensity,
    /// u_scale, u_saturation, u_dim, u_resolution, u_contrast, u_dither, u_rotate, u_alpha.
    loc: [Option<glow::UniformLocation>; 13],
}

const UNIFORMS: [&str; 13] = ["u_a", "u_b", "u_blend", "u_time", "u_intensity", "u_scale", "u_saturation", "u_dim", "u_resolution", "u_contrast", "u_dither", "u_rotate", "u_alpha"];

/// The wgpu side, kept in egui-wgpu's callback resources.
struct Wgpu {
    pipeline: wgpu::RenderPipeline,
    tex: [wgpu::Texture; 2],
    /// One for each time the background is drawn in a frame (the page, now playing's layer
    /// and canvas, the sidebar's tint, the bar's cover): every draw's uniforms are written
    /// before any of them is painted, so each needs a buffer of its own.
    draws: Vec<WgpuDraw>,
    /// The next of `draws` to hand out, round and round.
    next: usize,
    cur: usize,
}

struct WgpuDraw {
    uniforms: wgpu::Buffer,
    /// Bind group `i` samples `tex[i]` as the current art and the other as the previous.
    groups: [wgpu::BindGroup; 2],
}

/// More than the background is ever drawn in one frame.
const WGPU_DRAWS: usize = 8;

#[derive(Clone)]
pub struct Background {
    gl: Arc<Mutex<Option<Gl>>>,
    pending: Arc<Mutex<Option<Art>>>,
    /// A video's picture, replacing the current art with no crossfade.
    live: Arc<Mutex<Option<Art>>>,
    /// Drawn through wgpu, whose state lives with the renderer (`gl` stays empty).
    wgpu: bool,
}

impl Background {
    pub fn new_wgpu(rs: &eframe::egui_wgpu::RenderState) -> Self {
        let device = &rs.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("background"), source: wgpu::ShaderSource::Wgsl(WGSL.into()) });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                multisampled: false,
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("background"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                texture(1),
                texture(2),
                wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("background"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("background"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_main"), buffers: &[], compilation_options: Default::default() },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState { format: rs.target_format, blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let tex = [0, 1].map(|_| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("background art"),
                size: wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("background"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let views = tex.each_ref().map(|t| t.create_view(&Default::default()));
        let draws = (0..WGPU_DRAWS)
            .map(|_| {
                let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("background uniforms"),
                    size: 48,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let groups = [0, 1].map(|i| {
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("background"),
                        layout: &layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[i]) },
                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&views[i ^ 1]) },
                            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&sampler) },
                        ],
                    })
                });
                WgpuDraw { uniforms, groups }
            })
            .collect();
        // Both slots start with the idle backdrop.
        let idle = crate::art::idle();
        for t in &tex {
            upload_wgpu(&rs.queue, t, &idle);
        }
        rs.renderer.write().callback_resources.insert(Wgpu { pipeline, tex, draws, next: 0, cur: 0 });
        Self { gl: Default::default(), pending: Default::default(), live: Default::default(), wgpu: true }
    }

    pub fn new(gl: &glow::Context) -> anyhow::Result<Self> {
        let version = eframe::egui_glow::ShaderVersion::get(gl);
        let header = format!(
            "{}\n#define NEW_SHADER_INTERFACE {}\n{}\n",
            version.version_declaration(),
            version.is_new_shader_interface() as i32,
            if version.is_new_shader_interface() {
                "#define IN in\n#define OUT out"
            } else {
                "#define IN varying\n#define OUT varying"
            }
        );
        unsafe {
            let program = gl.create_program().map_err(anyhow::Error::msg)?;
            let mut shaders = Vec::new();
            for (kind, src) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, FRAGMENT)] {
                let s = gl.create_shader(kind).map_err(anyhow::Error::msg)?;
                gl.shader_source(s, &format!("{header}{src}"));
                gl.compile_shader(s);
                anyhow::ensure!(gl.get_shader_compile_status(s), "shader: {}", gl.get_shader_info_log(s));
                gl.attach_shader(program, s);
                shaders.push(s);
            }
            gl.link_program(program);
            anyhow::ensure!(gl.get_program_link_status(program), "link: {}", gl.get_program_info_log(program));
            for s in shaders {
                gl.detach_shader(program, s);
                gl.delete_shader(s);
            }
            let vao = gl.create_vertex_array().map_err(anyhow::Error::msg)?;
            let mk = || -> anyhow::Result<glow::Texture> {
                let t = gl.create_texture().map_err(anyhow::Error::msg)?;
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
                for (p, v) in [
                    (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                    (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                    (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                    (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
                ] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
                }
                Ok(t)
            };
            let tex = [mk()?, mk()?];
            let loc = UNIFORMS.map(|n| gl.get_uniform_location(program, n));
            let this = Self {
                gl: Arc::new(Mutex::new(Some(Gl { program, vao, tex, cur: 0, loc }))),
                pending: Default::default(),
                live: Default::default(),
                wgpu: false,
            };
            // Both slots start with the idle backdrop.
            let idle = crate::art::idle();
            for t in tex {
                upload(gl, t, &idle);
            }
            Ok(this)
        }
    }

    /// Queue new art; it is uploaded on the GL thread at the next paint.
    pub fn set_art(&self, art: Art) {
        *self.pending.lock().unwrap() = Some(art);
    }

    /// Show this in place of the current art right away (a video's picture).
    pub fn set_live(&self, art: Art) {
        *self.live.lock().unwrap() = Some(art);
    }

    pub fn paint(&self, painter: &egui::Painter, rect: egui::Rect, p: Params) {
        if self.wgpu {
            painter.add(eframe::egui_wgpu::Callback::new_paint_callback(rect, WgpuPaint { pending: self.pending.clone(), live: self.live.clone(), rect, p, draw: Default::default() }));
            return;
        }
        let (gl_state, pending, live) = (self.gl.clone(), self.pending.clone(), self.live.clone());
        let cb = eframe::egui_glow::CallbackFn::new(move |info, painter| {
            let gl = painter.gl();
            let mut guard = gl_state.lock().unwrap();
            let Some(s) = guard.as_mut() else { return };
            unsafe {
                if let Some(art) = pending.lock().unwrap().take() {
                    // New art goes into the other slot; the app animates `blend` 0→1.
                    s.cur ^= 1;
                    upload(gl, s.tex[s.cur], &art);
                }
                if let Some(art) = live.lock().unwrap().take() {
                    upload(gl, s.tex[s.cur], &art);
                }
                gl.use_program(Some(s.program));
                gl.bind_vertex_array(Some(s.vao));
                let [a, b, blend, time, intensity, scale, saturation, dim, resolution, contrast, dither, rotate, alpha] = s.loc.each_ref();
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(s.tex[s.cur]));
                gl.uniform_1_i32(a.as_ref(), 0);
                gl.active_texture(glow::TEXTURE1);
                gl.bind_texture(glow::TEXTURE_2D, Some(s.tex[s.cur ^ 1]));
                gl.uniform_1_i32(b.as_ref(), 1);
                gl.active_texture(glow::TEXTURE0);
                gl.uniform_1_f32(blend.as_ref(), p.blend);
                gl.uniform_1_f32(time.as_ref(), p.time);
                gl.uniform_1_f32(intensity.as_ref(), p.intensity);
                gl.uniform_1_f32(scale.as_ref(), p.scale);
                gl.uniform_1_f32(saturation.as_ref(), p.saturation);
                gl.uniform_1_f32(dim.as_ref(), p.dim);
                gl.uniform_1_f32(contrast.as_ref(), p.contrast);
                gl.uniform_1_f32(dither.as_ref(), p.dither);
                gl.uniform_1_f32(rotate.as_ref(), p.rotate);
                gl.uniform_1_f32(alpha.as_ref(), p.alpha);
                let vp = info.viewport_in_pixels();
                gl.uniform_2_f32(resolution.as_ref(), vp.width_px as f32, vp.height_px as f32);
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
            }
        });
        painter.add(egui::PaintCallback { rect, callback: Arc::new(cb) });
    }

    pub fn destroy(&self, gl: &glow::Context) {
        if let Some(s) = self.gl.lock().unwrap().take() {
            unsafe {
                gl.delete_program(s.program);
                gl.delete_vertex_array(s.vao);
                for t in s.tex {
                    gl.delete_texture(t);
                }
            }
        }
    }
}

struct WgpuPaint {
    pending: Arc<Mutex<Option<Art>>>,
    live: Arc<Mutex<Option<Art>>>,
    rect: egui::Rect,
    p: Params,
    /// Which of `Wgpu::draws` this one's uniforms went into.
    draw: std::sync::atomic::AtomicUsize,
}

impl eframe::egui_wgpu::CallbackTrait for WgpuPaint {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &eframe::egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut eframe::egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(s) = resources.get_mut::<Wgpu>() else { return Vec::new() };
        if let Some(art) = self.pending.lock().unwrap().take() {
            // New art goes into the other slot; the app animates `blend` 0→1.
            s.cur ^= 1;
            upload_wgpu(queue, &s.tex[s.cur], &art);
        }
        if let Some(art) = self.live.lock().unwrap().take() {
            upload_wgpu(queue, &s.tex[s.cur], &art);
        }
        let p = &self.p;
        let px = self.rect.size() * screen.pixels_per_point;
        let u = [p.blend, p.time, p.intensity, p.scale, p.saturation, p.dim, px.x, px.y, p.contrast, p.dither, p.rotate, p.alpha];
        let draw = s.next;
        s.next = (s.next + 1) % s.draws.len();
        self.draw.store(draw, std::sync::atomic::Ordering::Relaxed);
        queue.write_buffer(&s.draws[draw].uniforms, 0, &u.iter().flat_map(|f| f.to_ne_bytes()).collect::<Vec<u8>>());
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &eframe::egui_wgpu::CallbackResources) {
        let Some(s) = resources.get::<Wgpu>() else { return };
        pass.set_pipeline(&s.pipeline);
        let draw = &s.draws[self.draw.load(std::sync::atomic::Ordering::Relaxed)];
        pass.set_bind_group(0, &draw.groups[s.cur], &[]);
        pass.draw(0..3, 0..1);
    }
}

fn upload_wgpu(queue: &wgpu::Queue, tex: &wgpu::Texture, art: &Art) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        &art.rgba,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * SIZE), rows_per_image: Some(SIZE) },
        wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
    );
}

unsafe fn upload(gl: &glow::Context, tex: glow::Texture, art: &Art) {
    unsafe {
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            SIZE as i32,
            SIZE as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(&art.rgba)),
        );
    }
}

// ------------------------------------------------------------------ frosted glass

/// What's on screen behind a panel, blurred: shrunk to half size, then down and back up a
/// few smaller levels (dual Kawase), and drawn back with rounded corners. Only the panel's
/// own region is touched, at a fraction of its pixels, and nothing runs in a frame that
/// isn't drawn. OpenGL only: wgpu can't read the frame it's in the middle of drawing, so
/// panels there keep their plain tint (see [`glass_ready`]).
const GLASS_VERTEX: &str = r#"
OUT vec2 v_uv;
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  v_uv = p;
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const GLASS_HEAD: &str = r#"
precision highp float;
IN vec2 v_uv;
out vec4 frag_color;
uniform sampler2D u_tex;
uniform vec4 u_src;
uniform vec2 u_step;
vec4 at(vec2 uv) { return texture(u_tex, clamp(uv, u_src.xy, u_src.zw)); }
"#;

const GLASS_DOWN: &str = r#"
void main() {
  vec2 uv = mix(u_src.xy, u_src.zw, v_uv);
  vec4 c = at(uv) * 4.0;
  c += at(uv - u_step) + at(uv + u_step);
  c += at(uv + vec2(u_step.x, -u_step.y)) + at(uv - vec2(u_step.x, -u_step.y));
  frag_color = c / 8.0;
}
"#;

const GLASS_UP: &str = r#"
void main() {
  vec2 uv = mix(u_src.xy, u_src.zw, v_uv);
  vec4 c = at(uv + vec2(-u_step.x * 2.0, 0.0));
  c += at(uv + vec2(-u_step.x, u_step.y)) * 2.0;
  c += at(uv + vec2(0.0, u_step.y * 2.0));
  c += at(uv + vec2(u_step.x, u_step.y)) * 2.0;
  c += at(uv + vec2(u_step.x * 2.0, 0.0));
  c += at(uv + vec2(u_step.x, -u_step.y)) * 2.0;
  c += at(uv + vec2(0.0, -u_step.y * 2.0));
  c += at(uv + vec2(-u_step.x, -u_step.y)) * 2.0;
  frag_color = c / 12.0;
}
"#;

const GLASS_SHOW: &str = r#"
uniform vec2 u_size;
uniform float u_radius;
uniform float u_opacity;
uniform float u_saturate;
uniform float u_contrast;
uniform float u_fade;
uniform float u_bright;
void main() {
  vec3 c = at(mix(u_src.xy, u_src.zw, v_uv)).rgb;
  float gray = dot(c, vec3(0.299, 0.587, 0.114));
  c = mix(vec3(gray), c, u_saturate);
  c = clamp((c - 0.5) * u_contrast + 0.5, 0.0, 1.0);
  c = min(c * u_bright, vec3(1.0));
  vec2 p = (v_uv - 0.5) * u_size;
  vec2 q = abs(p) - u_size * 0.5 + u_radius;
  float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - u_radius;
  float a = clamp(0.5 - d, 0.0, 1.0) * u_opacity;
  // A band fading out downward, over its bottom u_fade px.
  if (u_fade > 0.0) { a *= clamp(v_uv.y * u_size.y / u_fade, 0.0, 1.0); }
  frag_color = vec4(c * a, a);
}
"#;

/// How a surface frosts.
#[derive(Clone, Copy)]
pub struct Frost {
    /// `[blur px, saturation, contrast]`, as a theme gives it.
    pub look: [f32; 3],
    pub radius: f32,
    pub opacity: f32,
    /// Fades out over this many points at the bottom.
    pub fade: f32,
}

const LEVELS: usize = 6;

struct GlassProgram {
    program: glow::Program,
    tex: Option<glow::UniformLocation>,
    src: Option<glow::UniformLocation>,
    step: Option<glow::UniformLocation>,
}

struct GlassGl {
    down: GlassProgram,
    up: GlassProgram,
    show: GlassProgram,
    /// u_size, u_radius, u_opacity, u_saturate, u_contrast, u_fade, u_bright.
    show_loc: [Option<glow::UniformLocation>; 7],
    vao: glow::VertexArray,
    fbo: glow::Framebuffer,
    tex: [glow::Texture; LEVELS],
    /// The window's size the textures were made for.
    size: [i32; 2],
}

static GLASS: OnceLock<Mutex<Option<GlassGl>>> = OnceLock::new();
static GLASS_ON: AtomicBool = AtomicBool::new(true);

/// Settings › Appearance: frosted panels on or off.
pub fn set_glass_on(on: bool) {
    GLASS_ON.store(on, Ordering::Relaxed);
}

/// Whether panels are really frosted now. If not, they stay opaque enough to read over
/// what's behind them.
pub fn glass_ready() -> bool {
    GLASS_ON.load(Ordering::Relaxed) && GLASS.get().is_some_and(|g| g.lock().unwrap().is_some())
}

/// Make the programs. Failing only leaves panels with their plain tint.
pub fn install_glass(gl: &glow::Context) {
    if std::env::var_os("YTM_NO_GLASS").is_some() {
        return;
    }
    match make_glass(gl) {
        Ok(g) => {
            let _ = GLASS.set(Mutex::new(Some(g)));
        }
        Err(e) => log::warn!("frosted glass unavailable: {e:#}"),
    }
}

pub fn destroy_glass(gl: &glow::Context) {
    if let Some(g) = GLASS.get().and_then(|g| g.lock().unwrap().take()) {
        unsafe {
            gl.delete_program(g.down.program);
            gl.delete_program(g.up.program);
            gl.delete_program(g.show.program);
            gl.delete_vertex_array(g.vao);
            gl.delete_framebuffer(g.fbo);
            for t in g.tex {
                gl.delete_texture(t);
            }
        }
    }
}

fn make_glass(gl: &glow::Context) -> anyhow::Result<GlassGl> {
    let version = eframe::egui_glow::ShaderVersion::get(gl);
    anyhow::ensure!(version.is_new_shader_interface(), "needs a newer OpenGL");
    let header = format!("{}\n#define IN in\n#define OUT out\n", version.version_declaration());
    unsafe {
        let program = |frag: &str| -> anyhow::Result<GlassProgram> {
            let p = gl.create_program().map_err(anyhow::Error::msg)?;
            let mut shaders = Vec::new();
            for (kind, src) in [(glow::VERTEX_SHADER, format!("{header}{GLASS_VERTEX}")), (glow::FRAGMENT_SHADER, format!("{header}{GLASS_HEAD}{frag}"))] {
                let s = gl.create_shader(kind).map_err(anyhow::Error::msg)?;
                gl.shader_source(s, &src);
                gl.compile_shader(s);
                anyhow::ensure!(gl.get_shader_compile_status(s), "glass shader: {}", gl.get_shader_info_log(s));
                gl.attach_shader(p, s);
                shaders.push(s);
            }
            gl.link_program(p);
            anyhow::ensure!(gl.get_program_link_status(p), "glass link: {}", gl.get_program_info_log(p));
            for s in shaders {
                gl.detach_shader(p, s);
                gl.delete_shader(s);
            }
            Ok(GlassProgram { program: p, tex: gl.get_uniform_location(p, "u_tex"), src: gl.get_uniform_location(p, "u_src"), step: gl.get_uniform_location(p, "u_step") })
        };
        let (down, up, show) = (program(GLASS_DOWN)?, program(GLASS_UP)?, program(GLASS_SHOW)?);
        let show_loc = ["u_size", "u_radius", "u_opacity", "u_saturate", "u_contrast", "u_fade", "u_bright"].map(|n| gl.get_uniform_location(show.program, n));
        let vao = gl.create_vertex_array().map_err(anyhow::Error::msg)?;
        let fbo = gl.create_framebuffer().map_err(anyhow::Error::msg)?;
        let mut tex = Vec::new();
        for _ in 0..LEVELS {
            let t = gl.create_texture().map_err(anyhow::Error::msg)?;
            gl.bind_texture(glow::TEXTURE_2D, Some(t));
            for (p, v) in [(glow::TEXTURE_MIN_FILTER, glow::LINEAR), (glow::TEXTURE_MAG_FILTER, glow::LINEAR), (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE), (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE)] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
            }
            tex.push(t);
        }
        Ok(GlassGl { down, up, show, show_loc, vao, fbo, tex: tex.try_into().map_err(|_| anyhow::anyhow!("textures"))?, size: [0, 0] })
    }
}

/// Frost what's already been drawn behind `rect`. Does nothing when glass isn't available.
pub fn frost(painter: &egui::Painter, rect: egui::Rect, f: Frost) {
    frost_bright(painter, rect, f, 1.0);
}

/// `frost`, with `brightness()` in the filter too.
pub fn frost_bright(painter: &egui::Painter, rect: egui::Rect, f: Frost, bright: f32) {
    if f.opacity <= 0.0 || f.look[0] <= 0.0 || !glass_ready() {
        return;
    }
    let cb = eframe::egui_glow::CallbackFn::new(move |info, painter| {
        let gl = painter.gl();
        let Some(lock) = GLASS.get() else { return };
        let mut guard = lock.lock().unwrap();
        let Some(g) = guard.as_mut() else { return };
        unsafe { draw_frost(gl, g, &info, f, bright) };
    });
    painter.add(egui::PaintCallback { rect, callback: Arc::new(cb) });
}

unsafe fn draw_frost(gl: &glow::Context, g: &mut GlassGl, info: &egui::PaintCallbackInfo, f: Frost, bright: f32) {
    unsafe {
        let [sw, sh] = info.screen_size_px.map(|v| v as i32);
        if sw <= 0 || sh <= 0 {
            return;
        }
        if g.size != [sw, sh] {
            // Textures follow the window: half, a quarter, ... of it. Under 2 MB at 1080p.
            g.size = [sw, sh];
            for (i, t) in g.tex.iter().enumerate() {
                gl.bind_texture(glow::TEXTURE_2D, Some(*t));
                gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, (sw >> (i + 1)).max(1), (sh >> (i + 1)).max(1), 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
            }
        }
        let vp = info.viewport_in_pixels();
        let sigma = (f.look[0] * info.pixels_per_point).max(1.0);
        // Each level about doubles the blur; the sample distance covers what's in between.
        let n = ((sigma / 1.5).log2().ceil() as i32).clamp(1, LEVELS as i32 - 1) as usize;
        let offset = (sigma / (1.5 * (1u32 << (n - 1)) as f32)).clamp(0.6, 2.0);
        let pad = (sigma * 2.5) as i32 + (2 << n);
        // The region to read, with room around it so its edges have something to blur with,
        // on a grid so every level lines up.
        let snap = 1 << n;
        let x0 = ((vp.left_px - pad).max(0) / snap) * snap;
        let y0 = ((vp.from_bottom_px - pad).max(0) / snap) * snap;
        let x1 = (((vp.left_px + vp.width_px + pad).min(sw) + snap - 1) / snap * snap).min(sw);
        let y1 = (((vp.from_bottom_px + vp.height_px + pad).min(sh) + snap - 1) / snap * snap).min(sh);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        // Level `i`'s region and its texture's size.
        let lv = |i: usize| (x0 >> (i + 1), y0 >> (i + 1), (x1 >> (i + 1)).max((x0 >> (i + 1)) + 1), (y1 >> (i + 1)).max((y0 >> (i + 1)) + 1));
        let tex_size = |i: usize| ((sw >> (i + 1)).max(1) as f32, (sh >> (i + 1)).max(1) as f32);
        let src_box = |i: usize| {
            let (a, b, c, d) = lv(i);
            let (w, h) = tex_size(i);
            [a as f32 / w, b as f32 / h, c as f32 / w, d as f32 / h]
        };
        gl.disable(glow::SCISSOR_TEST);
        gl.disable(glow::BLEND);
        gl.bind_vertex_array(Some(g.vao));
        gl.active_texture(glow::TEXTURE0);
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(g.fbo));
        gl.framebuffer_texture_2d(glow::DRAW_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(g.tex[0]), 0);
        let (a, b, c, d) = lv(0);
        gl.blit_framebuffer(x0, y0, x1, y1, a, b, c, d, glow::COLOR_BUFFER_BIT, glow::LINEAR);
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(g.fbo));
        let set = |p: &GlassProgram, src: [f32; 4], step: [f32; 2]| {
            gl.use_program(Some(p.program));
            gl.uniform_1_i32(p.tex.as_ref(), 0);
            gl.uniform_4_f32(p.src.as_ref(), src[0], src[1], src[2], src[3]);
            gl.uniform_2_f32(p.step.as_ref(), step[0], step[1]);
        };
        // Down...
        for i in 1..n {
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(g.tex[i]), 0);
            let (a, b, c, d) = lv(i);
            gl.viewport(a, b, c - a, d - b);
            gl.bind_texture(glow::TEXTURE_2D, Some(g.tex[i - 1]));
            let (w, h) = tex_size(i - 1);
            set(&g.down, src_box(i - 1), [offset / w, offset / h]);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
        // ... and back up.
        for i in (0..n.saturating_sub(1)).rev() {
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(g.tex[i]), 0);
            let (a, b, c, d) = lv(i);
            gl.viewport(a, b, c - a, d - b);
            gl.bind_texture(glow::TEXTURE_2D, Some(g.tex[i + 1]));
            let (w, h) = tex_size(i + 1);
            set(&g.up, src_box(i + 1), [offset * 0.5 / w, offset * 0.5 / h]);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
        // Onto the frame, in the panel's shape.
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.enable(glow::BLEND);
        gl.enable(glow::SCISSOR_TEST);
        let clip = info.clip_rect_in_pixels();
        gl.scissor(clip.left_px, clip.from_bottom_px, clip.width_px, clip.height_px);
        gl.viewport(vp.left_px, vp.from_bottom_px, vp.width_px, vp.height_px);
        gl.bind_texture(glow::TEXTURE_2D, Some(g.tex[0]));
        // The part of level 0 that's under the panel.
        let (w, h) = tex_size(0);
        let src = [vp.left_px as f32 * 0.5 / w, vp.from_bottom_px as f32 * 0.5 / h, (vp.left_px + vp.width_px) as f32 * 0.5 / w, (vp.from_bottom_px + vp.height_px) as f32 * 0.5 / h];
        set(&g.show, src, [0.0, 0.0]);
        let ppp = info.pixels_per_point;
        let [size, radius, opacity, saturate, contrast, fade, brightness] = g.show_loc.each_ref();
        gl.uniform_2_f32(size.as_ref(), vp.width_px as f32, vp.height_px as f32);
        gl.uniform_1_f32(radius.as_ref(), (f.radius * ppp).min(vp.width_px.min(vp.height_px) as f32 / 2.0));
        gl.uniform_1_f32(opacity.as_ref(), f.opacity.clamp(0.0, 1.0));
        gl.uniform_1_f32(saturate.as_ref(), f.look[1]);
        gl.uniform_1_f32(contrast.as_ref(), f.look[2]);
        gl.uniform_1_f32(fade.as_ref(), f.fade * ppp);
        gl.uniform_1_f32(brightness.as_ref(), bright);
        gl.draw_arrays(glow::TRIANGLES, 0, 3);
    }
}
