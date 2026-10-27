use glam::{Mat4, Vec3, Vec4};

use super::mesh::Aabb;

/// Orbit camera around a target point. Y is up.
#[derive(Debug, Clone, Copy)]
pub struct OrbitCamera {
    pub target: Vec3,
    /// Radians around +Y.
    pub yaw: f32,
    /// Radians above the XZ plane.
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        OrbitCamera {
            target: Vec3::ZERO,
            yaw: 35f32.to_radians(),
            pitch: 25f32.to_radians(),
            distance: 5.0,
            fov_y: 45f32.to_radians(),
        }
    }
}

impl OrbitCamera {
    const PITCH_LIMIT: f32 = 89.0 * std::f32::consts::PI / 180.0;

    pub fn forward(&self) -> Vec3 {
        -Vec3::new(
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
            self.pitch.cos() * self.yaw.cos(),
        )
    }

    pub fn eye(&self) -> Vec3 {
        self.target - self.forward() * self.distance
    }

    pub fn right(&self) -> Vec3 {
        self.forward().cross(Vec3::Y).normalize()
    }

    pub fn up(&self) -> Vec3 {
        self.right().cross(self.forward())
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target, Vec3::Y)
    }

    /// Reverse-Z, infinite far plane, Vulkan [0, 1] depth.
    /// The Y flip is done with a negative viewport height.
    pub fn projection(&self, aspect: f32) -> Mat4 {
        let near = (self.distance * 1e-3).max(1e-5);
        Mat4::perspective_infinite_reverse_rh(self.fov_y, aspect.max(1e-3), near)
    }

    pub fn orbit(&mut self, dx_pixels: f32, dy_pixels: f32) {
        const SPEED: f32 = 0.008;
        self.yaw -= dx_pixels * SPEED;
        self.pitch = (self.pitch + dy_pixels * SPEED).clamp(-Self::PITCH_LIMIT, Self::PITCH_LIMIT);
    }

    /// Pan so the point under the cursor follows it at the target's depth.
    pub fn pan(&mut self, dx_pixels: f32, dy_pixels: f32, viewport_height: f32) {
        let world_per_pixel =
            2.0 * self.distance * (self.fov_y * 0.5).tan() / viewport_height.max(1.0);
        self.target += (-self.right() * dx_pixels + self.up() * dy_pixels) * world_per_pixel;
    }

    pub fn zoom(&mut self, steps: f32) {
        self.distance = (self.distance * 0.88f32.powf(steps)).clamp(1e-4, 1e7);
    }

    /// Frame `bounds` so its bounding sphere fills the view.
    pub fn fit(&mut self, bounds: &Aabb, aspect: f32) {
        let radius = bounds.radius().max(1e-4);
        let half_fov =
            (self.fov_y * 0.5).min((self.fov_y * 0.5).tan().atan2(1.0 / aspect.max(1e-3)));
        self.target = bounds.center();
        self.distance = radius / half_fov.sin() * 1.1;
    }

    pub fn reset(&mut self, bounds: &Aabb, aspect: f32) {
        let fov_y = self.fov_y;
        *self = OrbitCamera {
            fov_y,
            ..Default::default()
        };
        self.fit(bounds, aspect);
    }
}

/// Clip-space planes for culling. With an infinite far plane only five are useful.
pub struct Frustum {
    planes: [Vec4; 5],
}

impl Frustum {
    pub fn from_view_proj(m: &Mat4) -> Self {
        let r0 = m.row(0);
        let r1 = m.row(1);
        let r2 = m.row(2);
        let r3 = m.row(3);
        let normalize = |p: Vec4| p / p.truncate().length().max(1e-12);
        Frustum {
            planes: [
                normalize(r3 + r0),
                normalize(r3 - r0),
                normalize(r3 + r1),
                normalize(r3 - r1),
                // Reverse Z: the near plane sits at z_clip = w.
                normalize(r3 - r2),
            ],
        }
    }

    pub fn intersects(&self, b: &Aabb) -> bool {
        self.planes.iter().all(|p| {
            // Test the box corner furthest along the plane normal.
            let corner = Vec3::select(p.truncate().cmpge(Vec3::ZERO), b.max, b.min);
            p.truncate().dot(corner) + p.w >= 0.0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_box_at(center: Vec3) -> Aabb {
        Aabb {
            min: center - Vec3::splat(0.5),
            max: center + Vec3::splat(0.5),
        }
    }

    #[test]
    fn frustum_keeps_visible_and_culls_hidden_boxes() {
        let camera = OrbitCamera::default();
        let view_proj = camera.projection(1.5) * camera.view();
        let frustum = Frustum::from_view_proj(&view_proj);
        assert!(frustum.intersects(&unit_box_at(camera.target)));
        // Directly behind the eye.
        assert!(!frustum.intersects(&unit_box_at(camera.eye() - camera.forward() * 5.0)));
        // Far off to the side.
        assert!(!frustum.intersects(&unit_box_at(camera.target + camera.right() * 1000.0)));
        // Very far ahead: the far plane is at infinity.
        assert!(frustum.intersects(&unit_box_at(camera.eye() + camera.forward() * 1e5)));
    }

    #[test]
    fn fit_centers_and_contains_bounds() {
        let mut camera = OrbitCamera::default();
        let bounds = unit_box_at(Vec3::new(3.0, 4.0, 5.0));
        camera.fit(&bounds, 0.5);
        assert!((camera.target - bounds.center()).length() < 1e-5);
        let frustum = Frustum::from_view_proj(&(camera.projection(0.5) * camera.view()));
        for corner in [bounds.min, bounds.max] {
            assert!(frustum.intersects(&Aabb {
                min: corner,
                max: corner
            }));
        }
    }
}
