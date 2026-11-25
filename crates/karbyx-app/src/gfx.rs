// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! Todo lo que toca la GPU: superficie wgpu, texto (glyphon) con caché por
//! fila y por etiqueta, y presentación del frame.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::sync::Arc;

use glyphon::cosmic_text::{Align as TextAlign, Ellipsize, EllipsizeHeightLimit};
use glyphon::{
    Attrs, Buffer, Cache, Color, Cursor, Family, FontSystem, Metrics, Resolution, Shaping, Style, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight, Wrap,
};
use karbyx_term::{CellView, GridSize, Rgb8, Snapshot, Theme};
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::capture;
use crate::panes::PaneId;
use crate::rects::{Rect, RectRenderer, linear};
use crate::ui::{Align, Area, Font, Label, Rgb};

const SANS_FAMILY: &str = "Helvetica Neue";

/// Medidas de la celda en píxeles físicos.
#[derive(Debug, Clone, Copy)]
pub struct CellMetrics {
    pub font_px: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub pad: f32,
}

/// Parámetros de fuente que afectan a las medidas.
#[derive(Debug, Clone, PartialEq)]
pub struct FontSpec {
    pub family: String,
    pub size_pt: f32,
    pub line_height: f32,
    pub padding_pt: f32,
}

/// Línea ya maquetada, reutilizable en cualquier fila y panel.
struct LineBuf {
    buffer: Buffer,
    last_used: u64,
}

/// Fila vacía: no se pinta texto.
const BLANK: u64 = 0;
/// Frames que una línea maquetada sobrevive sin usarse (≈5 s a 60 fps).
const LINE_TTL_FRAMES: u64 = 300;

struct LabelBuf {
    buffer: Buffer,
    alive: bool,
}

/// Texto de un panel de terminal a pintar este frame.
pub struct PaneText {
    pub pane: PaneId,
    pub ox: f32,
    pub oy: f32,
    pub clip: Area,
}

/// Trozo de texto con estilo.
#[derive(Debug, Clone, PartialEq)]
pub struct RichSpan {
    pub text: String,
    pub color: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
}

/// Texto de varias líneas, con ajuste al ancho.
#[derive(Debug, Clone, PartialEq)]
pub struct Rich {
    pub spans: Vec<RichSpan>,
    pub px: f32,
    /// Alto de línea en píxeles.
    pub line: f32,
}

/// Un texto rico colocado en pantalla.
#[derive(Debug, Clone, Copy)]
pub struct RichDraw {
    pub key: u64,
    pub x: f32,
    pub y: f32,
    pub clip: Area,
}

struct RichBuf {
    buffer: Buffer,
    height: f32,
    last_used: u64,
}

/// Posición dentro de un texto: línea lógica (separada por `\n`) y byte.
pub type TextPos = (usize, usize);

pub struct Gfx {
    instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    font_system: FontSystem,
    swash: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text: TextRenderer,
    rects: RectRenderer,
    /// Capa superpuesta (barra de búsqueda...): se pinta encima de todo.
    overlay_text: TextRenderer,
    overlay_rects: RectRenderer,
    /// Caché de líneas maquetadas por contenido: al hacer scroll las líneas
    /// cambian de fila pero no de texto, y se reutilizan sin volver a maquetar.
    lines: HashMap<u64, LineBuf>,
    /// Clave de línea de cada fila visible, por panel.
    pane_rows: HashMap<PaneId, Vec<u64>>,
    frame_no: u64,
    labels: HashMap<u64, LabelBuf>,
    rich: HashMap<u64, RichBuf>,
    pub metrics: CellMetrics,
    font: FontSpec,
    // La ventana va la última: debe destruirse después de la superficie.
    pub window: Arc<Window>,
}

fn measure(font_system: &mut FontSystem, spec: &FontSpec, scale: f32) -> CellMetrics {
    let font_px = spec.size_pt * scale;
    let cell_h = (font_px * spec.line_height).ceil();
    let mut b = Buffer::new(font_system, Metrics::new(font_px, cell_h));
    b.set_text("M", &Attrs::new().family(Family::Monospace), Shaping::Advanced, None);
    b.shape_until_scroll(font_system, false);
    let cell_w = b.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| g.w)).unwrap_or(font_px * 0.6);
    CellMetrics { font_px, cell_w, cell_h, pad: (spec.padding_pt * scale).round() }
}

