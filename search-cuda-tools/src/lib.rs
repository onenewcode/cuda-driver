use std::{path::PathBuf, process::Command};

pub use find_cuda_helper::{find_cuda_root, include_cuda};

pub fn find_nccl() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let Some(cuda_root) = find_cuda_helper::find_cuda_root() else {
        return None;
    };
    let Ok(output) = Command::new("ldconfig").arg("-p").output() else {
        return None;
    };
    if !unsafe { String::from_utf8_unchecked(output.stdout) }.contains("nccl") {
        return None;
    }
    Some(cuda_root)
}

#[inline]
pub fn detect_cuda() {
    println!("cargo:rustc-cfg=detected_cuda");
}

#[inline]
pub fn detect_nccl() {
    println!("cargo:rustc-cfg=detected_nccl");
}