use std::fs::File;
use std::io::Read;
use ozy::io;

#[derive(Clone, Debug)]
pub struct LineSegment {
    pub p0: glm::TVec4<f32>,
    pub p1: glm::TVec4<f32>
}

impl LineSegment {
    pub fn zero() -> Self {
        LineSegment {
            p0: glm::zero(),
            p1: glm::zero()
        }
    }

    pub fn length(&self) -> f32 {
        f32::sqrt(f32::powi(self.p1.x - self.p0.x, 2) + f32::powi(self.p1.y - self.p0.y, 2))
    }
}

//An infinite plane as defined by a point on the plane and a vector normal to the plane
pub struct Plane {
    pub point: glm::TVec4<f32>,
    pub normal: glm::TVec4<f32>,
}

impl Plane {
    pub fn new(point: glm::TVec4<f32>, normal: glm::TVec4<f32>) -> Self {
        Plane {
            point,
            normal
        }
    }
}

pub struct PlaneBoundaries {
    pub xmin: f32,
    pub xmax: f32,
    pub ymin: f32,
    pub ymax: f32
}

//Axis-aligned bounding box
pub struct AABB {
    pub position: glm::TVec4<f32>,
    pub width: f32,
    pub depth: f32,
    pub height: f32
}

pub struct Sphere {
    focus: glm::TVec3<f32>,
    radius: f32
}

#[derive(Debug)]
pub struct Terrain {
    pub vertices: Vec<glm::TVec3<f32>>,
    pub indices: Vec<u16>,
    pub face_normals: Vec<glm::TVec3<f32>>
}

impl Terrain {
    pub fn from_ozt(path: &str) -> Self {
        let mut terrain_file = match File::open(path) {
            Ok(file) => { file }
            Err(e) => {
                panic!("Error reading {}: {}", path, e);
            }
        };

        let vertices = {
            let byte_count = match io::read_u32(&mut terrain_file, "Error reading byte_count.") {
                Some(count) => { count as usize }
                None => { panic!("Couldn't read byte count"); }
            };

            let mut bytes = vec![0; byte_count];
            if let Err(e) = terrain_file.read_exact(bytes.as_mut_slice()) {
                panic!("Error reading vertex data from file: {}", e);
            }

            let byte_step = 12; // One f32 for each of x,y,z
            let mut vertices = Vec::with_capacity(byte_count / byte_step);            
            for i in (0..bytes.len()).step_by(byte_step) {
                let x_bytes = [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]];
                let y_bytes = [bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]];
                let z_bytes = [bytes[i + 8], bytes[i + 9], bytes[i + 10], bytes[i + 11]];

                let x = f32::from_le_bytes(x_bytes);
                let y = f32::from_le_bytes(y_bytes);
                let z = f32::from_le_bytes(z_bytes);

                vertices.push(glm::vec3(x, y, z));
            }
            vertices
        };
        
        let indices = {
            let index_count = match io::read_u32(&mut terrain_file, "Error reading index_count") {
                Some(n) => { (n / 2) as usize }
                None => { panic!("Couldn't read byte count"); }
            };
            
            let indices = match io::read_u16_data(&mut terrain_file, index_count) {
                Some(n) => { n }
                None => { panic!("Couldn't read byte count"); }
            };
            indices
        };

        let face_normals = {
            let byte_count = match io::read_u32(&mut terrain_file, "Error reading byte_count") {
                Some(n) => { n as usize }
                None => { panic!("Couldn't read byte count"); }
            };

            let mut bytes = vec![0; byte_count];
            if let Err(e) = terrain_file.read_exact(bytes.as_mut_slice()) {
                panic!("Error reading face normal data from file: {}", e);
            }

            let byte_step = 12;
            let mut normals = Vec::with_capacity(byte_count / byte_step);            
            for i in (0..bytes.len()).step_by(byte_step) {
                let x_bytes = [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]];
                let y_bytes = [bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]];
                let z_bytes = [bytes[i + 8], bytes[i + 9], bytes[i + 10], bytes[i + 11]];

                let x = f32::from_le_bytes(x_bytes);
                let y = f32::from_le_bytes(y_bytes);
                let z = f32::from_le_bytes(z_bytes);

                normals.push(glm::vec3(x, y, z));
            }
            normals
        };

        Self {
            vertices,
            indices,
            face_normals
        }
    }
}

pub fn segment_hit_plane(plane: &Plane, segment: &LineSegment) -> Option<glm::TVec4<f32>> {
    let denominator = glm::dot(&plane.normal, &(segment.p1 - segment.p0));

    //Check for divide-by-zero
    if denominator != 0.0 {
        let x = glm::dot(&plane.normal, &(plane.point - segment.p0)) / denominator;
        if x > 0.0 && x <= 1.0 {
            let result = (1.0 - x) * segment.p0 + x * segment.p1;
            Some(glm::vec4(result.x, result.y, result.z, 1.0))
        } else {
            None
        }        
    } else {
        None
    }
}

pub fn point_in_triangle(test_point: &glm::TVec2<f32>, p0: &glm::TVec2<f32>, p1: &glm::TVec2<f32>, p2: &glm::TVec2<f32>) -> bool {
    //Check if this collision point is actually in the triangle
    let d1 = sign(&test_point, &p0, &p1);
    let d2 = sign(&test_point, &p1, &p2);
    let d3 = sign(&test_point, &p2, &p0);

    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;

    !(has_neg && has_pos)
}

