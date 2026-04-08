/// Ask glibc to return free heap pages to the OS so RSS reflects actual usage.
pub fn trim_heap() {
    unsafe { libc::malloc_trim(0); }
}
