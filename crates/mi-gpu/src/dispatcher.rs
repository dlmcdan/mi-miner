use crate::pipeline::{MetalPipeline, MiningBuffers};
use metal::*;

/// Dispatches GPU compute work for SHA-256d mining.
pub struct GpuDispatcher {
    pipeline: MetalPipeline,
    buffers_a: MiningBuffers,
    #[allow(dead_code)]
    buffers_b: MiningBuffers, // reserved for double-buffering
    batch_size: u64,
    threadgroup_size: u64,
}

impl GpuDispatcher {
    pub fn new(pipeline: MetalPipeline, batch_size_log2: u32) -> Self {
        let batch_size = 1u64 << batch_size_log2;
        let threadgroup_size = pipeline
            .max_threads_per_threadgroup
            .min(256); // Typically 256 or 1024

        let buffers_a = pipeline.create_buffers(batch_size);
        let buffers_b = pipeline.create_buffers(batch_size);

        Self {
            pipeline,
            buffers_a,
            buffers_b,
            batch_size,
            threadgroup_size,
        }
    }

    /// Dispatch a batch of nonces for mining.
    /// Returns the number of nonces dispatched and optionally a found result.
    pub fn dispatch_batch(
        &self,
        midstate: &[u32; 8],
        tail: &[u32; 4],
        target: &[u32; 8],
        nonce_start: u32,
        intensity: f32,
    ) -> Option<(u32, [u32; 8])> {
        let actual_batch = ((self.batch_size as f32 * intensity) as u64).max(1024);

        let buffers = &self.buffers_a;
        buffers.set_work(midstate, tail, target, nonce_start);
        buffers.clear_output();

        let command_buffer = self.pipeline.queue.new_command_buffer();
        let encoder = command_buffer.new_compute_command_encoder();

        encoder.set_compute_pipeline_state(&self.pipeline.pipeline_state);
        encoder.set_buffer(0, Some(&buffers.input), 0);
        encoder.set_buffer(1, Some(&buffers.output), 0);

        let grid_size = MTLSize::new(actual_batch, 1, 1);
        let threadgroup_size = MTLSize::new(self.threadgroup_size, 1, 1);

        encoder.dispatch_threads(grid_size, threadgroup_size);
        encoder.end_encoding();

        command_buffer.commit();
        command_buffer.wait_until_completed();

        buffers.read_output()
    }

    /// Get the current batch size (affected by intensity).
    pub fn batch_size(&self) -> u64 {
        self.batch_size
    }

    /// Update batch size.
    #[allow(dead_code)]
    pub fn set_batch_size_log2(&mut self, log2: u32) {
        self.batch_size = 1u64 << log2;
    }
}
