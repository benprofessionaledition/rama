use std::collections::HashMap;
use std::sync::Arc;

use cudarc::driver::sys::CUdeviceptr;
use cudarc::driver::{CudaDevice, DriverError, LaunchAsync, LaunchConfig, CudaFunction};
use cudarc::nvrtc::compile_ptx;

use crate::{RunStateGPU, Config};

use super::device::Device;

const PTX_SRC: &str = "
extern \"C\" __global__ void matmul(float* A, float* B, float* C, int width, int C_rows, int C_cols) {
    int ROW = blockIdx.y*blockDim.y+threadIdx.y;
    int COL = blockIdx.x*blockDim.x+threadIdx.x;

    if (ROW < C_rows && COL < C_cols) {
        float tmpSum = 0;
        // each thread computes one element of the block sub-matrix
        for (int i = 0; i < width; i++) {
            tmpSum += A[ROW * width + i] * B[i * C_cols + COL];
        }
        C[ROW * C_cols + COL] = tmpSum;
    }
}

extern \"C\" __global__ void copy_from_slice(float *src, float *dest, int n) {
    int i = blockIdx.x*blockDim.x+threadIdx.x;
    if (i < n) {
        dest[i] = src[i];
    }
}

extern \"C\" __global__ void rmsnorm(float *output, float *input, float *weight, int start, int N) {
    int i = blockIdx.x*blockDim.x+threadIdx.x;
    if (i == 1) {
        float sum = 0.0;
        for (int d = 0; d < N; d++) {
            sum += input[d] * input[d];
        }
        float v = 1.0 / sqrtf((sum / N) + 0.00001);
        for (int k = 0; k < N; k++) {
            output[k] = weight[start + k] * v * input[k];
        }

        // output[i] = sum; //input[i];// weight[start + i];
    }

}

extern \"C\" __global__ void apply_position(float *q, float *k, float *pos_real, float *pos_img, int n_heads, int head_size) {
    int i = blockIdx.x*blockDim.x+threadIdx.x;
    if (i < head_size / 2) {
        float fcr = pos_real[i];
        float fci = pos_img[i];
        q[i * 2] = q[i * 2] * fcr - q[i * 2 + 1] * fci;
        q[i * 2 + 1] = q[i * 2] * fcr + q[i * 2 + 1] * fcr;
        k[i * 2] = k[i * 2] * fcr - k[i * 2 + 1] * fci;
        k[i * 2 + 1] = k[i * 2] * fcr + k[i * 2 + 1] * fcr;
    }
}

extern \"C\" __global__ void softmax(float *arr, int N) {

    int i = blockIdx.x*blockDim.x+threadIdx.x;
    if (i != 1) {return;}
    // replace this max with a CUDA reduction function.
    float max = -10.0;
    for (int idx = 0; idx < N; idx++) {
        if (arr[idx] > max) {
            max = arr[idx];
        }
    }
    for (int i = 0; i < N; i++){
        arr[i] = expf(arr[i] - max);
    }
    float sum = 0;
    for (int j = 0; j < N; j++) {
        sum += arr[j];
    }
    for (int j = 0; j < N; j++) {
        arr[j] /= sum;
    }


}


extern \"C\" __global__ void multi_head_attention(float *xb, float *att, float *q, float *k_cache, float *v_cache, int layer, int dim, int pos, int head_size, int seq_len, int n_heads) {

    // replace the input config with a config struct. the code below also needs serious refactoring later
    // after correctness checks.
    int hh = blockIdx.x*blockDim.x+threadIdx.x;
    if (hh != 1) {
        return;
    }
    for (int h = 0; h < n_heads; h++) {
        int loff = layer * seq_len * dim;
        float *q_h = q + h * head_size;
        float *att_h = att + h * seq_len;

        for (int t = 0; t < pos + 1; t++) {
            int koff = loff + t * dim + h * head_size;
            float *k = k_cache + koff;

            float sum = 0.0;
            for (int idx = 0; idx < head_size; idx++) {
                sum += q_h[idx] * k[idx];
            }

            sum = sum / sqrtf(head_size);
            att_h[t] = sum;
        }

        softmax(att_h, pos + 1);

        float *xb_tmp = xb + h * head_size;
        for (int k = 0; k < head_size; k++) {
            xb_tmp[k] = 0;
        }
        for (int t = 0; t < pos + 1; t++) {
            int koff = loff + t * dim + h * head_size;
            float *v = v_cache + koff;
            float a = att_h[t];
            for (int xbi = 0; xbi < head_size; xbi++) {
                xb_tmp[xbi] += a * v[xbi];

                ///TDBD
                // xb_tmp[xbi] = v[0];
            }
        }
    }

}


