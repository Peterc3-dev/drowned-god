//! bw-probe — minimal Vulkan compute bandwidth probe for the Strix Point iGPU.
//!
//! Two kernels:
//!   - copy:  dst = src              (2 buffers × n_bytes; bytes/iter = 2*N)
//!   - triad: a = b + scalar*c        (3 buffers × n_bytes; bytes/iter = 3*N)
//!
//! Allocates 1 GB working set (split across the kernel's buffer count), runs
//! a wide-load shader (uvec4 = 16 B per invocation), times via timestamp
//! queries on the compute queue, reports best-of-N GB/s.
//!
//! The intent: measure the iGPU's RAW achievable bandwidth on coalesced
//! linear access — independent of llama.cpp's K-quant dequant / coopmat
//! tile layout. If this exceeds llama-bench's effective 83 GB/s, the
//! difference is llama.cpp Vulkan backend overhead and there is room for
//! kernel optimization. If not, 83 is the hardware ceiling.
use anyhow::{anyhow, Context, Result};
use ash::vk;
use std::ffi::CString;
use std::time::Instant;

const COPY_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/copy.comp.spv"));
const TRIAD_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/triad.comp.spv"));

const WORKING_SET_BYTES: u64 = 1024 * 1024 * 1024; // 1 GB per buffer
const WG_SIZE: u32 = 256;
const TRIALS: usize = 5;
const LOOPS_PER_DISPATCH: u32 = 4; // amortize dispatch overhead

fn main() -> Result<()> {
    unsafe {
        let entry = ash::Entry::load().context("loading Vulkan entry")?;
        let app_name = CString::new("bw-probe")?;
        let app_info = vk::ApplicationInfo::default()
            .application_name(&app_name)
            .application_version(0)
            .engine_name(&app_name)
            .engine_version(0)
            .api_version(vk::API_VERSION_1_2);
        let instance_info = vk::InstanceCreateInfo::default().application_info(&app_info);
        let instance = entry.create_instance(&instance_info, None)?;

        let pdevs = instance.enumerate_physical_devices()?;
        let pdev = pdevs.into_iter().find(|&pd| {
            let props = instance.get_physical_device_properties(pd);
            let name = std::ffi::CStr::from_ptr(props.device_name.as_ptr()).to_string_lossy();
            name.to_lowercase().contains("amd") || name.to_lowercase().contains("radeon")
        }).ok_or_else(|| anyhow!("no AMD/Radeon device found"))?;

        let pdev_props = instance.get_physical_device_properties(pdev);
        let pdev_name = std::ffi::CStr::from_ptr(pdev_props.device_name.as_ptr()).to_string_lossy();
        let mem_props = instance.get_physical_device_memory_properties(pdev);
        let timestamp_period = pdev_props.limits.timestamp_period;
        println!("device: {}", pdev_name);
        println!("timestamp_period (ns): {}", timestamp_period);
        println!("api_version: 0x{:x}", pdev_props.api_version);

        // Find a compute-capable queue family
        let qf_props = instance.get_physical_device_queue_family_properties(pdev);
        let qf_idx = qf_props.iter().enumerate().find_map(|(i, q)| {
            if q.queue_flags.contains(vk::QueueFlags::COMPUTE) { Some(i as u32) } else { None }
        }).ok_or_else(|| anyhow!("no compute queue family"))?;

        let priorities = [1.0f32];
        let queue_info = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(qf_idx)
            .queue_priorities(&priorities);
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(std::slice::from_ref(&queue_info));
        let device = instance.create_device(pdev, &device_info, None)?;
        let queue = device.get_device_queue(qf_idx, 0);

        // Pick the largest DEVICE_LOCAL heap that we can map (HOST_VISIBLE optional)
        let mem_type_idx_dev_local = (0..mem_props.memory_type_count).find(|&i| {
            mem_props.memory_types[i as usize].property_flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        }).ok_or_else(|| anyhow!("no DEVICE_LOCAL memory type"))?;
        println!("using memory type index: {}", mem_type_idx_dev_local);

        // Run COPY (2-buffer) and TRIAD (3-buffer)
        run_kernel(&device, queue, qf_idx, mem_type_idx_dev_local, timestamp_period, "COPY",
            COPY_SPV, 2, /*push_constants=*/&[(WORKING_SET_BYTES / 16) as u32, LOOPS_PER_DISPATCH])?;
        run_kernel(&device, queue, qf_idx, mem_type_idx_dev_local, timestamp_period, "TRIAD",
            TRIAD_SPV, 3, /*push_constants=*/&[(WORKING_SET_BYTES / 16) as u32, LOOPS_PER_DISPATCH, 3])?;

        device.destroy_device(None);
        instance.destroy_instance(None);
    }
    Ok(())
}

