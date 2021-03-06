#version 430 core

const float SHININESS_LOWER_BOUND = 8.0;
const float SHININESS_UPPER_BOUND = 128.0;
const float LOD_DIST0 = 20.0;
const float LOD_DIST1 = 40.0;
const float LOD_DIST2 = 170.0;
const float LOD_DIST3 = 240.0;
const vec3 LOD_COLOR0 = vec3(1.0, 0.0, 0.0);
const vec3 LOD_COLOR1 = vec3(1.0, 0.57, 0.0);
const vec3 LOD_COLOR2 = vec3(0.0, 1.0, 0.0);
const vec3 LOD_COLOR3 = vec3(1.0, 0.0, 1.0);
const vec3 LOD_COLOR4 = vec3(0.0, 0.0, 1.0);
const int SHADOW_CASCADES = 6;
const float SHADOW_CASCADES_RECIPROCAL = 1.0 / SHADOW_CASCADES;

in vec3 tangent_sun_direction;
in vec3 tangent_view_position;
in vec3 tangent_space_pos;
in vec4 shadow_space_pos[SHADOW_CASCADES];
in vec3 f_world_pos;
in vec2 scaled_uvs;
in float clip_space_z;

out vec4 frag_color;

//Material textures
uniform sampler2D albedo_tex;
uniform sampler2D normal_tex;
uniform sampler2D roughness_tex;

uniform sampler2D shadow_map;                       //Shadow map texture
uniform vec3 view_position;                         //World space position of the camera
uniform bool complex_normals = false;               //Flag that controls whether or not we sample the normal from the normal map

//Debug visualization flags
uniform bool visualize_normals = false;
uniform bool visualize_shadowed = false;
uniform bool visualize_cascade_zone = false;

uniform vec3 sun_color = vec3(1.0, 1.0, 1.0);
uniform float ambient_strength = 0.0;
uniform float cascade_distances[SHADOW_CASCADES];

vec3 tangent_space_normal;

vec4 simple_diffuse(vec3 color, float diffuse, float ambient) {
    return vec4((diffuse + ambient) * color, 1.0);
}

float determine_shadowed(vec3 f_shadow_pos, int cascade) {
    float bias = 0.0025;
    //float bias = 0.0025 * (1.0 - max(0.0, dot(tangent_space_normal, tangent_sun_direction)));
    vec2 sample_uv = f_shadow_pos.xy;
    sample_uv.x = sample_uv.x * SHADOW_CASCADES_RECIPROCAL;
    sample_uv.x += cascade * SHADOW_CASCADES_RECIPROCAL;
    float sampled_depth = texture(shadow_map, sample_uv).r;
    return sampled_depth + bias < f_shadow_pos.z ? 1.0 : 0.0;
}

void main() {
    //Sample the albedo map for the fragment's base color
    vec3 albedo = texture(albedo_tex, scaled_uvs).xyz;

    //Compute this frag's tangent space normal
    if (complex_normals) {
        vec3 sampled_normal = texture(normal_tex, scaled_uvs).xyz;
        tangent_space_normal = normalize(sampled_normal * 2.0 - 1.0);
    } else {
        tangent_space_normal = vec3(0.0, 0.0, 1.0);
    }
    
    //Early exit if we're visualizing normals
    if (visualize_normals) {
        frag_color = vec4(tangent_space_normal * 0.5 + 0.5, 1.0);
        return;
    }

    //Compute diffuse lighting
    float diffuse = max(0.0, dot(tangent_sun_direction, tangent_space_normal));

    //Determine how shadowed the fragment is
    vec4 adj_shadow_space_pos;
    int shadow_cascade = -1;
    float shadow = 0.0;    
    for (int i = 0; i < SHADOW_CASCADES; i++) {
        if (clip_space_z < cascade_distances[i]) {
            adj_shadow_space_pos = shadow_space_pos[i] * 0.5 + 0.5;
            if (!(
                adj_shadow_space_pos.z < 0.0 ||
                adj_shadow_space_pos.z > 1.0 ||
                adj_shadow_space_pos.x < 0.0 ||
                adj_shadow_space_pos.x > 1.0 ||
                adj_shadow_space_pos.y < 0.0 ||
                adj_shadow_space_pos.y > 1.0
            )) {
                shadow_cascade = i;
            }
            break;
        }
    }

    //Compute how shadowed if we are potentially shadowed
    if (shadow_cascade > -1) {
        if (true) {
            //Do PCF
            //Average the 5x5 block of shadow texels centered at this pixel
            int bound = 1;
            vec2 texel_size = 1.0 / textureSize(shadow_map, 0);
            for (int x = -bound; x <= bound; x++) {
                for (int y = -bound; y <= bound; y++) {
                    shadow += determine_shadowed(vec3(adj_shadow_space_pos.xy + vec2(x, y) * texel_size, adj_shadow_space_pos.z), shadow_cascade);
                }
            }
            shadow /= 9.0;
        } else {
            shadow = determine_shadowed(adj_shadow_space_pos.xyz, shadow_cascade);
        }
    }

    if (visualize_cascade_zone) {
        if (shadow_cascade == 0) {
            frag_color = simple_diffuse(LOD_COLOR0, diffuse * (1.0 - shadow), ambient_strength);
        } else if (shadow_cascade == 1) {
            frag_color = simple_diffuse(LOD_COLOR1, diffuse * (1.0 - shadow), ambient_strength);
        } else if (shadow_cascade == 2) {
            frag_color = simple_diffuse(LOD_COLOR2, diffuse * (1.0 - shadow), ambient_strength);
        } else if (shadow_cascade == 3) {
            frag_color = simple_diffuse(LOD_COLOR3, diffuse * (1.0 - shadow), ambient_strength);
        }
        return;
    }

    //Early exit for shadow visualization
    if (visualize_shadowed) {
        frag_color = vec4(vec3(shadow), 1.0);
        return;
    }

    //Compute specular light w/ blinn-phong
    float roughness = texture(roughness_tex, scaled_uvs).x;
    vec3 view_direction = normalize(tangent_view_position - tangent_space_pos);
    vec3 halfway = normalize(view_direction + tangent_sun_direction);
    float specular_angle = max(0.0, dot(halfway, tangent_space_normal));
    float shininess = (1.0 - roughness) * (SHININESS_UPPER_BOUND - SHININESS_LOWER_BOUND) + SHININESS_LOWER_BOUND;
    float specular = pow(specular_angle, shininess);

    vec3 final_color = sun_color * ((specular + diffuse) * (1.0 - shadow) + ambient_strength) * albedo;
    frag_color = vec4(final_color, 1.0);
}