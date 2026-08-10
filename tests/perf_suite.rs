use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use enclave::registry::{ensure_registry, with_registry, Registry};

fn temp_state() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "enclave-performance-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&path).expect("create benchmark state");
    path
}

#[test]
#[ignore = "performance benchmark"]
fn registry_read_cache_benchmark() {
    let state = temp_state();
    ensure_registry(&state).expect("initialize registry");
    let registry_path = state.join("registry.json");
    let payload = serde_json::to_vec(&Registry::default()).expect("serialize registry");
    fs::write(&registry_path, payload).expect("write registry");

    let cold_start = Instant::now();
    let mut cold_value = 0u32;
    for _ in 0..1_000 {
        let raw = fs::read(&registry_path).expect("read registry");
        let registry: Registry = serde_json::from_slice(&raw).expect("parse registry");
        cold_value = cold_value.wrapping_add(registry.version);
    }
    let cold_elapsed = cold_start.elapsed();

    let warm_start = Instant::now();
    let mut warm_value = 0u32;
    for _ in 0..1_000 {
        warm_value = warm_value.wrapping_add(
            with_registry(&state, |registry| Ok(registry.version)).expect("cached registry read"),
        );
    }
    let warm_elapsed = warm_start.elapsed();

    black_box((cold_value, warm_value));
    println!("benchmark=registry_read iterations=1000");
    println!("cold_parse_seconds={:.9}", cold_elapsed.as_secs_f64());
    println!("cached_read_seconds={:.9}", warm_elapsed.as_secs_f64());
    println!(
        "speedup={:.2}x",
        cold_elapsed.as_secs_f64() / warm_elapsed.as_secs_f64()
    );
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "performance benchmark"]
fn single_file_archive_benchmark() {
    let state = temp_state();
    let source = state.join("fixture.bin");
    let output = state.join("archive.tar");
    let file = fs::File::create(&source).expect("create fixture");
    file.set_len(16 * 1024 * 1024).expect("size fixture");

    let external_start = Instant::now();
    for _ in 0..10 {
        let status = Command::new("tar")
            .args([
                "-C",
                state.to_str().expect("state path"),
                "-cf",
                output.to_str().expect("output path"),
                "--",
                "fixture.bin",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run tar");
        assert!(status.success());
    }
    let external_elapsed = external_start.elapsed();

    let rust_start = Instant::now();
    for _ in 0..10 {
        let output_file = fs::File::create(&output).expect("create archive output");
        let mut archive = tar::Builder::new(output_file);
        archive
            .append_path_with_name(&source, "fixture.bin")
            .expect("append fixture");
        archive.finish().expect("finish archive");
    }
    let rust_elapsed = rust_start.elapsed();
    black_box((external_elapsed, rust_elapsed));
    println!("benchmark=single_file_archive iterations=10 bytes=16777216");
    println!("external_tar_seconds={:.9}", external_elapsed.as_secs_f64());
    println!("rust_archive_seconds={:.9}", rust_elapsed.as_secs_f64());
    println!(
        "speedup={:.2}x",
        external_elapsed.as_secs_f64() / rust_elapsed.as_secs_f64()
    );
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "performance benchmark"]
fn many_file_archive_benchmark() {
    let state = temp_state();
    let source = state.join("many");
    let output = state.join("archive.tar");
    fs::create_dir_all(&source).expect("create many-file fixture");
    let file_count = std::env::var("ENCLAVE_PERF_MANY_FILES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1_000);
    for index in 0..file_count {
        fs::write(source.join(format!("file-{index:04}.bin")), [0u8; 4096])
            .expect("write many-file fixture");
    }

    let external_start = Instant::now();
    for _ in 0..3 {
        let status = Command::new("tar")
            .args([
                "-C",
                state.to_str().expect("state path"),
                "-cf",
                output.to_str().expect("output path"),
                "--",
                "many",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run tar");
        assert!(status.success());
    }
    let external_elapsed = external_start.elapsed();

    let rust_start = Instant::now();
    for _ in 0..3 {
        let output_file = fs::File::create(&output).expect("create archive output");
        let mut archive = tar::Builder::new(output_file);
        archive
            .append_dir_all("many", &source)
            .expect("append many-file fixture");
        archive.finish().expect("finish archive");
    }
    let rust_elapsed = rust_start.elapsed();
    black_box((external_elapsed, rust_elapsed));
    println!(
        "benchmark=many_file_archive iterations=3 files={} bytes={}",
        file_count,
        file_count * 4096
    );
    println!("external_tar_seconds={:.9}", external_elapsed.as_secs_f64());
    println!("rust_archive_seconds={:.9}", rust_elapsed.as_secs_f64());
    println!(
        "speedup={:.2}x",
        external_elapsed.as_secs_f64() / rust_elapsed.as_secs_f64()
    );
    let _ = fs::remove_dir_all(state);
}
