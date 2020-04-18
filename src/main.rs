mod utility;
use crate::{utility::debug::*, utility::*};

use ash::version::DeviceV1_0;
use ash::version::EntryV1_0;
use ash::version::InstanceV1_0;
use ash::vk;
use ash::vk_make_version;
use winit::event::{ElementState, Event, KeyboardInput, VirtualKeyCode, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};

use std::ffi::CString;
use std::os::raw::c_char;
use std::ptr;

// Constants
const NUM_FRAMES: usize = 2;

struct Gpu {
    // Physical device
    physical_device: vk::PhysicalDevice,
    _exts: Vec<vk::ExtensionProperties>,
    present_modes: Vec<vk::PresentModeKHR>,
    _memory_properties: vk::PhysicalDeviceMemoryProperties,
    _properties: vk::PhysicalDeviceProperties,
    graphics_queue_idx: u32,
    present_queue_idx: u32,
    // Logical device
    device: ash::Device,
    graphics_queue: vk::Queue,
    present_queue: vk::Queue,
}

// Resolution-dependent rendering stuff.
// TODO: Find a better name for this?
struct Apparatus {
    // - Surface capabilities and formats
    _surface_caps: vk::SurfaceCapabilitiesKHR,
    _surface_formats: Vec<vk::SurfaceFormatKHR>,
    // - Swapchain
    swapchain: vk::SwapchainKHR,
    _swapchain_format: vk::Format,
    _swapchain_extent: vk::Extent2D,
    _swapchain_images: Vec<vk::Image>,
    swapchain_imageviews: Vec<vk::ImageView>,
    // TODO: Depth image
    render_pass: vk::RenderPass,
    framebuffers: Vec<vk::Framebuffer>,
    // - Pipelines
    pipeline_layout: vk::PipelineLayout,
    graphics_pipeline: vk::Pipeline,
    // - Commands
    command_buffers: Vec<vk::CommandBuffer>,
    // - Synchronization primitives. these aren't really resolution-dependent
    //   and could technically be moved outside the struct. They are kept here
    //   because they're closely related to the rest of the members.
    image_available_semaphores: Vec<vk::Semaphore>,
    render_finished_semaphores: Vec<vk::Semaphore>,
    command_buffer_complete_fences: Vec<vk::Fence>,
}

struct VulkanApp {
    window: winit::window::Window,
    _entry: ash::Entry,
    instance: ash::Instance,
    surface: vk::SurfaceKHR,
    // - Extensions
    ext_debug_utils: ash::extensions::ext::DebugUtils,
    ext_surface: ash::extensions::khr::Surface,
    ext_swapchain: ash::extensions::khr::Swapchain,

    gpu: Gpu,
    command_pool: vk::CommandPool,
    vertex_buffer: vk::Buffer,
    vertex_buffer_memory: vk::DeviceMemory,
    apparatus: Apparatus, // Resolution-dependent apparatus

    debug_messenger: vk::DebugUtilsMessengerEXT,
    validation_layers: Vec<String>,
    current_frame: usize,
}

// This is required because the `vk::ShaderModuleCreateInfo` struct's `p_code`
// member expects a *u32, but `include_bytes!()` produces a Vec<u8>.
// TODO: Investigate how to properly address this.
#[allow(clippy::cast_ptr_alignment)]
fn create_shader_module(device: &ash::Device, code: Vec<u8>) -> vk::ShaderModule {
    let shader_module_create_info = vk::ShaderModuleCreateInfo {
        s_type: vk::StructureType::SHADER_MODULE_CREATE_INFO,
        p_next: ptr::null(),
        flags: vk::ShaderModuleCreateFlags::empty(),
        code_size: code.len(),
        p_code: code.as_ptr() as *const u32,
    };

    unsafe {
        device
            .create_shader_module(&shader_module_create_info, None)
            .expect("Failed to create shader module.")
    }
}

