//! Keyframe animation, skinning and morph targets, evaluated on the CPU.
//!
//! A [`Rig`] keeps the full source node hierarchy (the scene's mesh instances
//! are a flattened view of it). Posing the rig at a time produces world
//! matrices for every node; mesh instances take their transform from those,
//! and deformed primitives ([`Deform`]) are re-skinned into vertex data that
//! the renderer streams to the GPU each frame.

use std::collections::HashSet;

use glam::{Mat3, Mat4, Quat, Vec3};

use super::Vertex;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Property {
    Translation,
    Rotation,
    Scale,
    /// Morph target weights.
    Weights,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Interpolation {
    Step,
    Linear,
    /// Each keyframe stores in-tangent, value, out-tangent.
    CubicSpline,
}

/// One animated property of one node.
#[derive(Debug, Clone)]
pub struct Channel {
    /// Index into [`Rig::nodes`].
    pub node: usize,
    pub property: Property,
    pub interpolation: Interpolation,
    /// Keyframe times in seconds, ascending.
    pub times: Vec<f32>,
    /// Keyframe values, `stride` floats each (three groups per key for cubic splines).
    pub values: Vec<f32>,
    pub stride: usize,
}

impl Channel {
    /// Build a channel, or `None` if the value count doesn't match the keyframes.
    pub fn new(
        node: usize,
        property: Property,
        interpolation: Interpolation,
        times: Vec<f32>,
        values: Vec<f32>,
    ) -> Option<Self> {
        let groups = match interpolation {
            Interpolation::CubicSpline => 3,
            _ => 1,
        };
        let keys = times.len() * groups;
        if keys == 0 || values.is_empty() || !values.len().is_multiple_of(keys) {
            return None;
        }
        let stride = values.len() / keys;
        let expected = match property {
            Property::Translation | Property::Scale => Some(3),
            Property::Rotation => Some(4),
            Property::Weights => None,
        };
        if expected.is_some_and(|e| e != stride) {
            return None;
        }
        Some(Channel {
            node,
            property,
            interpolation,
            times,
            values,
            stride,
        })
    }

    pub fn duration(&self) -> f32 {
        self.times.last().copied().unwrap_or(0.0)
    }

    /// `part`: 0 in-tangent, 1 value, 2 out-tangent (cubic splines only).
    fn key(&self, index: usize, part: usize) -> &[f32] {
        let s = self.stride;
        let start = match self.interpolation {
            Interpolation::CubicSpline => (index * 3 + part) * s,
            _ => index * s,
        };
        &self.values[start..start + s]
    }

    fn value(&self, index: usize) -> &[f32] {
        match self.interpolation {
            Interpolation::CubicSpline => self.key(index, 1),
            _ => self.key(index, 0),
        }
    }

    /// Sample at `t`, clamping outside the keyframe range. `out` gets `stride` values.
    pub fn sample(&self, t: f32, out: &mut Vec<f32>) {
        out.clear();
        let n = self.times.len();
        if n == 1 || t <= self.times[0] {
            out.extend_from_slice(self.value(0));
        } else if t >= self.times[n - 1] {
            out.extend_from_slice(self.value(n - 1));
        } else {
            // times[i] <= t < times[i + 1]
            let i = self.times.partition_point(|&k| k <= t) - 1;
            let (t0, t1) = (self.times[i], self.times[i + 1]);
            let dt = t1 - t0;
            let u = if dt > 0.0 { (t - t0) / dt } else { 0.0 };
            match self.interpolation {
                Interpolation::Step => out.extend_from_slice(self.value(i)),
                Interpolation::Linear if self.property == Property::Rotation => {
                    let a = Quat::from_slice(self.value(i));
                    let b = Quat::from_slice(self.value(i + 1));
                    out.extend_from_slice(&a.slerp(b, u).to_array());
                }
                Interpolation::Linear => out.extend(
                    self.value(i)
                        .iter()
                        .zip(self.value(i + 1))
                        .map(|(a, b)| a + (b - a) * u),
                ),
                Interpolation::CubicSpline => {
                    let (u2, u3) = (u * u, u * u * u);
                    let h00 = 2.0 * u3 - 3.0 * u2 + 1.0;
                    let h10 = (u3 - 2.0 * u2 + u) * dt;
                    let h01 = -2.0 * u3 + 3.0 * u2;
                    let h11 = (u3 - u2) * dt;
                    let p0 = self.key(i, 1);
                    let m0 = self.key(i, 2);
                    let p1 = self.key(i + 1, 1);
                    let m1 = self.key(i + 1, 0);
                    out.extend(
                        (0..self.stride)
                            .map(|c| h00 * p0[c] + h10 * m0[c] + h01 * p1[c] + h11 * m1[c]),
                    );
                }
            }
        }
        if self.property == Property::Rotation {
            let q = Quat::from_slice(out).normalize();
            out.copy_from_slice(&q.to_array());
        }
    }
}

#[derive(Debug, Clone)]
pub struct Animation {
    pub name: String,
    pub channels: Vec<Channel>,
    pub duration: f32,
}

impl Animation {
    pub fn new(name: String, channels: Vec<Channel>) -> Self {
        let duration = channels.iter().map(Channel::duration).fold(0.0, f32::max);
        Animation {
            name,
            channels,
            duration,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trs {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Trs {
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

#[derive(Debug, Clone)]
pub struct RigNode {
    pub parent: Option<usize>,
    /// Rest transform as authored; used as-is unless the node is animated.
    pub rest_matrix: Mat4,
    /// Rest transform as TRS, the base that animation channels override.
    pub rest: Trs,
    /// Rest morph target weights (node weights, else the mesh's defaults).
    pub weights: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct Skin {
    /// Indices into [`Rig::nodes`].
    pub joints: Vec<usize>,
    /// One per joint; identity when the file has none.
    pub inverse_bind: Vec<Mat4>,
}

/// The evaluated state of every rig node at one moment.
pub struct Pose {
    pub world: Vec<Mat4>,
    pub weights: Vec<Vec<f32>>,
    /// Per skin, per joint: `world(joint) * inverse_bind`.
    pub joints: Vec<Vec<Mat4>>,
}

#[derive(Debug, Clone)]
pub struct Rig {
    pub nodes: Vec<RigNode>,
    pub skins: Vec<Skin>,
    pub animations: Vec<Animation>,
    /// Node indices with every parent before its children.
    order: Vec<usize>,
}

/// Which clips to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipSelection {
    /// The rest pose.
    None,
    One(usize),
    /// Every clip at once on a shared timeline, for files that animate each
    /// object in its own clip.
    All,
}

impl Rig {
    pub fn new(nodes: Vec<RigNode>, skins: Vec<Skin>, animations: Vec<Animation>) -> Self {
        let mut children = vec![Vec::new(); nodes.len()];
        for (i, n) in nodes.iter().enumerate() {
            if let Some(p) = n.parent {
                children[p].push(i);
            }
        }
        // Breadth-first from the roots; nodes caught in a parent cycle are never
        // reached and keep an identity world transform.
        let mut order: Vec<usize> = (0..nodes.len())
            .filter(|&i| nodes[i].parent.is_none())
            .collect();
        let mut head = 0;
        while head < order.len() {
            order.extend_from_slice(&children[order[head]]);
            head += 1;
        }
        Rig {
            nodes,
            skins,
            animations,
            order,
        }
    }

    /// Play all clips together by default when each animates its own nodes;
    /// otherwise they are alternatives (idle, walk, …), so start with the first.
    pub fn default_selection(&self) -> ClipSelection {
        match self.animations.len() {
            0 => ClipSelection::None,
            1 => ClipSelection::One(0),
            _ => {
                let mut seen = HashSet::new();
                let disjoint = self.animations.iter().all(|a| {
                    let nodes: HashSet<usize> = a.channels.iter().map(|c| c.node).collect();
                    nodes.into_iter().all(|n| seen.insert(n))
                });
                if disjoint {
                    ClipSelection::All
                } else {
                    ClipSelection::One(0)
                }
            }
        }
    }

    pub fn duration(&self, selection: ClipSelection) -> f32 {
        match selection {
            ClipSelection::None => 0.0,
            ClipSelection::One(i) => self.animations.get(i).map_or(0.0, |a| a.duration),
            ClipSelection::All => self
                .animations
                .iter()
                .map(|a| a.duration)
                .fold(0.0, f32::max),
        }
    }

    pub fn pose(&self, selection: ClipSelection, time: f32) -> Pose {
        let n = self.nodes.len();
        let mut trs: Vec<Option<Trs>> = vec![None; n];
        let mut weights: Vec<Vec<f32>> = self.nodes.iter().map(|n| n.weights.clone()).collect();

        let clips: &[Animation] = match selection {
            ClipSelection::None => &[],
            ClipSelection::One(i) => self.animations.get(i..=i).unwrap_or(&[]),
            ClipSelection::All => &self.animations,
        };
        let mut value = Vec::new();
        for channel in clips.iter().flat_map(|a| &a.channels) {
            let Some(node) = self.nodes.get(channel.node) else {
                continue;
            };
            channel.sample(time, &mut value);
            if channel.property == Property::Weights {
                weights[channel.node].clone_from(&value);
                continue;
            }
            let t = trs[channel.node].get_or_insert(node.rest);
            match channel.property {
                Property::Translation => t.translation = Vec3::from_slice(&value),
                Property::Rotation => t.rotation = Quat::from_slice(&value),
                Property::Scale => t.scale = Vec3::from_slice(&value),
                Property::Weights => unreachable!(),
            }
        }

        let mut world = vec![Mat4::IDENTITY; n];
        for &i in &self.order {
            let local = trs[i].map_or(self.nodes[i].rest_matrix, |t| t.matrix());
            world[i] = match self.nodes[i].parent {
                Some(p) => world[p] * local,
                None => local,
            };
        }
        let joints = self
            .skins
            .iter()
            .map(|s| {
                s.joints
                    .iter()
                    .zip(&s.inverse_bind)
                    .map(|(&j, ibm)| world.get(j).copied().unwrap_or(Mat4::IDENTITY) * *ibm)
                    .collect()
            })
            .collect();
        Pose {
            world,
            weights,
            joints,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MorphTarget {
    pub positions: Option<Vec<[f32; 3]>>,
    pub normals: Option<Vec<[f32; 3]>>,
}

/// Per-vertex data for a primitive whose vertices move: skin influences
/// and/or morph targets. `output` holds the current deformed vertices.
#[derive(Debug, Clone)]
pub struct Deform {
    pub base: Vec<Vertex>,
    /// Up to four (joint, weight) influences per vertex, weights normalized.
    pub influences: Option<Vec<([u16; 4], [f32; 4])>>,
    pub targets: Vec<MorphTarget>,
    pub output: Vec<Vertex>,
}

impl Deform {
    /// `None` if the primitive has neither usable skin data nor morph targets.
    pub fn new(
        base: &[Vertex],
        influences: Option<Vec<([u16; 4], [f32; 4])>>,
        targets: Vec<MorphTarget>,
    ) -> Option<Self> {
        let n = base.len();
        let influences = influences.filter(|i| i.len() == n).map(|mut i| {
            for (_, w) in &mut i {
                let sum: f32 = w.iter().sum();
                if sum > 0.0 {
                    w.iter_mut().for_each(|w| *w /= sum);
                }
            }
            i
        });
        let targets: Vec<MorphTarget> = targets
            .into_iter()
            .map(|t| MorphTarget {
                positions: t.positions.filter(|p| p.len() == n),
                normals: t.normals.filter(|p| p.len() == n),
            })
            .collect();
        let has_targets = targets
            .iter()
            .any(|t| t.positions.is_some() || t.normals.is_some());
        if influences.is_none() && !has_targets {
            return None;
        }
        Some(Deform {
            base: base.to_vec(),
            influences,
            targets,
            output: base.to_vec(),
        })
    }

    pub fn is_skinned(&self) -> bool {
        self.influences.is_some()
    }

    /// Recompute `output`: morph targets first, then skinning (if `joints` is given).
    pub fn apply(&mut self, morph_weights: &[f32], joints: Option<&[Mat4]>) {
        self.output.copy_from_slice(&self.base);
        let mut morphed_normals = false;
        for (target, &w) in self.targets.iter().zip(morph_weights) {
            if w == 0.0 {
                continue;
            }
            if let Some(d) = &target.positions {
                for (v, d) in self.output.iter_mut().zip(d) {
                    v.position = (Vec3::from(v.position) + Vec3::from(*d) * w).into();
                }
            }
            if let Some(d) = &target.normals {
                morphed_normals = true;
                for (v, d) in self.output.iter_mut().zip(d) {
                    v.normal = (Vec3::from(v.normal) + Vec3::from(*d) * w).into();
                }
            }
        }

        match (&self.influences, joints) {
            (Some(influences), Some(joints)) => {
                for (v, (j, w)) in self.output.iter_mut().zip(influences) {
                    let mut m = Mat4::ZERO;
                    for k in 0..4 {
                        if w[k] != 0.0 {
                            if let Some(jm) = joints.get(j[k] as usize) {
                                m += *jm * w[k];
                            }
                        }
                    }
                    if m == Mat4::ZERO {
                        continue;
                    }
                    v.position = m.transform_point3(Vec3::from(v.position)).into();
                    // Cofactor matrix: correct for normals under non-uniform scale.
                    let (c0, c1, c2) = (
                        m.x_axis.truncate(),
                        m.y_axis.truncate(),
                        m.z_axis.truncate(),
                    );
                    let normal_matrix = Mat3::from_cols(c1.cross(c2), c2.cross(c0), c0.cross(c1));
                    v.normal = (normal_matrix * Vec3::from(v.normal))
                        .try_normalize()
                        .unwrap_or(Vec3::Y)
                        .into();
                }
            }
            _ if morphed_normals => {
                for v in &mut self.output {
                    v.normal = Vec3::from(v.normal)
                        .try_normalize()
                        .unwrap_or(Vec3::Y)
                        .into();
                }
            }
            _ => {}
        }
    }
}

/// Transport state for the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playback {
    pub selection: ClipSelection,
    pub time: f32,
    pub playing: bool,
    pub speed: f32,
    pub looping: bool,
}

impl Default for Playback {
    fn default() -> Self {
        Playback {
            selection: ClipSelection::None,
            time: 0.0,
            playing: false,
            speed: 1.0,
            looping: true,
        }
    }
}

impl Playback {
    /// Start state for a freshly loaded scene: clip chosen, paused at the start.
    pub fn for_rig(rig: Option<&Rig>, previous: &Playback) -> Self {
        let selection = rig.map_or(ClipSelection::None, Rig::default_selection);
        Playback {
            selection,
            time: 0.0,
            playing: false,
            speed: previous.speed,
            looping: previous.looping,
        }
    }

    pub fn advance(&mut self, dt: f32, duration: f32) {
        if !self.playing {
            return;
        }
        if duration <= 0.0 {
            self.time = 0.0;
            return;
        }
        self.time += dt * self.speed;
        if self.time >= duration {
            if self.looping {
                self.time = self.time.rem_euclid(duration);
            } else {
                self.time = duration;
                self.playing = false;
            }
        }
    }

    pub fn toggle(&mut self, duration: f32) {
        if !self.playing && !self.looping && self.time >= duration {
            self.time = 0.0;
        }
        self.playing = !self.playing;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(property: Property, interpolation: Interpolation, values: &[f32]) -> Channel {
        let groups = if interpolation == Interpolation::CubicSpline {
            3
        } else {
            1
        };
        let stride = match property {
            Property::Rotation => 4,
            _ => 3,
        };
        let keys = values.len() / stride / groups;
        let times = (0..keys).map(|i| i as f32).collect();
        Channel::new(0, property, interpolation, times, values.to_vec()).unwrap()
    }

    fn sample(c: &Channel, t: f32) -> Vec<f32> {
        let mut out = Vec::new();
        c.sample(t, &mut out);
        out
    }

    #[test]
    fn linear_step_and_clamping() {
        let c = channel(
            Property::Translation,
            Interpolation::Linear,
            &[0.0, 0.0, 0.0, 2.0, 4.0, 6.0],
        );
        assert_eq!(sample(&c, 0.5), vec![1.0, 2.0, 3.0]);
        assert_eq!(sample(&c, -1.0), vec![0.0, 0.0, 0.0]);
        assert_eq!(sample(&c, 5.0), vec![2.0, 4.0, 6.0]);
        let s = channel(
            Property::Translation,
            Interpolation::Step,
            &[0.0, 0.0, 0.0, 2.0, 4.0, 6.0],
        );
        assert_eq!(sample(&s, 0.99), vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn rotation_slerps_and_stays_normalized() {
        let a = Quat::IDENTITY;
        let b = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let values: Vec<f32> = [a.to_array(), b.to_array()].concat();
        let c = channel(Property::Rotation, Interpolation::Linear, &values);
        let q = Quat::from_slice(&sample(&c, 0.5));
        assert!(q.angle_between(Quat::from_rotation_y(std::f32::consts::FRAC_PI_4)) < 1e-4);
        assert!((q.length() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cubic_spline_hits_keys_and_uses_tangents() {
        // Keys at 0 and 1 with zero tangents: smoothstep between the values.
        #[rustfmt::skip]
        let values = [
            0.0, 0.0, 0.0,  0.0, 0.0, 0.0,  0.0, 0.0, 0.0,
            0.0, 0.0, 0.0,  1.0, 1.0, 1.0,  0.0, 0.0, 0.0,
        ];
        let c = channel(Property::Translation, Interpolation::CubicSpline, &values);
        assert_eq!(sample(&c, 0.0), vec![0.0; 3]);
        assert_eq!(sample(&c, 1.0), vec![1.0; 3]);
        assert!((sample(&c, 0.25)[0] - 0.15625).abs() < 1e-6);
    }

    #[test]
    fn rejects_mismatched_values() {
        assert!(Channel::new(
            0,
            Property::Rotation,
            Interpolation::Linear,
            vec![0.0, 1.0],
            vec![0.0; 6]
        )
        .is_none());
        let w = Channel::new(
            0,
            Property::Weights,
            Interpolation::Linear,
            vec![0.0, 1.0],
            vec![0.0; 6],
        )
        .unwrap();
        assert_eq!(w.stride, 3);
    }

    fn rig_with_clip() -> Rig {
        let rest = Trs {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        };
        let node = |parent, x: f32| RigNode {
            parent,
            rest_matrix: Mat4::from_translation(Vec3::new(x, 0.0, 0.0)),
            rest: Trs {
                translation: Vec3::new(x, 0.0, 0.0),
                ..rest
            },
            weights: Vec::new(),
        };
        let clip = Animation::new(
            "move".into(),
            vec![Channel::new(
                0,
                Property::Translation,
                Interpolation::Linear,
                vec![0.0, 2.0],
                vec![0.0, 0.0, 0.0, 0.0, 4.0, 0.0],
            )
            .unwrap()],
        );
        // root (0) -> mid (2) -> leaf (1): the leaf is listed before its
        // parent to exercise the evaluation order.
        let nodes = vec![node(None, 0.0), node(Some(2), 1.0), node(Some(0), 1.0)];
        Rig::new(
            nodes,
            vec![Skin {
                joints: vec![1],
                inverse_bind: vec![Mat4::from_translation(Vec3::new(-2.0, 0.0, 0.0))],
            }],
            vec![clip],
        )
    }

    #[test]
    fn pose_composes_hierarchy_and_joints() {
        let rig = rig_with_clip();
        let rest = rig.pose(ClipSelection::None, 1.0);
        assert_eq!(rest.world[1].w_axis.truncate(), Vec3::new(2.0, 0.0, 0.0));
        // At bind pose the joint matrix is identity.
        assert!(rest.joints[0][0].abs_diff_eq(Mat4::IDENTITY, 1e-6));

        let posed = rig.pose(ClipSelection::One(0), 1.0);
        assert_eq!(posed.world[1].w_axis.truncate(), Vec3::new(2.0, 2.0, 0.0));
        assert_eq!(rig.duration(ClipSelection::All), 2.0);
    }

    #[test]
    fn skinning_follows_joint() {
        let rig = rig_with_clip();
        let base = vec![Vertex {
            position: [2.0, 0.0, 0.0],
            normal: [0.0, 1.0, 0.0],
            uv: [0.0; 2],
        }];
        let mut d = Deform::new(
            &base,
            Some(vec![([0, 0, 0, 0], [2.0, 0.0, 0.0, 0.0])]),
            Vec::new(),
        )
        .unwrap();
        let pose = rig.pose(ClipSelection::One(0), 2.0);
        d.apply(&[], Some(&pose.joints[0]));
        assert_eq!(d.output[0].position, [2.0, 4.0, 0.0]);
        assert_eq!(d.output[0].normal, [0.0, 1.0, 0.0]);
    }

    #[test]
    fn morph_targets_blend() {
        let base = vec![Vertex::default()];
        let target = MorphTarget {
            positions: Some(vec![[0.0, 2.0, 0.0]]),
            normals: None,
        };
        let mut d = Deform::new(&base, None, vec![target]).unwrap();
        d.apply(&[0.25], None);
        assert_eq!(d.output[0].position, [0.0, 0.5, 0.0]);
        assert!(Deform::new(&base, None, Vec::new()).is_none());
    }

    #[test]
    fn default_selection_plays_disjoint_clips_together() {
        let mut rig = rig_with_clip();
        assert_eq!(rig.default_selection(), ClipSelection::One(0));
        let mut other = rig.animations[0].clone();
        other.channels[0].node = 1;
        rig.animations.push(other);
        assert_eq!(rig.default_selection(), ClipSelection::All);
        rig.animations[1].channels[0].node = 0;
        assert_eq!(rig.default_selection(), ClipSelection::One(0));
    }

    #[test]
    fn playback_loops_or_stops() {
        let mut p = Playback {
            playing: true,
            ..Default::default()
        };
        p.advance(2.5, 2.0);
        assert!((p.time - 0.5).abs() < 1e-6 && p.playing);
        p.looping = false;
        p.advance(3.0, 2.0);
        assert_eq!(p.time, 2.0);
        assert!(!p.playing);
        p.toggle(2.0);
        assert_eq!(p.time, 0.0);
        assert!(p.playing);
    }
}
