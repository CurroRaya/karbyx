//! Herramienta de desarrollo: vuelca un frame renderizado a PNG.
//!
//! `FORGE_SCREENSHOT=/ruta/salida.png forge-app` guarda el frame cuando la
//! shell lleva un rato pintando y cierra la app. Útil para verificar el render
//! sin permisos de captura de pantalla y para futuros tests visuales.

use std::path::{Path, PathBuf};

pub fn target() -> Option<PathBuf> {
    std::env::var_os("FORGE_SCREENSHOT").map(PathBuf::from)
}

/// Espera antes de capturar (`FORGE_CAPTURE_DELAY` en ms; 4 s por defecto).
pub fn delay() -> std::time::Duration {
    let ms = std::env::var("FORGE_CAPTURE_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(4000);
    std::time::Duration::from_millis(ms)
}

/// Copia `texture` a memoria y la escribe como PNG RGBA.
pub fn save(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    path: &Path,
) -> anyhow::Result<()> {
    let (w, h) = (texture.width(), texture.height());
    let unpadded = w * 4;
    let padded = unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("capture"),
        size: (padded * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit(Some(enc.finish()));
    buffer.map_async(wgpu::MapMode::Read, .., |_| {});
    device.poll(wgpu::PollType::wait_indefinitely())?;

    let bgra =
        matches!(texture.format(), wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
    let data = buffer.slice(..).get_mapped_range()?;
    let mut rgba = Vec::with_capacity((unpadded * h) as usize);
    for row in data.chunks(padded as usize) {
        for px in row[..unpadded as usize].chunks(4) {
            if bgra {
                rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
            } else {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
    }
    drop(data);
    buffer.unmap();

    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgba)?;
    Ok(())
}
