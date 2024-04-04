﻿use crate::{convert, Communicator, ReduceType};
use cuda::{AsRaw, CudaDataType, DevSlice, Stream};
use std::ffi::c_void;

impl Communicator {
    pub fn all_reduce(
        &self,
        dst: &mut DevSlice,
        src: Option<&DevSlice>,
        dt: CudaDataType,
        op: ReduceType,
        stream: &Stream,
    ) {
        let size = dst.len();
        let recvbuff = unsafe { dst.as_raw() as *mut c_void };
        nccl!(ncclAllReduce(
            if let Some(src) = src {
                assert_eq!(src.len(), size);
                unsafe { src.as_raw() as _ }
            } else {
                recvbuff
            },
            recvbuff,
            size / dt.size(),
            convert(dt),
            op,
            self.as_raw(),
            stream.as_raw() as _,
        ));
    }
}
