fn main() {
    cxx_build::bridge("src/occ.rs")
        .cpp(true)
        .flag_if_supported("-std=c++17")
        .include(occt_sys::occt_include_path())
        .include("include")
        .compile("vcad-reconstruct-occ");

    println!("cargo:rerun-if-changed=src/occ.rs");
    println!("cargo:rerun-if-changed=include/occ_bridge.hxx");
}
