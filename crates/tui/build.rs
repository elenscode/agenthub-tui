use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};
fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut entries = String::new();
    for (arch, key) in [
        ("x86_64", "AGENTHUB_HELPER_X64"),
        ("aarch64", "AGENTHUB_HELPER_ARM64"),
    ] {
        println!("cargo:rerun-if-env-changed={key}");
        let data = env::var_os(key)
            .map(|p| {
                println!("cargo:rerun-if-changed={}", PathBuf::from(&p).display());
                fs::read(p).expect("read specified WSL Helper")
            })
            .unwrap_or_default();
        if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
            && env::var("PROFILE").as_deref() == Ok("release")
        {
            assert!(!data.is_empty(), "Windows release packaging requires {key}");
        }
        if !data.is_empty() {
            let machine: u16 = if arch == "x86_64" { 62 } else { 183 };
            assert!(
                data.starts_with(b"\x7fELF\x02\x01")
                    && data.get(18..20) == Some(&machine.to_le_bytes()),
                "Incorrect Linux Helper architecture for {key}"
            );
        }
        let path = out.join(format!("helper-{arch}"));
        fs::write(&path, &data).unwrap();
        entries += &format!(
            "({arch:?}, include_bytes!({path:?}) as &[u8], {:?}),\n",
            hex::encode(Sha256::digest(&data))
        );
    }
    fs::write(out.join("bundle.rs"), format!("&[{entries}]")).unwrap();
}