unsafe fn run_kernel(
    device: &ash::Device,
    queue: vk::Queue,
    qf_idx: u32,
    mem_type_idx: u32,
    timestamp_period: f32,
    label: &str,
    spv_bytes: &[u8],
    n_buffers: usize,
    pc: &[u32],
) -> Result<()> {
    println!("\n=== {} ===", label);
    let bytes_per_iter = WORKING_SET_BYTES * (n_buffers as u64); // r+w sum
    println!("buffers: {} × {} MB = {} MB working set",
        n_buffers, WORKING_SET_BYTES / (1024 * 1024),
        n_buffers as u64 * WORKING_SET_BYTES / (1024 * 1024));
    println!("bytes per dispatch (incl. {} loops): {} MB",
        LOOPS_PER_DISPATCH, bytes_per_iter * LOOPS_PER_DISPATCH as u64 / (1024 * 1024));

    // --- Allocate buffers ---
    let mut buffers = Vec::with_capacity(n_buffers);
    let mut memories = Vec::with_capacity(n_buffers);
    for _ in 0..n_buffers {
        let info = vk::BufferCreateInfo::default()
            .size(WORKING_SET_BYTES)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let buf = device.create_buffer(&info, None).context("create_buffer")?;
        let req = device.get_buffer_memory_requirements(buf);
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(req.size)
            .memory_type_index(mem_type_idx);
        let mem = device.allocate_memory(&alloc_info, None).context("allocate_memory")?;
        device.bind_buffer_memory(buf, mem, 0)?;
        buffers.push(buf);
        memories.push(mem);
    }

    // --- Descriptor set layout: n_buffers storage buffers ---
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = (0..n_buffers).map(|i| {
        vk::DescriptorSetLayoutBinding::default()
            .binding(i as u32)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
    }).collect();
    let dsl_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    let dsl = device.create_descriptor_set_layout(&dsl_info, None)?;

    let pc_range = vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::COMPUTE)
        .offset(0)
        .size((pc.len() * 4) as u32);
    let pl_info = vk::PipelineLayoutCreateInfo::default()
        .set_layouts(std::slice::from_ref(&dsl))
        .push_constant_ranges(std::slice::from_ref(&pc_range));
    let pl = device.create_pipeline_layout(&pl_info, None)?;

    // --- Shader module + pipeline ---
    let spv_aligned: Vec<u32> = spv_bytes.chunks(4).map(|c| {
        u32::from_le_bytes([c[0], c[1], c[2], c[3]])
    }).collect();
    let sm_info = vk::ShaderModuleCreateInfo::default().code(&spv_aligned);
    let shader_module = device.create_shader_module(&sm_info, None)?;
    let entry_name = CString::new("main")?;
    let stage = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(shader_module)
        .name(&entry_name);
    let pipeline_info = vk::ComputePipelineCreateInfo::default()
        .stage(stage)
        .layout(pl);
    let pipelines = device.create_compute_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        .map_err(|(_, e)| anyhow!("create_compute_pipelines: {:?}", e))?;
    let pipeline = pipelines[0];

    // --- Descriptor pool + set ---
    let pool_size = vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::STORAGE_BUFFER)
        .descriptor_count(n_buffers as u32);
    let dp_info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(1)
        .pool_sizes(std::slice::from_ref(&pool_size));
    let dp = device.create_descriptor_pool(&dp_info, None)?;
    let ds_alloc = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(dp)
        .set_layouts(std::slice::from_ref(&dsl));
    let ds = device.allocate_descriptor_sets(&ds_alloc)?[0];

    let buf_infos: Vec<vk::DescriptorBufferInfo> = buffers.iter().map(|&b| {
        vk::DescriptorBufferInfo::default()
            .buffer(b)
            .offset(0)
            .range(vk::WHOLE_SIZE)
    }).collect();
    let writes: Vec<vk::WriteDescriptorSet> = buf_infos.iter().enumerate().map(|(i, info)| {
        vk::WriteDescriptorSet::default()
            .dst_set(ds)
            .dst_binding(i as u32)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(std::slice::from_ref(info))
    }).collect();
    device.update_descriptor_sets(&writes, &[]);

    // --- Command pool + buffer + timestamp query ---
    let cp_info = vk::CommandPoolCreateInfo::default()
        .queue_family_index(qf_idx)
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
    let cp = device.create_command_pool(&cp_info, None)?;
    let cb_alloc = vk::CommandBufferAllocateInfo::default()
        .command_pool(cp)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    let cb = device.allocate_command_buffers(&cb_alloc)?[0];

    let qp_info = vk::QueryPoolCreateInfo::default()
        .query_type(vk::QueryType::TIMESTAMP)
        .query_count(2);
    let qp = device.create_query_pool(&qp_info, None)?;

    // Compute dispatch geometry: enough WGs to cover the buffer
    let n_vec4 = (WORKING_SET_BYTES / 16) as u32;
    // We don't need 1 invocation per element — the shader strides. Use a moderate dispatch.
    let wg_count = ((n_vec4 + WG_SIZE - 1) / WG_SIZE).min(65535);

    println!("dispatch: {} workgroups × {} threads = {} invocations", wg_count, WG_SIZE, wg_count * WG_SIZE);

    let mut best_gbps = 0.0f64;
    for trial in 0..TRIALS {
        // Record
        device.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty())?;
        let begin_info = vk::CommandBufferBeginInfo::default();
        device.begin_command_buffer(cb, &begin_info)?;
        device.cmd_reset_query_pool(cb, qp, 0, 2);
        device.cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, pipeline);
        device.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::COMPUTE, pl, 0, &[ds], &[]);
        let pc_bytes: Vec<u8> = pc.iter().flat_map(|v| v.to_le_bytes()).collect();
        device.cmd_push_constants(cb, pl, vk::ShaderStageFlags::COMPUTE, 0, &pc_bytes);
        device.cmd_write_timestamp(cb, vk::PipelineStageFlags::TOP_OF_PIPE, qp, 0);
        device.cmd_dispatch(cb, wg_count, 1, 1);
        device.cmd_write_timestamp(cb, vk::PipelineStageFlags::BOTTOM_OF_PIPE, qp, 1);
        device.end_command_buffer(cb)?;

        // Submit
        let submit = vk::SubmitInfo::default().command_buffers(std::slice::from_ref(&cb));
        let fence = device.create_fence(&vk::FenceCreateInfo::default(), None)?;
        let wall_t0 = Instant::now();
        device.queue_submit(queue, &[submit], fence)?;
        device.wait_for_fences(&[fence], true, u64::MAX)?;
        let wall_dt = wall_t0.elapsed().as_secs_f64();
        device.destroy_fence(fence, None);

        // Read timestamps
        let mut ts = [0u64; 2];
        device.get_query_pool_results(
            qp, 0, &mut ts,
            vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
        )?;
        let gpu_ns = (ts[1].saturating_sub(ts[0])) as f64 * timestamp_period as f64;
        let gpu_dt = gpu_ns / 1e9;
        let bytes_total = bytes_per_iter as f64 * LOOPS_PER_DISPATCH as f64;
        let gbps_gpu = bytes_total / gpu_dt / 1e9;
        let gbps_wall = bytes_total / wall_dt / 1e9;

        println!("  trial {}: gpu={:.3} ms ({:.2} GB/s)  wall={:.3} ms ({:.2} GB/s)",
            trial + 1, gpu_dt * 1000.0, gbps_gpu, wall_dt * 1000.0, gbps_wall);
        if gbps_gpu > best_gbps {
            best_gbps = gbps_gpu;
        }
    }
    println!("  BEST {}: {:.2} GB/s", label, best_gbps);

    // Cleanup
    device.destroy_query_pool(qp, None);
    device.destroy_command_pool(cp, None);
    device.destroy_descriptor_pool(dp, None);
    device.destroy_pipeline(pipeline, None);
    device.destroy_pipeline_layout(pl, None);
    device.destroy_shader_module(shader_module, None);
    device.destroy_descriptor_set_layout(dsl, None);
    for b in buffers { device.destroy_buffer(b, None); }
    for m in memories { device.free_memory(m, None); }
    Ok(())
}
