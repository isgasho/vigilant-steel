//! Common components and behaviors for entities.
//!
//! This contains `Position`, `Velocity`, `Hits`, ... `SysSimu` integrates
//! positions, finds collisions.

use specs::{Component, Entities, Entity, Read, ReadExpect, HashMapStorage,
            Join, LazyUpdate, NullStorage, ReadStorage, System, VecStorage,
            WriteStorage};
use std::f32::consts::PI;
use std::ops::Deref;
use vecmath::*;

use crate::Role;
use crate::blocks::Blocky;
#[cfg(feature = "network")]
use crate::net;
use crate::sat;
use crate::tree;

/// Bounding-box.
#[derive(Debug, Clone)]
pub struct AABox {
    pub xmin: f32,
    pub xmax: f32,
    pub ymin: f32,
    pub ymax: f32,
}

/// A rectangle bounding box.
impl AABox {
    /// Creates a box that doesn't contain anything.
    pub fn empty() -> AABox {
        AABox {
            xmin: ::std::f32::INFINITY,
            xmax: -::std::f32::INFINITY,
            ymin: ::std::f32::INFINITY,
            ymax: -::std::f32::INFINITY,
        }
    }

    /// Returns an array of the 4 corners' coordinates.
    pub fn corners(&self) -> [[f32; 2]; 4] {
        [
            [self.xmin, self.ymin],
            [self.xmax, self.ymin],
            [self.xmax, self.ymax],
            [self.xmin, self.ymax],
        ]
    }

    /// The square of the maximum radius from (0, 0) containing the whole
    /// box.
    pub fn compute_sq_radius(&self) -> f32 {
        self.corners()
            .iter()
            .map(|&c| vec2_square_len(c))
            .max_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap()
    }

    /// Add a square of size 1 by the location of its center.
    pub fn add_square1(&mut self, point: [f32; 2]) {
        *self = AABox {
            xmin: self.xmin.min(point[0] - 0.5),
            xmax: self.xmax.max(point[0] + 0.5),
            ymin: self.ymin.min(point[1] - 0.5),
            ymax: self.ymax.max(point[1] + 0.5),
        };
    }
}

/// Wrapper for entity deletion that triggers network update.
pub fn delete_entity(
    role: Role,
    entities: &Entities,
    lazy: &Read<LazyUpdate>,
    entity: Entity,
) {
    #[cfg(feature = "network")]
    {
        assert!(role.authoritative());
        if role.networked() {
            lazy.insert(entity, net::Delete);
        } else {
            entities.delete(entity).unwrap();
        }
    }

    #[cfg(not(feature = "network"))]
    {
        entities.delete(entity).unwrap();
    }
}

/// Position component, for entities that are somewhere in the world.
#[derive(Debug, Clone)]
pub struct Position {
    pub pos: [f32; 2],
    pub rot: f32,
}

impl Component for Position {
    type Storage = VecStorage<Self>;
}

/// Velocity component, for entities that move.
#[derive(Debug, Clone)]
pub struct Velocity {
    pub vel: [f32; 2],
    pub rot: f32,
}

impl Component for Velocity {
    type Storage = VecStorage<Self>;
}

/// Special collision.
///
/// No built-in collision response, just detect collision and mark that object.
/// Don't even mark the other object.
pub struct DetectCollision {
    pub bounding_box: AABox,
    pub radius: f32,
    pub mass: Option<f32>,
    pub ignore: Option<Entity>,
}

impl Component for DetectCollision {
    type Storage = VecStorage<Self>;
}

/// Attached to a Hit, indicates the effect on the receiving entity.
#[derive(Clone)]
pub enum HitEffect {
    /// Material collision, such as between blocky objects.
    Collision(f32, Entity),
    /// Caught in an explosion.
    Explosion(f32),
}

/// A single collision, stored in the Hits component.
pub struct Hit {
    /// Location of the hit, in this entity's coordinate system.
    pub rel_location: [f32; 2],
    pub effect: HitEffect,
}

/// Collision information: this flags an entity as having collided.
pub struct Hits {
    hits_vec: Vec<Hit>,
}

impl Hits {
    /// Record a `Hit`, possibly creating a new `Hits` component.
    pub fn record<'a>(
        hits: &mut WriteStorage<'a, Hits>,
        ent: Entity,
        hit: Hit,
    ) {
        if let Some(hits) = hits.get_mut(ent) {
            hits.hits_vec.push(hit);
            return;
        }
        hits.insert(
            ent,
            Hits {
                hits_vec: vec![hit],
            },
        ).unwrap();
    }
}

impl Component for Hits {
    type Storage = HashMapStorage<Self>;
}