impl Apparatus {
    pub fn new(
        window: &winit::window::Window,
        surface: vk::SurfaceKHR,
        gpu: &Gpu,
        command_pool: vk::CommandPool,
        vertex_buffer: vk::Buffer,
        ext_surface: &ash::extensions::khr::Surface,
        ext_swapchain: &ash::extensions::khr::Swapchain,
    ) -> Apparatus {
        let surface_caps = unsafe {
            ext_surface
                .get_physical_device_surface_capabilities(gpu.physical_device, surface)
                .expect("Failed to query for surface capabilities.")
        };

        let surface_formats = unsafe {
            ext_surface
                .get_physical_device_surface_formats(gpu.physical_device, surface)
                .expect("Failed to query for surface formats.")
        };

        // # Create swapchain
        let (swapchain, swapchain_format, swapchain_extent, swapchain_images) = {
            // Set number of images in swapchain
            let image_count = surface_caps.min_image_count.max(NUM_FRAMES as u32);

            // Choose swapchain format (i.e. color buffer format)
            let (swapchain_format, swapchain_color_space) = {
                let surface_format: vk::SurfaceFormatKHR = {
                    *surface_formats
                        .iter()
                        .find(|&f| {
                            f.format == vk::Format::B8G8R8A8_SRGB
                                && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
                        })
                        .unwrap_or(&surface_formats[0])
                };
                (surface_format.format, surface_format.color_space)
            };

            // Choose extent
            let extent = {
                if surface_caps.current_extent.width == u32::max_value() {
                    let window_size = window.inner_size();
                    vk::Extent2D {
                        width: (window_size.width as u32)
                            .max(surface_caps.min_image_extent.width)
                            .min(surface_caps.max_image_extent.width),
                        height: (window_size.height as u32)
                            .max(surface_caps.min_image_extent.height)
                            .min(surface_caps.max_image_extent.height),
                    }
                } else {
                    surface_caps.current_extent
                }
            };

            // Present mode
            let present_mode: vk::PresentModeKHR = {
                *gpu.present_modes
                    .iter()
                    .find(|&&mode| mode == vk::PresentModeKHR::MAILBOX)
                    .unwrap_or(&vk::PresentModeKHR::FIFO)
            };

            let mut info = vk::SwapchainCreateInfoKHR::builder()
                .surface(surface)
                .min_image_count(image_count)
                .image_format(swapchain_format)
                .image_color_space(swapchain_color_space)
                .image_extent(extent)
                .image_array_layers(1)
                .image_usage(
                    vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
                )
                // TODO: Investigate:
                // The vulkan tutorial sets this as `pre_transform(gpu.surface_caps.current_transform)`.
                .pre_transform(vk::SurfaceTransformFlagsKHR::IDENTITY)
                .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                .present_mode(present_mode)
                .clipped(true); // Allow Vulkan to discard operations outside of the renderable space

            // Sharing mode
            let indices = [gpu.graphics_queue_idx, gpu.present_queue_idx];
            if gpu.graphics_queue_idx != gpu.present_queue_idx {
                info = info
                    .image_sharing_mode(vk::SharingMode::CONCURRENT)
                    .queue_family_indices(&indices);
            } else {
                // Graphics and present are the same queue, so it can have
                // exclusive access to the swapchain
                info = info.image_sharing_mode(vk::SharingMode::EXCLUSIVE);
            }

            let swapchain = unsafe {
                ext_swapchain
                    .create_swapchain(&info, None)
                    .expect("Failed to create swapchain.")
            };

            let images = unsafe {
                ext_swapchain
                    .get_swapchain_images(swapchain)
                    .expect("Failed to get swapchain images.")
            };

            (swapchain, swapchain_format, extent, images)
        };

        // # Create swapchain image views
        let swapchain_imageviews = {
            let imageviews: Vec<vk::ImageView> = swapchain_images
                .iter()
                .map(|&image| {
                    let info = vk::ImageViewCreateInfo::builder()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(swapchain_format)
                        .components(vk::ComponentMapping {
                            r: vk::ComponentSwizzle::IDENTITY,
                            g: vk::ComponentSwizzle::IDENTITY,
                            b: vk::ComponentSwizzle::IDENTITY,
                            a: vk::ComponentSwizzle::IDENTITY,
                        })
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: 0,
                            layer_count: 1,
                        });

                    unsafe {
                        gpu.device
                            .create_image_view(&info, None)
                            .expect("Failed to create image view.")
                    }
                })
                .collect();

