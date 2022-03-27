use std::env;

fn main() {
    if env::var("CARGO_FEATURE_LOGIND").ok().is_none() && env::var("CARGO_FEATURE_LIBSEAT").ok().is_none() {
        println!("cargo:warning=You are compiling without logind/libseat support.");
        println!(
            "cargo:warning=This means that you'll likely need to run it as root if you want to launch it from a tty."
        );
        println!("cargo:warning=To enable logind support add `--feature logind` to your cargo invocation.");
        println!("cargo:warning=$ cargo run --feature logind");
        println!("cargo:warning=To enable libseat support add `--feature libseat` to your cargo invocation.");
        println!("cargo:warning=$ cargo run --feature libseat");
    }
}