impl Deref for Hits {
    type Target = [Hit];

    fn deref(&self) -> &[Hit] {
        &self.hits_vec
    }
}

/// Marks that this entity is controlled by the local player.
#[derive(Default)]
pub struct LocalControl;

impl Component for LocalControl {
    type Storage = NullStorage<Self>;
}

/// Delta resource, stores the simulation step.
pub struct DeltaTime(pub f32);

impl Default for DeltaTime {
    fn default() -> DeltaTime {
        DeltaTime(0.0)
    }
}

/// Simulation system, updates positions from velocities.
pub struct SysSimu;

impl<'a> System<'a> for SysSimu {
    type SystemData = (
        Read<'a, DeltaTime>,
        WriteStorage<'a, Position>,
        ReadStorage<'a, Velocity>,
    );

    fn run(&mut self, (dt, mut pos, vel): Self::SystemData) {
        let dt = dt.0;
        for (pos, vel) in (&mut pos, &vel).join() {
            pos.pos = vec2_add(pos.pos, vec2_scale(vel.vel, dt));
            pos.rot += vel.rot * dt;
            pos.rot %= 2.0 * PI;
        }
    }
}

/// Collision detection and response.
pub struct SysCollision;

impl<'a> System<'a> for SysCollision {
    type SystemData = (
        ReadExpect<'a, Role>,
        Read<'a, LazyUpdate>,
        Entities<'a>,
        WriteStorage<'a, Position>,
        WriteStorage<'a, Velocity>,
        ReadStorage<'a, Blocky>,
        ReadStorage<'a, DetectCollision>,
        WriteStorage<'a, Hits>,
    );

    fn run(
        &mut self,
        (
            role,
            lazy,
            entities,
            mut pos,
            mut vel,
            blocky,
            collision,
            mut hits,
        ): Self::SystemData,
){
        assert!(role.authoritative());

        hits.clear();

        // Detect collisions between Blocky objects
        let mut block_hits = Vec::new();
        for (e1, pos1, blocky1) in (&*entities, &pos, &blocky).join() {
            for (e2, pos2, blocky2) in (&*entities, &pos, &blocky).join() {
                if e2 >= e1 {
                    break;
                }
                if blocky1.blocks.is_empty() || blocky2.blocks.is_empty() {
                    continue;
                }
                let rad = blocky1.radius + blocky2.radius;
                if vec2_square_len(vec2_sub(pos1.pos, pos2.pos)) > rad * rad {
                    continue;
                }
                // Detect collisions using tree
                if let Some(hit) = find_collision_tree(
                    pos1,
                    &blocky1.tree,
                    0,
                    pos2,
                    &blocky2.tree,
                    0,
                ) {
                    block_hits.push((e1, e2, hit));
                }
            }
        }

        // Handle the detected collisions
        for (e1, e2, hit) in block_hits {
            handle_collision(
                e1,
                e2,
                &mut pos,
                &mut vel,
                &blocky,
                &mut hits,
                &hit,
                &lazy,
            );
        }

        // Detect collisions between Blocky and DetectCollision objects
        for (e2, pos2, blocky2) in (&*entities, &pos, &blocky).join() {
            if blocky2.blocks.is_empty() {
                continue;
            }
            for (e1, pos1, col1) in (&*entities, &pos, &collision).join() {
                if col1.ignore == Some(e2) {
                    continue;
                }
                let rad = col1.radius + blocky2.radius;
                if vec2_square_len(vec2_sub(pos1.pos, pos2.pos)) > rad * rad {
                    continue;
                }
                // Detect collisions using tree
                if let Some(hit) = find_collision_tree_box(
                    pos1,
                    &col1.bounding_box,
                    pos2,
                    &blocky2.tree,
                    0,
                ) {
                    let vel1 = vel.get(e1).unwrap().vel;
                    let vel2 = vel.get(e2).unwrap().vel;
                    let momentum = vec2_sub(vel1, vel2);
                    let momentum = vec2_len(momentum) * blocky2.mass;
                    // Store collision on the DetectCollision entity
                    store_collision(
                        pos1,
                        hit.location,
                        HitEffect::Collision(momentum, e2),
                        e1,
                        &mut hits,
                    );
                    if let Some(mass1) = col1.mass {
                        let impulse = vec2_scale(vel1, mass1);
                        let vel2 = vel.get_mut(e2).unwrap();
                        vel2.vel = vec2_add(
                            vel2.vel,
                            vec2_scale(impulse, 1.0 / blocky2.mass),
                        );
                        let rel = vec2_sub(hit.location, pos2.pos);
                        vel2.rot += (rel[0] * impulse[1] - rel[1] * impulse[0])
                            / blocky2.inertia;
                    }
                }
            }
        }
    }
}

