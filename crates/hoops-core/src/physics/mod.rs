use crate::model::{AuditGroundTruthRecord, CcdSubSegment, BALL_RADIUS_M, GRAVITY_M_S2, RIM_RADIUS_M};
use crate::wire_schema_v1::*;

pub const MAX_DEPN_SWEEPS: usize = 128;
pub const MAX_OUTER_RELIN: usize = 4;
pub const MAX_DYKSTRA_INNER: usize = 128;
pub const MAX_CCD_TOI_PASSES: usize = 32;
pub const MAX_SURFACE_LOCK_ITERS: usize = 64;
pub const SOR_OMEGA: f64 = 1.65;
pub const EPS_SEP_M: f64 = 1.0e-4;
pub const EPS_CONVERGED_M: f64 = 1.0e-6;

/// Parameterized SI court geometry (§7.1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CourtGeometry {
    pub half_length_m: f64,
    pub half_width_m: f64,
    pub rim_offset_m: f64,
    pub rim_height_m: f64,
    pub three_arc_radius_m: f64,
    pub corner_three_dist_m: f64,
    pub paint_width_m: f64,
    pub paint_length_m: f64,
    pub restricted_area_radius_m: f64,
    pub oob_runoff_m: f64,
}

impl CourtGeometry {
    pub fn from_rules(rules: &CourtRules) -> Self {
        Self {
            half_length_m: rules.length_m * 0.5,
            half_width_m: rules.width_m * 0.5,
            rim_offset_m: rules.rim_offset_m,
            rim_height_m: rules.rim_height_m,
            three_arc_radius_m: rules.three_arc_radius_m,
            corner_three_dist_m: rules.corner_three_dist_m,
            paint_width_m: rules.paint_width_m,
            paint_length_m: rules.paint_length_m,
            restricted_area_radius_m: rules.restricted_area_radius_m,
            oob_runoff_m: rules.oob_runoff_m,
        }
    }

    #[inline]
    pub fn rim_xy(&self, attack_dir: f64) -> (f64, f64) {
        (attack_dir * (self.half_length_m - self.rim_offset_m), 0.0)
    }

    #[inline]
    pub fn dist_to_rim(&self, x_m: f64, y_m: f64, attack_dir: f64) -> f64 {
        let (rx, ry) = self.rim_xy(attack_dir);
        libm::hypot(x_m - rx, y_m - ry)
    }

    #[inline]
    pub fn is_three_pointer(&self, x_m: f64, y_m: f64, attack_dir: f64) -> bool {
        let (rx, ry) = self.rim_xy(attack_dir);
        let dx_from_endline = self.half_length_m - x_m * attack_dir;
        let corner_break_dx = self.rim_offset_m
            + libm::sqrt(
                (self.three_arc_radius_m * self.three_arc_radius_m
                    - self.corner_three_dist_m * self.corner_three_dist_m)
                    .max(0.0),
            );
        if dx_from_endline <= corner_break_dx {
            libm::fabs(y_m) >= self.corner_three_dist_m
        } else {
            libm::hypot(x_m - rx, y_m - ry) >= self.three_arc_radius_m
        }
    }

    #[inline]
    pub fn shot_zone(&self, x_m: f64, y_m: f64, attack_dir: f64) -> u8 {
        if self.is_three_pointer(x_m, y_m, attack_dir) {
            let dx_from_endline = self.half_length_m - x_m * attack_dir;
            if dx_from_endline <= 4.3 && libm::fabs(y_m) >= self.corner_three_dist_m {
                3 // Corner3
            } else {
                4 // AboveBreak3
            }
        } else {
            let d = self.dist_to_rim(x_m, y_m, attack_dir);
            if d <= 2.4384 {
                0 // Rim
            } else if d <= 4.5 {
                1 // ShortMid
            } else {
                2 // LongMid
            }
        }
    }

    #[inline]
    pub fn is_in_paint(&self, x_m: f64, y_m: f64, attack_dir: f64, extend_oob_m: f64) -> bool {
        let endline_x = attack_dir * self.half_length_m;
        let ft_line_x = attack_dir * (self.half_length_m - self.paint_length_m);
        let min_x = endline_x.min(ft_line_x) - if attack_dir < 0.0 { extend_oob_m } else { 0.0 };
        let max_x = endline_x.max(ft_line_x) + if attack_dir > 0.0 { extend_oob_m } else { 0.0 };
        let half_w = self.paint_width_m * 0.5;
        x_m >= min_x && x_m <= max_x && libm::fabs(y_m) <= half_w
    }