            imageviews
        };

        // # Create render pass
        let render_pass = {
            let attachments = vec![
                // Color attachment
                vk::AttachmentDescription {
                    format: swapchain_format,
                    flags: vk::AttachmentDescriptionFlags::empty(),
                    samples: vk::SampleCountFlags::TYPE_1,
                    load_op: vk::AttachmentLoadOp::DONT_CARE,
                    store_op: vk::AttachmentStoreOp::STORE,
                    stencil_load_op: vk::AttachmentLoadOp::DONT_CARE,
                    stencil_store_op: vk::AttachmentStoreOp::DONT_CARE,
                    initial_layout: vk::ImageLayout::UNDEFINED,
                    final_layout: vk::ImageLayout::PRESENT_SRC_KHR,
                },
                // TODO: Depth attachment
                // vk::AttachmentDescription {
                //     format: depth_format, // TODO: Choose this format
                //     flags: vk::AttachmentDescriptionFlags::empty(),
                //     samples: vk::SampleCountFlags::TYPE_1,
                //     load_op: vk::AttachmentLoadOp::DONT_CARE,
                //     store_op: vk::AttachmentStoreOp::DONT_CARE,
                //     stencil_load_op: vk::AttachmentLoadOp::LOAD, // ?
                //     stencil_store_op: vk::AttachmentStoreOp::STORE, // ?
                //     initial_layout: vk::ImageLayout::UNDEFINED,
                //     final_layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                // },
            ];

            let color_attachment_ref = [vk::AttachmentReference {
                attachment: 0,
                layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            }];
            // TODO: Depth attachment ref
            // let depth_attachment_ref = vk::AttachmentReference {
            //     attachment: 1,
            //     layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
            // };

            let subpasses = [vk::SubpassDescription::builder()
                .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                .color_attachments(&color_attachment_ref)
                // .depth_stencil_attachment(&depth_attachment_ref)
                .build()];

            // TODO: Subpass dependencies

            let renderpass_create_info = vk::RenderPassCreateInfo::builder()
                .attachments(&attachments)
                .subpasses(&subpasses);
            // TODO: .dependencies(...);

            unsafe {
                gpu.device
                    .create_render_pass(&renderpass_create_info, None)
                    .expect("Failed to create render pass!")
            }
        };

        // # Create framebuffers
        let framebuffers: Vec<vk::Framebuffer> = {
            swapchain_imageviews
                .iter()
                .map(|&imageview| {
                    let attachments = [imageview];

                    let framebuffer_create_info = vk::FramebufferCreateInfo::builder()
                        .render_pass(render_pass)
                        .attachments(&attachments)
                        .width(swapchain_extent.width)
                        .height(swapchain_extent.height)
                        .layers(1);

                    unsafe {
                        gpu.device
                            .create_framebuffer(&framebuffer_create_info, None)
                            .expect("Failed to create Framebuffer!")
                    }
                })
                .collect()
        };

        // # Create graphics pipeline
        let (graphics_pipeline, pipeline_layout) = {
            let vert_shader_module = create_shader_module(
                &gpu.device,
                include_bytes!("../shaders/spv/17-shader-vertexbuffer.vert.spv").to_vec(),
            );
            let frag_shader_module = create_shader_module(
                &gpu.device,
                include_bytes!("../shaders/spv/17-shader-vertexbuffer.frag.spv").to_vec(),
            );

            let main_function_name = CString::new("main").unwrap();

            let shader_stages = [
                vk::PipelineShaderStageCreateInfo::builder()
                    .stage(vk::ShaderStageFlags::VERTEX)
                    .module(vert_shader_module)
                    .name(&main_function_name)
                    .build(),
                vk::PipelineShaderStageCreateInfo::builder()
                    .stage(vk::ShaderStageFlags::FRAGMENT)
                    .module(frag_shader_module)
                    .name(&main_function_name)
                    .build(),
            ];

            // (pos: vec2 + color: vec3) = 5 floats * 4 bytes per float
            const VERTEX_STRIDE: u32 = 20;
            let binding_descriptions = [vk::VertexInputBindingDescription::builder()
                .binding(0)
                .stride(VERTEX_STRIDE)
                .build()];
            let attribute_descriptions = [
                vk::VertexInputAttributeDescription::builder()
                    .location(0)
                    .binding(0)
                    .format(vk::Format::R32G32_SFLOAT)
                    .offset(0)
                    .build(),
                vk::VertexInputAttributeDescription::builder()
                    .location(1)
                    .binding(0)
                    .format(vk::Format::R32G32B32_SFLOAT)
                    .offset(8)
                    .build(),
            ];
            let vertex_input_state_create_info = {
                vk::PipelineVertexInputStateCreateInfo::builder()
                    .vertex_binding_descriptions(&binding_descriptions)
                    .vertex_attribute_descriptions(&attribute_descriptions)
            };

            let vertex_input_assembly_state_info =
                vk::PipelineInputAssemblyStateCreateInfo::builder()
                    .topology(vk::PrimitiveTopology::TRIANGLE_LIST)
                    .build();

            let viewports = [vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: swapchain_extent.width as f32,
                height: swapchain_extent.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            }];

            let scissors = [vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: swapchain_extent,
            }];

            let viewport_state_create_info = vk::PipelineViewportStateCreateInfo::builder()
                .scissors(&scissors)
                .viewports(&viewports)
                .build();

            let rasterization_state_create_info =
                vk::PipelineRasterizationStateCreateInfo::builder()
                    .polygon_mode(vk::PolygonMode::FILL)
                    .cull_mode(vk::CullModeFlags::BACK)
                    .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
                    .line_width(1.0)
                    .build();

            let multisample_state_create_info = vk::PipelineMultisampleStateCreateInfo::builder()
                .rasterization_samples(vk::SampleCountFlags::TYPE_1)
                .build();

            // TODO: Depth
            let depth_state_create_info =
                vk::PipelineDepthStencilStateCreateInfo::builder().build();

            let color_blend_attachment_states = [vk::PipelineColorBlendAttachmentState {
                blend_enable: vk::FALSE,
                color_write_mask: vk::ColorComponentFlags::all(),
                src_color_blend_factor: vk::BlendFactor::ONE,
                dst_color_blend_factor: vk::BlendFactor::ZERO,
                color_blend_op: vk::BlendOp::ADD,
                src_alpha_blend_factor: vk::BlendFactor::ONE,
                dst_alpha_blend_factor: vk::BlendFactor::ZERO,
                alpha_blend_op: vk::BlendOp::ADD,
            }];

            let color_blend_state = vk::PipelineColorBlendStateCreateInfo::builder()
                .attachments(&color_blend_attachment_states)
                .blend_constants([0.0, 0.0, 0.0, 0.0])
                .build();

            let pipeline_layout_create_info = vk::PipelineLayoutCreateInfo::builder();

            let pipeline_layout = unsafe {
                gpu.device
                    .create_pipeline_layout(&pipeline_layout_create_info, None)
                    .expect("Failed to create pipeline layout.")
            };

            let graphic_pipeline_create_infos = [vk::GraphicsPipelineCreateInfo::builder()
                .stages(&shader_stages)
                .vertex_input_state(&vertex_input_state_create_info)
                .input_assembly_state(&vertex_input_assembly_state_info)
                // Skip tesselation
                .viewport_state(&viewport_state_create_info)
                .rasterization_state(&rasterization_state_create_info)
                .multisample_state(&multisample_state_create_info)
                .depth_stencil_state(&depth_state_create_info)
                .color_blend_state(&color_blend_state)
                // No dynamic state
                .layout(pipeline_layout)
                .render_pass(render_pass)
                .subpass(0)
                .build()];

            let graphics_pipelines = unsafe {
                gpu.device
                    .create_graphics_pipelines(
                        vk::PipelineCache::null(),
                        &graphic_pipeline_create_infos,
                        None,
                    )
                    .expect("Failed to create Graphics Pipeline.")
            };

            unsafe {
                gpu.device.destroy_shader_module(vert_shader_module, None);
                gpu.device.destroy_shader_module(frag_shader_module, None);
            }

            (graphics_pipelines[0], pipeline_layout)
        };

        // # Allocate command buffers
        let command_buffers = {
            let info = vk::CommandBufferAllocateInfo::builder()
                .command_pool(command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(NUM_FRAMES as u32);

            unsafe {
                gpu.device
                    .allocate_command_buffers(&info)
                    .expect("Failed to allocate command buffer.")
            }
        };

        // # Record command buffers
        for (i, &command_buffer) in command_buffers.iter().enumerate() {
            let command_buffer_begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::SIMULTANEOUS_USE);

            unsafe {
                gpu.device
                    .begin_command_buffer(command_buffer, &command_buffer_begin_info)
                    .expect("Failed to begin recording Command Buffer");
            }

            let clear_values = [vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [0.0, 0.0, 0.0, 1.0],
                },
            }];

            let render_pass_begin_info = vk::RenderPassBeginInfo::builder()
                .render_pass(render_pass)
                .framebuffer(framebuffers[i])
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: swapchain_extent,
                })
                .clear_values(&clear_values);

            unsafe {
                gpu.device.cmd_begin_render_pass(
                    command_buffer,
                    &render_pass_begin_info,
                    vk::SubpassContents::INLINE,
                );
                gpu.device.cmd_bind_pipeline(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    graphics_pipeline,
                );

                let vertex_buffers = [vertex_buffer];
                let offsets = [0_u64];
                gpu.device
                    .cmd_bind_vertex_buffers(command_buffer, 0, &vertex_buffers, &offsets);

                gpu.device.cmd_draw(command_buffer, 3, 1, 0, 0);

                gpu.device.cmd_end_render_pass(command_buffer);

                gpu.device
                    .end_command_buffer(command_buffer)
                    .expect("Failed to record Command Buffer at Ending!");
            }
        }

        // # Synchronization primitives
        let (
            image_available_semaphores,
            render_finished_semaphores,
            command_buffer_complete_fences,
        ) = {
            let mut image_available_semaphores = Vec::new();
            let mut render_finished_semaphores = Vec::new();
            let mut command_buffer_complete_fences = Vec::new();
            let semaphore_create_info = vk::SemaphoreCreateInfo::builder();
            let fence_create_info =
                vk::FenceCreateInfo::builder().flags(vk::FenceCreateFlags::SIGNALED);

            for _ in 0..NUM_FRAMES {
                unsafe {
                    image_available_semaphores.push(
                        gpu.device
                            .create_semaphore(&semaphore_create_info, None)
                            .expect("Failed to create Semaphore Object!"),
                    );
                    render_finished_semaphores.push(
                        gpu.device
                            .create_semaphore(&semaphore_create_info, None)
                            .expect("Failed to create Semaphore Object!"),
                    );
                    command_buffer_complete_fences.push(
                        gpu.device
                            .create_fence(&fence_create_info, None)
                            .expect("Failed to create Fence Object!"),
                    );
                }
            }
            (
                image_available_semaphores,
                render_finished_semaphores,
                command_buffer_complete_fences,
            )
        };

        Apparatus {
            _surface_caps: surface_caps,
            _surface_formats: surface_formats,
            swapchain,
            _swapchain_format: swapchain_format,
            _swapchain_extent: swapchain_extent,
            _swapchain_images: swapchain_images,
            swapchain_imageviews,
            render_pass,
            framebuffers,
            graphics_pipeline,
            pipeline_layout,
            command_buffers,
            image_available_semaphores,
            render_finished_semaphores,
            command_buffer_complete_fences,
        }
    }

    pub fn destroy(&self, gpu: &Gpu, ext_swapchain: &ash::extensions::khr::Swapchain) {
        unsafe {
            for i in 0..NUM_FRAMES {
                gpu.device
                    .destroy_semaphore(self.image_available_semaphores[i], None);
                gpu.device
                    .destroy_semaphore(self.render_finished_semaphores[i], None);
                gpu.device
                    .destroy_fence(self.command_buffer_complete_fences[i], None);
            }

            gpu.device
                .destroy_pipeline_layout(self.pipeline_layout, None);
            gpu.device.destroy_pipeline(self.graphics_pipeline, None);
            for &framebuffer in self.framebuffers.iter() {
                gpu.device.destroy_framebuffer(framebuffer, None);
            }
            gpu.device.destroy_render_pass(self.render_pass, None);
            for &imageview in self.swapchain_imageviews.iter() {
                gpu.device.destroy_image_view(imageview, None);
            }
            ext_swapchain.destroy_swapchain(self.swapchain, None);
        }
    }
}

