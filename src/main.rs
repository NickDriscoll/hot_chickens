//#![windows_subsystem = "windows"]
#![allow(non_snake_case)]
extern crate minimp3 as mp3;
extern crate nalgebra_glm as glm;
extern crate openxr as xr;
extern crate ozy_engine as ozy;
extern crate tinyfiledialogs as tfd;

mod collision;
mod gadget;
mod structs;
mod render;
mod xrutil;

use render::{compute_shadow_cascade_matrices, render_main_scene, render_cascaded_shadow_map, CascadedShadowMap, FragmentFlag, RenderEntity, SceneData, ViewData};
use render::{NEAR_DISTANCE, FAR_DISTANCE};

use alto::{sys::ALint, Source, SourceState};
use chrono::offset::Local;
use glfw::{Action, Context, Key, SwapInterval, Window, WindowEvent, WindowHint, WindowMode};
use gl::types::*;
use image::{ImageBuffer, DynamicImage};
use imgui::{ColorEdit, DrawCmd, EditableColor, FontAtlasRefMut, Slider, TextureId, im_str};
use core::ops::RangeInclusive;
use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::{ErrorKind, Seek, SeekFrom};
use std::path::Path;
use std::process::exit;
use std::mem::size_of;
use std::os::raw::c_void;
use std::sync::mpsc;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};
use raw_window_handle::{HasRawWindowHandle, RawWindowHandle};
use ozy::{glutil, io};
use ozy::glutil::ColorSpace;
use ozy::render::{Framebuffer, RenderTarget, ScreenState, TextureKeeper};
use ozy::structs::OptionVec;

use crate::collision::*;
use crate::gadget::*;
use crate::structs::*;

#[cfg(windows)]
use winapi::{um::{winuser::GetWindowDC, wingdi::wglGetCurrentContext}};

fn shader_compile_or_error(vert: &str, frag: &str) -> GLuint {
    match glutil::compile_program_from_files(vert, frag)  { 
        Ok(program) => { program }
        Err(e) => {
            tfd::message_box_ok("Error compiling OpenGL shader.", &format!("An error occurred while compiling an OpenGL shader:\n\nVert:\t{}\nFrag:\t{}\n\n{}", vert, frag, e), tfd::MessageBoxIcon::Error);
            exit(-1);
        }
    }
}

//Sends the message or prints an error
fn send_or_error<T>(s: &Sender<T>, message: T) {
    if let Err(e) = s.send(message) {
        println!("Error sending message to thread: {}", e);
    }
}

fn vec_to_array(vec: glm::TVec3<f32>) -> [f32; 3] {    
    [vec.x, vec.y, vec.z]
}

//Sets a flag to a value or unsets the flag if it already is the value
fn handle_radio_flag<F: Eq + Default>(current_flag: &mut F, new_flag: F) {
    if *current_flag != new_flag {
        *current_flag = new_flag;
    } else {
        *current_flag = F::default();
    }
}

fn reset_player_position(player: &mut Player) {    
    player.tracking_position = glm::vec3(0.0, 0.0, 3.0);
    player.tracking_velocity = glm::zero();
    player.tracked_segment = LineSegment::zero();
    player.last_tracked_segment = LineSegment::zero();
    player.jumps_remaining = Player::MAX_JUMPS;
    player.movement_state = MoveState::Falling;
}

fn resize_main_window(window: &mut Window, framebuffer: &mut Framebuffer, screen_state: &mut ScreenState, size: glm::TVec2<u32>, pos: (i32, i32), window_mode: WindowMode) {    
    framebuffer.size = (size.x as GLsizei, size.y as GLsizei);
    *screen_state = ScreenState::new(glm::vec2(size.x, size.y), glm::identity(), glm::half_pi(), NEAR_DISTANCE, FAR_DISTANCE);
    window.set_monitor(window_mode, pos.0, pos.1, size.x, size.y, Some(144));
}

fn clamp<T: PartialOrd>(x: T, min: T, max: T) -> T {
    if x < min { min }
    else if x > max { max }
    else { x }
}

fn write_matrix_to_buffer(buffer: &mut [f32], index: usize, matrix: glm::TMat4<f32>) {    
    for k in 0..16 {
        buffer[16 * index + k] = matrix[k];
    }
}

