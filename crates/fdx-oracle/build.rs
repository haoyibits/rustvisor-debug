//! Compiles the vendor agent for the host, with no T32_FDX_* option: only
//! the BufferE paths (`T32_Fdx_SendPoll`, `T32_Fdx_ReceivePoll`) are used.

fn main() {
    cc::Build::new()
        .file("vendor/t32fdx.c")
        .include("vendor")
        .warnings(false)
        .compile("t32fdx");
    println!("cargo:rerun-if-changed=vendor/t32fdx.c");
    println!("cargo:rerun-if-changed=vendor/t32fdx.h");
}
