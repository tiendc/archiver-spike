fn main() {
    // The `unrar` crate builds UnRAR, which calls advapi32 (CryptGenRandom,
    // the registry, process tokens, SetFileSecurityW) without linking it;
    // std used to pull it in and no longer does.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-lib=advapi32");
    }
}
