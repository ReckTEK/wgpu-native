use std::collections::BTreeSet;
use std::env;
use std::fmt::Write;
use std::path::PathBuf;
use std::process::Command;

fn sdk_path(sdk_name: &str) -> String {
    if let Ok(path) = env::var("SDKROOT") {
        return path;
    }
    let output = Command::new("xcrun")
        .args(["--sdk", sdk_name, "--show-sdk-path"])
        .output()
        .expect("xcrun failed")
        .stdout;
    std::str::from_utf8(&output)
        .expect("invalid output from `xcrun`")
        .trim()
        .to_owned()
}

fn main() {
    println!("cargo:rerun-if-changed=ffi/webgpu-headers/webgpu.h");
    println!("cargo:rerun-if-changed=ffi/wgpu.h");
    println!("cargo:rerun-if-env-changed=TARGET");
    println!("cargo:rerun-if-env-changed=SDKROOT");
    println!("cargo:rerun-if-env-changed=BINDGEN_EXTRA_CLANG_ARGS");

    #[rustfmt::skip]
    let types_to_rename = vec![
        ("WGPUAdapter", "WGPUAdapterImpl"),
        ("WGPUBindGroup", "WGPUBindGroupImpl"),
        ("WGPUBindGroupLayout", "WGPUBindGroupLayoutImpl"),
        ("WGPUBuffer", "WGPUBufferImpl"),
        ("WGPUCommandBuffer", "WGPUCommandBufferImpl"),
        ("WGPUCommandEncoder", "WGPUCommandEncoderImpl"),
        ("WGPUComputePassEncoder", "WGPUComputePassEncoderImpl"),
        ("WGPUComputePipeline", "WGPUComputePipelineImpl"),
        ("WGPUDevice", "WGPUDeviceImpl"),
        ("WGPUInstance", "WGPUInstanceImpl"),
        ("WGPUPipelineLayout", "WGPUPipelineLayoutImpl"),
        ("WGPUQuerySet", "WGPUQuerySetImpl"),
        ("WGPUQueue", "WGPUQueueImpl"),
        ("WGPURenderBundle", "WGPURenderBundleImpl"),
        ("WGPURenderBundleEncoder", "WGPURenderBundleEncoderImpl"),
        ("WGPURenderPassEncoder", "WGPURenderPassEncoderImpl"),
        ("WGPURenderPipeline", "WGPURenderPipelineImpl"),
        ("WGPUSampler", "WGPUSamplerImpl"),
        ("WGPUShaderModule", "WGPUShaderModuleImpl"),
        ("WGPUSurface", "WGPUSurfaceImpl"),
        ("WGPUTexture", "WGPUTextureImpl"),
        ("WGPUTextureView", "WGPUTextureViewImpl"),
    ];
    let mut builder = bindgen::Builder::default()
        .header("ffi/wgpu.h")
        .clang_arg("-Iffi/webgpu-headers")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .allowlist_item("WGPU.*")
        .allowlist_item("wgpu.*")
        .prepend_enum_name(false)
        .size_t_is_usize(true)
        .layout_tests(true)
        .clang_macro_fallback();

    for (old_name, new_name) in types_to_rename {
        let line = format!("pub type {old_name} = *const crate::{new_name};");
        builder = builder
            .blocklist_type(old_name)
            .blocklist_type(format!("{old_name}Impl"))
            .raw_line(line);
    }

    if let Ok(target) = env::var("TARGET") {
        match target.as_str() {
            "aarch64-apple-ios" => {
                builder = builder
                    .clang_arg("-isysroot")
                    .clang_arg(sdk_path("iphoneos"))
                    .clang_arg("--target=arm64-apple-ios");
            }
            "aarch64-apple-ios-sim" => {
                builder = builder
                    .clang_arg("-isysroot")
                    .clang_arg(sdk_path("iphonesimulator"))
                    .clang_arg("--target=arm64-apple-ios-simulator");
            }
            "x86_64-apple-ios" => {
                builder = builder
                    .clang_arg("-isysroot")
                    .clang_arg(sdk_path("iphonesimulator"))
                    .clang_arg("--target=x86_64-apple-ios-simulator");
            }
            "aarch64-apple-darwin" | "x86_64-apple-darwin" => {
                builder = builder.clang_arg("-isysroot").clang_arg(sdk_path("macosx"));
            }
            "aarch64-apple-tvos" => {
                builder = builder
                    .clang_arg("-isysroot")
                    .clang_arg(sdk_path("appletvos"))
                    .clang_arg("--target=arm64-apple-tvos");
            }
            "aarch64-apple-tvos-sim" | "x86_64-apple-tvos" => {
                let arch = if target.starts_with("aarch64") {
                    "arm64"
                } else {
                    "x86_64"
                };
                builder = builder
                    .clang_arg("-isysroot")
                    .clang_arg(sdk_path("appletvsimulator"))
                    .clang_arg(format!("--target={arch}-apple-tvos-simulator"));
            }
            "aarch64-apple-visionos" | "aarch64-apple-visionos-sim" => {
                let simulator = target.ends_with("-sim");
                builder = builder
                    .clang_arg("-isysroot")
                    .clang_arg(sdk_path(if simulator { "xrsimulator" } else { "xros" }))
                    .clang_arg(if simulator {
                        "--target=arm64-apple-xros-simulator"
                    } else {
                        "--target=arm64-apple-xros"
                    });
            }
            _ => {}
        }
    }

    let bindings = builder.generate().expect("Unable to generate bindings");
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    write_proc_table(&bindings.to_string(), &out_path);
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}

fn write_proc_table(bindings: &str, out_path: &std::path::Path) {
    // Bindgen has already applied the target's preprocessor conditions to both headers.
    let names: BTreeSet<&str> = bindings
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("pub fn "))
        .map(|line| {
            line.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .next()
                .expect("function declaration has no name")
        })
        .filter(|name| name.starts_with("wgpu"))
        .collect();
    assert!(names.contains("wgpuGetProcAddress"));
    assert!(names.contains("wgpuCreateInstance"));
    assert!(names.contains("wgpuSetLogCallback"));

    let mut source = String::from(
        "// Generated from the target's paired C headers.\n\
         fn lookup_proc(name: &[u8]) -> native::WGPUProc {\n\
         let address = match name {\n",
    );
    for name in &names {
        writeln!(source, "b\"{name}\" => native::{name} as *const (),").unwrap();
    }
    source.push_str(
        "_ => return None,\n};\n\
         // WGPUProc is the C API's erased function-pointer type.\n\
         Some(unsafe { std::mem::transmute::<*const (), unsafe extern \"C\" fn()>(address) })\n}\n\
         #[cfg(test)]\nconst PROC_NAMES: &[&[u8]] = &[\n",
    );
    for name in &names {
        writeln!(source, "b\"{name}\",").unwrap();
    }
    source.push_str("];");
    std::fs::write(out_path.join("proc_table.rs"), source)
        .expect("Couldn't write procedure lookup table!");
}