/// Huella del texto visible de una línea: carácter, color y estilo. El
/// fondo, subrayados y selección se pintan aparte y no cuentan, así que una
/// línea seleccionada reutiliza su maquetación. Las filas vacías dan `BLANK`.
fn line_key(cells: &[CellView]) -> u64 {
    if cells.iter().all(|c| c.spacer || c.c == ' ' || c.c == '\0') {
        return BLANK;
    }
    let mut h = DefaultHasher::new();
    for c in cells.iter().filter(|c| !c.spacer) {
        c.c.hash(&mut h);
        if c.c != ' ' {
            (c.fg, c.bold, c.italic).hash(&mut h);
        }
    }
    h.finish().max(1)
}

fn label_key(l: &Label) -> u64 {
    let mut h = DefaultHasher::new();
    l.text.hash(&mut h);
    l.px.to_bits().hash(&mut h);
    l.color.hash(&mut h);
    l.bold.hash(&mut h);
    (l.align as u8).hash(&mut h);
    (l.font as u8).hash(&mut h);
    (l.area.w as u32).hash(&mut h);
    h.finish()
}

impl Gfx {
    pub async fn new(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        font: FontSpec,
    ) -> anyhow::Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        )));
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await?;
        let (device, queue) = adapter.request_device(&wgpu::DeviceDescriptor::default()).await?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: if capture::target().is_some() {
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC
            } else {
                wgpu::TextureUsages::RENDER_ATTACHMENT
            },
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);

        let mut font_system = FontSystem::new();
        font_system.db_mut().set_monospace_family(font.family.clone());
        font_system.db_mut().set_sans_serif_family(SANS_FAMILY);
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text = TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let rects = RectRenderer::new(&device, format);
        let overlay_text = TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let overlay_rects = RectRenderer::new(&device, format);
        let metrics = measure(&mut font_system, &font, window.scale_factor() as f32);

        Ok(Self {
            instance,
            device,
            queue,
            surface,
            config,
            font_system,
            swash: SwashCache::new(),
            viewport,
            atlas,
            text,
            rects,
            overlay_text,
            overlay_rects,
            lines: HashMap::new(),
            pane_rows: HashMap::new(),
            frame_no: 0,
            labels: HashMap::new(),
            rich: HashMap::new(),
            metrics,
            font,
            window,
        })
    }

    pub fn scale(&self) -> f32 {
        self.window.scale_factor() as f32
    }

    pub fn resize_surface(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    /// Cambia la fuente (o recalcula tras un cambio de escala).
    pub fn set_font(&mut self, font: FontSpec) {
        if font.family != self.font.family {
            self.font_system.db_mut().set_monospace_family(font.family.clone());
        }
        self.font = font;
        let scale = self.scale();
        self.metrics = measure(&mut self.font_system, &self.font, scale);
        self.lines.clear();
        self.pane_rows.clear();
    }

    pub fn font(&self) -> &FontSpec {
        &self.font
    }

    /// Tamaño del grid que cabe en un área.
    pub fn grid_size(&self, area: Area) -> GridSize {
        let m = self.metrics;
        let w = area.w - 2.0 * m.pad;
        let h = area.h - 2.0 * m.pad;
        GridSize {
            cols: (w / m.cell_w).floor().max(2.0) as u16,
            rows: (h / m.cell_h).floor().max(1.0) as u16,
            cell_width: m.cell_w as u16,
            cell_height: m.cell_h as u16,
        }
    }

    /// Celda (fila, columna, mitad derecha) bajo un punto de un panel.
    pub fn cell_at(
        &self,
        area: Area,
        x: f32,
        y: f32,
        snap_rows: usize,
        snap_cols: usize,
    ) -> (usize, usize, bool) {
        let m = self.metrics;
        let fx = ((x - area.x - m.pad) / m.cell_w).max(0.0);
        let fy = ((y - area.y - m.pad) / m.cell_h).max(0.0);
        let col = (fx as usize).min(snap_cols.saturating_sub(1));
        let row = (fy as usize).min(snap_rows.saturating_sub(1));
        (row, col, fx.fract() >= 0.5)
    }

    /// Prepara el texto de las filas de un panel. Solo se maquetan las líneas
    /// que no estén ya en la caché.
    pub fn prepare_rows(&mut self, pane: PaneId, snap: &Snapshot) {
        let keys: Vec<u64> = (0..snap.rows).map(|r| self.line_buffer(snap.row(r))).collect();
        self.pane_rows.insert(pane, keys);
    }

    /// Clave de la línea; la maqueta si no estaba en caché.
    fn line_buffer(&mut self, cells: &[CellView]) -> u64 {
        let key = line_key(cells);
        if key == BLANK {
            return BLANK;
        }
        let frame = self.frame_no;
        if let Some(l) = self.lines.get_mut(&key) {
            l.last_used = frame;
            return key;
        }
        let m = self.metrics;
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(m.font_px, m.cell_h));
        buffer.set_monospace_width(Some(m.cell_w));
        buffer.set_wrap(Wrap::None);
        buffer.set_size(None, Some(m.cell_h));

        // Agrupa celdas consecutivas con el mismo estilo en un span.
        let mut spans: Vec<(String, Attrs<'static>)> = Vec::new();
        let base = Attrs::new().family(Family::Monospace);
        let mut ascii = true;
        for c in cells.iter().filter(|c| !c.spacer) {
            let ch = if c.c == '\0' { ' ' } else { c.c };
            ascii &= ch.is_ascii();
            let mut a = base.clone().color(Color::rgb(c.fg[0], c.fg[1], c.fg[2]));
            if c.bold {
                a = a.weight(Weight::BOLD);
            }
            if c.italic {
                a = a.style(Style::Italic);
            }
            match spans.last_mut() {
                // Los espacios se unen al span anterior: su color no se ve.
                Some((s, prev)) if prev == &a || ch == ' ' => s.push(ch),
                _ => spans.push((ch.to_string(), a)),
            }
        }
        // El texto ASCII no necesita el motor de maquetación complejo
        // (HarfBuzz): la maquetación básica es mucho más barata.
        let shaping = if ascii { Shaping::Basic } else { Shaping::Advanced };
        buffer.set_rich_text(spans.iter().map(|(s, a)| (s.as_str(), a.clone())), &base, shaping, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        self.lines.insert(key, LineBuf { buffer, last_used: frame });
        key
    }

    /// Maqueta un texto al ancho `w`. Devuelve su clave y su alto.
    pub fn rich(&mut self, r: &Rich, w: f32) -> (u64, f32) {
        let mut h = DefaultHasher::new();
        (r.px.to_bits(), r.line.to_bits(), w.to_bits()).hash(&mut h);
        for s in &r.spans {
            (&s.text, s.color, s.bold, s.italic, s.mono).hash(&mut h);
        }
        let key = h.finish();
        let frame = self.frame_no;
        if let Some(b) = self.rich.get_mut(&key) {
            b.last_used = frame;
            return (key, b.height);
        }
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(r.px, r.line));
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_size(Some(w.max(1.0)), None);
        let base = Attrs::new().family(Family::SansSerif);
        let spans = r.spans.iter().map(|s| {
            let mut a = base.clone().color(Color::rgb(s.color[0], s.color[1], s.color[2]));
            if s.mono {
                a = a.family(Family::Monospace).metrics(Metrics::new(r.px * 0.89, r.line));
            }
            if s.bold {
                a = a.weight(Weight::SEMIBOLD);
            }
            if s.italic {
                a = a.style(Style::Italic);
            }
            (s.text.as_str(), a)
        });
        buffer.set_rich_text(spans, &base, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        let height = buffer.layout_runs().map(|r| r.line_height).sum::<f32>().max(r.line);
        self.rich.insert(key, RichBuf { buffer, height, last_used: frame });
        (key, height)
    }

    /// Ancho de la línea más larga de un texto ya maquetado.
    pub fn rich_width(&self, key: u64) -> f32 {
        self.rich.get(&key).map_or(0.0, |b| b.buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max))
    }

    /// Posición del texto bajo un punto (relativo a su esquina).
    pub fn rich_hit(&self, key: u64, x: f32, y: f32) -> Option<TextPos> {
        let b = self.rich.get(&key)?;
        let y = y.clamp(0.0, (b.height - 1.0).max(0.0));
        b.buffer.hit(x, y).map(|c| (c.line, c.index))
    }

    /// Esquina superior izquierda del cursor en una posición.
    pub fn rich_caret(&self, key: u64, pos: TextPos) -> Option<(f32, f32)> {
        let b = self.rich.get(&key)?;
        let mut best = None;
        for run in b.buffer.layout_runs().filter(|r| r.line_i == pos.0) {
            let start = run.glyphs.first().map_or(0, |g| g.start);
            let end = run.glyphs.last().map_or(0, |g| g.end);
            if pos.1 >= start || best.is_none() {
                let x = run
                    .glyphs
                    .iter()
                    .find(|g| g.start >= pos.1)
                    .map_or(if pos.1 >= end { run.line_w } else { 0.0 }, |g| g.x);
                best = Some((x, run.line_top));
            }
        }
        best
    }

    /// Cajas (relativas) del texto entre dos posiciones.
    pub fn rich_boxes(&self, key: u64, a: TextPos, b: TextPos) -> Vec<Area> {
        let Some(buf) = self.rich.get(&key) else { return Vec::new() };
        let cur = |p: TextPos| Cursor::new(p.0, p.1);
        let mut out = Vec::new();
        for run in buf.buffer.layout_runs().filter(|r| r.line_i >= a.0 && r.line_i <= b.0) {
            for (x, w) in run.highlight(cur(a), cur(b)) {
                if w > 0.0 {
                    out.push(Area::new(x, run.line_top, w, run.line_height));
                }
            }
        }
        out
    }

    fn label_buffer(&mut self, l: &Label) -> u64 {
        let key = label_key(l);
        let fs = &mut self.font_system;
        let entry = self.labels.entry(key).or_insert_with(|| {
            let mut buffer = Buffer::new(fs, Metrics::new(l.px, l.area.h));
            buffer.set_size(Some(l.area.w), Some(l.area.h));
            buffer.set_wrap(Wrap::None);
            buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
            let family = match l.font {
                Font::Sans => Family::SansSerif,
                Font::Mono => Family::Monospace,
            };
            let mut attrs = Attrs::new().family(family).color(Color::rgb(l.color[0], l.color[1], l.color[2]));
            if l.bold {
                attrs = attrs.weight(Weight::SEMIBOLD);
            }
            let align = match l.align {
                Align::Left => TextAlign::Left,
                Align::Right => TextAlign::Right,
                Align::Center => TextAlign::Center,
            };
            buffer.set_text(&l.text, &attrs, Shaping::Advanced, Some(align));
            buffer.shape_until_scroll(fs, false);
            LabelBuf { buffer, alive: false }
        });
        entry.alive = true;
        key
    }

    /// Pinta y presenta un frame. Devuelve `true` si se guardó una captura.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        rects: &[Rect],
        panes: &[PaneText],
        labels: &[Label],
        rich: &[RichDraw],
        overlay_rects: &[Rect],
        overlay_labels: &[Label],
        foreground: Rgb8,
        clear: Rgb8,
        capture_to: Option<&Path>,
    ) -> bool {
        // Paneles que ya no se pintan y líneas que llevan tiempo sin usarse.
        self.pane_rows.retain(|id, _| panes.iter().any(|p| p.pane == *id));
        let frame = self.frame_no;
        self.lines.retain(|_, l| l.last_used + LINE_TTL_FRAMES >= frame);
        self.rich.retain(|_, b| b.last_used + 2 >= frame);
        self.frame_no += 1;
        for l in self.labels.values_mut() {
            l.alive = false;
        }
        let keys: Vec<u64> = labels.iter().map(|l| self.label_buffer(l)).collect();
        let overlay_keys: Vec<u64> = overlay_labels.iter().map(|l| self.label_buffer(l)).collect();
        self.labels.retain(|_, l| l.alive);

        let m = self.metrics;
        let default_color = Color::rgb(foreground[0], foreground[1], foreground[2]);
        let bounds = |a: Area| TextBounds {
            left: a.x as i32,
            top: a.y as i32,
            right: a.right() as i32,
            bottom: a.bottom() as i32,
        };
        let mut areas: Vec<TextArea> = Vec::new();
        for p in panes {
            if let Some(rows) = self.pane_rows.get(&p.pane) {
                areas.extend(rows.iter().enumerate().filter(|(_, k)| **k != BLANK).filter_map(|(r, k)| {
                    Some(TextArea {
                        buffer: &self.lines.get(k)?.buffer,
                        left: p.ox,
                        top: p.oy + r as f32 * m.cell_h,
                        scale: 1.0,
                        bounds: bounds(p.clip),
                        default_color,
                        custom_glyphs: &[],
                    })
                }));
            }
        }
        for (l, key) in labels.iter().zip(&keys) {
            areas.push(TextArea {
                buffer: &self.labels[key].buffer,
                left: l.area.x,
                top: l.area.y,
                scale: 1.0,
                bounds: bounds(l.clip.map_or(l.area, |c| c.intersect(l.area))),
                default_color,
                custom_glyphs: &[],
            });
        }
        for r in rich {
            let Some(b) = self.rich.get(&r.key) else { continue };
            areas.push(TextArea {
                buffer: &b.buffer,
                left: r.x,
                top: r.y,
                scale: 1.0,
                bounds: bounds(r.clip),
                default_color,
                custom_glyphs: &[],
            });
        }

        let overlay_areas: Vec<TextArea> = overlay_labels
            .iter()
            .zip(&overlay_keys)
            .map(|(l, key)| TextArea {
                buffer: &self.labels[key].buffer,
                left: l.area.x,
                top: l.area.y,
                scale: 1.0,
                bounds: bounds(l.clip.map_or(l.area, |c| c.intersect(l.area))),
                default_color,
                custom_glyphs: &[],
            })
            .collect();

        self.viewport
            .update(&self.queue, Resolution { width: self.config.width, height: self.config.height });
        for (renderer, list) in [(&mut self.text, areas), (&mut self.overlay_text, overlay_areas)] {
            if let Err(e) = renderer.prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                list,
                &mut self.swash,
            ) {
                log::warn!("prepare texto: {e}");
            }
        }
        let screen = [self.config.width as f32, self.config.height as f32];
        self.rects.prepare(&self.device, &self.queue, screen, rects);
        self.overlay_rects.prepare(&self.device, &self.queue, screen, overlay_rects);

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            // Sin pedir otro redibujado: hacerlo aquí crea un bucle al 100% de CPU
            // mientras la ventana está oculta. El siguiente evento volverá a pintar.
            wgpu::CurrentSurfaceTexture::Occluded | wgpu::CurrentSurfaceTexture::Timeout => return false,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return false;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                if let Ok(s) = self.instance.create_surface(self.window.clone()) {
                    self.surface = s;
                    self.surface.configure(&self.device, &self.config);
                }
                self.window.request_redraw();
                return false;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                log::error!("error de validación de wgpu");
                return false;
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let bg = linear(clear, 1.0);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: bg[0] as f64,
                            g: bg[1] as f64,
                            b: bg[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.rects.render(&mut pass);
            if let Err(e) = self.text.render(&self.atlas, &self.viewport, &mut pass) {
                log::warn!("render texto: {e}");
            }
            self.overlay_rects.render(&mut pass);
            if let Err(e) = self.overlay_text.render(&self.atlas, &self.viewport, &mut pass) {
                log::warn!("render texto: {e}");
            }
        }
        self.queue.submit(Some(encoder.finish()));
        let mut captured = false;
        if let Some(path) = capture_to {
            match capture::save(&self.device, &self.queue, &frame.texture, path) {
                Ok(()) => {
                    eprintln!("captura guardada en {}", path.display());
                    captured = true;
                }
                Err(e) => eprintln!("error al capturar: {e}"),
            }
        }
        self.queue.present(frame);
        self.atlas.trim();
        captured
    }
}

