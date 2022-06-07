mod alloc;
mod bind;
mod format;
mod mem;

pub mod frame;
pub mod texture;

use std::{collections::HashMap, sync::Arc};

use ash::vk;
use smithay::{
    backend::{
        allocator::Format as DrmFormat,
        renderer::{Renderer, TextureFilter, Unbind},
    },
    reexports::wayland_server::protocol::wl_shm,
    utils::{Physical, Size, Transform},
};

use self::{
    alloc::{AllocationId, AllocationIdTracker},
    frame::VulkanFrame,
    texture::VulkanTexture,
};

use super::{
    device::{Device, DeviceHandle},
    error::VkError,
    UnsupportedVulkanVersion,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Vk(#[from] VkError),

    #[error(transparent)]
    Version(#[from] UnsupportedVulkanVersion),

    #[error("required extensions are not enabled")]
    MissingRequiredExtensions,

    /// No rendering target was set or the previous target is no longer valid.
    ///
    /// You must [`Bind`](smithay::backend::renderer::Bind) a target for the Vulkan renderer.
    #[error("no rendering target set")]
    NoTarget,

    #[error("required extensions for dmabuf import/export are not enabled or available")]
    DmabufNotSupported,

    /// The mandatory wl_shm formats, [`Argb8888`] and [`Xrgb8888`], are not supported.
    ///
    /// [`Argb8888`]: wl_shm::Format::Argb8888
    /// [`Xrgb8888`]: wl_shm::Format::Xrgb8888
    #[error("the mandatory wl_shm formats are not supported")]
    MissingMandatoryFormats,

    /// The maximum number of device allocations was reached.
    #[error("the maximum number of device allocations ({0}) was reached")]
    TooManyAllocations(usize),
}

/// TODO:
/// - Renderpass creation (full clear and partial clear)
/// - ImportMem
/// - Bind<VulkanTexture>
/// - Offscreen<VulkanTexture>
/// - ExportMem
/// - ImportDma
/// - Bind<Dmabuf>
/// - Offscreen<Dmabuf>
/// - ExportDma
///
/// State tracking:
/// - Ensure we do not exceed limits set by maxMemoryAllocationCount
#[derive(Debug)]
pub struct VulkanRenderer {
    /// Command pool used to allocate the staging and rendering command buffers.
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    // TODO: Refactor to support asynchronous upload.
    staging_command_buffer: vk::CommandBuffer,
    /// Whether the staging command buffer is recording commands.
    recording_staging: bool,

    allocator: AllocationIdTracker,

    staging_buffers: Vec<StagingBuffer>,

    /// Used to signal when queue submission commands have completed.
    ///
    /// This is in a signalled state by default.
    submit_fence: vk::Fence,

    memory_properties: vk::PhysicalDeviceMemoryProperties,

    renderpasses: HashMap<vk::Format, vk::RenderPass>,

    /// Renderer format info.
    formats: Formats,

    /// Currently bound render target.
    ///
    /// Rendering will fail if the render target is not set.
    target: Option<RenderTarget>,

    /// The device handle.
    ///
    /// Since a Vulkan renderer owns some Vulkan objects, we need this handle to ensure objects do not outlive
    /// the renderer.
    device: Arc<DeviceHandle>,
}

impl VulkanRenderer {
    /// Returns a list of device extensions the device must enable to use a [`VulkanRenderer`] most optimally.
    ///
    /// This set of extensions is required in order to use a [`Dmabuf`] for import or export into the renderer.
    ///
    /// If the device does not support all of the specified extensions, a smaller extension subset in
    /// [`VulkanRenderer::required_device_extensions`] may be used instead.
    ///
    /// This list satisfies the requirement that all enabled extensions also enable their dependencies
    /// (`VUID-vkCreateDevice-ppEnabledExtensionNames-01387`).
    pub fn optimal_device_extensions() -> &'static [&'static str] {
        &[
            "VK_KHR_external_memory_fd",
            "VK_EXT_external_memory_dma_buf",
            "VK_EXT_image_drm_format_modifier",
            // Or Vulkan 1.2
            "VK_KHR_image_format_list",
        ]
    }

    /// Returns a list of the device extensions the device must enable to use a [`VulkanRenderer`].
    ///
    /// This extension list contains the absolute minimum requirements for the renderer. Note that a renderer
    /// constructed from a device with these extensions enabled will be unable to use a [`Dmabuf`] for import
    /// or export.
    ///
    /// This list satisfies the requirement that all enabled extensions also enable their dependencies
    /// (`VUID-vkCreateDevice-ppEnabledExtensionNames-01387`).
    pub fn required_device_extensions() -> &'static [&'static str] {
        &[
            "VK_EXT_image_drm_format_modifier",
            // Or Vulkan 1.2
            "VK_KHR_image_format_list",
        ]
    }

    // TODO: There may be some required device capabilities?

    pub fn new(device: &Device) -> Result<VulkanRenderer, Error> {
        // Verify the required extensions are supported.
        // VUID-vkCreateDevice-ppEnabledExtensionNames-01387
        if !Self::required_device_extensions()
            .iter()
            .all(|extension| device.is_extension_enabled(extension))
        {
            return Err(Error::MissingRequiredExtensions);
        }

        let queue_family_index = device.queue_family_index() as u32;
        let device = device.handle();

        let memory_properties = unsafe {
            device
                .instance()
                .raw()
                .get_physical_device_memory_properties(device.phy())
        };

        let device_properties = unsafe { device.instance().raw().get_physical_device_properties(device.phy()) };

        // Create the renderer using null handles.
        //
        // This heavily simplifies initialization since we do not need manually destroy every handle if one
        // command fails. Instead we rely on the fact that Vulkan allows null handles to be passed into
        // "destroy" commands, which does nothing, and rely on the drop implementation for destroying all
        // Vulkan objects.
        let mut renderer = VulkanRenderer {
            command_pool: vk::CommandPool::null(),
            command_buffer: vk::CommandBuffer::null(),
            staging_command_buffer: vk::CommandBuffer::null(),
            recording_staging: false,
            allocator: AllocationIdTracker::new(device_properties.limits.max_memory_allocation_count as usize),
            staging_buffers: Vec::new(),
            submit_fence: vk::Fence::null(),
            memory_properties,
            renderpasses: HashMap::new(),
            formats: Formats {
                shm_format_info: Vec::new(),
                shm_formats: Vec::new(),
            },
            target: None,
            device,
        };

        let device_handle = renderer.device();
        let device_handle = device_handle.raw();

        let command_pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        renderer.command_pool =
            unsafe { device_handle.create_command_pool(&command_pool_info, None) }.map_err(VkError::from)?;

        let command_buffer_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(renderer.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(2);

        let mut command_buffers =
            unsafe { device_handle.allocate_command_buffers(&command_buffer_info) }.map_err(VkError::from)?;
        // Remove backwards to prevent shifting.
        renderer.command_buffer = command_buffers.remove(1);
        renderer.staging_command_buffer = command_buffers.remove(0);

        // The fence is created as signalled for two reasons:
        // 1. The first frame rendered will not wait forever waiting for a previous frame that never happened.
        // 2. If the renderer is immediately destroyed, we don't wait for the fence to never get signalled.
        let fence_info = vk::FenceCreateInfo::builder().flags(vk::FenceCreateFlags::SIGNALED);
        renderer.submit_fence = unsafe { device_handle.create_fence(&fence_info, None) }.map_err(VkError::from)?;

        // Initialize the list of supported formats
        renderer.init_shm_formats()?;

        // Initialize the renderpasses used with argb8888 since it is very common.
        unsafe { renderer.create_renderpass(vk::Format::B8G8R8A8_SRGB) }?;

        Ok(renderer)
    }

    pub fn device(&self) -> Arc<DeviceHandle> {
        self.device.clone()
    }

    // TODO: Offscreen texture creation with a specific format?
}

impl Renderer for VulkanRenderer {
    type Error = Error;
    type TextureId = VulkanTexture;
    type Frame = VulkanFrame;

    fn downscale_filter(&mut self, _filter: TextureFilter) -> Result<(), Self::Error> {
        todo!("not implemented")
    }

    fn upscale_filter(&mut self, _filter: TextureFilter) -> Result<(), Self::Error> {
        todo!("not implemented")
    }

    fn render<F, R>(
        &mut self,
        size: Size<i32, Physical>,
        _dst_transform: Transform,
        rendering: F,
    ) -> Result<R, Self::Error>
    where
        F: FnOnce(&mut Self, &mut Self::Frame) -> R,
    {
        let target = self.target.ok_or(Error::NoTarget)?;
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: size.w as u32,
                height: size.h as u32,
            },
        };

        // Begin recording
        let device = self.device.raw();

        let begin_info = vk::CommandBufferBeginInfo::builder()
            // We will only submit this command buffer once.
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        unsafe { device.begin_command_buffer(self.command_buffer, &begin_info) }.map_err(VkError::from)?;

        let begin_pass_info = vk::RenderPassBeginInfo::builder()
            .render_area(render_area)
            .render_pass(target.render_pass)
            .framebuffer(target.framebuffer);

        unsafe { device.cmd_begin_render_pass(self.command_buffer, &begin_pass_info, vk::SubpassContents::INLINE) }

        let mut frame = VulkanFrame {
            command_buffer: self.command_buffer,
            target,
            started: false,
            device: self.device.clone(),
        };

        let result = rendering(self, &mut frame);

        // Again to not cause double borrows.
        let device = self.device.raw();

        // End the renderpass
        unsafe { device.cmd_end_render_pass(self.command_buffer) };

        // Finish recording the staging command buffer.
        if self.recording_staging {
            self.recording_staging = false;
            unsafe { device.end_command_buffer(self.staging_command_buffer) }.map_err(VkError::from)?;
        }

        // Finalize the command buffer
        unsafe { device.end_command_buffer(self.command_buffer) }.map_err(VkError::from)?;

        // Submit commands to the queue for execution.
        let submit_info = vk::SubmitInfo::builder()
            .command_buffers(&[self.command_buffer])
            .build();

        // VUID-vkQueueSubmit-fence-00063
        unsafe { device.reset_fences(&[self.submit_fence]) }.map_err(VkError::from)?;
        unsafe { device.queue_submit(self.device.queue(), &[submit_info], self.submit_fence) }
            .map_err(VkError::from)?;

        Ok(result)
    }

    fn id(&self) -> usize {
        todo!("not implemented")
    }
}

