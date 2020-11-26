#version 330 core

in mat3 tangent_matrix;
in vec4 world_space_pos;
in vec4 shadow_space_pos;
in vec2 f_uvs;

out vec4 frag_color;

//Material maps
uniform sampler2D albedo_map;
uniform sampler2D normal_map;
uniform sampler2D roughness_map;

//Shadow map
uniform sampler2D shadow_map;

uniform vec4 sun_direction;
uniform vec4 view_position;
uniform float uv_scale = 1.0;

uniform bool complex_normals = false;
uniform bool visualize_normals = false;

const float AMBIENT = 0.1;

void main() {
    vec2 scaled_uvs = f_uvs * uv_scale;
    vec4 view_direction = normalize(view_position - world_space_pos);

    vec3 albedo = texture(albedo_map, scaled_uvs).xyz;

    vec3 world_space_normal;
    if (complex_normals) {
        vec3 sampled_normal = texture(normal_map, scaled_uvs).xyz;
        vec3 tangent_normal = vec3(sampled_normal.xy * 2.0 - 1.0, sampled_normal.z);
        tangent_normal.y *= -1.0;       //Flip the y because OpenGL loads textures upside-down
        world_space_normal = normalize(tangent_matrix * tangent_normal);
    } else {
        world_space_normal = normalize(tangent_matrix[2]);
    }

    //Determine if the fragment is shadowed
    float shadow = 0.0; 
    vec4 adj_shadow_space_pos = shadow_space_pos * 0.5 + 0.5;
    vec2 texel_size = 1.0 / textureSize(shadow_map, 0);
    
    //Check if this fragment can even receive shadows
    if (adj_shadow_space_pos.z > 1.0 || adj_shadow_space_pos.x < 0.0 || adj_shadow_space_pos.x > 1.0 || adj_shadow_space_pos.y < 0.0 || adj_shadow_space_pos.y > 1.0) {
        shadow = 0.0;
    } else {
        //Do PCF
        //Average the nxn block of shadow texels centered at this pixel
        float bias = 0.001;
        int bound = 1;
        for (int x = -bound; x <= bound; x++) {
            for (int y = -bound; y <= bound; y++) {
                float sampled_depth = texture(shadow_map, adj_shadow_space_pos.xy + vec2(x, y) * texel_size).r;
                shadow += sampled_depth + bias < adj_shadow_space_pos.z ? 1.0 : 0.0;
            }
        }
        shadow /= 9.0;
    }

    float diffuse = max(0.0, dot(vec3(sun_direction), world_space_normal));
    
    float roughness = texture(roughness_map, scaled_uvs).x;
    vec4 halfway = normalize(view_direction + sun_direction);
    float specular_angle = max(0.0, dot(vec3(halfway), world_space_normal));
    float shininess = (1.0 - roughness) * (128.0 - 16.0) + 16.0;
    float specular = pow(specular_angle, shininess);

    vec3 final_color = ((specular + diffuse) * (1.0 - shadow) + AMBIENT) * albedo;
    if (visualize_normals) {
        frag_color = vec4(world_space_normal / 2.0 + 0.5, 1.0);
    } else {
        frag_color = vec4(final_color, 1.0);
    }
}