impl VulkanApp {
    pub fn recreate_resolution_dependent_state(&mut self) {
        unsafe {
            self.gpu
                .device
                .device_wait_idle()
                .expect("Failed to wait device idle!")
        };
        self.apparatus.destroy(&self.gpu, &self.ext_swapchain);
        self.apparatus = Apparatus::new(
            &self.window,
            self.surface,
            &self.gpu,
            self.command_pool,
            self.vertex_buffer,
            &self.ext_surface,
            &self.ext_swapchain,
        );
    }

    pub fn new(event_loop: &winit::event_loop::EventLoop<()>) -> VulkanApp {
        const APP_NAME: &str = "Hello Triangle";
        const ENABLE_DEBUG_MESSENGER_CALLBACK: bool = true;
        let validation_layers = vec![String::from("VK_LAYER_KHRONOS_validation")];
        let device_extensions = vec![String::from("VK_KHR_swapchain")];

        // # Init window
        let window = {
            winit::window::WindowBuilder::new()
                .with_title(APP_NAME)
                .with_inner_size(winit::dpi::LogicalSize::new(800, 600))
                .build(event_loop)
                .expect("Failed to create window.")
        };

        // # Init Ash
        let entry = ash::Entry::new().unwrap();

        // # Create Vulkan instance
        let instance = {
            let app_name = CString::new(APP_NAME).unwrap();
            let engine_name = CString::new("grapheme").unwrap();
            let app_info = vk::ApplicationInfo::builder()
                .application_name(&app_name)
                .application_version(vk_make_version!(1, 0, 0))
                .engine_name(&engine_name)
                .engine_version(vk_make_version!(1, 0, 0))
                .api_version(vk_make_version!(1, 0, 92))
                .build();

            // Ensure that all desired validation layers are available
            if !validation_layers.is_empty() {
                // Enumerate available validation layers
                let layer_props = entry
                    .enumerate_instance_layer_properties()
                    .expect("Failed to enumerate instance layers properties.");
                // Iterate over all desired layers
                for layer in validation_layers.iter() {
                    let is_layer_found = layer_props
                        .iter()
                        .any(|&prop| tools::vk_to_string(&prop.layer_name) == *layer);
                    if !is_layer_found {
                        panic!(
                            "Validation layer '{}' requested, but not found. \
                               (1) Install the Vulkan SDK and set up validation layers, \
                               or (2) remove any validation layers in the Rust code.",
                            layer
                        );
                    }
                }
            }

            let required_validation_layer_raw_names: Vec<CString> = validation_layers
                .iter()
                .map(|layer_name| CString::new(layer_name.to_string()).unwrap())
                .collect();
            let layer_names: Vec<*const c_char> = required_validation_layer_raw_names
                .iter()
                .map(|layer_name| layer_name.as_ptr())
                .collect();

            let extension_names = platforms::required_extension_names();

            let create_info = vk::InstanceCreateInfo::builder()
                .enabled_layer_names(&layer_names)
                .application_info(&app_info)
                .enabled_extension_names(&extension_names);

            let instance: ash::Instance = unsafe {
                entry
                    .create_instance(&create_info, None)
                    .expect("Failed to create instance.")
            };

            instance
        };

        // # Debug messenger callback
        let ext_debug_utils = ash::extensions::ext::DebugUtils::new(&entry, &instance);
        let debug_messenger = {
            if !ENABLE_DEBUG_MESSENGER_CALLBACK {
                ash::vk::DebugUtilsMessengerEXT::null()
            } else {
                let messenger_ci = populate_debug_messenger_create_info();
                unsafe {
                    ext_debug_utils
                        .create_debug_utils_messenger(&messenger_ci, None)
                        .expect("Debug Utils Callback")
                }
            }
        };

        // # Create surface
        let ext_surface = ash::extensions::khr::Surface::new(&entry, &instance);
        let surface = unsafe {
            platforms::create_surface(&entry, &instance, &window)
                .expect("Failed to create surface.")
        };

        // # Enumerate eligible GPUs
        struct CandidateGpu {
            physical_device: vk::PhysicalDevice,
            exts: Vec<vk::ExtensionProperties>,
            present_modes: Vec<vk::PresentModeKHR>,
            memory_properties: vk::PhysicalDeviceMemoryProperties,
            properties: vk::PhysicalDeviceProperties,
            graphics_queue_idx: u32,
            present_queue_idx: u32,
        }
        let candidate_gpus: Vec<CandidateGpu> = {
            let physical_devices = unsafe {
                &instance
                    .enumerate_physical_devices()
                    .expect("Failed to enumerate Physical Devices!")
            };

            let mut candidate_gpus = Vec::new();

            for &physical_device in physical_devices {
                let exts = unsafe {
                    instance
                        .enumerate_device_extension_properties(physical_device)
                        .expect("Failed to get device extension properties.")
                };
                // Are desired extensions supported?
                let are_exts_supported = {
                    let available_exts: Vec<String> = exts
                        .iter()
                        .map(|&ext| tools::vk_to_string(&ext.extension_name))
                        .collect();

                    device_extensions.iter().all(|required_ext| {
                        available_exts
                            .iter()
                            .any(|available_ext| required_ext == available_ext)
                    })
                };
                if !are_exts_supported {
                    continue;
                }

                let surface_formats = unsafe {
                    ext_surface
                        .get_physical_device_surface_formats(physical_device, surface)
                        .expect("Failed to query for surface formats.")
                };
                let present_modes = unsafe {
                    ext_surface
                        .get_physical_device_surface_present_modes(physical_device, surface)
                        .expect("Failed to query for surface present mode.")
                };
                // Are there any surface formats and present modes?
                if surface_formats.is_empty() || present_modes.is_empty() {
                    continue;
                }

                let memory_properties =
                    unsafe { instance.get_physical_device_memory_properties(physical_device) };
                let properties =
                    unsafe { instance.get_physical_device_properties(physical_device) };

                // Queue family indices
                let queue_families = unsafe {
                    instance.get_physical_device_queue_family_properties(physical_device)
                };
                let opt_graphics_queue_idx = queue_families.iter().position(|&fam| {
                    fam.queue_count > 0 && fam.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                });
                let opt_present_queue_idx =
                    queue_families.iter().enumerate().position(|(i, &fam)| {
                        let is_present_supported = unsafe {
                            ext_surface.get_physical_device_surface_support(
                                physical_device,
                                i as u32,
                                surface,
                            )
                        };
                        fam.queue_count > 0 && is_present_supported
                    });
                // Is there a graphics queue and a present queue?
                if opt_graphics_queue_idx.is_none() || opt_present_queue_idx.is_none() {
                    continue;
                }

                if let Some(graphics_queue_idx) = opt_graphics_queue_idx {
                    if let Some(present_queue_idx) = opt_present_queue_idx {
                        candidate_gpus.push(CandidateGpu {
                            physical_device,
                            exts,
                            present_modes,
                            memory_properties,
                            properties,
                            graphics_queue_idx: graphics_queue_idx as u32,
                            present_queue_idx: present_queue_idx as u32,
                        });
                    }
                }
            }

            candidate_gpus
        };

        // # Create a logical device, queues, the command pool, sync primitives, and the final gpu struct
        let gpu = {
            // Pick the most eligible of the candidate GPU.
            // Currently, we just pick the first one. Winner winner chicken dinner!
            // TODO: Might want to pick the most powerful GPU in the future.
            let cgpu = candidate_gpus
                .first()
                .expect("Failed to find a suitable GPU.");

            use std::collections::HashSet;
            let mut unique_queue_families = HashSet::new();
            unique_queue_families.insert(cgpu.graphics_queue_idx);
            unique_queue_families.insert(cgpu.present_queue_idx);

            let queue_priorities = [1.0_f32];
            let mut queue_create_infos = vec![];
            for &queue_family in unique_queue_families.iter() {
                let queue_create_info = vk::DeviceQueueCreateInfo {
                    s_type: vk::StructureType::DEVICE_QUEUE_CREATE_INFO,
                    p_next: ptr::null(),
                    flags: vk::DeviceQueueCreateFlags::empty(),
                    queue_family_index: queue_family,
                    p_queue_priorities: queue_priorities.as_ptr(),
                    queue_count: queue_priorities.len() as u32,
                };
                queue_create_infos.push(queue_create_info);
            }

            let physical_device_features = vk::PhysicalDeviceFeatures {
                sampler_anisotropy: vk::TRUE, // enable anisotropy device feature from Chapter-24.
                ..Default::default()
            };

            let raw_ext_names: Vec<CString> = device_extensions
                .iter()
                .map(|ext| CString::new(ext.to_string()).unwrap())
                .collect();
            let ext_names: Vec<*const c_char> =
                raw_ext_names.iter().map(|ext| ext.as_ptr()).collect();

            let device_create_info = vk::DeviceCreateInfo {
                s_type: vk::StructureType::DEVICE_CREATE_INFO,
                p_next: ptr::null(),
                flags: vk::DeviceCreateFlags::empty(),
                queue_create_info_count: queue_create_infos.len() as u32,
                p_queue_create_infos: queue_create_infos.as_ptr(),
                enabled_layer_count: 0,
                pp_enabled_layer_names: ptr::null(),
                enabled_extension_count: device_extensions.len() as u32,
                pp_enabled_extension_names: ext_names.as_ptr(),
                p_enabled_features: &physical_device_features,
            };

            let device: ash::Device = unsafe {
                instance
                    .create_device(cgpu.physical_device, &device_create_info, None)
                    .expect("Failed to create logical Device!")
            };

            let graphics_queue = unsafe { device.get_device_queue(cgpu.graphics_queue_idx, 0) };
            let present_queue = unsafe { device.get_device_queue(cgpu.present_queue_idx, 0) };

            Gpu {
                // Physical device
                physical_device: cgpu.physical_device,
                _exts: cgpu.exts.clone(),
                present_modes: cgpu.present_modes.clone(),
                _memory_properties: cgpu.memory_properties,
                _properties: cgpu.properties,
                graphics_queue_idx: cgpu.graphics_queue_idx,
                present_queue_idx: cgpu.present_queue_idx,
                // Logical device
                device,
                graphics_queue,
                present_queue,
            }
        };

        // # Create command pool
        let command_pool = {
            let info = vk::CommandPoolCreateInfo::builder()
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                .queue_family_index(gpu.graphics_queue_idx);

            unsafe {
                gpu.device
                    .create_command_pool(&info, None)
                    .expect("Failed to create command pool")
            }
        };

        let ext_swapchain = ash::extensions::khr::Swapchain::new(&instance, &gpu.device);

        // # Create the vertex buffer and allocate its memory
        let (vertex_buffer, vertex_buffer_memory) = {
            const VERTICES_DATA: [f32; 15] = [
                0.0, -0.5, 1.0, 0.0, 0.0, -0.5, 0.5, 0.0, 0.0, 1.0, 0.5, 0.5, 0.0, 1.0, 0.0,
            ];
            let buffer_size = std::mem::size_of_val(&VERTICES_DATA) as vk::DeviceSize;
            let device_memory_properties =
                unsafe { instance.get_physical_device_memory_properties(gpu.physical_device) };

            // ## Create staging buffer in host-visible memory
            // TODO: Replace with allocator library?
            let (staging_buffer, staging_buffer_memory) = VulkanApp::create_buffer(
                &gpu.device,
                buffer_size,
                vk::BufferUsageFlags::TRANSFER_SRC,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                &device_memory_properties,
            );
            // ## Copy data to staging buffer
            unsafe {
                let data_ptr = gpu
                    .device
                    .map_memory(
                        staging_buffer_memory,
                        0,
                        buffer_size,
                        vk::MemoryMapFlags::empty(),
                    )
                    .expect("Failed to map memory.") as *mut f32;

                data_ptr.copy_from_nonoverlapping(VERTICES_DATA.as_ptr(), VERTICES_DATA.len());
                gpu.device.unmap_memory(staging_buffer_memory);
            }
            // ## Create vertex buffer in device-local memory
            // TODO: Replace with allocator library?
            let (vertex_buffer, vertex_buffer_memory) = VulkanApp::create_buffer(
                &gpu.device,
                buffer_size,
                vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::VERTEX_BUFFER,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
                &device_memory_properties,
            );

            // ## Copy staging buffer -> vertex buffer
            {
                let allocate_info = vk::CommandBufferAllocateInfo::builder()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1);

                let command_buffers = unsafe {
                    gpu.device
                        .allocate_command_buffers(&allocate_info)
                        .expect("Failed to allocate command buffer.")
                };
                let command_buffer = command_buffers[0];

                let begin_info = vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

                unsafe {
                    gpu.device
                        .begin_command_buffer(command_buffer, &begin_info)
                        .expect("Failed to begin command buffer.");

                    let copy_regions = [vk::BufferCopy {
                        src_offset: 0,
                        dst_offset: 0,
                        size: buffer_size,
                    }];

                    gpu.device.cmd_copy_buffer(
                        command_buffer,
                        staging_buffer,
                        vertex_buffer,
                        &copy_regions,
                    );

                    gpu.device
                        .end_command_buffer(command_buffer)
                        .expect("Failed to end command buffer");
                }

                let submit_info = [vk::SubmitInfo::builder()
                    .command_buffers(&command_buffers)
                    .build()];

                unsafe {
                    gpu.device
                        .queue_submit(gpu.graphics_queue, &submit_info, vk::Fence::null())
                        .expect("Failed to Submit Queue.");
                    gpu.device
                        .queue_wait_idle(gpu.graphics_queue)
                        .expect("Failed to wait Queue idle");

                    gpu.device
                        .free_command_buffers(command_pool, &command_buffers);
                }
            }

            unsafe {
                gpu.device.destroy_buffer(staging_buffer, None);
                gpu.device.free_memory(staging_buffer_memory, None);
            }

            (vertex_buffer, vertex_buffer_memory)
        };