impl Drop for VulkanRenderer {
    fn drop(&mut self) {
        let device = self.device.raw();

        unsafe {
            // It appears we do not need to explicitly free the command buffers. Done for sake of clarity.
            device.free_command_buffers(self.command_pool, &[self.command_buffer]);
            device.destroy_command_pool(self.command_pool, None);

            // VUID-vkDestroyFence-fence-01120: Wait for the fence to be signalled, indicating queue
            // submission commands have been completed.
            //
            // This will always return within a reasonable amount of time for one of two reasons:
            //
            // 1. We waited on the fence, indicating execution is complete.
            // 2. The renderer was immediately dropped, the fence is created as initially signalled.
            //
            // The timeout may seem absurd, at a maximum wait of 584 years. The Vulkan specification states we
            // should not be waiting too long (in the worst case a few seconds) before the fences are
            // signalled and the drop implementation continues.
            let _ = device.wait_for_fences(&[self.submit_fence], true, u64::MAX);
            device.destroy_fence(self.submit_fence, None);

            // Unbind the current framebuffer.
            let _ = self.unbind();

            let device = self.device.raw();

            // Destroy the renderpasses
            for (_, renderpass) in self.renderpasses.drain() {
                device.destroy_render_pass(renderpass, None);
            }

            // Since all command execution must be completed, destroy any staging buffers that were just
            // executed.
            self.free_staging_buffers();
        }
    }
}