    pub fn footprint_intersects_paint(
        &self,
        x_m: f64,
        y_m: f64,
        attack_dir: f64,
        extend_oob_m: f64,
        radius_m: f64,
    ) -> bool {
        let forward = x_m * attack_dir;
        let closest_x = forward.clamp(
            self.half_length_m - self.paint_length_m,
            self.half_length_m + extend_oob_m,
        );
        let half_width = self.paint_width_m * 0.5;
        let closest_y = y_m.clamp(-half_width, half_width);
        let dx = closest_x - forward;
        let dy = closest_y - y_m;
        dx * dx + dy * dy <= radius_m * radius_m
    }

    #[inline]
    pub fn is_in_restricted_area(&self, x_m: f64, y_m: f64, attack_dir: f64) -> bool {
        self.dist_to_rim(x_m, y_m, attack_dir) <= self.restricted_area_radius_m
    }

    #[inline]
    pub fn is_in_ldb(&self, x_m: f64, y_m: f64, attack_dir: f64) -> bool {
        let dx_from_endline = self.half_length_m - x_m * attack_dir;
        dx_from_endline >= 0.0 && dx_from_endline <= self.paint_length_m && libm::fabs(y_m) <= 3.9624
    }

    #[inline]
    pub fn is_out_of_bounds(&self, x_m: f64, y_m: f64) -> bool {
        libm::fabs(x_m) >= self.half_length_m || libm::fabs(y_m) >= self.half_width_m
    }

    /// Strict inbounds AI navigation rectangle inside court boundary lines.
    #[inline]
    pub fn inbounds_nav_bounds(&self, margin_m: f64) -> PlayableBounds {
        PlayableBounds {
            min_x: -self.half_length_m + margin_m,
            max_x: self.half_length_m - margin_m,
            min_y: -self.half_width_m + margin_m,
            max_y: self.half_width_m - margin_m,
        }
    }

    /// Check whether ball passed behind backboard / support stanchion or over top plane (NBA).
    #[inline]
    pub fn is_over_or_behind_backboard(
        &self,
        pos: [f64; 3],
        attack_dir: f64,
        is_nba: bool,
    ) -> bool {
        let board_x = self.half_length_m - self.rim_offset_m + 0.38;
        let axial = pos[0] * attack_dir;
        (axial > board_x + 0.22 && libm::fabs(pos[1]) <= 1.15 && pos[2] >= 2.2)
            || (is_nba
                && axial > board_x + 0.05
                && pos[2] > 3.97
                && libm::fabs(pos[1]) <= 0.96)
    }

