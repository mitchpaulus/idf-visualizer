//! Orbit camera (Z-up) and CPU ray picking.

use glam::{Mat4, Vec3};

pub struct OrbitCamera {
    pub target: Vec3,
    pub distance: f32,
    /// Radians around Z; 0 looks from -Y toward +Y (from the south).
    pub yaw: f32,
    /// Radians above the horizon.
    pub pitch: f32,
    pub fov_y: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            distance: 50.0,
            yaw: 0.6,
            pitch: 0.5,
            fov_y: 45f32.to_radians(),
        }
    }
}

impl OrbitCamera {
    pub fn eye(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        // yaw=0, pitch=0 -> looking from -Y; positive yaw swings east.
        let dir = Vec3::new(sy * cp, -cy * cp, sp);
        self.target + dir * self.distance
    }

    pub fn view(&self) -> Mat4 {
        glam::camera::rh::view::look_at_mat4(self.eye(), self.target, Vec3::Z)
    }

    pub fn proj(&self, aspect: f32) -> Mat4 {
        let near = (self.distance * 0.001).max(0.01);
        let far = (self.distance * 100.0).max(1000.0);
        glam::camera::rh::proj::directx::perspective(self.fov_y, aspect.max(0.01), near, far)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.proj(aspect) * self.view()
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw += dx * 0.008;
        self.pitch = (self.pitch + dy * 0.008).clamp(-1.55, 1.55);
    }

    pub fn pan(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        // Move the target in the camera's screen plane; scale so a full-height
        // drag pans about the visible height at the target distance.
        let world_per_px = 2.0 * self.distance * (self.fov_y / 2.0).tan() / viewport_height.max(1.0);
        let view = self.view();
        let right = Vec3::new(view.x_axis.x, view.y_axis.x, view.z_axis.x);
        let up = Vec3::new(view.x_axis.y, view.y_axis.y, view.z_axis.y);
        self.target += (-right * dx + up * dy) * world_per_px;
    }

    pub fn zoom(&mut self, scroll: f32) {
        self.distance = (self.distance * 0.998f32.powf(scroll * 3.0)).clamp(0.05, 1e6);
    }

    /// Frame the given bounding box.
    pub fn fit(&mut self, min: Vec3, max: Vec3) {
        if !min.is_finite() || !max.is_finite() {
            return;
        }
        let center = (min + max) / 2.0;
        let radius = ((max - min).length() / 2.0).max(0.5);
        self.target = center;
        self.distance = radius / (self.fov_y / 2.0).sin() * 1.1;
    }

    /// World-space ray through a point given in normalized device coords
    /// (x, y in [-1, 1], y up). Returns (origin, direction).
    pub fn ray(&self, ndc_x: f32, ndc_y: f32, aspect: f32) -> (Vec3, Vec3) {
        let inv = self.view_proj(aspect).inverse();
        let near = inv.project_point3(Vec3::new(ndc_x, ndc_y, 0.0));
        let far = inv.project_point3(Vec3::new(ndc_x, ndc_y, 0.99));
        (near, (far - near).normalize())
    }
}

/// Möller–Trumbore ray/triangle intersection; returns t.
pub fn ray_triangle(orig: Vec3, dir: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let e1 = b - a;
    let e2 = c - a;
    let p = dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-9 {
        return None;
    }
    let inv_det = 1.0 / det;
    let t_vec = orig - a;
    let u = t_vec.dot(p) * inv_det;
    if !(-1e-4..=1.0001).contains(&u) {
        return None;
    }
    let q = t_vec.cross(e1);
    let v = dir.dot(q) * inv_det;
    if v < -1e-4 || u + v > 1.0001 {
        return None;
    }
    let t = e2.dot(q) * inv_det;
    (t > 1e-4).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ray_hits_triangle() {
        let t = ray_triangle(
            Vec3::new(0.25, 0.25, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
        );
        assert!((t.unwrap() - 5.0).abs() < 1e-5);
    }

    #[test]
    fn ray_misses_triangle() {
        let t = ray_triangle(
            Vec3::new(2.0, 2.0, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
        );
        assert!(t.is_none());
    }

    #[test]
    fn camera_ray_through_center_hits_target() {
        let cam = OrbitCamera {
            target: Vec3::new(3.0, 4.0, 5.0),
            distance: 10.0,
            ..Default::default()
        };
        let (o, d) = cam.ray(0.0, 0.0, 1.5);
        // Ray from eye through NDC center passes through the target.
        let to_target = (cam.target - o).normalize();
        assert!((d - to_target).length() < 1e-3);
    }
}