// Impl details

#[derive(Debug)]
struct StagingBuffer {
    buffer: vk::Buffer,
    buffer_size: vk::DeviceSize,
    memory: vk::DeviceMemory,
    memory_allocation_id: AllocationId,
}

#[derive(Debug)]
struct Formats {
    /// Information about the supported shm formats, such as the max extent of an image.
    shm_format_info: Vec<ShmFormatInfo>,

    /// Supported shm formats.
    shm_formats: Vec<wl_shm::Format>,
}

#[derive(Debug)]
struct ShmFormatInfo {
    shm: wl_shm::Format,
    vk: vk::Format,
    max_extent: vk::Extent2D,
}

#[derive(Debug, Clone, Copy)]
struct RenderTarget {
    framebuffer: vk::Framebuffer,
    render_pass: vk::RenderPass,
    width: u32,
    height: u32,
}

impl VulkanRenderer {
    fn get_or_create_renderpasses(&mut self, format: vk::Format) -> Option<vk::RenderPass> {
        self.renderpasses.get(&format).copied()
    }

    unsafe fn create_renderpass(&mut self, format: vk::Format) -> Result<vk::RenderPass, VkError> {
        /*
        The Vulkan renderer has two render passes per format:

        The first renderpass performs a full clear of the framebuffer.
        The second renderpass does not perform a full clear.

        Each renderpass has two subpass dependencies.

        The first subpass performs all the memory imports that might be sticking around in staging buffers.

        The second subpass performs the color attachment.
        */

        let subpass_dependencies = [
            vk::SubpassDependency::builder()
                // First subpass
                .src_subpass(vk::SUBPASS_EXTERNAL)
                .src_stage_mask(
                    vk::PipelineStageFlags::HOST
                        | vk::PipelineStageFlags::TRANSFER
                        | vk::PipelineStageFlags::TOP_OF_PIPE
                        | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                )
                .src_access_mask(
                    vk::AccessFlags::HOST_WRITE
                        | vk::AccessFlags::TRANSFER_WRITE
                        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                )
                // why 0?
                .dst_subpass(0)
                .dst_stage_mask(vk::PipelineStageFlags::ALL_GRAPHICS) // TODO: .dst_access_mask()
                .build(),
            vk::SubpassDependency::builder()
                .src_subpass(0)
                .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                // Last subpass
                .dst_subpass(vk::SUBPASS_EXTERNAL)
                .dst_stage_mask(
                    vk::PipelineStageFlags::TRANSFER
                        | vk::PipelineStageFlags::HOST
                        | vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                )
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::MEMORY_READ)
                .build(),
        ];