fn find_collision_tree(
    pos1: &Position,
    tree1: &tree::Tree,
    idx1: usize,
    pos2: &Position,
    tree2: &tree::Tree,
    idx2: usize,
) -> Option<sat::Collision> {
    let n1 = &tree1.0[idx1];
    let n2 = &tree2.0[idx2];
    if let Some(hit) = sat::find(pos1, &n1.bounds, pos2, &n2.bounds) {
        if let tree::Content::Internal(left, right) = n1.content {
            match find_collision_tree(pos1, tree1, left, pos2, tree2, idx2) {
                None => {
                    find_collision_tree(pos1, tree1, right, pos2, tree2, idx2)
                }
                r => r,
            }
        } else if let tree::Content::Internal(left, right) = n2.content {
            match find_collision_tree(pos1, tree1, idx1, pos2, tree2, left) {
                None => {
                    find_collision_tree(pos1, tree1, idx1, pos2, tree2, right)
                }
                r => r,
            }
        } else {
            Some(hit)
        }
    } else {
        None
    }
}

fn find_collision_tree_box(
    pos1: &Position,
    box1: &AABox,
    pos2: &Position,
    tree2: &tree::Tree,
    idx2: usize,
) -> Option<sat::Collision> {
    let n2 = &tree2.0[idx2];
    if let Some(hit) = sat::find(pos1, box1, pos2, &n2.bounds) {
        if let tree::Content::Internal(left, right) = n2.content {
            match find_collision_tree_box(pos1, box1, pos2, tree2, left) {
                None => {
                    find_collision_tree_box(pos1, box1, pos2, tree2, right)
                }
                r => r,
            }
        } else {
            Some(hit)
        }
    } else {
        None
    }
}

pub fn find_collision_tree_ray(
    pos: [f32; 2],
    dir: [f32; 2],
    tree: &tree::Tree,
) -> Option<(f32, [f32; 2])> {
    find_collision_tree_ray_(pos, dir, tree, 0)
}

fn find_collision_tree_ray_(
    pos: [f32; 2],
    dir: [f32; 2],
    tree: &tree::Tree,
    idx: usize,
) -> Option<(f32, [f32; 2])> {
    let n = &tree.0[idx];
    let mut tmin: Option<f32> = None;
    // Left side
    let t = (n.bounds.xmin - pos[0]) / dir[0];
    if t > 0.0 && n.bounds.ymin <= pos[1] + dir[1] * t
        && pos[1] + dir[1] * t <= n.bounds.ymax
    {
        tmin = match tmin {
            Some(m) => Some(m.min(t)),
            None => Some(t),
        }
    }
    // Right side
    let t = (n.bounds.xmax - pos[0]) / dir[0];
    if t > 0.0 && n.bounds.ymin <= pos[1] + dir[1] * t
        && pos[1] + dir[1] * t <= n.bounds.ymax
    {
        tmin = match tmin {
            Some(m) => Some(m.min(t)),
            None => Some(t),
        }
    }
    // Bottom side
    let t = (n.bounds.ymin - pos[1]) / dir[1];
    if t > 0.0 && n.bounds.xmin <= pos[0] + dir[0] * t
        && pos[0] + dir[0] * t <= n.bounds.xmax
    {
        tmin = match tmin {
            Some(m) => Some(m.min(t)),
            None => Some(t),
        }
    }
    // Top side
    let t = (n.bounds.ymax - pos[1]) / dir[1];
    if t > 0.0 && n.bounds.xmin <= pos[0] + dir[0] * t
        && pos[0] + dir[0] * t <= n.bounds.xmax
    {
        tmin = match tmin {
            Some(m) => Some(m.min(t)),
            None => Some(t),
        }
    }

    let tmin = match tmin {
        Some(t) => t,
        None => return None,
    };

    if let tree::Content::Internal(left, right) = n.content {
        match (
            find_collision_tree_ray_(pos, dir, tree, left),
            find_collision_tree_ray_(pos, dir, tree, right),
        ) {
            (None, r) => r,
            (r, None) => r,
            (Some(r1), Some(r2)) => Some(if r1.0 < r2.0 { r1 } else { r2 }),
        }
    } else {
        Some((
            tmin,
            [pos[0] + tmin * dir[0], pos[1] + tmin * dir[1]],
        ))
    }
}

