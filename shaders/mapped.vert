#version 430 core

layout (location = 0) in vec3 position;
layout (location = 1) in vec3 tangent;
layout (location = 2) in vec3 bitangent;
layout (location = 3) in vec3 normal;
layout (location = 4) in vec2 uv;

out vec3 tangent_sun_direction;
out vec3 tangent_view_position;
out vec3 tangent_space_pos;
out vec4 shadow_space_pos;
out vec3 f_world_pos;
out vec2 scaled_uvs;

//Constant per-frame uniforms
uniform mat4 shadow_matrix;
uniform vec3 sun_direction;
uniform vec3 view_position;

//Verying per geometry
uniform mat4 view_projection;
uniform mat4 model_matrix;
uniform vec2 uv_scale = vec2(1.0, 1.0);
uniform vec2 uv_offset = vec2(0.0, 0.0);

void main() {
    mat4 normal_matrix = transpose(mat4(inverse(mat3(model_matrix))));
    vec3 T = normalize(vec3(normal_matrix * vec4(tangent, 0.0)));
    vec3 B = normalize(vec3(normal_matrix * vec4(bitangent, 0.0)));
    vec3 N = normalize(vec3(normal_matrix * vec4(normal, 0.0)));
    mat3 tangent_matrix = transpose(mat3(T, B, N));

    vec4 world_space_pos = model_matrix * vec4(position, 1.0);
    shadow_space_pos = shadow_matrix * world_space_pos;

    tangent_space_pos = tangent_matrix * vec3(world_space_pos);
    tangent_sun_direction = tangent_matrix * sun_direction;
    tangent_view_position = tangent_matrix * view_position;
    f_world_pos = vec3(world_space_pos);
    
    scaled_uvs = uv * uv_scale + uv_offset;
    
    gl_Position = view_projection * world_space_pos;
}