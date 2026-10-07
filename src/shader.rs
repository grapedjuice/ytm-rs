//! Animated album-art background, a port of Kawarp (MIT © 2026 Better Lyrics), the
//! effect behind better-lyrics-shaders: the blurred cover is domain-warped by two
//! octaves of simplex noise, zoomed, vignetted, saturated and dithered. Tracks
//! crossfade. Everything runs in one fragment pass over a 64×64 texture.

use std::sync::{Arc, Mutex};

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
  float t = u_time * 0.05;
  float centerWeight = 1.0 - smoothstep(0.0, 0.7, length(uv - 0.5));
  float n1 = snoise(uv * 0.35 + vec2(t, t * 0.7));
  float n2 = snoise(uv * 0.35 + vec2(-t * 0.8, t * 0.5) + vec2(50.0, 50.0));
  float n3 = snoise(uv * 0.9 + vec2(t * 1.2, -t) + vec2(100.0, 0.0));
  float n4 = snoise(uv * 0.9 + vec2(-t, t * 1.1) + vec2(0.0, 100.0));
  vec2 warp = vec2(n1 * 0.65 + n3 * 0.35, n2 * 0.65 + n4 * 0.35) * centerWeight;
  vec2 w = clamp(uv + warp * u_intensity, 0.0, 1.0);

  vec3 color = mix(TEX(u_b, w).rgb, TEX(u_a, w).rgb, u_blend);

  vec2 c = v_uv - 0.5;
  color *= 1.0 - dot(c, c) * 0.3;
  float gray = dot(color, vec3(0.299, 0.587, 0.114));
  color = mix(vec3(gray), color, u_saturation);
  color *= u_dim;
  // Dither to hide banding in the smooth gradients.
  color += (hash(vec3(floor(v_uv * u_resolution), floor(u_time * 30.0))) - 0.5) / 255.0 * 1.5;
  FRAG = vec4(color, 1.0);
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
}

struct Gl {
    program: glow::Program,
    vao: glow::VertexArray,
    tex: [glow::Texture; 2],
    /// Index of the texture holding the current art.
    cur: usize,
}

#[derive(Clone)]
pub struct Background {
    gl: Arc<Mutex<Option<Gl>>>,
    pending: Arc<Mutex<Option<Art>>>,
}

impl Background {
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
            let this = Self {
                gl: Arc::new(Mutex::new(Some(Gl { program, vao, tex, cur: 0 }))),
                pending: Default::default(),
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

    pub fn paint(&self, painter: &egui::Painter, rect: egui::Rect, p: Params) {
        let (gl_state, pending) = (self.gl.clone(), self.pending.clone());
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
                gl.use_program(Some(s.program));
                gl.bind_vertex_array(Some(s.vao));
                let loc = |n: &str| gl.get_uniform_location(s.program, n);
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(s.tex[s.cur]));
                gl.uniform_1_i32(loc("u_a").as_ref(), 0);
                gl.active_texture(glow::TEXTURE1);
                gl.bind_texture(glow::TEXTURE_2D, Some(s.tex[s.cur ^ 1]));
                gl.uniform_1_i32(loc("u_b").as_ref(), 1);
                gl.active_texture(glow::TEXTURE0);
                gl.uniform_1_f32(loc("u_blend").as_ref(), p.blend);
                gl.uniform_1_f32(loc("u_time").as_ref(), p.time);
                gl.uniform_1_f32(loc("u_intensity").as_ref(), p.intensity);
                gl.uniform_1_f32(loc("u_scale").as_ref(), p.scale);
                gl.uniform_1_f32(loc("u_saturation").as_ref(), p.saturation);
                gl.uniform_1_f32(loc("u_dim").as_ref(), p.dim);
                let vp = info.viewport_in_pixels();
                gl.uniform_2_f32(loc("u_resolution").as_ref(), vp.width_px as f32, vp.height_px as f32);
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

unsafe fn upload(gl: &glow::Context, tex: glow::Texture, art: &Art) {
    unsafe {
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
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
