// Ported from anomalyco/hex (MIT License, Copyright (c) 2026 Christopher Langton).
// See THIRD_PARTY_NOTICES.md. Original: src/rune_hud.wgsl (`dot_scope_fragment`),
// evaluated here on the CPU for every pixel, and the uniform set-up of src/rune_hud.rs.
//! The Dot scope fragment shader on the CPU: plate, rim, drop shadow, glow
//! and the dot grid, as premultiplied BGRA for `wl_shm`.

use super::dot_scope::{Pose, palette};
use super::indicator::Indicator;

type Vec2 = [f32; 2];

fn sstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn rounded_box(point: Vec2, half_size: Vec2, radius: f32) -> f32 {
    let q = [point[0].abs() - half_size[0] + radius, point[1].abs() - half_size[1] + radius];
    let outside = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2)).sqrt();
    outside + q[0].max(q[1]).min(0.0) - radius
}

fn coverage(distance: f32, softness: f32) -> f32 {
    sstep(softness, -softness, distance)
}

fn composite(dest: &mut [f32; 4], color: [f32; 3], alpha: f32) {
    let alpha = alpha.clamp(0.0, 1.0);
    for c in 0..3 {
        dest[c] = color[c] * alpha + dest[c] * (1.0 - alpha);
    }
    dest[3] = alpha + dest[3] * (1.0 - alpha);
}

fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
}

fn dither(result: &mut [f32; 4], x: u32, y: u32) {
    if result[3] > 0.0 {
        let hash = |px: f32, py: f32| {
            let v = (px * 0.06711056 + py * 0.00583715).fract();
            (52.9829189 * v).fract()
        };
        let (px, py) = (x as f32, y as f32);
        let n1 = hash(px, py);
        let n2 = hash(px + 17.0, py + 31.0);
        let d = (n1 - n2) / 255.0 * result[3];
        let alpha = result[3];
        for c in result.iter_mut().take(3) {
            *c = (*c + d).clamp(0.0, alpha);
        }
    }
}

/// Renders the HUD into `out` (BGRA, premultiplied; `width` x `height`
/// device pixels, `backing_scale` device pixels per point). The plate sits
/// in the middle of the window.
pub fn render(model: &Indicator, width: u32, height: u32, backing_scale: f32, out: &mut [u8]) {
    let pose: Pose = model.pose();
    let tuning = model.tuning();
    let colors = palette(true, &pose, tuning.saturation);
    let (columns, rows) = (model.columns() as i32, model.rows() as i32);
    let dots = model.dots();
    let resolution = [width as f32, height as f32];

    let scale = pose.scale.max(0.001);
    let pixel = 1.0 / (backing_scale * scale);
    let softness = pose.softness.max(0.0);
    let aa = pixel * 0.75 + softness * 0.72;
    let half_size = [pose.width * 0.5, pose.height * 0.5];
    let radius = (pose.height * 0.5).clamp(0.0, pose.height * 0.5);
    let pitch = tuning.spacing;
    let center_offset = [(columns as f32 - 1.0) * 0.5, (rows as f32 - 1.0) * 0.5];
    let (unlit, full, curve) = (0.07, 0.96, 0.9);
    let presence = 0.5 + 0.5 * pose.live.max(pose.cool);
    let base = colors.base;
    let core_color = colors.core;
    let glow_strength = 0.3 * 0.55 * pose.glow.min(1.0);
    let glow_reach = 34.0 + 20.0 * pose.glow;
    let middle = mix3(base, core_color, 0.5);

    for py in 0..height {
        for px in 0..width {
            let position = [px as f32 + 0.5, py as f32 + 0.5];
            let window_point = [
                (position[0] - resolution[0] * 0.5) / backing_scale,
                (position[1] - resolution[1] * 0.5) / backing_scale,
            ];
            let point = [window_point[0] / scale, window_point[1] / scale];
            let distance = rounded_box(point, half_size, radius);
            let mut result = [0.0f32; 4];

            // A soft drop shadow 3 pt below.
            let shadow_distance = rounded_box([point[0], point[1] - 3.0], half_size, radius);
            composite(
                &mut result,
                [colors.shadow[0], colors.shadow[1], colors.shadow[2]],
                colors.shadow[3] * (0.5 - 0.5 * (shadow_distance / 6.5).tanh()),
            );

            let shape = coverage(distance, aa);
            composite(&mut result, [colors.plate[0], colors.plate[1], colors.plate[2]], colors.plate[3] * shape);
            composite(
                &mut result,
                [colors.rim[0], colors.rim[1], colors.rim[2]],
                colors.rim[3] * coverage(distance.abs() - 0.3, aa),
            );

            if pose.glow > 0.01 {
                let t = ((point[0] * point[0] + point[1] * point[1]).sqrt() / glow_reach).clamp(0.0, 1.0);
                composite(&mut result, mix3(middle, base, t), glow_strength * (1.0 - t) * shape);
            }

            if shape > 0.0 {
                let i0 = (point[0] / pitch + center_offset[0]).round() as i32;
                let j0 = (point[1] / pitch + center_offset[1]).round() as i32;
                for dj in -1..=1 {
                    for di in -1..=1 {
                        let (i, j) = (i0 + di, j0 + dj);
                        if i < 0 || j < 0 || i >= columns || j >= rows {
                            continue;
                        }
                        let center = [
                            (i as f32 - center_offset[0]) * pitch,
                            (j as f32 - center_offset[1]) * pitch,
                        ];
                        let cap = -rounded_box(center, half_size, radius);
                        let mask = sstep(1.4, 4.6, cap);
                        if mask <= 0.0 {
                            continue;
                        }
                        let [value, fresh] = dots[(j * columns + i) as usize];
                        let power = value.max(1e-5);
                        let row = center[1].abs() / half_size[1];
                        let floor_alpha = unlit * (1.0 - 0.7 * sstep(0.35, 0.95, row)) * presence * pose.grid;
                        let alpha = floor_alpha
                            + (full - floor_alpha) * power.powf(curve * (1.4 + (1.0 - 1.4) * fresh));
                        let wobble = 0.82 + (1.0 - 0.82) * fresh;
                        let dot_radius = (0.25 + 0.17 * power.powf(0.6))
                            * (1.0 + (wobble - 1.0) * sstep(0.03, 0.2, power))
                            * pitch;
                        let core = (power * power * 1.1).clamp(0.0, 0.9) * (0.3 + 0.7 * fresh);
                        let color = mix3(base, core_color, core);
                        let from_center = ((point[0] - center[0]).powi(2) + (point[1] - center[1]).powi(2)).sqrt();
                        if power > 0.05 {
                            composite(
                                &mut result,
                                color,
                                0.132 * power * power * mask * shape * coverage(from_center - dot_radius * 2.0, aa),
                            );
                        }
                        composite(&mut result, color, alpha * mask * shape * coverage(from_center - dot_radius, aa));
                    }
                }
            }

            for c in result.iter_mut() {
                *c *= pose.opacity;
            }
            dither(&mut result, px, py);
            let o = ((py * width + px) * 4) as usize;
            out[o] = (result[2] * 255.0 + 0.5) as u8;
            out[o + 1] = (result[1] * 255.0 + 0.5) as u8;
            out[o + 2] = (result[0] * 255.0 + 0.5) as u8;
            out[o + 3] = (result[3] * 255.0 + 0.5) as u8;
        }
    }
}