    #[inline]
    pub fn playable_bounds(&self, r_i: f64) -> PlayableBounds {
        PlayableBounds {
            min_x: -self.half_length_m - self.oob_runoff_m + r_i,
            max_x: self.half_length_m + self.oob_runoff_m - r_i,
            min_y: -self.half_width_m - self.oob_runoff_m + r_i,
            max_y: self.half_width_m + self.oob_runoff_m - r_i,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayableBounds {
    pub min_x: f64,
    pub max_x: f64,
    pub min_y: f64,
    pub max_y: f64,
}

impl PlayableBounds {
    #[inline]
    pub fn clamp(&self, x: f64, y: f64) -> (f64, f64) {
        (x.clamp(self.min_x, self.max_x), y.clamp(self.min_y, self.max_y))
    }

    #[inline]
    pub fn violation(&self, x: f64, y: f64) -> f64 {
        let vx = (self.min_x - x).max(x - self.max_x).max(0.0);
        let vy = (self.min_y - y).max(y - self.max_y).max(0.0);
        vx.max(vy)
    }

    #[inline]
    pub fn clamp_outward_velocity(&self, x: f64, y: f64, vx: &mut f64, vy: &mut f64, tol: f64) {
        if x <= self.min_x + tol && *vx < 0.0 {
            *vx = 0.0;
        }
        if x >= self.max_x - tol && *vx > 0.0 {
            *vx = 0.0;
        }
        if y <= self.min_y + tol && *vy < 0.0 {
            *vy = 0.0;
        }
        if y >= self.max_y - tol && *vy > 0.0 {
            *vy = 0.0;
        }
    }

    /// Analytical earliest non-negative TOI with any of the 4 playable walls.
    #[inline]
    pub fn earliest_wall_toi(&self, x: f64, y: f64, vx: f64, vy: f64) -> f64 {
        let mut best = f64::INFINITY;
        if vx < 0.0 {
            let t = if x <= self.min_x {
                0.0
            } else {
                (self.min_x - x) / vx
            };
            if t >= 0.0 && t < best {
                best = t;
            }
        } else if vx > 0.0 {
            let t = if x >= self.max_x {
                0.0
            } else {
                (self.max_x - x) / vx
            };
            if t >= 0.0 && t < best {
                best = t;
            }
        }
        if vy < 0.0 {
            let t = if y <= self.min_y {
                0.0
            } else {
                (self.min_y - y) / vy
            };
            if t >= 0.0 && t < best {
                best = t;
            }
        } else if vy > 0.0 {
            let t = if y >= self.max_y {
                0.0
            } else {
                (self.max_y - y) / vy
            };
            if t >= 0.0 && t < best {
                best = t;
            }
        }
        best
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CcdActor {
    pub slot: usize,
    pub canonical_actor_id: u8,
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
    pub radius: f64,
    pub inv_mass: f64,
    pub bounds: PlayableBounds,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CcdDebugHooks {
    pub force_dykstra_early_exit: bool,
}

fn deterministic_coincident_normal(cid_a: u8, cid_b: u8) -> (f64, f64) {
    let cmin = cid_a.min(cid_b) as usize;
    let cmax = cid_a.max(cid_b) as usize;
    let bucket = (cmin * 31 + cmax) % 256;
    let theta = 2.0 * core::f64::consts::PI * (bucket as f64) / 256.0;
    (libm::cos(theta), libm::sin(theta))
}

fn pair_normal(a: &CcdActor, b: &CcdActor) -> (f64, f64, f64) {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let dist = libm::hypot(dx, dy);
    if dist <= 1.0e-9 {
        let (nx, ny) = deterministic_coincident_normal(a.canonical_actor_id, b.canonical_actor_id);
        (nx, ny, dist)
    } else {
        (dx / dist, dy / dist, dist)
    }
}

fn measure_max_residual(actors: &[CcdActor]) -> f64 {
    let mut max_res = 0.0_f64;
    for a in actors {
        max_res = max_res.max(a.bounds.violation(a.x, a.y));
    }
    for i in 0..actors.len() {
        for j in (i + 1)..actors.len() {
            let dist = libm::hypot(actors[j].x - actors[i].x, actors[j].y - actors[i].y);
            let pen = (actors[i].radius + actors[j].radius) - dist;
            if pen > max_res {
                max_res = pen;
            }
        }
    }
    max_res
}

fn clamp_all_bounds(actors: &mut [CcdActor]) {
    for a in actors.iter_mut() {
        let (cx, cy) = a.bounds.clamp(a.x, a.y);
        a.x = cx;
        a.y = cy;
    }
}

fn canonical_radial_fallback(actors: &mut [CcdActor], tick: u32, max_pen_m: f64, audit: &mut AuditGroundTruthRecord) {
    let n = actors.len();
    if n == 0 {
        return;
    }
    let mut r_max = 0.0_f64;
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    for a in actors.iter() {
        if a.radius > r_max {
            r_max = a.radius;
        }
        sum_x += a.x;
        sum_y += a.y;
    }
    let pitch = 2.0 * r_max + 0.05;
    let mut min_bx = f64::NEG_INFINITY;
    let mut max_bx = f64::INFINITY;
    let mut min_by = f64::NEG_INFINITY;
    let mut max_by = f64::INFINITY;
    for a in actors.iter() {
        min_bx = min_bx.max(a.bounds.min_x);
        max_bx = max_bx.min(a.bounds.max_x);
        min_by = min_by.max(a.bounds.min_y);
        max_by = max_by.min(a.bounds.max_y);
    }
    let cx = (sum_x / (n as f64)).clamp(min_bx + 1.5 * pitch + r_max, max_bx - 1.5 * pitch - r_max);
    let cy = (sum_y / (n as f64)).clamp(min_by + pitch + r_max, max_by - pitch - r_max);

    for (k, a) in actors.iter_mut().enumerate() {
        let col = (k % 4) as f64;
        let row = (k / 4) as f64;
        let gx = cx + (col - 1.5) * pitch;
        let gy = cy + (row - 1.0) * pitch;
        let (clx, cly) = a.bounds.clamp(gx, gy);
        a.x = clx;
        a.y = cly;
        a.vx = 0.0;
        a.vy = 0.0;
    }
    audit.warnings.push(AuditWarning::DepenetrationFallbackUsed {
        tick,
        max_pen_um: (max_pen_m * 1.0e6).round().max(0.0) as u32,
    });
}

fn project_joint_depenetration(
    actors: &mut [CcdActor],
    tick: u32,
    hooks: CcdDebugHooks,
    audit: &mut AuditGroundTruthRecord,
) {
    let n = actors.len();
    if n == 0 {
        return;
    }
    // 1. 128 SOR sweeps in canonical (min_cid, max_cid) order
    if !hooks.force_dykstra_early_exit {
        for _ in 0..MAX_DEPN_SWEEPS {
            for i in 0..n {
                for j in (i + 1)..n {
                    let (nx, ny, dist) = pair_normal(&actors[i], &actors[j]);
                    let min_d = actors[i].radius + actors[j].radius;
                    let pen = min_d - dist;
                    if pen > 0.0 {
                        let push = SOR_OMEGA * (pen + EPS_SEP_M);
                        let w_sum = actors[i].inv_mass + actors[j].inv_mass;
                        let fi = if w_sum > 0.0 {
                            actors[i].inv_mass / w_sum
                        } else {
                            0.5
                        };
                        let fj = 1.0 - fi;
                        actors[i].x -= nx * push * fi;
                        actors[i].y -= ny * push * fi;
                        actors[j].x += nx * push * fj;
                        actors[j].y += ny * push * fj;
                    }
                }
            }
            clamp_all_bounds(actors);
            if measure_max_residual(actors) <= EPS_CONVERGED_M {
                return;
            }
        }
    }

    // 2. Outer 4 x Inner 128 Re-linearized Dykstra QP with wall/corner inward bias
    if !hooks.force_dykstra_early_exit {
        let mut prev_res = measure_max_residual(actors);
        let mut prev_normals = [(0.0_f64, 0.0_f64); 45];
        let mut flip_count = 0u32;

        for outer in 0..MAX_OUTER_RELIN {
            let mut normals = [(0.0_f64, 0.0_f64); 45];
            let mut pair_active = [false; 45];
            let mut p_idx = 0usize;
            for i in 0..n {
                for j in (i + 1)..n {
                    let (mut nx, mut ny, dist) = pair_normal(&actors[i], &actors[j]);
                    let min_d = actors[i].radius + actors[j].radius;
                    if dist < min_d + 0.02 {
                        pair_active[p_idx] = true;
                        // Corner/wall inward normal bias if both near same boundary
                        let eps_wall = 1.0e-2;
                        let mut wx = 0.0;
                        let mut wy = 0.0;
                        if actors[i].x <= actors[i].bounds.min_x + eps_wall
                            && actors[j].x <= actors[j].bounds.min_x + eps_wall
                        {
                            wx += 1.0;
                        }
                        if actors[i].x >= actors[i].bounds.max_x - eps_wall
                            && actors[j].x >= actors[j].bounds.max_x - eps_wall
                        {
                            wx -= 1.0;
                        }
                        if actors[i].y <= actors[i].bounds.min_y + eps_wall
                            && actors[j].y <= actors[j].bounds.min_y + eps_wall
                        {
                            wy += 1.0;
                        }
                        if actors[i].y >= actors[i].bounds.max_y - eps_wall
                            && actors[j].y >= actors[j].bounds.max_y - eps_wall
                        {
                            wy -= 1.0;
                        }
                        let wlen = libm::hypot(wx, wy);
                        if wlen > 0.0 {
                            let in_x = wx / wlen;
                            let in_y = wy / wlen;
                            let dot_in = nx * in_x + ny * in_y;
                            if libm::fabs(dot_in) < 0.25 || dot_in < 0.0 {
                                let bx = nx + 0.35 * in_x;
                                let by = ny + 0.35 * in_y;
                                let blen = libm::hypot(bx, by).max(1.0e-9);
                                nx = bx / blen;
                                ny = by / blen;
                            }
                        }
                    }
                    if outer > 0 && pair_active[p_idx] {
                        let dot_prev = nx * prev_normals[p_idx].0 + ny * prev_normals[p_idx].1;
                        if dot_prev < -0.5 {
                            flip_count += 1;
                        }
                    }
                    normals[p_idx] = (nx, ny);
                    prev_normals[p_idx] = (nx, ny);
                    p_idx += 1;
                }
            }
            if flip_count >= 2 {
                break;
            }

            let mut y_pair = [(0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64); 45];
            let mut y_box = [(0.0_f64, 0.0_f64); 10];

            for _inner in 0..MAX_DYKSTRA_INNER {
                let mut idx = 0usize;
                for i in 0..n {
                    for j in (i + 1)..n {
                        let (nx, ny) = normals[idx];
                        let target = actors[i].radius + actors[j].radius + EPS_SEP_M;
                        let xi = actors[i].x - y_pair[idx].0;
                        let yi = actors[i].y - y_pair[idx].1;
                        let xj = actors[j].x - y_pair[idx].2;
                        let yj = actors[j].y - y_pair[idx].3;
                        let proj = (xj - xi) * nx + (yj - yi) * ny;
                        let deficit = target - proj;
                        let (new_xi, new_yi, new_xj, new_yj) = if deficit > 0.0 {
                            let w_sum = actors[i].inv_mass + actors[j].inv_mass;
                            let fi = if w_sum > 0.0 {
                                actors[i].inv_mass / w_sum
                            } else {
                                0.5
                            };
                            let fj = 1.0 - fi;
                            (
                                xi - nx * deficit * fi,
                                yi - ny * deficit * fi,
                                xj + nx * deficit * fj,
                                yj + ny * deficit * fj,
                            )
                        } else {
                            (xi, yi, xj, yj)
                        };
                        y_pair[idx] = (new_xi - xi, new_yi - yi, new_xj - xj, new_yj - yj);
                        actors[i].x = new_xi;
                        actors[i].y = new_yi;
                        actors[j].x = new_xj;
                        actors[j].y = new_yj;
                        idx += 1;
                    }
                }
                for (k, a) in actors.iter_mut().enumerate() {
                    let bx = a.x - y_box[k].0;
                    let by = a.y - y_box[k].1;
                    let (cx, cy) = a.bounds.clamp(bx, by);
                    y_box[k] = (cx - bx, cy - by);
                    a.x = cx;
                    a.y = cy;
                }
            }

            let cur_res = measure_max_residual(actors);
            if cur_res <= EPS_CONVERGED_M {
                return;
            }
            if (prev_res - cur_res).abs() < 1.0e-9 {
                break;
            }
            prev_res = cur_res;
        }
    }

    // 3. Unconditional canonical 4x3 radial separation fallback
    let final_res = measure_max_residual(actors);
    if final_res > EPS_CONVERGED_M {
        canonical_radial_fallback(actors, tick, final_res, audit);
    }
}

fn clamp_near_contact_and_wall_velocities(actors: &mut [CcdActor]) {
    let n = actors.len();
    for i in 0..n {
        for j in (i + 1)..n {
            let (nx, ny, dist) = pair_normal(&actors[i], &actors[j]);
            let min_d = actors[i].radius + actors[j].radius + EPS_SEP_M;
            if dist <= min_d {
                let rvx = actors[j].vx - actors[i].vx;
                let rvy = actors[j].vy - actors[i].vy;
                let v_rel = rvx * nx + rvy * ny;
                if v_rel < 0.0 {
                    let w_sum = actors[i].inv_mass + actors[j].inv_mass;
                    let fi = if w_sum > 0.0 {
                        actors[i].inv_mass / w_sum
                    } else {
                        0.5
                    };
                    let fj = 1.0 - fi;
                    actors[i].vx += nx * v_rel * fi;
                    actors[i].vy += ny * v_rel * fi;
                    actors[j].vx -= nx * v_rel * fj;
                    actors[j].vy -= ny * v_rel * fj;
                }
            }
        }
    }
    for a in actors.iter_mut() {
        a.bounds
            .clamp_outward_velocity(a.x, a.y, &mut a.vx, &mut a.vy, EPS_CONVERGED_M);
    }
}

/// Analytical earliest non-negative pair TOI (`0.0` if touching/overlapping and approaching).
fn pair_toi(a: &CcdActor, b: &CcdActor) -> f64 {
    let px = a.x - b.x;
    let py = a.y - b.y;
    let vx = a.vx - b.vx;
    let vy = a.vy - b.vy;
    let r_sum = a.radius + b.radius;
    let c = px * px + py * py - r_sum * r_sum;
    let b_coef = px * vx + py * vy;
    if c <= 0.0 {
        if b_coef < 0.0 {
            return 0.0;
        }
        return f64::INFINITY;
    }
    if b_coef >= 0.0 {
        return f64::INFINITY;
    }
    let a_coef = vx * vx + vy * vy;
    if a_coef <= 1.0e-18 {
        return f64::INFINITY;
    }
    let disc = b_coef * b_coef - a_coef * c;
    if disc < 0.0 {
        return f64::INFINITY;
    }
    let t = (-b_coef - libm::sqrt(disc)) / a_coef;
    if t >= 0.0 {
        t
    } else {
        f64::INFINITY
    }
}

fn record_sub_segment(
    audit: &mut AuditGroundTruthRecord,
    actors: &[CcdActor],
    tau_start: f64,
    tau_end: f64,
) {
    let mut positions = [(0.0, 0.0); 10];
    let mut velocities = [(0.0, 0.0); 10];
    let mut radii = [0.0; 10];
    let mut mask = 0u16;
    for a in actors {
        if a.slot < 10 {
            positions[a.slot] = (a.x, a.y);
            velocities[a.slot] = (a.vx, a.vy);
            radii[a.slot] = a.radius;
            mask |= 1u16 << a.slot;
        }
    }
    audit.ccd_sub_segments.push(CcdSubSegment {
        tau_start,
        tau_end,
        positions,
        velocities,
        radii,
        active_mask: mask,
    });
}

/// Full §7.2 Swept CCD pipeline (Step 0 SOR+4x128 Dykstra+4x3 fallback, Step 1..32 TOI, Step 33 stop-at-surface).
pub fn run_swept_ccd_step(
    actors: &mut [CcdActor],
    dt_s: f64,
    tick: u32,
    hooks: CcdDebugHooks,
    audit: &mut AuditGroundTruthRecord,
) -> Vec<(usize, usize)> {
    let mut contact_pairs = Vec::new();
    if actors.is_empty() {
        return contact_pairs;
    }
    // Enforce canonical actor id ascending order
    actors.sort_by_key(|a| a.canonical_actor_id);

    // Step 0
    project_joint_depenetration(actors, tick, hooks, audit);
    clamp_near_contact_and_wall_velocities(actors);

    let mut t_rem = dt_s;
    let mut tau_prev = 0.0;
    let n = actors.len();

    // Step 1..32 simultaneous cluster TOI
    for _pass in 0..MAX_CCD_TOI_PASSES {
        if t_rem <= 1.0e-12 {
            break;
        }
        let mut tau_min = f64::INFINITY;
        for i in 0..n {
            let wt = actors[i]
                .bounds
                .earliest_wall_toi(actors[i].x, actors[i].y, actors[i].vx, actors[i].vy);
            if wt < tau_min {
                tau_min = wt;
            }
            for j in (i + 1)..n {
                let pt = pair_toi(&actors[i], &actors[j]);
                if pt < tau_min {
                    tau_min = pt;
                }
            }
        }
        if tau_min > t_rem {
            for a in actors.iter_mut() {
                a.x += a.vx * t_rem;
                a.y += a.vy * t_rem;
                let (cx, cy) = a.bounds.clamp(a.x, a.y);
                a.x = cx;
                a.y = cy;
            }
            record_sub_segment(audit, actors, tau_prev, dt_s);
            t_rem = 0.0;
            break;
        }

        if tau_min > 0.0 {
            for a in actors.iter_mut() {
                a.x += a.vx * tau_min;
                a.y += a.vy * tau_min;
                let (cx, cy) = a.bounds.clamp(a.x, a.y);
                a.x = cx;
                a.y = cy;
            }
            let tau_next = tau_prev + tau_min;
            record_sub_segment(audit, actors, tau_prev, tau_next);
            tau_prev = tau_next;
            t_rem -= tau_min;
        }

        // Simultaneous contact cluster within 1e-7s
        for i in 0..n {
            for j in (i + 1)..n {
                let pt = pair_toi(&actors[i], &actors[j]);
                if pt <= 1.0e-7 {
                    contact_pairs.push((actors[i].slot, actors[j].slot));
                    let (nx, ny, _) = pair_normal(&actors[i], &actors[j]);
                    let rvx = actors[j].vx - actors[i].vx;
                    let rvy = actors[j].vy - actors[i].vy;
                    let v_rel = rvx * nx + rvy * ny;
                    if v_rel < 0.0 {
                        // Inelastic collision impulse (restitution = 0.05)
                        let j_imp = -(1.05 * v_rel)
                            / (actors[i].inv_mass + actors[j].inv_mass).max(1.0e-9);
                        actors[i].vx -= nx * j_imp * actors[i].inv_mass;
                        actors[i].vy -= ny * j_imp * actors[i].inv_mass;
                        actors[j].vx += nx * j_imp * actors[j].inv_mass;
                        actors[j].vy += ny * j_imp * actors[j].inv_mass;
                    }
                }
            }
        }

        // Joint position projection + velocity clamping
        project_joint_depenetration(actors, tick, hooks, audit);
        clamp_near_contact_and_wall_velocities(actors);
    }

    // Step 33: stop-at-surface convergence loop (no fixed step blind advance)
    if t_rem > 1.0e-12 {
        for _iter in 0..MAX_SURFACE_LOCK_ITERS {
            if t_rem <= 1.0e-12 {
                break;
            }
            clamp_near_contact_and_wall_velocities(actors);
            let mut tau_next = f64::INFINITY;
            for i in 0..n {
                let wt = actors[i].bounds.earliest_wall_toi(
                    actors[i].x,
                    actors[i].y,
                    actors[i].vx,
                    actors[i].vy,
                );
                if wt < tau_next {
                    tau_next = wt;
                }
                for j in (i + 1)..n {
                    let pt = pair_toi(&actors[i], &actors[j]);
                    if pt < tau_next {
                        tau_next = pt;
                    }
                }
            }
            let tau_step = if tau_next > 1.0e-9 {
                t_rem.min(tau_next)
            } else {
                // Zero out velocities of touching components and retry
                for i in 0..n {
                    for j in (i + 1)..n {
                        let (_, _, d) = pair_normal(&actors[i], &actors[j]);
                        if d <= actors[i].radius + actors[j].radius + EPS_SEP_M {
                            actors[i].vx = 0.0;
                            actors[i].vy = 0.0;
                            actors[j].vx = 0.0;
                            actors[j].vy = 0.0;
                        }
                    }
                    actors[i].bounds.clamp_outward_velocity(
                        actors[i].x,
                        actors[i].y,
                        &mut actors[i].vx,
                        &mut actors[i].vy,
                        EPS_SEP_M,
                    );
                }
                0.0
            };
            if tau_step > 0.0 {
                for a in actors.iter_mut() {
                    a.x += a.vx * tau_step;
                    a.y += a.vy * tau_step;
                    let (cx, cy) = a.bounds.clamp(a.x, a.y);
                    a.x = cx;
                    a.y = cy;
                }
                let next_t = tau_prev + tau_step;
                record_sub_segment(audit, actors, tau_prev, next_t);
                tau_prev = next_t;
                t_rem -= tau_step;
            }
        }
        if t_rem > 1.0e-12 {
            // Freeze remaining mutually-pressing motion and finish remaining dt safely
            record_sub_segment(audit, actors, tau_prev, dt_s);
        }
    }

    contact_pairs
}

/// 3D projectile trajectory test for `has_chance_to_score` (§7.3).
pub fn ball_has_chance_to_score(
    pos: [f64; 3],
    vel: [f64; 3],
    rim_xy: (f64, f64),
    rim_height_m: f64,
) -> bool {
    let dz = pos[2] - rim_height_m;
    let disc = vel[2] * vel[2] + 2.0 * GRAVITY_M_S2 * dz;
    if disc < 0.0 {
        return false;
    }
    let t_down = (vel[2] + libm::sqrt(disc)) / GRAVITY_M_S2;
    if t_down <= 0.0 {
        return false;
    }
    let land_x = pos[0] + vel[0] * t_down;
    let land_y = pos[1] + vel[1] * t_down;
    let dist = libm::hypot(land_x - rim_xy.0, land_y - rim_xy.1);
    dist <= RIM_RADIUS_M + BALL_RADIUS_M + 0.08
}

#[inline]
pub fn is_ball_completely_above_ring(ball_z_m: f64, rim_height_m: f64) -> bool {
    (ball_z_m - BALL_RADIUS_M) > rim_height_m
}

#[inline]
pub fn is_inside_rim_cylinder(
    ball_pos: [f64; 3],
    rim_xy: (f64, f64),
    rim_height_m: f64,
) -> bool {
    ball_pos[2] >= rim_height_m - BALL_RADIUS_M
        && libm::hypot(ball_pos[0] - rim_xy.0, ball_pos[1] - rim_xy.1)
            <= RIM_RADIUS_M + BALL_RADIUS_M * 0.5
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlightTouchOutcome {
    LegalNoChange,
    Blocked,
    InterceptedLive,
    Goaltending,
    BasketInterferenceOffenseCancel,
    BasketInterferenceDefenseScore { add_extra_ft: bool, add_tech_foul: bool },
}

/// 5-layer Goaltending / Basket Interference / Legal Block裁决 (§7.3).
#[allow(clippy::too_many_arguments)]
pub fn evaluate_flight_touch(
    goaltend_mode: u8, // 0=NbaRule11, 1=FibaArt31
    is_free_throw: bool,
    ft_shots_remaining: u8,
    ft_on_complete: Option<PostFtOutcome>,
    by_offense: bool,
    is_dunk_followthrough_or_safety_hang: bool,
    rim_touched: bool,
    backboard_contact: BackboardContact,
    ball_pos: [f64; 3],
    ball_vel: [f64; 3],
    rim_xy: (f64, f64),
    rim_height_m: f64,
    whistle_occurred: bool,
    game_clock_expired: bool,
    touching_rim_or_net_or_below: bool,
) -> FlightTouchOutcome {
    let has_chance = ball_has_chance_to_score(ball_pos, ball_vel, rim_xy, rim_height_m);
    let above_ring = is_ball_completely_above_ring(ball_pos[2], rim_height_m);
    let in_cyl = is_inside_rim_cylinder(ball_pos, rim_xy, rim_height_m);

    // Layer 1: Free-throw flight & post-rim interference
    if is_free_throw {
        if by_offense {
            return FlightTouchOutcome::BasketInterferenceOffenseCancel;
        }
        if !rim_touched && backboard_contact == BackboardContact::None {
            if goaltend_mode == 0 {
                // NBA Rule 11 & Rule 9 §I-h: +1 FT strictly only if shots_remaining == 1 && LiveReboundOnLastMiss
                let remain_in_play = ft_shots_remaining == 1
                    && matches!(ft_on_complete, Some(PostFtOutcome::LiveReboundOnLastMiss));
                return FlightTouchOutcome::BasketInterferenceDefenseScore {
                    add_extra_ft: remain_in_play,
                    add_tech_foul: false,
                };
            } else {
                // FIBA Art. 31.3.3 & Interp 31-2: tech strictly if final/only FT of series (!ResumeInterruptedFtSeries)
                let is_last_or_only = ft_shots_remaining == 1
                    && !matches!(ft_on_complete, Some(PostFtOutcome::ResumeInterruptedFtSeries));
                return FlightTouchOutcome::BasketInterferenceDefenseScore {
                    add_extra_ft: false,
                    add_tech_foul: is_last_or_only,
                };
            }
        }
        // After rim/backboard touch on FT
        if goaltend_mode == 0 {
            if has_chance || in_cyl || touching_rim_or_net_or_below {
                return FlightTouchOutcome::BasketInterferenceDefenseScore {
                    add_extra_ft: false,
                    add_tech_foul: false,
                };
            }
            return FlightTouchOutcome::LegalNoChange;
        } else {
            let is_non_last = ft_shots_remaining > 1
                || matches!(ft_on_complete, Some(PostFtOutcome::ResumeInterruptedFtSeries));
            if is_non_last && has_chance {
                return FlightTouchOutcome::BasketInterferenceDefenseScore {
                    add_extra_ft: false,
                    add_tech_foul: false,
                };
            }
            // FIBA last/only FT after rim touch: legal to rebound/tap, EXCEPT slapping board/shaking rim/reaching from below while has_chance (Interp 31-2: 1 pt, NO tech)
            if !is_non_last && has_chance && touching_rim_or_net_or_below {
                return FlightTouchOutcome::BasketInterferenceDefenseScore {
                    add_extra_ft: false,
                    add_tech_foul: false,
                };
            }
            return FlightTouchOutcome::LegalNoChange;
        }
    }

    // Layer 2: Rim cylinder, NBA dunk/safety exemption, FIBA whistle/game-clock dead-rim restriction
    if by_offense && is_dunk_followthrough_or_safety_hang && goaltend_mode == 0 {
        return FlightTouchOutcome::LegalNoChange;
    }
    if goaltend_mode == 0 && (in_cyl || rim_touched) && has_chance {
        return if by_offense {
            FlightTouchOutcome::BasketInterferenceOffenseCancel
        } else {
            FlightTouchOutcome::BasketInterferenceDefenseScore {
                add_extra_ft: false,
                add_tech_foul: false,
            }
        };
    }
    if goaltend_mode == 1 && rim_touched {
        if (whistle_occurred || game_clock_expired) && has_chance {
            return if by_offense {
                FlightTouchOutcome::BasketInterferenceOffenseCancel
            } else {
                FlightTouchOutcome::BasketInterferenceDefenseScore {
                    add_extra_ft: false,
                    add_tech_foul: false,
                }
            };
        }
        if touching_rim_or_net_or_below && has_chance {
            return if by_offense {
                FlightTouchOutcome::BasketInterferenceOffenseCancel
            } else {
                FlightTouchOutcome::BasketInterferenceDefenseScore {
                    add_extra_ft: false,
                    add_tech_foul: false,
                }
            };
        }
        return FlightTouchOutcome::LegalNoChange;
    }

    // Layer 3: Backboard contact without rim touch
    if !rim_touched && backboard_contact != BackboardContact::None && has_chance {
        if goaltend_mode == 0 {
            if backboard_contact == BackboardContact::AtOrAboveRim || ball_vel[2] > 0.0 {
                return FlightTouchOutcome::Goaltending;
            }
        } else if above_ring {
            return FlightTouchOutcome::Goaltending;
        } else if !by_offense {
            return FlightTouchOutcome::Blocked;
        }
    }

    // Layer 4: Descending flight goaltending (FIBA strictly vz < 0.0; NBA vz <= 0.0)
    if !rim_touched && has_chance {
        if goaltend_mode == 0 && ball_vel[2] <= 0.0 && ball_pos[2] > rim_height_m {
            return FlightTouchOutcome::Goaltending;
        }
        if goaltend_mode == 1 && ball_vel[2] < 0.0 && above_ring {
            return FlightTouchOutcome::Goaltending;
        }
    }

    // Layer 5: Legal block on ascending (or FIBA vz == 0.0 apex) untouched shot or intercept of no-chance ball
    if !by_offense {
        if !rim_touched
            && backboard_contact == BackboardContact::None
            && !in_cyl
            && (ball_vel[2] > 0.0 || (goaltend_mode == 1 && ball_vel[2] == 0.0 && has_chance))
        {
            return FlightTouchOutcome::Blocked;
        }
        if !rim_touched && !has_chance && ball_vel[2] <= 0.0 {
            return FlightTouchOutcome::InterceptedLive;
        }
    }

    FlightTouchOutcome::LegalNoChange
}