        // # Set up the apparatus
        let apparatus = Apparatus::new(
            &window,
            surface,
            &gpu,
            command_pool,
            vertex_buffer,
            &ext_surface,
            &ext_swapchain,
        );

        VulkanApp {
            window,
            _entry: entry,
            instance,
            surface,
            // - Extensions
            ext_debug_utils,
            ext_surface,
            ext_swapchain,
            // - Device
            gpu,
            command_pool,
            vertex_buffer,
            vertex_buffer_memory,
            // - Resolution-dependent apparatus
            apparatus,

            debug_messenger,
            validation_layers,

            current_frame: 0,
        }
    }

    fn create_buffer(
        device: &ash::Device,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
        required_memory_properties: vk::MemoryPropertyFlags,
        device_memory_properties: &vk::PhysicalDeviceMemoryProperties,
    ) -> (vk::Buffer, vk::DeviceMemory) {
        // Create buffer
        let buffer_create_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        let buffer = unsafe {
            device
                .create_buffer(&buffer_create_info, None)
                .expect("Failed to create vertex buffer.")
        };
        // Locate memory type
        let mem_requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        let memory_type_index = device_memory_properties
            .memory_types
            .iter()
            .enumerate()
            .position(|(i, &m)| {
                (mem_requirements.memory_type_bits & (1 << i)) > 0
                    && m.property_flags.contains(required_memory_properties)
            })
            .expect("Failed to find suitable memory type.") as u32;
        // Allocate memory
        // TODO: Replace with allocator library?
        let allocate_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(mem_requirements.size)
            .memory_type_index(memory_type_index);

        let buffer_memory = unsafe {
            device
                .allocate_memory(&allocate_info, None)
                .expect("Failed to allocate vertex buffer memory.")
        };
        // Bind memory to buffer
        unsafe {
            device
                .bind_buffer_memory(buffer, buffer_memory, 0)
                .expect("Failed to bind Buffer.");
        }

        (buffer, buffer_memory)
    }

    fn draw_frame(&mut self) {
        let wait_fences = [self.apparatus.command_buffer_complete_fences[self.current_frame]];

        let (image_index, _is_sub_optimal) = unsafe {
            self.gpu
                .device
                .wait_for_fences(&wait_fences, true, std::u64::MAX)
                .expect("Failed to wait for Fence.");

            let result = self.ext_swapchain.acquire_next_image(
                self.apparatus.swapchain,
                std::u64::MAX,
                self.apparatus.image_available_semaphores[self.current_frame],
                vk::Fence::null(),
            );
            match result {
                Ok(image_idx) => image_idx,
                Err(error_code) => {
                    match error_code {
                        vk::Result::ERROR_OUT_OF_DATE_KHR => {
                            // Window is resized. Recreate the swapchain
                            // and exit early without drawing this frame.
                            self.recreate_resolution_dependent_state();
                            return;
                        }
                        _ => panic!("Failed to acquire swapchain image."),
                    }
                }
            }
        };

        let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let wait_semaphores = [self.apparatus.image_available_semaphores[self.current_frame]];
        let signal_semaphores = [self.apparatus.render_finished_semaphores[self.current_frame]];
        let command_buffers = [self.apparatus.command_buffers[image_index as usize]];

        let submit_infos = [vk::SubmitInfo::builder()
            .wait_semaphores(&wait_semaphores)
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(&command_buffers)
            .signal_semaphores(&signal_semaphores)
            .build()];

        unsafe {
            self.gpu
                .device
                .reset_fences(&wait_fences)
                .expect("Failed to reset fence.");

            self.gpu
                .device
                .queue_submit(
                    self.gpu.graphics_queue,
                    &submit_infos,
                    self.apparatus.command_buffer_complete_fences[self.current_frame],
                )
                .expect("Failed to execute queue submit.");
        }

        let swapchains = [self.apparatus.swapchain];
        let image_indices = [image_index];

        let present_info = vk::PresentInfoKHR::builder()
            .wait_semaphores(&signal_semaphores)
            .swapchains(&swapchains)
            .image_indices(&image_indices);

        // Present the queue
        {
            let result = unsafe {
                self.ext_swapchain
                    .queue_present(self.gpu.present_queue, &present_info)
            };

            if let Err(error_code) = result {
                match error_code {
                    vk::Result::ERROR_OUT_OF_DATE_KHR | vk::Result::SUBOPTIMAL_KHR => {
                        // Window is resized. Recreate the swapchain
                        self.recreate_resolution_dependent_state();
                    }
                    _ => panic!("Failed to present queue."),
                }
            }
        }

        self.current_frame = (self.current_frame + 1) % NUM_FRAMES;
    }
}

