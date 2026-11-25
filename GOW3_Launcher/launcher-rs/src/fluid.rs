//! Tiny stable-fluids simulation used as the launcher's bottom backdrop.
//!
//! The grid is deliberately small (160x40) and every buffer is allocated once,
//! so a frame costs one advection pass, a handful of Jacobi iterations and a
//! bilinear resample into a premultiplied RGBA image. That keeps the whole
//! update cheap enough to run next to the software renderer, while the smooth
//! interpolation hides the low resolution.

const NX: usize = 160;
const NY: usize = 40;
pub const OUT_W: usize = 480;
pub const OUT_H: usize = 120;

const CELLS: usize = NX * NY;
const PRESSURE_ITERATIONS: usize = 10;

/// (horizontal position, phase, strength)
const EMITTERS: [(f32, f32, f32); 3] = [(0.27, 0.0, 1.0), (0.5, 2.1, 0.9), (0.73, 4.2, 1.0)];

pub struct Fluid {
    u: Vec<f32>,
    v: Vec<f32>,
    u_prev: Vec<f32>,
    v_prev: Vec<f32>,
    density: Vec<f32>,
    density_prev: Vec<f32>,
    pressure: Vec<f32>,
    divergence: Vec<f32>,
    time: f32,
    columns: Vec<(usize, usize, f32)>,
    rows: Vec<(usize, usize, f32)>,
    fade: Vec<f32>,
    pixels: Vec<u8>,
}

impl Fluid {
    pub fn new() -> Self {
        let mut fluid = Self {
            u: vec![0.0; CELLS],
            v: vec![0.0; CELLS],
            u_prev: vec![0.0; CELLS],
            v_prev: vec![0.0; CELLS],
            density: vec![0.0; CELLS],
            density_prev: vec![0.0; CELLS],
            pressure: vec![0.0; CELLS],
            divergence: vec![0.0; CELLS],
            time: 0.0,
            columns: Vec::with_capacity(OUT_W),
            rows: Vec::with_capacity(OUT_H),
            fade: Vec::with_capacity(OUT_H),
            pixels: vec![0; OUT_W * OUT_H * 4],
        };
        for x in 0..OUT_W {
            fluid.columns.push(sample_weights(x, OUT_W, NX));
        }
        for y in 0..OUT_H {
            fluid.rows.push(sample_weights(y, OUT_H, NY));
            // The top of the band melts into the page background; the bottom
            // keeps full strength so the glow reads as coming from the base.
            let t = ((y as f32 + 0.5) / OUT_H as f32 / 0.55).clamp(0.0, 1.0);
            fluid.fade.push(t * t * (3.0 - 2.0 * t));
        }
        // Seed a few puffs so the first frame is already alive.
        for (position, _, strength) in EMITTERS {
            fluid.seed(position * NX as f32, NY as f32 - 7.0, strength);
        }
        fluid
    }

    fn seed(&mut self, cx: f32, cy: f32, strength: f32) {
        let rx = NX as f32 * 0.09;
        let ry = NY as f32 * 0.3;
        for j in 1..NY - 1 {
            for i in 1..NX - 1 {
                let gx = (i as f32 - cx) / rx;
                let gy = (j as f32 - cy) / ry;
                let g = (-(gx * gx + gy * gy)).exp();
                self.density[j * NX + i] += g * strength * 2.2;
            }
        }
    }

    pub fn step(&mut self, dt: f32) {
        self.time += dt;
        self.u_prev.copy_from_slice(&self.u);
        self.v_prev.copy_from_slice(&self.v);
        self.density_prev.copy_from_slice(&self.density);

        self.emit(dt);
        self.advect_velocity(dt);
        self.advect_density(dt);
        self.project();
        self.dissipate();
        self.boundaries();
    }

