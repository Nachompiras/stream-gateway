extern crate bindgen;

use std::env;
use std::path::PathBuf;

fn main() {
    // Tell cargo to look for the SRT library
    println!("cargo:rustc-link-lib=srt");
    println!("cargo:rerun-if-changed=wrapper.h");

    // Use bindgen to generate Rust bindings
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .generate()
        .expect("Unable to generate bindings");
    
    println!("OUT_DIR: {}", env::var("OUT_DIR").unwrap());
            // Write the bindings to the output directory
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}