impl Drop for VulkanApp {
    fn drop(&mut self) {
        unsafe {
            self.gpu
                .device
                .destroy_command_pool(self.command_pool, None);

            self.apparatus.destroy(&self.gpu, &self.ext_swapchain);

            self.gpu.device.destroy_buffer(self.vertex_buffer, None);
            self.gpu.device.free_memory(self.vertex_buffer_memory, None);

            self.gpu.device.destroy_device(None);
            self.ext_surface.destroy_surface(self.surface, None);

            if !self.validation_layers.is_empty() {
                self.ext_debug_utils
                    .destroy_debug_utils_messenger(self.debug_messenger, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}

impl VulkanApp {
    pub fn main_loop(mut self, event_loop: EventLoop<()>) {
        event_loop.run(move |event, _, control_flow| match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,
                WindowEvent::KeyboardInput { input, .. } => match input {
                    KeyboardInput {
                        virtual_keycode,
                        state,
                        ..
                    } => match (virtual_keycode, state) {
                        (Some(VirtualKeyCode::Escape), ElementState::Pressed) => {
                            *control_flow = ControlFlow::Exit
                        }
                        (Some(VirtualKeyCode::Return), ElementState::Pressed) => {
                            *control_flow = ControlFlow::Exit
                        }
                        _ => {}
                    },
                },
                _ => {}
            },
            Event::MainEventsCleared => {
                self.window.request_redraw();
            }
            Event::RedrawRequested(_window_id) => {
                self.draw_frame();
            }
            Event::LoopDestroyed => {
                unsafe {
                    self.gpu
                        .device
                        .device_wait_idle()
                        .expect("Failed to wait device idle!")
                };
            }
            _ => (),
        })
    }
}

fn main() {
    let event_loop = EventLoop::new();

    let vulkan_app = VulkanApp::new(&event_loop);
    vulkan_app.main_loop(event_loop);
}
