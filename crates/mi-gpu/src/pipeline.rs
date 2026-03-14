use metal::*;
use std::path::Path;

/// Holds the Metal compute pipeline state.
pub struct MetalPipeline {
    pub device: Device,
    pub queue: CommandQueue,
    pub pipeline_state: ComputePipelineState,
    pub max_threads_per_threadgroup: u64,
}

impl MetalPipeline {
    /// Initialize the Metal pipeline from a compiled .metallib file.
    pub fn new(metallib_path: &Path) -> Result<Self, String> {
        let device = Device::system_default().ok_or("No Metal device found")?;

        tracing::info!(
            "Metal device: {} ({})",
            device.name(),
            if device.is_low_power() {
                "low power"
            } else {
                "high performance"
            }
        );

        let library = device
            .new_library_with_file(metallib_path)
            .map_err(|e| format!("Failed to load metallib: {e}"))?;

        let function = library
            .get_function("sha256d_mine", None)
            .map_err(|e| format!("Failed to get kernel function: {e}"))?;

        let pipeline_state = device
            .new_compute_pipeline_state_with_function(&function)
            .map_err(|e| format!("Failed to create pipeline state: {e}"))?;

        let max_threads = pipeline_state.max_total_threads_per_threadgroup();
        tracing::info!("Max threads per threadgroup: {max_threads}");

        let queue = device.new_command_queue();

        Ok(Self {
            device,
            queue,
            pipeline_state,
            max_threads_per_threadgroup: max_threads,
        })
    }

    /// Initialize from embedded metallib bytes (compiled at build time).
    #[allow(dead_code)]
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let device = Device::system_default().ok_or("No Metal device found")?;

        tracing::info!(
            "Metal device: {} ({})",
            device.name(),
            if device.is_low_power() {
                "low power"
            } else {
                "high performance"
            }
        );

        let library = device
            .new_library_with_data(bytes)
            .map_err(|e| format!("Failed to load metallib from bytes: {e}"))?;

        let function = library
            .get_function("sha256d_mine", None)
            .map_err(|e| format!("Failed to get kernel function: {e}"))?;

        let pipeline_state = device
            .new_compute_pipeline_state_with_function(&function)
            .map_err(|e| format!("Failed to create pipeline state: {e}"))?;

        let max_threads = pipeline_state.max_total_threads_per_threadgroup();
        tracing::info!("Max threads per threadgroup: {max_threads}");

        let queue = device.new_command_queue();

        Ok(Self {
            device,
            queue,
            pipeline_state,
            max_threads_per_threadgroup: max_threads,
        })
    }

    /// Create Metal buffers for mining.
    pub fn create_buffers(&self, batch_size: u64) -> MiningBuffers {
        // Input buffer: 21 uint32 values
        // [0..7] midstate, [8..11] tail, [12..19] target, [20] nonce_start
        let input_size = 21 * std::mem::size_of::<u32>();
        let input_buffer = self.device.new_buffer(
            input_size as u64,
            MTLResourceOptions::StorageModeShared,
        );

        // Output buffer: 10 uint32 values (found_flag, nonce, hash[8])
        let output_size = 10 * std::mem::size_of::<u32>();
        let output_buffer = self.device.new_buffer(
            output_size as u64,
            MTLResourceOptions::StorageModeShared,
        );

        MiningBuffers {
            input: input_buffer,
            output: output_buffer,
            batch_size,
        }
    }
}

pub struct MiningBuffers {
    pub input: Buffer,
    pub output: Buffer,
    #[allow(dead_code)]
    pub batch_size: u64,
}

impl MiningBuffers {
    /// Write mining work data to the input buffer.
    pub fn set_work(&self, midstate: &[u32; 8], tail: &[u32; 4], target: &[u32; 8], nonce_start: u32) {
        let ptr = self.input.contents() as *mut u32;
        unsafe {
            for i in 0..8 {
                ptr.add(i).write(midstate[i]);
            }
            for i in 0..4 {
                ptr.add(8 + i).write(tail[i]);
            }
            for i in 0..8 {
                ptr.add(12 + i).write(target[i]);
            }
            ptr.add(20).write(nonce_start);
        }
    }

    /// Clear the output buffer (reset found flag).
    pub fn clear_output(&self) {
        let ptr = self.output.contents() as *mut u32;
        unsafe {
            for i in 0..10 {
                ptr.add(i).write(0);
            }
        }
    }

    /// Read the output buffer. Returns Some((nonce, hash)) if found.
    pub fn read_output(&self) -> Option<(u32, [u32; 8])> {
        let ptr = self.output.contents() as *const u32;
        unsafe {
            let found = ptr.read();
            if found != 0 {
                let nonce = ptr.add(1).read();
                let mut hash = [0u32; 8];
                for i in 0..8 {
                    hash[i] = ptr.add(2 + i).read();
                }
                Some((nonce, hash))
            } else {
                None
            }
        }
    }
}
