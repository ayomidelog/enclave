use super::splice_to_pipe;
use std::fs;
use std::io::Read;
use std::os::fd::FromRawFd;

#[test]
fn splice_streams_regular_file_into_pipe_or_reports_unsupported() {
    let path = std::env::temp_dir().join(format!(
        "enclave-splice-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::write(&path, b"splice fixture").expect("write source");
    let source = fs::File::open(&path).expect("open source");
    let mut pipe = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let result = splice_to_pipe(&source, pipe[1], 14).expect("splice result");
    unsafe { libc::close(pipe[1]) };
    if result {
        let mut output = String::new();
        unsafe { fs::File::from_raw_fd(pipe[0]) }
            .read_to_string(&mut output)
            .expect("read pipe");
        assert_eq!(output, "splice fixture");
    } else {
        unsafe { libc::close(pipe[0]) };
    }
    let _ = fs::remove_file(path);
}