fn main() {
    let Z_UP = glm::vec3(0.0, 0.0, 1.0);

    //Do a bunch of OpenXR initialization

    //Initialize the configuration data
    let config = {
        match Configuration::from_file(Configuration::CONFIG_FILEPATH) {
            Some(cfg) => { cfg }
            None => {
                let mut int_options = HashMap::new();
                let mut string_options = HashMap::new();
                int_options.insert(String::from(Configuration::WINDOWED_WIDTH), 1280);
                int_options.insert(String::from(Configuration::WINDOWED_HEIGHT), 720);
                string_options.insert(String::from(Configuration::LEVEL_NAME), String::from("recreate"));
                let c = Configuration {
                    int_options,
                    string_options
                };
                c.to_file(Configuration::CONFIG_FILEPATH);
                c
            }
        }
    };

    //Initialize the OpenXR instance
    let xr_instance = {
        let openxr_entry = xr::Entry::linked();
        let app_info = xr::ApplicationInfo {
            application_name: "hot_chickens",
            application_version: 1,
            engine_name: "ozy_engine",
            engine_version: 1
        };

        //Get the set of OpenXR extentions supported on this system
        let extension_set = match openxr_entry.enumerate_extensions() {
            Ok(set) => { Some(set) }
            Err(e) => {
                println!("Extention enumerations error: {}", e);
                None
            }
        };

        //Make sure the local OpenXR implementation supports OpenGL
        if let Some(set) = &extension_set {
            if !set.khr_opengl_enable {
                println!("OpenXR implementation does not support OpenGL!");
                exit(-1);
            }
        } 

        if let Ok(layer_properties) = openxr_entry.enumerate_layers() {
            for layer in layer_properties.iter() {
                println!("{}", layer.layer_name);
            }
        }
        
        //Create the instance
        let mut instance = None;

        if let Some(ext_set) = &extension_set {
            match openxr_entry.create_instance(&app_info, ext_set, &[]) {
                Ok(inst) => { instance = Some(inst) }
                Err(e) => { 
                    println!("Error creating OpenXR instance: {}", e);
                    instance = None;
                }
            }
        }
        
        instance
    };

    //Get the system id
    let xr_systemid = match &xr_instance {
        Some(inst) => {
            match inst.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY) {
                Ok(id) => { Some(id) }
                Err(e) => { 
                    println!("Error getting OpenXR system: {}", e);
                    None
                }
            }
        }
        None => { None }
    };

    let xr_viewconfiguration_views = match (&xr_instance, xr_systemid) {
        (Some(inst), Some(sys_id)) => {
            match inst.enumerate_view_configuration_views(sys_id, xr::ViewConfigurationType::PRIMARY_STEREO) {
                Ok(vcvs) => { Some(vcvs) }
                Err(e) => {
                    println!("Couldn't get ViewConfigurationViews: {}", e);
                    None
                }
            }
        }
        _ => { None }
    };

    //Get the max swapchain size
    let xr_swapchain_size = match &xr_viewconfiguration_views {
        Some(views) => { Some(glm::vec2(views[0].recommended_image_rect_width, views[0].recommended_image_rect_height)) }
        _ => { None }
    };

    //Get the OpenXR runtime's OpenGL version requirements
    let xr_graphics_reqs = match &xr_instance {
        Some(inst) => {
            match xr_systemid {
                Some(sysid) => {
                    match inst.graphics_requirements::<xr::OpenGL>(sysid) {
                        Ok(reqs) => { Some(reqs) }
                        Err(e) => {
                            println!("Couldn't get OpenXR graphics requirements: {}", e);
                            None
                        }
                    }
                }
                None => { None }
            }
        }
        None => { None }
    };

    //Create the actionset
    let xr_controller_actionset = match &xr_instance {
        Some(inst) => {
            match inst.create_action_set("controllers", "Controllers", 1) {
                Ok(set) => { Some(set) }
                Err(e) => {
                    println!("Error creating XrActionSet: {}", e);
                    None
                }
            }
        }
        None => { None }
    };

    //Create the paths to appropriate equipment
    let left_grip_pose_path = xrutil::make_path(&xr_instance, xrutil::LEFT_GRIP_POSE);
    let left_aim_pose_path = xrutil::make_path(&xr_instance, xrutil::LEFT_AIM_POSE);
    let left_trigger_float_path = xrutil::make_path(&xr_instance, xrutil::LEFT_TRIGGER_FLOAT);
    let right_trigger_float_path = xrutil::make_path(&xr_instance, xrutil::RIGHT_TRIGGER_FLOAT);
    let right_grip_pose_path = xrutil::make_path(&xr_instance, xrutil::RIGHT_GRIP_POSE);
    let right_aim_pose_path = xrutil::make_path(&xr_instance, xrutil::RIGHT_AIM_POSE);
    let right_trackpad_force_path = xrutil::make_path(&xr_instance, xrutil::RIGHT_TRACKPAD_FORCE);
    let right_trackpad_click_path = xrutil::make_path(&xr_instance, xrutil::RIGHT_TRACKPAD_CLICK);
    let left_stick_vector_path = xrutil::make_path(&xr_instance, xrutil::LEFT_STICK_VECTOR2);
    let left_trackpad_vector_path = xrutil::make_path(&xr_instance, xrutil::LEFT_TRACKPAD_VECTOR2);
    let right_a_button_bool_path = xrutil::make_path(&xr_instance, xrutil::RIGHT_A_BUTTON_BOOL);

    //Create the hand subaction paths
    let left_hand_subaction_path = xrutil::make_path(&xr_instance, xr::USER_HAND_LEFT);
    let right_hand_subaction_path = xrutil::make_path(&xr_instance, xr::USER_HAND_RIGHT);

    //Create the XrActions
    let left_hand_pose_action = xrutil::make_action(&left_hand_subaction_path, &xr_controller_actionset, "left_hand_pose", "Left hand pose");
    let left_hand_aim_action = xrutil::make_action::<xr::Posef>(&left_hand_subaction_path, &xr_controller_actionset, "left_hand_aim", "Left hand aim");
    let left_gadget_action = xrutil::make_action::<f32>(&left_hand_subaction_path, &xr_controller_actionset, "left_hand_gadget", "Left hand gadget");
    let right_gadget_action = xrutil::make_action::<f32>(&right_hand_subaction_path, &xr_controller_actionset, "right_hand_gadget", "Right hand gadget");
    let right_hand_grip_action = xrutil::make_action(&right_hand_subaction_path, &xr_controller_actionset, "right_hand_pose", "Right hand pose");
    let right_hand_aim_action = xrutil::make_action(&right_hand_subaction_path, &xr_controller_actionset, "right_hand_aim", "Right hand aim");
    let go_home_action = xrutil::make_action::<bool>(&right_hand_subaction_path, &xr_controller_actionset, "item_menu", "Interact with item menu");
    let player_move_action = xrutil::make_action::<xr::Vector2f>(&left_hand_subaction_path, &xr_controller_actionset, "player_move", "Player movement");

    //Suggest interaction profile bindings
    match (&xr_instance,
           &left_hand_pose_action,
           &left_hand_aim_action,
           &left_gadget_action,
           &right_gadget_action,
           &right_hand_grip_action,
           &player_move_action,
           &left_grip_pose_path,
           &left_aim_pose_path,
           &left_trigger_float_path,
           &right_trigger_float_path,
           &right_grip_pose_path,
           &left_stick_vector_path,
           &left_trackpad_vector_path,
           &right_trackpad_force_path,
           &go_home_action,
           &right_hand_aim_action,
           &right_aim_pose_path,
           &right_trackpad_click_path,
           &right_a_button_bool_path) {
        (Some(inst),
         Some(l_grip_action),
         Some(l_aim_action),
         Some(l_trigger_action),
         Some(r_trigger_action),
         Some(r_action),
         Some(move_action),
         Some(l_grip_path),
         Some(l_aim_path),
         Some(l_trigger_path),
         Some(r_trigger_path),
         Some(r_path),
         Some(l_stick_path),
         Some(l_trackpad_path),
         Some(r_trackpad_force),
         Some(i_menu_action),
         Some(r_aim_action),
         Some(r_aim_path),
         Some(r_track_click_path),
         Some(r_a_button_path)) => {
            //Valve Index
            let bindings = [
                xr::Binding::new(l_grip_action, *l_grip_path),
                xr::Binding::new(l_aim_action, *l_aim_path),
                xr::Binding::new(r_aim_action, *r_aim_path),
                xr::Binding::new(l_trigger_action, *l_trigger_path),
                xr::Binding::new(r_trigger_action, *r_trigger_path),
                xr::Binding::new(r_action, *r_path),
                xr::Binding::new(move_action, *l_stick_path),
                xr::Binding::new(i_menu_action, *r_trackpad_force)
            ];
            xrutil::suggest_bindings(inst, xrutil::VALVE_INDEX_INTERACTION_PROFILE, &bindings);

            //HTC Vive
            let bindings = [
                xr::Binding::new(l_grip_action, *l_grip_path),
                xr::Binding::new(l_aim_action, *l_aim_path),
                xr::Binding::new(r_aim_action, *r_aim_path),
                xr::Binding::new(l_trigger_action, *l_trigger_path),
                xr::Binding::new(r_trigger_action, *r_trigger_path),
                xr::Binding::new(r_action, *r_path),
                xr::Binding::new(move_action, *l_trackpad_path),                   
                xr::Binding::new(i_menu_action, *r_track_click_path)
            ];
            xrutil::suggest_bindings(inst, xrutil::HTC_VIVE_INTERACTION_PROFILE, &bindings);

            //Oculus Touch
            let bindings = [
                xr::Binding::new(l_grip_action, *l_grip_path),
                xr::Binding::new(l_aim_action, *l_aim_path),
                xr::Binding::new(r_aim_action, *r_aim_path),
                xr::Binding::new(l_trigger_action, *l_trigger_path),
                xr::Binding::new(r_trigger_action, *r_trigger_path),
                xr::Binding::new(r_action, *r_path),
                xr::Binding::new(move_action, *l_stick_path),
                xr::Binding::new(i_menu_action, *r_a_button_path)
            ];
            xrutil::suggest_bindings(inst, xrutil::OCULUS_TOUCH_INTERACTION_PROFILE, &bindings);
        }
        _ => {}
    }

    //Initializing GLFW and creating a window

    //Initialize glfw
    let mut glfw = match glfw::init(glfw::FAIL_ON_ERRORS) {
        Ok(g) => { g }
        Err(e) => { panic!("{}", e) }
    };
    
    //Ask for an OpenGL version based on what OpenXR requests. Default to 4.3
    match xr_graphics_reqs {
        Some(r) => { glfw.window_hint(glfw::WindowHint::ContextVersion(r.min_api_version_supported.major() as u32, r.min_api_version_supported.minor() as u32)); }
        None => { glfw.window_hint(glfw::WindowHint::ContextVersion(4, 3)); }
    }

    //Initialize screen state
    let mut screen_state = {
        let window_size = get_window_size(&config);
        let fov_radians = glm::half_pi();
        ScreenState::new(window_size, glm::identity(), fov_radians, NEAR_DISTANCE, FAR_DISTANCE)
    };

    //Create the window
	glfw.window_hint(WindowHint::OpenGlProfile(glfw::OpenGlProfileHint::Core));
	glfw.window_hint(WindowHint::Samples(Some(render::MSAA_SAMPLES)));
    let (mut window, events) = match glfw.create_window(screen_state.get_window_size().x, screen_state.get_window_size().y, "THCATO", glfw::WindowMode::Windowed) {
        Some(stuff) => { stuff }
        None => {
            panic!("Unable to create a window!");
        }
    };
    window.set_resizable(false);
    window.set_key_polling(true);
    window.set_mouse_button_polling(true);
    window.set_cursor_pos_polling(true);
    window.set_scroll_polling(true);
    window.set_framebuffer_size_polling(true);

    //Load OpenGL function pointers
    gl::load_with(|symbol| window.get_proc_address(symbol));

    //OpenGL static configuration
	unsafe {
        gl::Enable(gl::CULL_FACE);										//Enable face culling
        gl::DepthFunc(gl::LEQUAL);										//Pass the fragment with the smallest z-value.
		gl::Enable(gl::FRAMEBUFFER_SRGB); 								//Enable automatic linear->SRGB space conversion
        gl::Enable(gl::BLEND);											//Enable alpha blending
        gl::Enable(gl::MULTISAMPLE);                                    //Enable MSAA
		gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);			//Set blend func to (Cs * alpha + Cd * (1.0 - alpha))
        gl::ClearColor(0.26, 0.4, 0.46, 1.0);							//Set the clear color

		#[cfg(gloutput)]
		{
            use std::ptr;
			gl::Enable(gl::DEBUG_OUTPUT);									                                    //Enable verbose debug output
			gl::Enable(gl::DEBUG_OUTPUT_SYNCHRONOUS);						                                    //Synchronously call the debug callback function
			gl::DebugMessageCallback(ozy::glutil::gl_debug_callback, ptr::null());		                        //Register the debug callback
			gl::DebugMessageControl(gl::DONT_CARE, gl::DONT_CARE, gl::DONT_CARE, 0, ptr::null(), gl::TRUE);
		}
    }

    //Initialize OpenXR session
    let (xr_session, mut xr_framewaiter, mut xr_framestream): (Option<xr::Session<xr::OpenGL>>, Option<xr::FrameWaiter>, Option<xr::FrameStream<xr::OpenGL>>) = match &xr_instance {
        Some(inst) => {
            match xr_systemid {
                Some(sysid) => unsafe {
                    #[cfg(windows)] {
                        let hwnd = match window.raw_window_handle() {
                            RawWindowHandle::Windows(handle) => {
                                handle.hwnd as winapi::shared::windef::HWND
                            }
                            _ => { panic!("Unsupported window system"); }
                        };
                
                        let session_create_info = xr::opengl::SessionCreateInfo::Windows {
                            h_dc: GetWindowDC(hwnd),
                            h_glrc: wglGetCurrentContext()
                        };

                        match inst.create_session::<xr::OpenGL>(sysid, &session_create_info) {
                            Ok(sesh) => {
                                match sesh.0.begin(xr::ViewConfigurationType::PRIMARY_STEREO) {
                                    Ok(_) => { (Some(sesh.0), Some(sesh.1), Some(sesh.2)) }
                                    Err(e) => {
                                        println!("Error beginning XrSession: {}", e);
                                        (None, None, None)
                                    }
                                }                            
                            }
                            Err(e) => {
                                println!("Error initializing OpenXR session: {}", e);
                                (None, None, None)
                            }
                        }
                    }

                    #[cfg(unix)] {
                        (None, None, None)
                    }
                }
                None => { (None, None, None) }
            }
        }
        None => { (None, None, None) }
    };

    //Set controller actionset as active
    match (&xr_session, &xr_controller_actionset) {
        (Some(session), Some(actionset)) => {
            if let Err(e) = session.attach_action_sets(&[&actionset]) {
                println!("Unable to attach action sets: {}", e);
            }
        }
        _ => {}
    }

    //Define tracking space with z-up instead of the default y-up
    let space_pose = {
        let quat = glm::quat_rotation(&Z_UP, &glm::vec3(0.0, 1.0, 0.0));
        xr::Posef {
            orientation: xr::Quaternionf {
                x: quat.coords.x,
                y: quat.coords.y,
                z: quat.coords.z,
                w: quat.coords.w,
            },
            position: xr::Vector3f {
                x: 0.0,
                y: 0.0,
                z: 0.0
            }
        }
    };
    let tracking_space = xrutil::make_reference_space(&xr_session, xr::ReferenceSpaceType::STAGE, space_pose);           //Create tracking space
    let view_space = xrutil::make_reference_space(&xr_session, xr::ReferenceSpaceType::VIEW, xr::Posef::IDENTITY);       //Create view space
    
    let left_hand_grip_space = xrutil::make_actionspace(&xr_session, left_hand_subaction_path, &left_hand_pose_action, space_pose); //Create left hand grip space
    let left_hand_aim_space = xrutil::make_actionspace(&xr_session, left_hand_subaction_path, &left_hand_aim_action, space_pose); //Create left hand aim space
    let right_hand_grip_space = xrutil::make_actionspace(&xr_session, right_hand_subaction_path, &right_hand_grip_action, space_pose); //Create right hand grip space
    let right_hand_aim_space = xrutil::make_actionspace(&xr_session, right_hand_subaction_path, &right_hand_aim_action, space_pose); //Create right hand aim space

    //Create swapchains
    let mut xr_swapchains = match (&xr_session, &xr_viewconfiguration_views) {
        (Some(session), Some(viewconfig_views)) => {
            let mut failed = false;
            let mut scs = Vec::with_capacity(viewconfig_views.len());
            for viewconfig in viewconfig_views {
                let create_info = xr::SwapchainCreateInfo {
                    create_flags: xr::SwapchainCreateFlags::EMPTY,
                    usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT | xr::SwapchainUsageFlags::TRANSFER_DST,
                    format: gl::SRGB8_ALPHA8,
                    sample_count: viewconfig.recommended_swapchain_sample_count,
                    width: viewconfig.recommended_image_rect_width,
                    height: viewconfig.recommended_image_rect_height,
                    face_count: 1,
                    array_size: 1,
                    mip_count: 1
                };
    
                match session.create_swapchain(&create_info) {
                    Ok(sc) => { scs.push(sc); }
                    Err(e) => {
                        println!("Error creating swapchain: {}", e); 
                        failed = true;
                        break;
                    }
                }
            }

            if failed { None }
            else { Some(scs) }
        }
        _ => { None }
    };

    //Create swapchain FBO
    let xr_swapchain_framebuffer = unsafe {
        let mut p = 0;
        gl::GenFramebuffers(1, &mut p);
        p
    };

    //MSAA rendertarget which will have the scene rendered into it before blitting to the actual HMD swapchain image
    //This gets around the fact that SteamVR refuses to allocate MSAA rendertargets :) :) :)
    let xr_swapchain_rendertarget = match xr_swapchain_size {
        Some(size) => unsafe { Some(RenderTarget::new_multisampled((size.x as GLint, size.y as GLint), render::MSAA_SAMPLES as GLint)) }
        None => { None }
    };

    let xr_swapchain_images = match &xr_swapchains {
        Some(chains) => {
            let mut failed = false;
            let mut image_arr = Vec::with_capacity(chains.len());
            for chain in chains.iter() {
                match chain.enumerate_images() {
                    Ok(images) => {
                        image_arr.push(images);
                    }
                    Err(e) => {
                        println!("Error getting swapchain images: {}", e);
                        failed = true;
                        break;
                    }
                }
            }

            if failed { None }
            else { Some(image_arr) }
        }
        None => { None }
    };

    //Compile shader programs
    let standard_program = shader_compile_or_error("shaders/standard.vert", "shaders/standard.frag");
    let shadow_program = shader_compile_or_error("shaders/shadow.vert", "shaders/shadow.frag");
    let skybox_program = shader_compile_or_error("shaders/skybox.vert", "shaders/skybox.frag");
    let imgui_program = shader_compile_or_error("shaders/ui/imgui.vert", "shaders/ui/imgui.frag");
    
    //Initialize default framebuffer
    let mut default_framebuffer = Framebuffer {
        name: 0,
        size: (screen_state.get_window_size().x as GLsizei, screen_state.get_window_size().y as GLsizei),
        clear_flags: gl::DEPTH_BUFFER_BIT | gl::COLOR_BUFFER_BIT,
        cull_face: gl::BACK
    };

    //Camera state
    let mut mouselook_enabled = false;
    let mut mouse_clicked = false;
    let mut camera_position = glm::vec3(0.0, -8.0, 5.5);
    let mut last_camera_position = camera_position;
    let mut camera_input: glm::TVec3<f32> = glm::zero();             //This is a unit vector in view space that represents the input camera movement vector
    let mut camera_orientation = glm::vec2(0.0, -glm::half_pi::<f32>() * 0.6);
    let mut camera_speed = 5.0;
    let camera_hit_sphere_radius = 0.5;
    let mut camera_collision = true;

    //Initialize shadow data
    let mut shadow_view;
    let cascade_size = 2048;
    let shadow_rendertarget = unsafe { RenderTarget::new_shadow((cascade_size * render::SHADOW_CASCADES as GLint, cascade_size)) };
    let sun_shadow_map = CascadedShadowMap::new(shadow_rendertarget.texture, shadow_program, cascade_size);

    //Initialize scene data struct
    let mut scene_data = SceneData::default();
    scene_data.sun_shadow_map = sun_shadow_map;
    scene_data.skybox_program = skybox_program;

    let shadow_cascade_distances = {
        //Manually picking the cascade distances because math is hard
        //The shadow cascade distances are negative bc they apply to view space
        let mut cascade_distances = [0.0; render::SHADOW_CASCADES + 1];
        cascade_distances[0] = -(render::NEAR_DISTANCE);
        cascade_distances[1] = -(render::NEAR_DISTANCE + 5.0);
        cascade_distances[2] = -(render::NEAR_DISTANCE + 15.0);
        cascade_distances[3] = -(render::NEAR_DISTANCE + 25.0);
        cascade_distances[4] = -(render::NEAR_DISTANCE + 75.0);
        cascade_distances[5] = -(render::NEAR_DISTANCE + 125.0);
        cascade_distances[6] = -(render::NEAR_DISTANCE + 300.0);

        //Compute the clip space distances and save them in the scene_data struct
        for i in 0..cascade_distances.len() {
            let p = screen_state.get_clipping_from_view() * glm::vec4(0.0, 0.0, cascade_distances[i], 1.0);
            scene_data.sun_shadow_map.clip_space_distances[i] = p.z;
        }

        cascade_distances
    };

	//Create the skybox cubemap
	scene_data.skybox_cubemap = unsafe {
		let name = "siege";
		let paths = [
			&format!("skyboxes/{}_rt.tga", name),		//Right side
			&format!("skyboxes/{}_lf.tga", name),		//Left side
			&format!("skyboxes/{}_up.tga", name),		//Up side
			&format!("skyboxes/{}_dn.tga", name),		//Down side
			&format!("skyboxes/{}_bk.tga", name),		//Back side
			&format!("skyboxes/{}_ft.tga", name)		//Front side
		];

		let mut cubemap = 0;
		gl::GenTextures(1, &mut cubemap);
		gl::BindTexture(gl::TEXTURE_CUBE_MAP, cubemap);
		gl::TexParameteri(gl::TEXTURE_CUBE_MAP, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
		gl::TexParameteri(gl::TEXTURE_CUBE_MAP, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
		gl::TexParameteri(gl::TEXTURE_CUBE_MAP, gl::TEXTURE_WRAP_R, gl::CLAMP_TO_EDGE as i32);
		gl::TexParameteri(gl::TEXTURE_CUBE_MAP, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
		gl::TexParameteri(gl::TEXTURE_CUBE_MAP, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);

		//Place each piece of the skybox on the correct face
        //gl::TEXTURE_CUBEMAP_POSITIVE_X + i gets you the right cube face
		for i in 0..6 {
			let image_data = glutil::image_data_from_path(paths[i], ColorSpace::Gamma);
			gl::TexImage2D(gl::TEXTURE_CUBE_MAP_POSITIVE_X + i as u32,
						   0,
						   image_data.internal_format as i32,
						   image_data.width as i32,
						   image_data.height as i32,
						   0,
						   image_data.format,
						   gl::UNSIGNED_BYTE,
				  		   &image_data.data[0] as *const u8 as *const c_void);
		}
		cubemap
	};

    //Initialize texture caching struct
    let mut texture_keeper = TextureKeeper::new();
    let default_tex_params = [  
        (gl::TEXTURE_WRAP_S, gl::REPEAT),
	    (gl::TEXTURE_WRAP_T, gl::REPEAT),
	    (gl::TEXTURE_MIN_FILTER, gl::LINEAR_MIPMAP_LINEAR),
	    (gl::TEXTURE_MAG_FILTER, gl::LINEAR)
    ];

    //Player state
    let mut player = Player {
        tracking_position: glm::zero(),
        tracking_velocity: glm::zero(),
        tracked_segment: LineSegment::zero(),
        last_tracked_segment: LineSegment::zero(),
        movement_state: MoveState::Falling,
        radius: 0.15,
        jumps_remaining: Player::MAX_JUMPS,
        was_holding_jump: false
    };

    //Water gun state
    const MAX_WATER_PRESSURE: f32 = 30.0;
    const MAX_WATER_REMAINING: f32 = 75.0;
    let mut water_gun_force = glm::zero();
    let mut infinite_ammo = false;
    let mut remaining_water = MAX_WATER_REMAINING;
    let mut water_pillar_scale: glm::TVec3<f32> = glm::zero();

    //Water gun graphics data
    let water_cylinder_path = "models/water_cylinder.ozy";
    let water_cylinder_entity_index = scene_data.entities.insert(RenderEntity::from_ozy(water_cylinder_path, standard_program, 1, &mut texture_keeper, &default_tex_params));
    
    //Matrices for relating tracking space and world space
    let mut world_from_tracking = glm::identity();
    let mut tracking_from_world = glm::affine_inverse(world_from_tracking);

    let mut screen_space_mouse = glm::zero();

    //Creating Dear ImGui context
    let mut imgui_context = imgui::Context::create();
    imgui_context.style_mut().use_dark_colors();
    {
        let io = imgui_context.io_mut();
        io.display_size[0] = screen_state.get_window_size().x as f32;
        io.display_size[1] = screen_state.get_window_size().y as f32;
    }

    //Create and upload Dear IMGUI font atlas
    match imgui_context.fonts() {
        FontAtlasRefMut::Owned(atlas) => unsafe {
            let mut tex = 0;
            let font_atlas = atlas.build_alpha8_texture();
            
            let font_atlas_params = [                
                (gl::TEXTURE_WRAP_S, gl::REPEAT),
                (gl::TEXTURE_WRAP_T, gl::REPEAT),
                (gl::TEXTURE_MIN_FILTER, gl::NEAREST),
                (gl::TEXTURE_MAG_FILTER, gl::NEAREST)
            ];

            gl::GenTextures(1, &mut tex);
            gl::BindTexture(gl::TEXTURE_2D, tex);            
            glutil::apply_texture_parameters(&font_atlas_params);
            gl::TexImage2D(gl::TEXTURE_2D, 0, gl::RED as GLsizei, font_atlas.width as GLsizei, font_atlas.height as GLsizei, 0, gl::RED, gl::UNSIGNED_BYTE, font_atlas.data.as_ptr() as _);
            atlas.tex_id = TextureId::new(tex as usize);
        }
        FontAtlasRefMut::Shared(_) => {
            panic!("Not dealing with this case.");
        }
    };
    
    //Load terrain data
    let terrain;
    {
        let terrain_name = match config.string_options.get(Configuration::LEVEL_NAME) {
            Some(name) => { name }
            None => { "testmap" }
        };
        terrain = Terrain::from_ozt(&format!("models/{}.ozt", terrain_name));
        println!("Loaded {} collision triangles from {}.ozt", terrain.indices.len() / 3, terrain_name);

        //Load the scene data from the level file
        match File::open(&format!("maps/{}.lvl", terrain_name)) {
            Ok(mut file) => {
                loop {
                    //Read ozy name
                    let ozy_name = match io::read_pascal_strings(&mut file, 1) {
                        Ok(v) => { v[0].clone() }
                        Err(e) => {
                            //We expect this call to eventually return EOF
                            if e.kind() == ErrorKind::UnexpectedEof {
                                break;
                            }
                            panic!("Error reading from level file: {}", e);
                        }
                    };

                    //Read number of matrices
                    let matrices_count = match io::read_u32(&mut file) {
                        Ok(count) => { count as usize } 
                        Err(e) => {
                            panic!("Error reading from level file: {}", e);
                        }
                    };
                    let matrix_floats = match io::read_f32_data(&mut file, matrices_count as usize * 16) {
                        Ok(floats) => { floats }
                        Err(e) => {
                            panic!("Error reading from level file: {}", e);
                        }
                    };

                    let mut entity = RenderEntity::from_ozy(&format!("models/{}", ozy_name), standard_program, matrices_count, &mut texture_keeper, &default_tex_params);
                    entity.update_buffer(&matrix_floats);                
                    scene_data.entities.insert(entity);
                }                
            }
            Err(e) => {
                panic!("Couldn't open level file: {}", e);
            }
        }
    }

    //Create dragon
    let mut dragon_position = glm::vec3(56.009315, 21.064762, 17.284132);
    let dragon_entity_index = scene_data.entities.insert(RenderEntity::from_ozy("models/dragon.ozy", standard_program, 1, &mut texture_keeper, &default_tex_params));

    //Load gadget models
    let mut wand_entity = RenderEntity::from_ozy("models/wand.ozy", standard_program, 2, &mut texture_keeper, &default_tex_params);
    let mut stick_entity = RenderEntity::from_ozy("models/stick.ozy", standard_program, 2, &mut texture_keeper, &default_tex_params);

    //Gadget state setup
    let mut left_hand_gadget = Gadget::Shotgun;
    let mut right_hand_gadget = Gadget::Shotgun;
    let mut left_gadget_index = scene_data.entities.insert(wand_entity.clone());
    let mut right_gadget_index = scene_data.entities.insert(stick_entity.clone());

    //Set up global flags lol
    let mut is_fullscreen = false;
    let mut wireframe = false;
    let mut true_wireframe = false;
    let mut click_action = ClickAction::None;
    let mut hmd_pov = false;
    let mut do_vsync = true;
    let mut do_imgui = true;
    let mut screenshot_this_frame = false;
    if let Some(_) = &xr_instance {
        hmd_pov = true;
        do_vsync = false;
        glfw.set_swap_interval(SwapInterval::None);
    }

    let mut frame_count = 0;
    let mut last_frame_instant = Instant::now();
    let mut last_xr_render_time = xr::Time::from_nanos(1);
    let mut elapsed_time = 0.0;

    //Init audio system
    const DEFAULT_MUSIC_PATH: &str = "music/town_battle.mp3";
    let mut bgm_volume = 50.0;
    let (audio_sender, audio_receiver) = mpsc::channel();
    thread::spawn(move || {
        const IDEAL_FRAMES_QUEUED: ALint = 10;
        fn load_decoder(path: &str) -> mp3::Decoder<File> {
            let bgm_file = match File::open(path) {
                Ok(f) => { f }
                Err(e) => {
                    println!("Couldn't open bgm file: {}", e);
                    exit(-1);
                }
            };
            mp3::Decoder::new(bgm_file)
        }

        fn set_linearized_gain(ctxt: &alto::Context, volume: f32) {            
            let gain_factor = (f32::exp(volume / 100.0) - 1.0) / (glm::e::<f32>() - 1.0);
            ctxt.set_gain(gain_factor).unwrap();
        }

        //Initializing the OpenAL context
        //This can fail if OpenAL is not installed on the host system
        let alto_context = match alto::Alto::load_default() {
            Ok(a) => { 
                let alto = a;
                match alto.default_output() {
                    Some(string) => {
                        match alto.open(Some(&string)) {
                            Ok(dev) => {
                                match dev.new_context(None) {
                                    Ok(ctxt) => { ctxt }
                                    Err(e) => {
                                        tfd::message_box_ok("OpenAL Error", &format!("Error creating OpenAL context: {}\n\nThe game will still work, but without any audio.", e), tfd::MessageBoxIcon::Warning);
                                        return;
                                    }
                                }
                            }
                            Err(e) => {
                                tfd::message_box_ok("OpenAL Error", &format!("Error opening default audio device: {}\n\nThe game will still work, but without any audio.", e), tfd::MessageBoxIcon::Warning);
                                return;
                            }
                        }
                    }
                    None => {
                        tfd::message_box_ok("OpenAL Error", "No default audio output device found\n\nThe game will still work, but without any audio.", tfd::MessageBoxIcon::Warning);
                        return;
                    }
                }
            }
            Err(e) => {
                tfd::message_box_ok("OpenAL Error", &format!("Error initializing OpenAL: {}\n\nThe game will still work, but without any audio.", e), tfd::MessageBoxIcon::Warning);
                return;
            }
        };
        set_linearized_gain(&alto_context, bgm_volume);

        let mut decoder = load_decoder(DEFAULT_MUSIC_PATH);

        let mut kanye_source = alto_context.new_streaming_source().unwrap();
        let mut kickstart_bgm = true;
        loop {
            //Process all commands from the main thread
            while let Ok(command) = audio_receiver.try_recv() {
                match command {
                    AudioCommand::SetListenerPosition(pos) => { alto_context.set_position(pos).unwrap(); }
                    AudioCommand::SetListenerVelocity(vel) => { alto_context.set_velocity(vel).unwrap(); }
                    AudioCommand::SetListenerOrientation(ori) => { alto_context.set_orientation(ori).unwrap(); }
                    AudioCommand::SetSourcePosition(pos, i) => { kanye_source.set_position(pos).unwrap(); }
                    AudioCommand::SetListenerGain(volume) => { set_linearized_gain(&alto_context, volume); }
                    AudioCommand::SelectNewBGM => {
                        kanye_source.pause();
                        match tfd::open_file_dialog("Choose bgm", "music/", Some((&["*.mp3"], "mp3 files (*.mp3)"))) {
                            Some(res) => {
                                kanye_source.stop();
                                decoder = load_decoder(&res);
                            
                                //Clear out any residual sound data from the old mp3
                                while kanye_source.buffers_queued() > 0 {
                                    kanye_source.unqueue_buffer().unwrap();
                                }
                                kickstart_bgm = true;
                            }
                            None => { kanye_source.play(); }
                        }
                    }
                    AudioCommand::PlayPause => {
                        match kanye_source.state() {
                            SourceState::Playing | SourceState::Initial => {
                                kanye_source.pause();
                                kickstart_bgm = false;
                            }
                            SourceState::Paused | SourceState::Stopped => {
                                kanye_source.play();
                                kickstart_bgm = true;
                            }
                            SourceState::Unknown(code) => { println!("Source is in an unknown state: {}", code); }
                        }
                    }
                }
            }

            //If there are fewer than the ideal frames queued, prepare and queue a frame
            if kanye_source.buffers_queued() < IDEAL_FRAMES_QUEUED {
                match decoder.next_frame() {
                    Ok(frame) => {
                        if frame.channels == 1 {
                            let mut mono_samples = Vec::with_capacity(frame.data.len());
                            for sample in frame.data {
                                mono_samples.push(
                                    alto::Mono {
                                        center: sample
                                    }
                                );
                            }

                            if let Ok(sample_buffer) = alto_context.new_buffer(mono_samples, frame.sample_rate) {
                                kanye_source.queue_buffer(sample_buffer).unwrap();
                            }
                        } else if frame.channels == 2 {
                            let mut stereo_samples = Vec::with_capacity(frame.data.len());
                            for i in (0..frame.data.len()).step_by(2) {
                                stereo_samples.push(
                                    alto::Stereo {
                                        left: frame.data[i],
                                        right: frame.data[i + 1]
                                    }
                                );
                            }

                            if let Ok(sample_buffer) = alto_context.new_buffer(stereo_samples, frame.sample_rate) {
                                kanye_source.queue_buffer(sample_buffer).unwrap();
                            }
                        } else {
                            println!("Audio file must have one or two channels.");
                            return;
                        }
                    }
                    Err(e) => {
                        match e {
                            mp3::Error::Eof => {
                                println!("Looping the mp3");
                                decoder.reader_mut().seek(SeekFrom::Start(0)).unwrap();
                            }
                            _ => { println!("Error decoding mp3 frame: {}", e); }
                        }
                    }
                }
            }

            //Unqueue any processed buffers
            while kanye_source.buffers_processed() > 0 {
                kanye_source.unqueue_buffer().unwrap();
            }

            if kanye_source.state() != SourceState::Playing && kickstart_bgm && kanye_source.buffers_queued() == IDEAL_FRAMES_QUEUED {
                println!("starting...");
                kanye_source.play();
            }

            //Sleeping to avoid throttling a CPU core
            thread::sleep(Duration::from_millis(10));
        }
    });

    let key_directions = {
        let mut hm = HashMap::new();
        hm.insert(Key::W, glm::vec3(0.0, 0.0, -1.0));
        hm.insert(Key::S, glm::vec3(0.0, 0.0, 1.0));
        hm.insert(Key::A, glm::vec3(-1.0, 0.0, 0.0));
        hm.insert(Key::D, glm::vec3(1.0, 0.0, 0.0));
        hm.insert(Key::Q, glm::vec3(0.0, -1.0, 0.0));
        hm.insert(Key::E, glm::vec3(0.0, 1.0, 0.0));
        hm
    };

    //Main loop
    while !window.should_close() {
        let imgui_io = imgui_context.io_mut();
        //Compute the number of seconds since the start of the last frame (i.e at 60fps, delta_time ~= 0.016667)
        let delta_time = {
			let frame_instant = Instant::now();
			let dur = frame_instant.duration_since(last_frame_instant);
			last_frame_instant = frame_instant;
			dur.as_secs_f32()
        };
        elapsed_time += delta_time;
        frame_count += 1;
        imgui_io.delta_time = delta_time;
        let framerate = imgui_io.framerate;

        //Sync OpenXR actions
        if let (Some(session), Some(controller_actionset)) = (&xr_session, &xr_controller_actionset) {
            if let Err(e) = session.sync_actions(&[xr::ActiveActionSet::new(controller_actionset)]) {
                println!("Unable to sync actions: {}", e);
            }
        }

        //Get action states
        let left_stick_state = xrutil::get_actionstate(&xr_session, &player_move_action);
        let left_trigger_state = xrutil::get_actionstate(&xr_session, &left_gadget_action);
        let right_trigger_state = xrutil::get_actionstate(&xr_session, &right_gadget_action);
        let right_trackpad_force_state = xrutil::get_actionstate(&xr_session, &go_home_action);

        //Emergency escape button
        if let Some(state) = right_trackpad_force_state {
            if state.changed_since_last_sync && state.current_state {
                reset_player_position(&mut player);
            }
        }

        //Poll window events and handle them
        glfw.poll_events();
        for (_, event) in glfw::flush_messages(&events) {
            match event {
                WindowEvent::Close => { window.set_should_close(true); }
                WindowEvent::Key(key, _, Action::Press, _) => {
                    match key_directions.get(&key) {
                        Some(dir) => {
                            camera_input += dir;
                        }
                        None => {
                            match key {
                                Key::Escape => { do_imgui = !do_imgui; }
                                Key::LeftShift => {
                                    camera_speed *= 5.0;
                                }
                                Key::LeftControl => {
                                    camera_speed /= 5.0;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                WindowEvent::Key(key, _, Action::Release, _) => {
                    match key_directions.get(&key) {
                        Some(dir) => {
                            camera_input -= dir;
                        }
                        None => {
                            match key {
                                Key::LeftShift => {
                                    camera_speed /= 5.0;
                                }
                                Key::LeftControl => {
                                    camera_speed *= 5.0;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                WindowEvent::MouseButton(glfw::MouseButtonLeft, action, ..) => {
                    match action {
                        Action::Press => {
                            imgui_io.mouse_down[0] = true;
                            mouse_clicked = true;
                        }
                        Action::Release => {
                            imgui_io.mouse_down[0] = false;
                            mouse_clicked = false;
                        }
                        Action::Repeat => {}
                    }
                }
                WindowEvent::MouseButton(glfw::MouseButtonRight, glfw::Action::Press, ..) => {
                    imgui_io.mouse_down[1] = true;
                }
                WindowEvent::MouseButton(glfw::MouseButtonRight, glfw::Action::Release, ..) => {
                    imgui_io.mouse_down[1] = false;
                    if mouselook_enabled {
                        window.set_cursor_mode(glfw::CursorMode::Normal);
                    } else {
                        window.set_cursor_mode(glfw::CursorMode::Hidden);
                    }
                    mouselook_enabled = !mouselook_enabled;
                }
                WindowEvent::CursorPos(x, y) => {
                    imgui_io.mouse_pos = [x as f32, y as f32];
                    screen_space_mouse = glm::vec2(x as f32, y as f32);
                    if mouselook_enabled {
                        const CAMERA_SENSITIVITY_DAMPENING: f32 = 0.002;
                        let offset = glm::vec2(screen_space_mouse.x as f32 - screen_state.get_window_size().x as f32 / 2.0, screen_space_mouse.y as f32 - screen_state.get_window_size().y as f32 / 2.0);
                        camera_orientation += offset * CAMERA_SENSITIVITY_DAMPENING;
                        if camera_orientation.y < -glm::pi::<f32>() {
                            camera_orientation.y = -glm::pi::<f32>();
                        } else if camera_orientation.y > 0.0 {
                            camera_orientation.y = 0.0;
                        }
                    }
                }
                WindowEvent::Scroll(x, y) => {
                    imgui_io.mouse_wheel_h = x as f32;
                    imgui_io.mouse_wheel = y as f32;
                }
                WindowEvent::FramebufferSize(width, height) => {
                    imgui_io.display_size[0] = width as f32;
                    imgui_io.display_size[1] = height as f32;
                }
                _ => {  }
            }
        }
        drop(imgui_io);
        
        //Begin drawing imgui frame
        let imgui_ui = imgui_context.frame();

        //Gravity the player
        const GRAVITY_VELOCITY_CAP: f32 = 10.0;
        const ACCELERATION_GRAVITY: f32 = 20.0;        //20.0 m/s^2
        if player.movement_state != MoveState::Grounded {
            player.tracking_velocity.z -= ACCELERATION_GRAVITY * delta_time;
            if player.tracking_velocity.z > GRAVITY_VELOCITY_CAP {
                player.tracking_velocity.z = GRAVITY_VELOCITY_CAP;
            }
        }

        //Handle player inputs
        {
            const MOVEMENT_SPEED: f32 = 5.0;
            const DEADZONE_MAGNITUDE: f32 = 0.1;

            //Responding to the player's input movement vector 
            if let Some(stick_state) = &left_stick_state {
                if stick_state.changed_since_last_sync {                            
                    if let Some(pose) = xrutil::locate_space(&left_hand_aim_space, &tracking_space, stick_state.last_change_time) {
                        let hand_space_vec = glm::vec4(stick_state.current_state.x, stick_state.current_state.y, 0.0, 0.0);
                        let magnitude = glm::length(&hand_space_vec);
                        if magnitude < DEADZONE_MAGNITUDE {
                            if player.movement_state == MoveState::Grounded {                                
                                player.tracking_velocity.x = 0.0;
                                player.tracking_velocity.y = 0.0;
                            }
                        } else {
                            //World space untreated vector
                            let untreated = xrutil::pose_to_mat4(&pose, &world_from_tracking) * hand_space_vec;
                            let ugh = glm::normalize(&glm::vec3(untreated.x, untreated.y, 0.0)) * MOVEMENT_SPEED * magnitude;                            
                            player.tracking_velocity = glm::vec3(ugh.x, ugh.y, player.tracking_velocity.z);
                            player.movement_state = MoveState::Falling;
                        }
                    }
                }
            }

            //Then match on what gadgets the player is holding            
            match left_hand_gadget {
                Gadget::Shotgun => {

                }
                Gadget::StickyHand => {

                }
                Gadget::WaterCannon => {
                    //Calculate the force of shooting the water gun for the left hand
                    if let Some(state) = left_trigger_state {
                        if let Some(pose) = xrutil::locate_space(&left_hand_aim_space, &tracking_space, last_xr_render_time) {
                            let hand_transform = xrutil::pose_to_mat4(&pose, &world_from_tracking);
                            let hand_space_vec = glm::vec4(0.0, 1.0, 0.0, 0.0);
                            let world_space_vec = hand_transform * hand_space_vec;
        
                            //Calculate water gun force vector
                            water_gun_force = glm::vec4_to_vec3(&(-state.current_state * world_space_vec));
        
                            if state.current_state > 0.0 {
                                water_pillar_scale.y = 100.0;
                                if player.movement_state != MoveState::Falling {
                                    set_player_falling(&mut player);
                                }
                            }
                        }
                    }
        
                    if water_gun_force != glm::zero() && remaining_water > 0.0 {
                        let update_force = water_gun_force * delta_time * MAX_WATER_PRESSURE;
                        if !infinite_ammo {
                            remaining_water -= glm::length(&update_force);
                        }
                        let xz_scale = remaining_water / MAX_WATER_REMAINING;
                        water_pillar_scale.x = xz_scale;
                        water_pillar_scale.z = xz_scale;
                        player.tracking_velocity += update_force;
        
                        if let Some(entity) = scene_data.entities.get_mut_element(water_cylinder_entity_index) {
                            //Update the water gun's pillar of water
                            entity.uv_offset += glm::vec2(0.0, 5.0) * delta_time;
                            entity.uv_scale.y = water_pillar_scale.y;
                        }
                    } else {
                        water_pillar_scale = glm::zero();
                    }
                }
            }

            match right_hand_gadget {
                Gadget::Shotgun => {

                }
                Gadget::StickyHand => {

                }
                Gadget::WaterCannon => {
                    //Calculate the force of shooting the water gun for the right hand
                    if let Some(state) = right_trigger_state {
                        if let Some(pose) = xrutil::locate_space(&right_hand_aim_space, &tracking_space, last_xr_render_time) {
                            let hand_transform = xrutil::pose_to_mat4(&pose, &world_from_tracking);
                            let hand_space_vec = glm::vec4(0.0, 1.0, 0.0, 0.0);
                            let world_space_vec = hand_transform * hand_space_vec;
                            let hand_origin = hand_transform * glm::vec4(0.0, 0.0, 0.0, 1.0);
                            let hand_origin = glm::vec4_to_vec3(&hand_origin);
        
                            //Calculate water gun force vector
                            water_gun_force = glm::vec4_to_vec3(&(-state.current_state * world_space_vec));
        
                            if state.current_state > 0.0 {
                                water_pillar_scale.y = 100.0;
                                if player.movement_state != MoveState::Falling {
                                    set_player_falling(&mut player);
                                }
                            }
                        }
                    }
        
                    if water_gun_force != glm::zero() && remaining_water > 0.0 {
                        let update_force = water_gun_force * delta_time * MAX_WATER_PRESSURE;
                        if !infinite_ammo {
                            remaining_water -= glm::length(&update_force);
                        }
                        let xz_scale = remaining_water / MAX_WATER_REMAINING;
                        water_pillar_scale.x = xz_scale;
                        water_pillar_scale.z = xz_scale;
                        player.tracking_velocity += update_force;
        
                        if let Some(entity) = scene_data.entities.get_mut_element(water_cylinder_entity_index) {
                            //Update the water gun's pillar of water
                            entity.uv_offset += glm::vec2(0.0, 5.0) * delta_time;
                            entity.uv_scale.y = water_pillar_scale.y;
                        }
                    } else {
                        water_pillar_scale = glm::zero();
                    }
                }
            }

            if player.movement_state != MoveState::Falling {
                remaining_water = MAX_WATER_REMAINING;
            }
        }

        //If the user is controlling the camera, force the mouse cursor into the center of the screen
        if mouselook_enabled {
            window.set_cursor_pos(screen_state.get_window_size().x as f64 / 2.0, screen_state.get_window_size().y as f64 / 2.0);
        }

        let camera_velocity = camera_speed * glm::vec4_to_vec3(&(glm::affine_inverse(*screen_state.get_view_from_world()) * glm::vec3_to_vec4(&camera_input)));
        camera_position += camera_velocity * delta_time;

        //Place dragon at clicking position
        if click_action == ClickAction::PlacingDragon && mouse_clicked {
            let fovx_radians = 2.0 * f32::atan(f32::tan(screen_state.get_fov_radians() / 2.0) * screen_state.get_aspect_ratio());
            let max_coords = glm::vec4(
                NEAR_DISTANCE * f32::tan(fovx_radians / 2.0),
                NEAR_DISTANCE * f32::tan(screen_state.get_fov_radians() / 2.0),
                -NEAR_DISTANCE,
                1.0
            );
            let normalized_coords = glm::vec4(
                screen_space_mouse.x * 2.0 / screen_state.get_window_size().x as f32 - 1.0,
                -screen_space_mouse.y * 2.0 / screen_state.get_window_size().y as f32 + 1.0,
                1.0,
                1.0
            );
            let view_space_mouse = glm::matrix_comp_mult(&normalized_coords, &max_coords);
            let world_space_mouse = screen_state.get_world_from_view() * view_space_mouse;

            let ray_origin = glm::vec3(camera_position.x, camera_position.y, camera_position.z);
            let mouse_ray_dir = glm::normalize(&(glm::vec4_to_vec3(&world_space_mouse) - ray_origin));

            //Update dragon's position if the ray hit
            if let Some((_, point)) = ray_hit_terrain(&terrain, &ray_origin, &mouse_ray_dir) {
                dragon_position = point;
            }
        }

        //Construct the dragon's model matrix
        if let Some(entity) = scene_data.entities.get_mut_element(dragon_entity_index) {
            let mm = glm::translation(&dragon_position) * glm::rotation(elapsed_time, &Z_UP) * ozy::routines::uniform_scale(0.5);
            unsafe { entity.update_single_transform(0, &mm); }
            let pos = [mm[12], mm[13], mm[14]];
            send_or_error(&audio_sender, AudioCommand::SetSourcePosition(pos, 0));
        }

        //Update tracking space location
        player.tracking_position += player.tracking_velocity * delta_time;
        world_from_tracking = glm::translation(&player.tracking_position);

        //Collision handling section

        //The user is considered to be always standing on the ground in tracking space
        player.tracked_segment = xrutil::tracked_player_segment(&view_space, &tracking_space, last_xr_render_time, &world_from_tracking);

        //We try to do all work related to terrain collision here in order
        //to avoid iterating over all of the triangles more than once
        for i in (0..terrain.indices.len()).step_by(3) {
            let triangle = get_terrain_triangle(&terrain, i);                              //Get the triangle in question
            let triangle_plane = Plane::new(
                triangle.a,
                triangle.normal
            );
            let triangle_sphere = {
                let focus = 0.5 * (triangle.c + 0.5 * (triangle.a + triangle.b));
                let radius = {
                    let a_dist = glm::distance(&focus, &triangle.a);
                    let b_dist = glm::distance(&focus, &triangle.b);
                    let c_dist = glm::distance(&focus, &triangle.c);
                    glm::max3_scalar(a_dist, b_dist, c_dist)
                };
                Sphere {
                    focus,
                    radius
                }
            };

            //Check if this triangle is hitting the camera
            if camera_collision {
                if glm::distance(&camera_position, &triangle_sphere.focus) < camera_hit_sphere_radius + triangle_sphere.radius {
                    let (dist, point_on_plane) = projected_point_on_plane(&camera_position, &triangle_plane);                
                    if robust_point_in_triangle(&point_on_plane, &triangle) && f32::abs(dist) < camera_hit_sphere_radius {
                        camera_position += triangle.normal * (camera_hit_sphere_radius - dist);
                    } else {
                        //Check if the camera is hitting an edge
                        let (best_dist, best_point) = closest_point_on_triangle(&camera_position, &triangle);

                        if best_dist < camera_hit_sphere_radius {
                            let new_pos = camera_position + glm::normalize(&(camera_position - best_point)) * (camera_hit_sphere_radius - best_dist);
                            camera_position = new_pos;
                        }
                    }
                }
            }

            //Check player capsule against triangle
            const MIN_NORMAL_LIKENESS: f32 = 0.5;
            {
                let player_capsule = Capsule {
                    segment: LineSegment {
                        p0: player.tracked_segment.p0,
                        p1: player.tracked_segment.p1 + glm::vec3(0.0, 0.0, player.radius)
                    },
                    radius: player.radius
                };
                let capsule_ray = glm::normalize(&(player_capsule.segment.p1 - player_capsule.segment.p0));

                //Finding the closest point on the triangle to the line segment of the capsule
                let ref_point = match ray_hit_plane(&player_capsule.segment.p0, &capsule_ray, &triangle_plane) {
                    Some((_, intersection)) => {
                        if robust_point_in_triangle(&intersection, &triangle) {
                            intersection
                        } else {
                            closest_point_on_triangle(&intersection, &triangle).1
                        }
                    }
                    None => { triangle.a }
                };
                
                //The point on the capsule line-segment that is to be used as the focus for the sphere
                let capsule_ref = closest_point_on_line_segment(&ref_point, &player_capsule.segment.p0, &player_capsule.segment.p1);
                
                //Now do a triangle-sphere test with a sphere at this reference point
                let (dist, point_on_plane) = projected_point_on_plane(&capsule_ref, &triangle_plane);
                
                //Branch on whether or not the sphere is colliding with the face of the triangle or an edge
                if robust_point_in_triangle(&point_on_plane, &triangle) && f32::abs(dist) < player.radius {
                    if glm::dot(&triangle.normal, &Z_UP) >= MIN_NORMAL_LIKENESS {
                        let denom = glm::dot(&triangle.normal, &Z_UP);
                        let t = (glm::dot(&triangle.normal, &(triangle.a - capsule_ref)) + player.radius) / denom;
                        player.tracking_velocity = glm::zero();
                        player.jumps_remaining = Player::MAX_JUMPS;
                        player.tracking_position += Z_UP * t;
                        remaining_water = MAX_WATER_REMAINING;
                    } else {                        
                        player.tracking_position += triangle.normal * (player.radius - dist);
                    }
                } else {
                    let (best_dist, best_point) = closest_point_on_triangle(&capsule_ref, &triangle);

                    if best_dist < player.radius {
                        let push_dir = glm::normalize(&(capsule_ref - best_point));
                        player.tracking_position += push_dir * (player.radius - best_dist);
                        if glm::dot(&push_dir, &Z_UP) >= MIN_NORMAL_LIKENESS {
                            player.tracking_velocity = glm::zero();
                            player.jumps_remaining = Player::MAX_JUMPS;
                        }
                    }
                }
            }
        }

        //After all collision processing has been completed, update the tracking space matrices once more
        world_from_tracking = glm::translation(&player.tracking_position);
        tracking_from_world = glm::affine_inverse(world_from_tracking);

        //Compute the view_projection matrices for the shadow maps
        shadow_view = glm::look_at(&(scene_data.sun_direction * 20.0), &glm::zero(), &Z_UP);

        player.last_tracked_segment = player.tracked_segment.clone();

        //Tell the audio thread about the listener's current state
        {
            //Just doing the match here to determine if the listener should be the HMD or the free camera
            let (listener_pos, listener_vel, listener_forward, listener_up) = match &xr_instance {
                Some(_) => {
                    let head_pose_mat = match xrutil::locate_space(&view_space, &tracking_space, last_xr_render_time) {
                        Some(space) => { xrutil::pose_to_mat4(&space, &world_from_tracking) }
                        None => { glm::identity() }
                    };

                    let pos = player.tracked_segment.p0;
                    let vel = pos - player.last_tracked_segment.p0;
                    let forward = glm::vec4_to_vec3(&(head_pose_mat * glm::vec4(0.0, 0.0, -1.0, 0.0)));
                    let up = glm::vec4_to_vec3(&(head_pose_mat * glm::vec4(0.0, 1.0, 0.0, 0.0)));
                    (vec_to_array(pos), vec_to_array(vel), vec_to_array(forward), vec_to_array(up))
                }
                None => {
                    let camera_vel = camera_position - last_camera_position;
                    let camera_forward = glm::vec4_to_vec3(&(screen_state.get_world_from_view() * glm::vec4(0.0, 0.0, -1.0, 0.0)));
                    let camera_up = glm::vec4_to_vec3(&(screen_state.get_world_from_view() * glm::vec4(0.0, 1.0, 0.0, 0.0)));
                    
                    (vec_to_array(camera_position), vec_to_array(camera_vel), vec_to_array(camera_forward), vec_to_array(camera_up))
                }
            };

            send_or_error(&audio_sender, AudioCommand::SetListenerPosition(listener_pos));
            send_or_error(&audio_sender, AudioCommand::SetListenerVelocity(listener_vel));
            send_or_error(&audio_sender, AudioCommand::SetListenerOrientation((listener_forward, listener_up)));
        }

        last_camera_position = camera_position;

        //Pre-render phase

        //Draw ImGui
        if do_imgui {
            let win = imgui::Window::new(im_str!("Hacking window"));
            if let Some(win_token) = win.begin(&imgui_ui) {
                imgui_ui.text(im_str!("Frametime: {:.2}ms\tFPS: {:.2}\tFrame: {}", delta_time * 1000.0, framerate, frame_count));
                imgui_ui.checkbox(im_str!("Wireframe view"), &mut wireframe);
                imgui_ui.checkbox(im_str!("TRUE wireframe view"), &mut true_wireframe);
                imgui_ui.checkbox(im_str!("Complex normals"), &mut scene_data.complex_normals);
                imgui_ui.checkbox(im_str!("Camera collision"), &mut camera_collision);
                if let Some(_) = &xr_instance {
                    imgui_ui.checkbox(im_str!("HMD Point-of-view"), &mut hmd_pov);
                    imgui_ui.checkbox(im_str!("Infinite ammo"), &mut infinite_ammo);
                } else {
                    if imgui_ui.checkbox(im_str!("Lock FPS (v-sync)"), &mut do_vsync) {
                        if do_vsync {
                            glfw.set_swap_interval(SwapInterval::Sync(1));
                        } else {
                            glfw.set_swap_interval(SwapInterval::None);
                        }
                    }
                }
                imgui_ui.separator();

                //Do visualization radio selection
                imgui_ui.text(im_str!("Debug visualization options:"));
                if imgui_ui.radio_button_bool(im_str!("Visualize normals"), scene_data.fragment_flag == FragmentFlag::Normals) { handle_radio_flag(&mut scene_data.fragment_flag, FragmentFlag::Normals); }
                if imgui_ui.radio_button_bool(im_str!("Visualize LOD zones"), scene_data.fragment_flag == FragmentFlag::LodZones) { handle_radio_flag(&mut scene_data.fragment_flag, FragmentFlag::LodZones); }
                if imgui_ui.radio_button_bool(im_str!("Visualize how shadowed"), scene_data.fragment_flag == FragmentFlag::Shadowed) { handle_radio_flag(&mut scene_data.fragment_flag, FragmentFlag::Shadowed); }
                if imgui_ui.radio_button_bool(im_str!("Visualize shadow cascades"), scene_data.fragment_flag == FragmentFlag::CascadeZones) { handle_radio_flag(&mut scene_data.fragment_flag, FragmentFlag::CascadeZones); }
                imgui_ui.separator();

                imgui_ui.text(im_str!("What does a mouse click do?"));
                if imgui_ui.radio_button_bool(im_str!("Places the dragon"), click_action == ClickAction::PlacingDragon) { handle_radio_flag(&mut click_action, ClickAction::PlacingDragon); }
                imgui_ui.separator();

                imgui_ui.text(im_str!("Lighting controls:"));
                Slider::new(im_str!("Ambient strength")).range(RangeInclusive::new(0.0, 0.5)).build(&imgui_ui, &mut scene_data.ambient_strength);

                let sun_color_editor = ColorEdit::new(im_str!("Sun color"), EditableColor::Float3(&mut scene_data.sun_color));
                if sun_color_editor.build(&imgui_ui) {}

                imgui_ui.separator();

                //Music controls section
                imgui_ui.text(im_str!("Music controls"));
                if Slider::new(im_str!("Master Volume")).range(RangeInclusive::new(0.0, 100.0)).build(&imgui_ui, &mut bgm_volume) {
                    send_or_error(&audio_sender, AudioCommand::SetListenerGain(bgm_volume));
                }

                if imgui_ui.button(im_str!("Play/Pause"), [0.0, 32.0]) {
                    send_or_error(&audio_sender, AudioCommand::PlayPause);
                }

                imgui_ui.same_line(0.0);
                if imgui_ui.button(im_str!("Choose mp3"), [0.0, 32.0]) {
                    send_or_error(&audio_sender, AudioCommand::SelectNewBGM);
                }

                imgui_ui.separator();
                
                //Reset player position button
                if let Some(_) = &xr_instance {
                    if imgui_ui.button(im_str!("Reset player position"), [0.0, 32.0]) {
                        reset_player_position(&mut player);
                    }
                }

                //Fullscreen button
                if imgui_ui.button(im_str!("Toggle fullscreen"), [0.0, 32.0]) {
                    //Toggle window fullscreen
                    if !is_fullscreen {
                        is_fullscreen = true;
                        glfw.with_primary_monitor_mut(|_, opt_monitor| {
                            if let Some(monitor) = opt_monitor {
                                let pos = monitor.get_pos();
                                if let Some(mode) = monitor.get_video_mode() {
                                    resize_main_window(&mut window, &mut default_framebuffer, &mut screen_state, glm::vec2(mode.width, mode.height), pos, WindowMode::FullScreen(monitor));
                                }
                            }
                        });
                    } else {
                        is_fullscreen = false;
                        let window_size = get_window_size(&config);
                        resize_main_window(&mut window, &mut default_framebuffer, &mut screen_state, window_size, (200, 200), WindowMode::Windowed);
                    }
                }

                if imgui_ui.button(im_str!("Print camera position"), [0.0, 32.0]) {
                    println!("Camera position on frame {}: ({}, {}, {})", frame_count, camera_position.x, camera_position.y, camera_position.z);
                }

                if imgui_ui.button(im_str!("Take screenshot"), [0.0, 32.0]) {
                    screenshot_this_frame = true;
                }

                //Do quit button
                if imgui_ui.button(im_str!("Quit"), [0.0, 32.0]) { window.set_should_close(true); }

                //End the window
                win_token.end(&imgui_ui);
            }

            //Shadow cascade viewer
            /*
            let win = imgui::Window::new(im_str!("Shadow map"));
            if let Some(win_token) = win.begin(&imgui_ui) {
                let im = imgui::Image::new(TextureId::new(shadow_rendertarget.texture as usize), [(cascade_size * render::SHADOW_CASCADES as i32 / 6) as f32, (cascade_size / 6) as f32]).uv1([1.0, -1.0]);
                im.build(&imgui_ui);

                win_token.end(&imgui_ui);
            }
            */
        }

        //Create a view matrix from the camera state
        {
            let new_view_matrix = glm::rotation(camera_orientation.y, &glm::vec3(1.0, 0.0, 0.0)) *
                                  glm::rotation(camera_orientation.x, &Z_UP) *
                                  glm::translation(&(-camera_position));
            screen_state.update_view(new_view_matrix);
        }

        //Render
        unsafe {
            //Setting up OpenGL state for 3D rendering
            gl::Enable(gl::DEPTH_TEST);         //Depth test
            gl::Enable(gl::CULL_FACE);          //Backface culling
            gl::Disable(gl::SCISSOR_TEST);      //Disabling scissor test because it gets enabled before 2D rendering

            if wireframe { gl::PolygonMode(gl::FRONT_AND_BACK, gl::LINE); }

            //Render into HMD
            match (&xr_session, &mut xr_swapchains, &xr_swapchain_size, &xr_swapchain_rendertarget, &xr_swapchain_images, &mut xr_framewaiter, &mut xr_framestream, &tracking_space) {
                (Some(session), Some(swapchains), Some(sc_size), Some(sc_rendertarget), Some(sc_images), Some(framewaiter), Some(framestream), Some(t_space)) => {
                    let swapchain_size = glm::vec2(sc_size.x as GLint, sc_size.y as GLint);
                    match framewaiter.wait() {
                        Ok(wait_info) => {
                            last_xr_render_time = wait_info.predicted_display_time;
                            framestream.begin().unwrap();
                            let (viewflags, views) = session.locate_views(xr::ViewConfigurationType::PRIMARY_STEREO, wait_info.predicted_display_time, t_space).unwrap();
                            
                            //Fetch the hand poses from the runtime
                            let left_grip_pose = xrutil::locate_space(&left_hand_grip_space, &tracking_space, wait_info.predicted_display_time);
                            let right_grip_pose = xrutil::locate_space(&right_hand_grip_space, &tracking_space, wait_info.predicted_display_time);
                            let right_hand_aim_pose = xrutil::locate_space(&right_hand_aim_space, &tracking_space, wait_info.predicted_display_time);

                            //Right here is where we want to update the controller objects' transforms
                            {
                                if let Some(pose) = &left_grip_pose {
                                    if let Some(entity) = scene_data.entities.get_mut_element(left_gadget_index) {
                                        entity.update_single_transform(0, &xrutil::pose_to_mat4(pose, &world_from_tracking))
                                    }
                                }
                                if let Some(pose) = &right_grip_pose {
                                    if let Some(entity) = scene_data.entities.get_mut_element(right_gadget_index) {
                                        entity.update_single_transform(0, &xrutil::pose_to_mat4(pose, &world_from_tracking))
                                    }
                                }
                            }

                            if let Some(p) = right_hand_aim_pose {
                                if let Some(entity) = scene_data.entities.get_mut_element(water_cylinder_entity_index) {
                                    let mm = xrutil::pose_to_mat4(&p, &world_from_tracking) * glm::scaling(&water_pillar_scale);
                                    entity.update_single_transform(0, &mm);
                                }
                            }

                            if let Some(pose) = xrutil::locate_space(&view_space, &tracking_space, wait_info.predicted_display_time) {
                                //Render shadow map
                                shadow_rendertarget.bind();
                                let v_mat = xrutil::pose_to_viewmat(&pose, &tracking_from_world);
                                let projection = *screen_state.get_clipping_from_view();
                                scene_data.sun_shadow_map.matrices = compute_shadow_cascade_matrices(&shadow_cascade_distances, &shadow_view, &v_mat, &projection);
                                render_cascaded_shadow_map(&scene_data.sun_shadow_map, scene_data.entities.as_slice());

                                //Draw the companion view if we're showing HMD POV
                                if hmd_pov {
                                    let v_world_pos = xrutil::pose_to_mat4(&pose, &world_from_tracking);
                                    let view_state = ViewData::new(
                                        glm::vec3(v_world_pos[12], v_world_pos[13], v_world_pos[14]),
                                        v_mat,
                                        projection
                                    );
                                    default_framebuffer.bind();
                                    render_main_scene(&scene_data, &view_state);
                                }
                            }

                            for i in 0..views.len() {
                                let image_index = swapchains[i].acquire_image().unwrap();
                                swapchains[i].wait_image(xr::Duration::INFINITE).unwrap();

                                //Compute view projection matrix
                                //We have to translate to right-handed z-up from right-handed y-up
                                let eye_pose = views[i].pose;
                                let fov = views[i].fov;
                                let view_matrix = xrutil::pose_to_viewmat(&eye_pose, &tracking_from_world);
                                let eye_world_matrix = xrutil::pose_to_mat4(&eye_pose, &world_from_tracking);

                                //Use the fov to get the t, b, l, and r values of the perspective matrix
                                let near_value = NEAR_DISTANCE;
                                let far_value = FAR_DISTANCE;
                                let l = near_value * f32::tan(fov.angle_left);
                                let r = near_value * f32::tan(fov.angle_right);
                                let t = near_value * f32::tan(fov.angle_up);
                                let b = near_value * f32::tan(fov.angle_down);
                                let perspective = glm::mat4(
                                    2.0 * near_value / (r - l), 0.0, (r + l) / (r - l), 0.0,
                                    0.0, 2.0 * near_value / (t - b), (t + b) / (t - b), 0.0,
                                    0.0, 0.0, -(far_value + near_value) / (far_value - near_value), -2.0 * far_value * near_value / (far_value - near_value),
                                    0.0, 0.0, -1.0, 0.0
                                );

                                //Actually rendering
                                sc_rendertarget.bind();   //Rendering into an MSAA rendertarget
                                let view_data = ViewData::new(
                                    glm::vec3(eye_world_matrix[12], eye_world_matrix[13], eye_world_matrix[14]),
                                    view_matrix,
                                    perspective
                                );
                                render_main_scene(&scene_data, &view_data);

                                //Blit the MSAA image into the swapchain image
                                let color_texture = sc_images[i][image_index as usize];
                                gl::BindFramebuffer(gl::FRAMEBUFFER, xr_swapchain_framebuffer);
                                gl::FramebufferTexture2D(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, color_texture, 0);
                                gl::BindFramebuffer(gl::READ_FRAMEBUFFER, sc_rendertarget.framebuffer.name);
                                gl::BlitFramebuffer(0, 0, sc_size.x as GLint, sc_size.y as GLint, 0, 0, sc_size.x as GLint, sc_size.y as GLint, gl::COLOR_BUFFER_BIT, gl::NEAREST);

                                swapchains[i].release_image().unwrap();
                            }

                            //End the frame
                            //TODO: Figure out why image_array_index has to always be zero now
                            let end_result = framestream.end(wait_info.predicted_display_time, xr::EnvironmentBlendMode::OPAQUE,
                                &[&xr::CompositionLayerProjection::new()
                                    .space(t_space)
                                    .views(&[
                                        xr::CompositionLayerProjectionView::new()
                                            .pose(views[0].pose)
                                            .fov(views[0].fov)
                                            .sub_image( 
                                                xr::SwapchainSubImage::new()
                                                    .swapchain(&swapchains[0])
                                                    .image_array_index(0)
                                                    .image_rect(xr::Rect2Di {
                                                        offset: xr::Offset2Di { x: 0, y: 0 },
                                                        extent: xr::Extent2Di {width: swapchain_size.x, height: swapchain_size.y}
                                                    })
                                            ),
                                        xr::CompositionLayerProjectionView::new()
                                            .pose(views[1].pose)
                                            .fov(views[1].fov)
                                            .sub_image(
                                                xr::SwapchainSubImage::new()
                                                    .swapchain(&swapchains[1])
                                                    .image_array_index(0)
                                                    .image_rect(xr::Rect2Di {
                                                        offset: xr::Offset2Di { x: 0, y: 0 },
                                                        extent: xr::Extent2Di {width: swapchain_size.x, height: swapchain_size.y}
                                                    })
                                            )
                                    ])
                                ]
                            );

                            if let Err(e) = end_result {
                                println!("Framestream end error: {}", e);
                            }
                        }
                        Err(e) => {
                            println!("Error doing framewaiter.wait(): {}", e);
                        }
                    }
                }
                _ => {}
            }

            //Main window rendering
            if !hmd_pov {
                //Render shadows
                shadow_rendertarget.bind();
                let projection = *screen_state.get_clipping_from_view();
                scene_data.sun_shadow_map.matrices = compute_shadow_cascade_matrices(&shadow_cascade_distances, &shadow_view, screen_state.get_view_from_world(), &projection);
                render_cascaded_shadow_map(&scene_data.sun_shadow_map, scene_data.entities.as_slice());

                //Render main scene
                let freecam_viewdata = ViewData::new(
                    camera_position,
                    *screen_state.get_view_from_world(),
                    *screen_state.get_clipping_from_view()
                );
                default_framebuffer.bind();
                render_main_scene(&scene_data, &freecam_viewdata);
            }

            //Take a screenshot here as to not get the dev gui in it
            if screenshot_this_frame {
                let mut buffer = vec![0u8; (screen_state.get_window_size().x * screen_state.get_window_size().y) as usize * 4];
                gl::ReadPixels(0, 0, screen_state.get_window_size().x as GLint, screen_state.get_window_size().y as GLint, gl::RGBA, gl::UNSIGNED_BYTE, buffer.as_mut_slice() as *mut [u8] as *mut c_void);

                let dynamic_image = match ImageBuffer::from_raw(screen_state.get_window_size().x, screen_state.get_window_size().y, buffer) {
                    Some(im) => { Some(DynamicImage::ImageRgba8(im).flipv()) }
                    None => { 
                        println!("Unable to convert raw to image::DynamicImage");
                        None
                    }
                };

                if let Some(dyn_image) = dynamic_image {
                    //Create the screenshot directory if there isn't one
                    let screenshot_dir = "screenshots";
                    if !Path::new(screenshot_dir).is_dir() {
                        if let Err(e) = fs::create_dir(screenshot_dir) {
                            println!("Unable to create screenshot directory: {}", e);
                        }
                    }

                    if let Err(e) = dyn_image.save(format!("{}/{}.png", screenshot_dir, Local::now().format("%F_%H%M%S"))) {
                        println!("Error taking screenshot: {}", e);
                    }
                }

                screenshot_this_frame = false;
            }

            //Render 2D elements
            gl::PolygonMode(gl::FRONT_AND_BACK, gl::FILL);  //Make sure we're not doing wireframe rendering
            gl::Disable(gl::DEPTH_TEST);                    //Disable depth testing
            gl::Disable(gl::CULL_FACE);                     //Disable any face culling
            gl::Enable(gl::SCISSOR_TEST);                   //Scissor test enable for Dear ImGui clipping
            gl::BindFramebuffer(gl::FRAMEBUFFER, default_framebuffer.name);
            gl::Viewport(0, 0, default_framebuffer.size.0, default_framebuffer.size.1);
            if true_wireframe { gl::PolygonMode(gl::FRONT_AND_BACK, gl::LINE); }

            //Render Dear ImGui
            gl::UseProgram(imgui_program);
            glutil::bind_matrix4(imgui_program, "projection", screen_state.get_clipping_from_screen());
            {
                let draw_data = imgui_ui.render();
                if draw_data.total_vtx_count > 0 {
                    for list in draw_data.draw_lists() {
                        let vert_size = 8;
                        let mut verts = vec![0.0; list.vtx_buffer().len() * vert_size];

                        let mut current_vertex = 0;
                        let vtx_buffer = list.vtx_buffer();
                        for vtx in vtx_buffer.iter() {
                            verts[current_vertex * vert_size] = vtx.pos[0];
                            verts[current_vertex * vert_size + 1] = vtx.pos[1];
                            verts[current_vertex * vert_size + 2] = vtx.uv[0];
                            verts[current_vertex * vert_size + 3] = vtx.uv[1];
    
                            verts[current_vertex * vert_size + 4] = vtx.col[0] as f32 / 255.0;
                            verts[current_vertex * vert_size + 5] = vtx.col[1] as f32 / 255.0;
                            verts[current_vertex * vert_size + 6] = vtx.col[2] as f32 / 255.0;
                            verts[current_vertex * vert_size + 7] = vtx.col[3] as f32 / 255.0;
    
                            current_vertex += 1;
                        }

                        let imgui_vao = glutil::create_vertex_array_object(&verts, list.idx_buffer(), &[2, 2, 4]);

                        for command in list.commands() {
                            match command {
                                DrawCmd::Elements {count, cmd_params} => {
                                    gl::BindVertexArray(imgui_vao);
                                    gl::ActiveTexture(gl::TEXTURE0);
                                    gl::BindTexture(gl::TEXTURE_2D, cmd_params.texture_id.id() as GLuint);
                                    gl::Scissor(cmd_params.clip_rect[0] as GLint,
                                                screen_state.get_window_size().y as GLint - cmd_params.clip_rect[3] as GLint,
                                                (cmd_params.clip_rect[2] - cmd_params.clip_rect[0]) as GLint,
                                                (cmd_params.clip_rect[3] - cmd_params.clip_rect[1]) as GLint
                                    );
                                    gl::DrawElementsBaseVertex(gl::TRIANGLES, count as GLint, gl::UNSIGNED_SHORT, (cmd_params.idx_offset * size_of::<GLushort>()) as _, cmd_params.vtx_offset as GLint);
                                }
                                DrawCmd::ResetRenderState => { println!("DrawCmd::ResetRenderState."); }
                                DrawCmd::RawCallback {..} => { println!("DrawCmd::RawCallback."); }
                            }
                        }
                        
                        //Free the vertex and index buffers
                        let mut bufs = [0, 0];
                        gl::GetIntegerv(gl::ARRAY_BUFFER_BINDING, &mut bufs[0]);
                        gl::GetIntegerv(gl::ELEMENT_ARRAY_BUFFER_BINDING, &mut bufs[1]);
                        let bufs = [bufs[0] as GLuint, bufs[1] as GLuint];
                        gl::DeleteBuffers(2, &bufs[0]);
                        gl::DeleteVertexArrays(1, &imgui_vao);
                    }
                }
            }
        }

        window.swap_buffers();
    }
}