    fn emit(&mut self, dt: f32) {
        let time = self.time;
        for (position, phase, strength) in EMITTERS {
            let sway = (time * 0.55 + phase).sin();
            let cx = (position + sway * 0.07) * NX as f32;
            let cy = NY as f32 - 4.0 + (time * 0.8 + phase * 1.7).sin() * 1.1;
            let rx = NX as f32 * 0.1;
            let ry = NY as f32 * 0.32;
            let i_start = ((cx - 3.0 * rx).max(1.0)) as usize;
            let i_end = ((cx + 3.0 * rx).min(NX as f32 - 1.0)) as usize;
            let j_start = ((cy - 3.0 * ry).max(1.0)) as usize;
            let j_end = ((cy + 3.0 * ry).min(NY as f32 - 1.0)) as usize;
            let drift = (time * 0.6 + phase).cos() * 7.0;
            for j in j_start..=j_end {
                for i in i_start..=i_end {
                    let gx = (i as f32 - cx) / rx;
                    let gy = (j as f32 - cy) / ry;
                    let g = (-(gx * gx + gy * gy)).exp();
                    let k = j * NX + i;
                    // Sources go into the previous frame: advection then carries
                    // them forward instead of overwriting them this same tick.
                    self.density_prev[k] += g * strength * 1.6 * dt;
                    self.v_prev[k] -= g * 9.0 * dt;
                    // Spread sideways away from the plume core, with a slow
                    // common drift so the shapes curl instead of pulsing.
                    self.u_prev[k] += g * (gx * 9.0 + drift * 0.4) * dt;
                }
            }
        }
    }

    fn advect_velocity(&mut self, dt: f32) {
        for j in 0..NY {
            for i in 0..NX {
                let k = j * NX + i;
                let x = (i as f32 - dt * self.u_prev[k]).clamp(0.0, (NX - 1) as f32);
                let y = (j as f32 - dt * self.v_prev[k]).clamp(0.0, (NY - 1) as f32);
                self.u[k] = bilinear(&self.u_prev, x, y);
                self.v[k] = bilinear(&self.v_prev, x, y);
            }
        }
    }

    fn advect_density(&mut self, dt: f32) {
        for j in 0..NY {
            for i in 0..NX {
                let k = j * NX + i;
                let x = (i as f32 - dt * self.u[k]).clamp(0.0, (NX - 1) as f32);
                let y = (j as f32 - dt * self.v[k]).clamp(0.0, (NY - 1) as f32);
                self.density[k] = bilinear(&self.density_prev, x, y);
            }
        }
    }

    fn project(&mut self) {
        for j in 1..NY - 1 {
            for i in 1..NX - 1 {
                let k = j * NX + i;
                self.divergence[k] = -0.5
                    * (self.u[k + 1] - self.u[k - 1] + self.v[k + NX] - self.v[k - NX]);
                self.pressure[k] = 0.0;
            }
        }
        for _ in 0..PRESSURE_ITERATIONS {
            for j in 1..NY - 1 {
                for i in 1..NX - 1 {
                    let k = j * NX + i;
                    self.pressure[k] = (self.divergence[k]
                        + self.pressure[k - 1]
                        + self.pressure[k + 1]
                        + self.pressure[k - NX]
                        + self.pressure[k + NX])
                        * 0.25;
                }
            }
        }
        for j in 1..NY - 1 {
            for i in 1..NX - 1 {
                let k = j * NX + i;
                self.u[k] -= 0.5 * (self.pressure[k + 1] - self.pressure[k - 1]);
                self.v[k] -= 0.5 * (self.pressure[k + NX] - self.pressure[k - NX]);
            }
        }
    }

    fn dissipate(&mut self) {
        for k in 0..CELLS {
            // Slight friction and fade keep the band calm and never saturating.
            self.u[k] = (self.u[k] * 0.985).clamp(-60.0, 60.0);
            self.v[k] = (self.v[k] * 0.985).clamp(-60.0, 60.0);
            self.density[k] *= 0.995;
        }
    }

