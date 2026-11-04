use std::{env, path::PathBuf};

/// Every generated client and server must default to Masa's hooks. Without
/// this, rpcstack's tonic-build falls back to `tonic::rpcstack::noop::NoopHooks`
/// and the policy silently does nothing. `tests/hooks.rs` guards this.
const DEFAULT_HOOKS: &str = "masa::DefaultHooks";

const PROTOS: &[&str] = &[
    "textservice",
    "media",
    "url_shorten",
    "usermention",
    "user",
    "user_timeline",
    "home_timeline",
    "post_storage",
    "uniqueid",
    "compose_post",
    "register_user",
    "frontend",
    "social_graph",
];

fn main() {
    // Vendored protoc: no system protobuf-compiler needed.
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    for name in PROTOS {
        let proto = format!("proto/{name}.proto");
        println!("cargo:rerun-if-changed={proto}");
        tonic_build::configure()
            .default_hooks_path(DEFAULT_HOOKS)
            .file_descriptor_set_path(out_dir.join(format!("{name}_descriptor.bin")))
            .compile(&[proto], &["proto"])
            .unwrap();
    }
}