pub fn ray_hit_terrain(terrain: &Terrain, ray_origin: &glm::TVec4<f32>, ray_direction: &glm::TVec4<f32>) -> Option<glm::TVec4<f32>> {
    let mut smallest_t = f32::INFINITY;
    let mut closest_intersection = None;
    for i in (0..terrain.indices.len()).step_by(3) {
        //Get the vertices of the triangle
        let (a, b, c) = get_terrain_triangle(&terrain, i);
        let normal = terrain.face_normals[i / 3];

        let plane = Plane::new(glm::vec4(a.x, a.y, a.z, 1.0), glm::vec4(normal.x, normal.y, normal.z, 1.0));

        //Pre-compute the denominator to avoid divide-by-zero
        //Denominator of zero means that the ray is parallel to the plane
        let denominator = glm::dot(&ray_direction, &plane.normal);
        if denominator == 0.0 { continue; }

        //Compute ray-plane intersection
        let t = glm::dot(&(plane.point - ray_origin), &plane.normal) / denominator;
        let intersection = ray_origin + t * ray_direction;

        let (test_point, a, b, c) = if glm::dot(&plane.normal, &glm::vec4(0.0, 0.0, 1.0, 0.0)) > glm::epsilon::<f32>() {
            (glm::vec2(intersection.x, intersection.y), glm::vec2(a.x, a.y), glm::vec2(b.x, b.y), glm::vec2(c.x, c.y))
        } else {
            (glm::vec2(intersection.x, intersection.z), glm::vec2(a.x, a.z), glm::vec2(b.x, b.z), glm::vec2(c.x, c.z))
        };

        //If the intersection is in the triangle, check if it's the closest intersection to the camera so far
        if point_in_triangle(&test_point, &a, &b, &c) && t > 0.0 && t < smallest_t {
            smallest_t = t;
            closest_intersection = Some(intersection);
        }
    }

    closest_intersection
}

pub fn segment_hit_bounded_plane(plane: &Plane, segment: &LineSegment, boundaries: &PlaneBoundaries) -> Option<glm::TVec4<f32>> {
    let collision_point = segment_hit_plane(&plane, &segment);
    if let Some(point) = collision_point {
        let on_aabb = point.x > boundaries.xmin &&
                      point.x < boundaries.xmax &&
                      point.y > boundaries.ymin &&
                      point.y < boundaries.ymax;

        if on_aabb {
            Some(point)
        } else {
            None
        }
    } else {
        None
    }
}

pub fn point_plane_distance(point: &glm::TVec4<f32>, plane: &Plane) -> f32 {
    glm::dot(&plane.normal, &(point - plane.point))
}

pub fn sign(test: &glm::TVec2<f32>, p0: &glm::TVec2<f32>, p1: &glm::TVec2<f32>) -> f32 {
    (test.x - p1.x) * (p0.y - p1.y) - (p0.x - p1.x) * (test.y - p1.y)
}

pub fn aabb_get_top_plane(aabb: &AABB) -> (Plane, PlaneBoundaries) {    
    let mut pos = aabb.position;
    pos.z += aabb.height * 2.0;
    let plane = Plane::new(pos, glm::vec4(0.0, 0.0, 1.0, 0.0));
    let aabb_boundaries = PlaneBoundaries {
        xmin: -aabb.width + aabb.position.x,
        xmax: aabb.width + aabb.position.x,
        ymin: -aabb.depth + aabb.position.y,
        ymax: aabb.depth + aabb.position.y
    };

    (plane, aabb_boundaries)
}

pub fn aabb_get_bottom_plane(aabb: &AABB) -> (Plane, PlaneBoundaries) {
    let mut pos = aabb.position;
    let plane = Plane::new(pos, glm::vec4(0.0, 0.0, -1.0, 0.0));
    let aabb_boundaries = PlaneBoundaries {
        xmin: -aabb.width + aabb.position.x,
        xmax: aabb.width + aabb.position.x,
        ymin: -aabb.depth + aabb.position.y,
        ymax: aabb.depth + aabb.position.y
    };

    (plane, aabb_boundaries)
}

//The returned plane's reference point is the intersection point
pub fn segment_plane_tallest_collision(segment: &LineSegment, planes: &[Plane]) -> Option<Plane> {    
    let mut max_height = -f32::INFINITY;
    let mut collision = None;
    for plane in planes.iter() {
        if let Some(point) = segment_hit_plane(plane, &segment) {
            if point.z > max_height {
                max_height = point.z;
                collision = Some(Plane::new(point, plane.normal));
            }
        }
    }
    collision
}

pub fn get_terrain_triangle(terrain: &Terrain, triangle_index: usize) -> (glm::TVec3<f32>, glm::TVec3<f32>, glm::TVec3<f32>) {    
    //Get the vertices of the triangle
    let a = terrain.vertices[terrain.indices[triangle_index] as usize];
    let b = terrain.vertices[terrain.indices[triangle_index + 1] as usize];
    let c = terrain.vertices[terrain.indices[triangle_index + 2] as usize];
    (a, b, c)
}