    fn boundaries(&mut self) {
        for i in 0..NX {
            let top = i;
            let bottom = (NY - 1) * NX + i;
            self.u[top] = 0.0;
            self.v[top] = 0.0;
            self.density[top] = 0.0;
            self.u[bottom] = 0.0;
            self.v[bottom] = 0.0;
            self.density[bottom] = 0.0;
        }
        for j in 0..NY {
            let left = j * NX;
            let right = j * NX + NX - 1;
            self.u[left] = 0.0;
            self.v[left] = 0.0;
            self.density[left] = 0.0;
            self.u[right] = 0.0;
            self.v[right] = 0.0;
            self.density[right] = 0.0;
        }
    }

    /// Premultiplied RGBA, ready for `Image::from_rgba8_premultiplied`.
    pub fn render(&mut self) -> &[u8] {
        for (y, &(y0, y1, wy)) in self.rows.iter().enumerate() {
            let fade = self.fade[y];
            let row = y0 * NX;
            let next_row = y1 * NX;
            let mut out = y * OUT_W * 4;
            for &(x0, x1, wx) in &self.columns {
                let top = self.density[row + x0] * (1.0 - wx) + self.density[row + x1] * wx;
                let bottom =
                    self.density[next_row + x0] * (1.0 - wx) + self.density[next_row + x1] * wx;
                let density = top * (1.0 - wy) + bottom * wy;
                let t = (density * 0.36).clamp(0.0, 1.0) * fade;
                let alpha = t * t * 0.8;
                let (r, g, b) = if t < 0.55 {
                    mix((46, 7, 16), (197, 41, 73), t / 0.55)
                } else {
                    mix((197, 41, 73), (255, 150, 140), (t - 0.55) / 0.45)
                };
                let a = (alpha * 255.0) as u32;
                let pixel = &mut self.pixels[out..out + 4];
                pixel[0] = ((r as u32 * a) / 255) as u8;
                pixel[1] = ((g as u32 * a) / 255) as u8;
                pixel[2] = ((b as u32 * a) / 255) as u8;
                pixel[3] = a as u8;
                out += 4;
            }
        }
        &self.pixels
    }
}

/// Maps one output pixel to its two source neighbours and blend weight.
fn sample_weights(index: usize, out_len: usize, source_len: usize) -> (usize, usize, f32) {
    let source = (index as f32 + 0.5) * source_len as f32 / out_len as f32 - 0.5;
    let source = source.clamp(0.0, (source_len - 1) as f32);
    let low = source.floor() as usize;
    let high = (low + 1).min(source_len - 1);
    (low, high, source - low as f32)
}

#[inline]
fn bilinear(field: &[f32], x: f32, y: f32) -> f32 {
    let x0 = x as usize;
    let y0 = y as usize;
    let x1 = (x0 + 1).min(NX - 1);
    let y1 = (y0 + 1).min(NY - 1);
    let wx = x - x0 as f32;
    let wy = y - y0 as f32;
    let top = field[y0 * NX + x0] * (1.0 - wx) + field[y0 * NX + x1] * wx;
    let bottom = field[y1 * NX + x0] * (1.0 - wx) + field[y1 * NX + x1] * wx;
    top * (1.0 - wy) + bottom * wy
}

#[inline]
fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    (
        (a.0 as f32 + (b.0 as f32 - a.0 as f32) * t) as u8,
        (a.1 as f32 + (b.1 as f32 - a.1 as f32) * t) as u8,
        (a.2 as f32 + (b.2 as f32 - a.2 as f32) * t) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulation_stays_finite_and_bounded() {
        let mut fluid = Fluid::new();
        for _ in 0..240 {
            fluid.step(0.04);
        }
        for &value in fluid.u.iter().chain(fluid.v.iter()).chain(fluid.density.iter()) {
            assert!(value.is_finite(), "fluid field produced a non-finite value");
        }
        assert!(fluid.density.iter().all(|value| *value >= 0.0 && *value < 40.0));
        assert!(fluid.u.iter().all(|value| value.abs() <= 60.0));
        let pixels = fluid.render();
        assert_eq!(pixels.len(), OUT_W * OUT_H * 4);
        assert!(pixels.chunks(4).any(|pixel| pixel[3] > 40), "the band must be visible");
        assert_eq!(&pixels[..4], &[0, 0, 0, 0], "the top row must blend out");
    }
}