extern \"C\" __global__ void array_add(float *x, float *xb, int N) {
    int i = blockIdx.x*blockDim.x+threadIdx.x;
    if (i < N) {
        x[i] += xb[i];
    }
}

extern \"C\" __global__ void array_mult(float *x, float *xb, int N) {
    int i = blockIdx.x*blockDim.x+threadIdx.x;
    if (i < N) {
        x[i] *= xb[i];
    }
}

extern \"C\" __global__ void sinu(float *x, int N) {
    int i = blockIdx.x*blockDim.x+threadIdx.x;
    if (i < N) {
        x[i] = x[i] * (1.0 / (1.0 + expf(-x[i])));
    }
}

extern \"C\" __global__ void another(float *arr, int N) {
    int i = blockIdx.x*blockDim.x+threadIdx.x;
    if (i < N) {
        arr[i] = 100.0;
    }

}

extern \"C\" __global__ void test_call_another(float *arr, int N) {
    int i = blockIdx.x*blockDim.x+threadIdx.x;

    if (i < N) {
        another(arr, N);
    }
}

";

///
/// Brief Introduction to CUDA Programming
/// blockIdx.x, blockIdx.y, blockIdx.z are built-in variables that returns the block ID
/// in the x-axis, y-axis, and z-axis of the block that is executing the given block of code.
///
/// threadIdx.x, threadIdx.y, threadIdx.z are built-in variables that return the
/// thread ID in the x-axis, y-axis, and z-axis of the thread that is being executed by this
/// stream processor in this particular block.
///
/// blockDim.x, blockDim.y, blockDim.z are built-in variables that return the “block
/// dimension” (i.e., the number of threads in a block in the x-axis, y-axis, and z-axis).
///
/// The full global thread ID in x dimension can be computed by:
///  x = blockIdx.x * blockDim.x + threadIdx.x;
///
/// Personally I found this blog post quite easy to follow or as a reference:
///     https://www.quantstart.com/articles/Matrix-Matrix-Multiplication-on-the-GPU-with-Nvidia-CUDA/
///

const ROW_TILE_WIDTH: usize = 32;
const COL_TILE_WIDTH: usize = 32;

pub struct GPU {
    // Reference to the GPU device.
    pub gpu: Arc<CudaDevice>,
    // A map from string to a loaded function in the device.
    pub cuda_funs: HashMap<String, CudaFunction>,
}

///
/// Expected APIs:
/// let g = GPU::new();
/// g.copy_weights(); // copy transformer weights & state weights into GPU memory.
/// g.matmul(o, a, b);
/// let host_o = mut [X; Y];
/// g.copy_to_host(o, host_o);
///
impl GPU {
    pub fn new() -> Self {
        let dev = CudaDevice::new(0).unwrap();
        let ptx = compile_ptx(PTX_SRC).unwrap();
        dev.load_ptx(ptx, "module", &["matmul", "copy_from_slice",
            "rmsnorm", "apply_position", "softmax", "multi_head_attention",
            "array_add", "array_mult", "sinu",
            "test_call_another"]).unwrap();
        // let f: CudaFunction = dev.get_func("matmul", "matmul").unwrap();
        let cf = HashMap::new();
        // cf.insert("matmul".to_string(), f);
        Self {
            gpu: dev,
            cuda_funs: cf,
        }
    }

    pub fn array_add(&self, output: CUdeviceptr, inp: CUdeviceptr, n: usize) {
        let f = self.gpu.get_func("module", "array_add").unwrap();
        unsafe { f.launch(LaunchConfig::for_num_elems(n as u32), (output, inp, n,)) }.unwrap();
    }