        let attachment_references = [vk::AttachmentReference::builder()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .build()];

        let subpass_description = [vk::SubpassDescription::builder()
            .color_attachments(&attachment_references)
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .build()];

        let device = self.device.raw();

        let attachment_description = [vk::AttachmentDescription::builder()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            // We want to load on load for this render pass.
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::GENERAL)
            .final_layout(vk::ImageLayout::GENERAL)
            .build()];

        let render_pass_create_info = vk::RenderPassCreateInfo::builder()
            .attachments(&attachment_description)
            .subpasses(&subpass_description)
            .dependencies(&subpass_dependencies);

        let renderpass = unsafe { device.create_render_pass(&render_pass_create_info, None) }?;

        self.renderpasses.insert(format, renderpass);

        Ok(renderpass)
    }

    fn recording_staging_buffer(&mut self) -> Result<vk::CommandBuffer, VkError> {
        if !self.recording_staging {
            let begin_info = vk::CommandBufferBeginInfo::builder().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

            unsafe {
                self.device
                    .raw()
                    .begin_command_buffer(self.staging_command_buffer, &begin_info)
            }?;
        }

        Ok(self.staging_command_buffer)
    }

    /// # Safety
    ///
    /// Commands referring to the staging buffers must have completed execution.
    unsafe fn free_staging_buffers(&mut self) {
        let device = self.device.raw();

        unsafe {
            for staging_buffer in self.staging_buffers.drain(..) {
                device.destroy_buffer(staging_buffer.buffer, None);
                device.free_memory(staging_buffer.memory, None);
            }
        }
    }
}