/// Cómo pintar el cursor de un panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Hollow,
}

fn blend(a: Rgb8, b: Rgb8, t: f32) -> Rgb8 {
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

/// Ajusta el snapshot para pintarlo: cursor de bloque y atenuado de los
/// paneles sin foco.
pub fn style_snapshot(snap: &mut Snapshot, theme: &Theme, cursor: CursorStyle, dim: bool) {
    if dim {
        for c in &mut snap.cells {
            c.fg = blend(c.fg, theme.background, 0.35);
        }
    }
    let cur = snap.cursor;
    if cursor == CursorStyle::Block && cur.visible && cur.row < snap.rows && cur.col < snap.cols {
        let i = cur.row * snap.cols + cur.col;
        snap.cells[i].bg = theme.cursor;
        snap.cells[i].fg = theme.background;
        if snap.cells[i].wide && cur.col + 1 < snap.cols {
            snap.cells[i + 1].bg = theme.cursor;
        }
    }
}

/// Rectángulos de un panel: fondos, subrayados y cursor hueco.
pub fn terminal_rects(
    snap: &Snapshot,
    theme: &Theme,
    m: &CellMetrics,
    ox: f32,
    oy: f32,
    cursor: CursorStyle,
) -> Vec<Rect> {
    let mut rects = Vec::with_capacity(256);
    let thick = (m.font_px / 14.0).max(1.0);
    for r in 0..snap.rows {
        let row = snap.row(r);
        let y = oy + r as f32 * m.cell_h;
        let mut c = 0;
        while c < snap.cols {
            let bg = row[c].bg;
            let start = c;
            while c < snap.cols && row[c].bg == bg {
                c += 1;
            }
            if bg != snap.background {
                rects.push(Rect {
                    pos: [ox + start as f32 * m.cell_w, y],
                    size: [(c - start) as f32 * m.cell_w, m.cell_h],
                    color: linear(bg, 1.0),
                    ..Default::default()
                });
            }
        }
        for (c, cell) in row.iter().enumerate() {
            let x = ox + c as f32 * m.cell_w;
            if cell.underline {
                rects.push(Rect {
                    pos: [x, y + m.cell_h - 2.0 * thick],
                    size: [m.cell_w, thick],
                    color: linear(cell.fg, 1.0),
                    ..Default::default()
                });
            }
            if cell.strikeout {
                rects.push(Rect {
                    pos: [x, y + m.cell_h * 0.55],
                    size: [m.cell_w, thick],
                    color: linear(cell.fg, 1.0),
                    ..Default::default()
                });
            }
        }
    }
    let cur = snap.cursor;
    if cursor == CursorStyle::Hollow && cur.visible {
        let x = ox + cur.col as f32 * m.cell_w;
        let y = oy + cur.row as f32 * m.cell_h;
        let col = linear(theme.cursor, 1.0);
        rects.extend([
            Rect { pos: [x, y], size: [m.cell_w, thick], color: col, ..Default::default() },
            Rect {
                pos: [x, y + m.cell_h - thick],
                size: [m.cell_w, thick],
                color: col,
                ..Default::default()
            },
            Rect { pos: [x, y], size: [thick, m.cell_h], color: col, ..Default::default() },
            Rect {
                pos: [x + m.cell_w - thick, y],
                size: [thick, m.cell_h],
                color: col,
                ..Default::default()
            },
        ]);
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(c: char, fg: Rgb8, bg: Rgb8) -> CellView {
        CellView {
            c,
            fg,
            bg,
            bold: false,
            italic: false,
            underline: false,
            strikeout: false,
            wide: false,
            spacer: false,
        }
    }

    fn line(text: &str, fg: Rgb8, bg: Rgb8) -> Vec<CellView> {
        text.chars().map(|c| cell(c, fg, bg)).collect()
    }

    #[test]
    fn blank_rows_are_skipped() {
        assert_eq!(line_key(&line("      ", [1, 1, 1], [0, 0, 0])), BLANK);
        assert_ne!(line_key(&line("  x   ", [1, 1, 1], [0, 0, 0])), BLANK);
    }

    #[test]
    fn key_ignores_background_and_space_color() {
        // Misma línea seleccionada (otro fondo): misma maquetación.
        assert_eq!(
            line_key(&line("hola", [9, 9, 9], [0, 0, 0])),
            line_key(&line("hola", [9, 9, 9], [50, 50, 50]))
        );
        // El color de los espacios no cuenta.
        let mut a = line("a b", [9, 9, 9], [0, 0, 0]);
        let b = a.clone();
        a[1].fg = [200, 0, 0];
        assert_eq!(line_key(&a), line_key(&b));
        // El color del texto sí.
        assert_ne!(
            line_key(&line("hola", [9, 9, 9], [0, 0, 0])),
            line_key(&line("hola", [8, 8, 8], [0, 0, 0]))
        );
    }
}