    pub fn array_mult(&self, output: CUdeviceptr, inp: CUdeviceptr, n: i32) {
        let f = self.gpu.get_func("module", "array_mult").unwrap();
        unsafe { f.launch(LaunchConfig::for_num_elems(n as u32), (output, inp, n,)) }.unwrap();
    }

    pub fn sinu(&self, output: CUdeviceptr, n: i32) {
        let f = self.gpu.get_func("module", "sinu").unwrap();
        unsafe { f.launch(LaunchConfig::for_num_elems(n as u32), (output, n,)) }.unwrap();

    }

    pub fn multi_head_attention(&self, gpu_state: &RunStateGPU, cfg: &Config, layer: usize, pos: usize) {

        // extern \"C\" __global__ void multi_head_attention(float *xb, float *att, float *q, float *k_cache, float *v_cache, int layer, int dim, int pos, int head_size, int seq_len, int n_heads) {

        let head_size = cfg.dim / cfg.n_heads;
        let f = self.gpu.get_func("module", "multi_head_attention").unwrap();
        unsafe { f.launch(LaunchConfig::for_num_elems(cfg.n_heads as u32), (
            gpu_state.xb,
            gpu_state.att,
            gpu_state.q,
            gpu_state.key_cache,
            gpu_state.value_cache,
            layer,
            cfg.dim,
            pos,
            head_size,
            cfg.seq_len,
            cfg.n_heads,
        )) }.unwrap();

    }

    pub fn copy_from_slice(&self, src: CUdeviceptr, dest: CUdeviceptr, n: i32) {
        let f = self.gpu.get_func("module", "copy_from_slice").unwrap();
        unsafe { f.launch(LaunchConfig::for_num_elems(n as u32), (src, dest, n,)) }.unwrap();
    }

    pub fn rmsnorm(&self, o: CUdeviceptr, x: CUdeviceptr, w: CUdeviceptr, start: i32, n: i32) {
        let f = self.gpu.get_func("module", "rmsnorm").unwrap();
        unsafe { f.launch(LaunchConfig::for_num_elems(n as u32), (o, x, w, start, n,)) }.unwrap();
    }

    pub fn matmul2(&self, o: CUdeviceptr, a: CUdeviceptr, b: CUdeviceptr, width: usize, o_rows: i32, o_cols: i32) {
        let f = self.gpu.get_func("module", "matmul").unwrap();
        let cfg = LaunchConfig {
            block_dim: (COL_TILE_WIDTH as u32, ROW_TILE_WIDTH as u32, 1),
            grid_dim: ((o_cols/COL_TILE_WIDTH as i32 + 2) as u32, (o_rows/ROW_TILE_WIDTH as i32 + 2) as u32, 1),
            shared_mem_bytes: 0,
        };
        unsafe { f.launch(cfg, (a, b, o, width, o_rows, o_cols)) }.unwrap();
    }

    pub fn apply_position(&self, q: CUdeviceptr, k: CUdeviceptr, pos_real: CUdeviceptr, pos_img: CUdeviceptr, n_heads: i32, head_size: i32) {
        let f = self.gpu.get_func("module", "apply_position").unwrap();
        unsafe { f.launch(LaunchConfig::for_num_elems((head_size / 2 + 1) as u32), (q, k, pos_real, pos_img, n_heads, head_size)) }.unwrap();
    }

    pub fn softmax(&self, arr: CUdeviceptr, size: i32) {
        let f = self.gpu.get_func("module", "softmax").unwrap();
        unsafe { f.launch(LaunchConfig::for_num_elems(size as u32), (arr, size)) }.unwrap();
    }

    ///
    /// o_buf: the buffer to write the GPU ram into
    pub fn debug(&self, o_buf: &mut Vec<f32>, input: CUdeviceptr) {
        // unsafe { let _ = memcpy_dtoh_sync(o_buf, input); };
        // println!("--------------------\noutput_buf is: {:?}\n", o_buf)
    }
}