fn store_collision<'a>(
    pos: &Position,
    hit: [f32; 2],
    effect: HitEffect,
    ent: Entity,
    hits: &mut WriteStorage<'a, Hits>,
) {
    let (s, c) = pos.rot.sin_cos();
    let x = hit[0] - pos.pos[0];
    let y = hit[1] - pos.pos[1];
    let rel_loc = [x * c + y * s, -x * s + y * c];

    Hits::record(
        hits,
        ent,
        Hit {
            rel_location: rel_loc,
            effect: effect,
        },
    );
}

const ELASTICITY: f32 = 0.6;

/// Cross-product of planar vector with orthogonal vector.
fn cross(a: [f32; 2], b: f32) -> [f32; 2] {
    [a[1] * b, -a[0] * b]
}

/// Compute cross product of planar vectors and take dot with itself.
fn cross_dot2(a: [f32; 2], b: [f32; 2]) -> f32 {
    let c = a[0] * b[1] - a[1] * b[0];
    c * c
}

fn handle_collision<'a>(
    ent: Entity,
    o_ent: Entity,
    position: &mut WriteStorage<'a, Position>,
    velocity: &mut WriteStorage<'a, Velocity>,
    blocky: &ReadStorage<'a, Blocky>,
    hits: &mut WriteStorage<'a, Hits>,
    hit: &sat::Collision,
    lazy: &Read<'a, LazyUpdate>,
) {
    let blk = blocky.get(ent).unwrap();
    let o_blk = blocky.get(o_ent).unwrap();
    let (impulse, rap, rbp) = {
        let pos = position.get(ent).unwrap();
        let o_pos = position.get(o_ent).unwrap();
        let vel = velocity.get(ent).unwrap();
        let o_vel = velocity.get(o_ent).unwrap();

        // Compute impulse
        let rap = vec2_sub(hit.location, pos.pos);
        let rbp = vec2_sub(hit.location, o_pos.pos);
        let vab1 = vec2_sub(
            vec2_add(vel.vel, cross(rap, -vel.rot)),
            vec2_add(o_vel.vel, cross(rbp, -o_vel.rot)),
        );
        let n = hit.direction;
        let ma = blk.mass;
        let mb = o_blk.mass;
        let ia = blk.inertia;
        let ib = o_blk.inertia;

        (
            (-(1.0 + ELASTICITY) * vec2_dot(vab1, n))
                / (1.0 / ma + 1.0 / mb + cross_dot2(rap, n) / ia
                    + cross_dot2(rbp, n) / ib),
            rap,
            rbp,
        )
    };

    {
        // Compute location in object space
        let pos = position.get_mut(ent).unwrap();
        store_collision(
            pos,
            hit.location,
            HitEffect::Collision(impulse, o_ent),
            ent,
            hits,
        );

        // Move object out of collision
        pos.pos = vec2_add(
            pos.pos,
            vec2_scale(hit.direction, hit.depth * 0.5 + 0.05),
        );

        // Update velocity
        let vel = velocity.get_mut(ent).unwrap();
        vel.vel = vec2_add(
            vel.vel,
            vec2_scale(hit.direction, impulse / blk.mass),
        );
        vel.rot += impulse
            * (rap[0] * hit.direction[1] - rap[1] * hit.direction[0])
            / blk.inertia;
    }
    {
        // Compute location in object space
        let pos = position.get_mut(o_ent).unwrap();
        store_collision(
            pos,
            hit.location,
            HitEffect::Collision(impulse, ent),
            o_ent,
            hits,
        );

        // Move object out of collision
        pos.pos = vec2_add(
            pos.pos,
            vec2_scale(hit.direction, -(hit.depth * 0.5 + 0.05)),
        );

        // Update velocity
        let vel = velocity.get_mut(o_ent).unwrap();
        vel.vel = vec2_add(
            vel.vel,
            vec2_scale(hit.direction, -impulse / o_blk.mass),
        );
        vel.rot += -impulse
            * (rbp[0] * hit.direction[1] - rbp[1] * hit.direction[0])
            / o_blk.inertia;
    }

    #[cfg(feature = "network")]
    lazy.insert(ent, net::Dirty);
}

pub fn affect_area<'a>(
    entities: &Entities<'a>,
    pos: &ReadStorage<'a, Position>,
    blocky: &ReadStorage<'a, Blocky>,
    hits: &mut WriteStorage<'a, Hits>,
    center: [f32; 2],
    radius: f32,
    effect: HitEffect,
) {
    for (ent, pos, blk) in (&**entities, &*pos, &*blocky).join() {
        let entity_radius = blk.radius;
        let dist = vec2_square_len(vec2_sub(pos.pos, center));
        let rad = radius + entity_radius;
        if dist < rad * rad {
            store_collision(pos, center, effect.clone(), ent, hits);
        }
    }
}
