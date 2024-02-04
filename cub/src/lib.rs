use cuda::{nvrtc::compile, AsRaw, ContextGuard, DevSlice, KernelFn, Stream};
use std::ffi::{c_uint, c_void};

pub struct ReduceMean {
    f: KernelFn,
    block_size: usize,
}

impl ReduceMean {
    pub fn new(max_item_size: usize, block_size: usize, ctx: &ContextGuard) -> Self {
        let ty_arg = "float";
        let ty_cal = "float";
        let item_per_thread = (max_item_size + block_size - 1) / block_size;
        let name = format!("reduce_mean_{item_per_thread}_{block_size}");
        let code = format!(
            r#"
#include <cub/block/block_reduce.cuh>

extern "C" __global__ void {name}(
    {ty_arg} const *__restrict__ x_,
    {ty_arg}       *__restrict__ y_,
    {ty_arg} init,
    unsigned int leading_dim,
    unsigned int item_size
) {{
    auto x = x_ + blockIdx.x * leading_dim;
    auto y = y_ + blockIdx.x;

    {ty_cal} thread_data[{item_per_thread}];
    for (unsigned int i = threadIdx.x, j = 0; j < {item_per_thread}; i += blockDim.x, ++j) {{
        thread_data[j] = {ty_cal}(i < item_size ? x[i] : init);
    }}

    using BlockReduce = cub::BlockReduce<{ty_cal}, {block_size}>;
    __shared__ typename BlockReduce::TempStorage tempStorage;
    auto acc = BlockReduce(tempStorage).Reduce(thread_data, cub::Sum());

    if (threadIdx.x == 0) *y = {ty_arg}(acc / {ty_cal}(item_size));
}}
"#
        );
        compile(&code, &[&name], ctx);
        Self {
            f: KernelFn::get(&name).unwrap(),
            block_size,
        }
    }

    pub fn launch(&self, x: &DevSlice, y: &DevSlice, item_len: usize, stream: &Stream) {
        let row = y.len();
        let leading_dim = x.len() / row;
        debug_assert_eq!(x.len() % row, 0);

        let x_ptr = unsafe { x.as_raw() };
        let y_ptr = unsafe { y.as_raw() };
        let init = 0.0f32;
        let leading_dim = leading_dim as c_uint;
        let item_len = item_len as c_uint;
        let params: &[*const c_void] = &[
            (&x_ptr) as *const _ as _,
            (&y_ptr) as *const _ as _,
            (&init) as *const _ as _,
            (&leading_dim) as *const _ as _,
            (&item_len) as *const _ as _,
        ];
        self.f.launch(
            row as c_uint,
            self.block_size as c_uint,
            params.as_ptr(),
            0,
            Some(&stream),
        );
    }
}

#[cfg(test)]
mod test {
    use super::ReduceMean;
    use cuda::{nvrtc::compile, AsRaw, KernelFn};
    use rand::Rng;
    use std::ffi::{c_uint, c_void};

    /// 计算 reduceMean 并与 kernel 函数的结果进行比较。
    fn check(data: &[f32], result: &[f32], item_len: usize) -> (f64, f64) {
        // &self, x: &DevSlice, y: &DevSlice, item_len: usize
        let row = result.len();
        let col = data.len() / row;
        debug_assert_eq!(data.len(), row * col);

        let mut answer = vec![0.0f32; row];
        for (i, ans) in answer.iter_mut().enumerate() {
            *ans = data[i * col..][..item_len].iter().sum::<f32>() / item_len as f32;
        }
        test_utils::diff(&result, &answer)
    }