impl Device for GPU {
    type Err = DriverError;
    fn matmul(o: &mut [f32], a: &[f32], b: &[f32], width: usize, o_rows: usize, o_cols: usize) {
        let ptx = compile_ptx(PTX_SRC).unwrap();

        let dev = CudaDevice::new(0)?;

        dev.load_ptx(ptx, "matmul", &["matmul"]).unwrap();
        let f = dev.get_func("matmul", "matmul").unwrap();
        let a_dev = dev.htod_sync_copy(&a)?;
        let b_dev: cudarc::driver::CudaSlice<f32> = dev.htod_sync_copy(&b)?;
        let mut o_dev = dev.htod_sync_copy(&o)?;
        // println!("Copied in {:?}", start.elapsed());

        let cfg = LaunchConfig {
            block_dim: (COL_TILE_WIDTH as u32, ROW_TILE_WIDTH as u32, 1),
            grid_dim: ((o_cols/COL_TILE_WIDTH + 1) as u32, (o_rows/ROW_TILE_WIDTH + 1) as u32, 1),
            shared_mem_bytes: 0,
        };

        // let cfg = LaunchConfig {
        //     block_dim: (o_cols as u32, o_rows as u32, 1),
        //     grid_dim: (20, 20, 1),
        //     shared_mem_bytes: 0,
        // };

        unsafe { f.launch(cfg, (&a_dev, &b_dev, &mut o_dev, width, o_rows, o_cols)) }?;
        dev.dtoh_sync_copy_into(&o_dev,  o)?;
        // println!("Found {:?} in {:?}", o, start.elapsed());

        Ok(())

    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use cudarc::driver::{sys::CUdeviceptr, CudaSlice, DevicePtr, DeviceRepr};
    use rand::prelude::*;

    #[test]
    fn test_test_call_another() {
        let a_host = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];

        let gpu = GPU::new();
        let f = gpu.gpu.get_func("module", "test_call_another").unwrap();
        let a_dev = gpu.gpu.htod_sync_copy(&a_host).unwrap();
        unsafe { f.launch(LaunchConfig::for_num_elems(6), (&a_dev, 4,)) }.unwrap();
        let b_host = gpu.gpu.sync_reclaim(a_dev).unwrap();
        let b_host_eval = [100.0f32, 100.0, 100.0, 100.0, 5.0, 6.0];
        println!("{:?}", b_host);
        assert_eq!(b_host, b_host_eval)
    }

    #[test]
    fn test_softmax() {
        let gpu = GPU::new();
        let a_host = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let a_dev = gpu.gpu.htod_sync_copy(&a_host).unwrap();
        gpu.softmax(*a_dev.device_ptr(), 6);
        let b_host = gpu.gpu.sync_reclaim(a_dev).unwrap();
        let b_expected = [0.0042697787, 0.011606461, 0.031549633, 0.085760795, 0.23312204, 0.6336913];
        b_host.iter().zip(b_expected).for_each(|(t, r)|
            {
                assert!((*t - r).abs() < f32::EPSILON);
            }
        );
    }

    #[test]
    fn test_matrix_mul2() {
        let a_host = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let b_host = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mut c_host = [0.0f32; 4];
        let _ = GPU::matmul(&mut c_host, &a_host, &b_host, 3, 2, 2);

        assert_eq!(c_host, [22.0, 28.0, 49.0, 64.0]);

        let mut rng = thread_rng();

        // Test size larger than 1024 threads
        const SIZE: usize = 288*288;
        let mut arr1 = [0.0f32; SIZE];
        let mut arr2 = [0.0f32; SIZE];
        let mut oo = [0.0f32; 288];
        for i in 0..SIZE {
            arr1[i] = rng.gen::<f32>();
            arr2[i] = rng.gen::<f32>();
        }

        let e = GPU::matmul(&mut oo, &arr1, &arr2, 288, 288, 288);
        match e {
            Ok(_) => (),
            Err(_) => panic!("error!"),
        }

        assert_ne!(oo[0], 0.0f32);
        assert_ne!(oo[287], 0.0f32);
    }

}