    #[test]
    fn general() {
        const ROW: usize = 256;
        const COL: usize = 256;

        let name = "reduce_mean_general";
        let code = format!(
            r#"
#include <cub/block/block_reduce.cuh>

extern "C" __global__ void {name}(
    float const *__restrict__ x_,
    float       *__restrict__ y_
) {{
    auto x = x_ + blockIdx.x * {COL};
    auto y = y_ + blockIdx.x;

    using BlockReduce = cub::BlockReduce<float, {COL}>;
    __shared__ typename BlockReduce::TempStorage tempStorage;
    auto acc = BlockReduce(tempStorage).Reduce(x[threadIdx.x], cub::Sum());
    if (threadIdx.x == 0) *y = acc / {COL};
}}
"#
        );

        cuda::init();
        let Some(dev) = cuda::Device::fetch() else {
            return;
        };
        dev.context().apply(|ctx| {
            compile(&code, &[&name], ctx);
            let function = KernelFn::get(&name).unwrap();

            let stream = ctx.stream();
            let mut rng = rand::thread_rng();
            let mut x_data = vec![0.0f32; ROW * COL];
            rng.fill(&mut x_data[..]);
            let x = stream.from_slice(&x_data);
            let y = stream.malloc_for::<f32>(ROW);
            let y = y.as_slice(ctx);

            {
                let x_ptr = unsafe { x.as_raw() };
                let y_ptr = unsafe { y.as_raw() };
                let params: [*const c_void; 2] =
                    [(&x_ptr) as *const _ as _, (&y_ptr) as *const _ as _];
                function.launch(
                    ROW as c_uint,
                    COL as c_uint,
                    params.as_ptr(),
                    0,
                    Some(&stream),
                );
                stream.synchronize();
            }

            let mut result = vec![0.0f32; ROW];
            y.copy_out(&mut result);
            let (abs_diff, rel_diff) = check(&x_data, &result, COL);
            assert!(abs_diff < 1e-6, "abs_diff: {abs_diff}");
            assert!(rel_diff < 1e-6, "rel_diff: {rel_diff}");
        });
    }

    #[test]
    fn padding() {
        const ROW: usize = 256;
        const COL: usize = 711;
        const BLOCK_SIZE: usize = 1024;

        let name = "reduce_mean_padding";
        let code = format!(
            r#"
#include <cub/block/block_reduce.cuh>

extern "C" __global__ void {name}(
    float const *__restrict__ x_,
    float       *__restrict__ y_,
    unsigned int item_size
) {{
    auto x = x_ + blockIdx.x * item_size;
    auto y = y_ + blockIdx.x;

    using BlockReduce = cub::BlockReduce<float, {BLOCK_SIZE}>;
    __shared__ typename BlockReduce::TempStorage tempStorage;
    auto acc = BlockReduce(tempStorage).Reduce(x[threadIdx.x], cub::Sum(), item_size);
    if (threadIdx.x == 0) *y = acc / float(item_size);
}}
"#
        );

        cuda::init();
        let Some(dev) = cuda::Device::fetch() else {
            return;
        };
        dev.context().apply(|ctx| {
            compile(&code, &[&name], ctx);
            let function = KernelFn::get(&name).unwrap();

            let stream = ctx.stream();
            let mut rng = rand::thread_rng();
            let mut x_data = vec![0.0f32; ROW * COL];
            rng.fill(&mut x_data[..]);
            let x = stream.from_slice(&x_data);
            let y = stream.malloc_for::<f32>(ROW);
            let y = y.as_slice(ctx);

            {
                let x_ptr = unsafe { x.as_raw() };
                let y_ptr = unsafe { y.as_raw() };
                let item_size = COL as u32;
                let params: &[*const c_void] = &[
                    (&x_ptr) as *const _ as _,
                    (&y_ptr) as *const _ as _,
                    (&item_size) as *const _ as _,
                ];
                function.launch(
                    ROW as c_uint,
                    BLOCK_SIZE as c_uint,
                    params.as_ptr(),
                    0,
                    Some(&stream),
                );
                stream.synchronize();
            }

            let mut result = vec![0.0f32; ROW];
            y.copy_out(&mut result);
            let (abs_diff, rel_diff) = check(&x_data, &result, COL);
            assert!(abs_diff < 1e-6, "abs_diff: {abs_diff}");
            assert!(rel_diff < 1e-6, "rel_diff: {rel_diff}");
        });
    }

    #[test]
    fn folding() {
        const ROW: usize = 256;
        const COL: usize = 1024;
        const VALID: usize = 711;
        const BLOCK_SIZE: usize = 256;

        cuda::init();
        let Some(dev) = cuda::Device::fetch() else {
            return;
        };
        dev.context().apply(|ctx| {
            let kernel = ReduceMean::new(COL, BLOCK_SIZE, ctx);

            let stream = ctx.stream();
            let mut rng = rand::thread_rng();
            let mut x_data = vec![0.0f32; ROW * COL];
            rng.fill(&mut x_data[..]);
            let x = stream.from_slice(&x_data);
            let y = stream.malloc_for::<f32>(ROW);
            let y = y.as_slice(ctx);

            kernel.launch(&x.as_slice(ctx), &y, VALID, &stream);

            let mut result = vec![0.0f32; ROW];
            y.copy_out(&mut result);
            let (abs_diff, rel_diff) = check(&x_data, &result, VALID);
            assert!(abs_diff < 1e-6, "abs_diff: {abs_diff}");
            assert!(rel_diff < 1e-6, "rel_diff: {rel_diff}");
        });
    }
}