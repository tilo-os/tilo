use futures_util::{pin_mut, stream::StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::{
        fd::{FromRawFd, IntoRawFd, OwnedFd},
        unix::net::UnixStream,
    },
};
use zlink::{Connection, proxy, test_utils::mock_socket::MockSocket};

#[proxy("org.example.FileService")]
trait FileServiceProxy {
    // Send FDs: indexes are passed as regular parameters
    async fn spawn_process(
        &mut self,
        command: String,
        stdin_fd: u32,
        stdout_fd: u32,
        #[zlink(fds)] fds: Vec<OwnedFd>,
    ) -> zlink::Result<Result<ProcessInfo, FileError>>;

    // Receive FDs: response contains indexes referencing the FD vector
    #[zlink(return_fds)]
    async fn open_files(
        &mut self,
        paths: Vec<String>,
    ) -> zlink::Result<(Result<Vec<FileInfo>, FileError>, Vec<OwnedFd>)>;
}

#[derive(Debug, Serialize, Deserialize)]
struct ProcessInfo {
    pid: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileInfo {
    path: String,
    fd: u32, // Index into the FD vector
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "error")]
enum FileError {
    NotFound,
    InvalidFile,
}

#[tokio::test]
async fn send_fds_with_regular_params() {
    let (stdin_read, mut stdin_write) = UnixStream::pair().unwrap();
    let (_stdout_read, stdout_write) = UnixStream::pair().unwrap();

    // Write test data to verify FDs are valid before sending
    stdin_write.write_all(b"test input").unwrap();

    let response = r#"{"parameters":{"pid":1234}}"#;
    let socket = MockSocket::with_responses(&[response]);
    let mut conn = Connection::new(socket);

    let fds = vec![stdin_read.into(), stdout_write.into()];
    // Pass FD indexes as regular parameters: stdin_fd=0, stdout_fd=1
    let result = conn
        .spawn_process("/bin/cat".to_string(), 0, 1, fds)
        .await
        .unwrap();
    let process_info = result.unwrap();
    assert_eq!(process_info.pid, 1234);
}

#[tokio::test]
async fn receive_fds_in_response() {
    let (file1_read, mut file1_write) = UnixStream::pair().unwrap();
    let (file2_read, mut file2_write) = UnixStream::pair().unwrap();

    // Write test data to the write ends before passing the read ends
    file1_write.write_all(b"config data").unwrap();
    file2_write.write_all(b"binary data").unwrap();
    // Close write ends so read_to_end will complete
    drop(file1_write);
    drop(file2_write);

    let response = r#"{
        "parameters": [
            {"path": "/etc/config.txt", "fd": 0},
            {"path": "/var/data.bin", "fd": 1}
        ]
    }"#;
    let fds = vec![vec![file1_read.into(), file2_read.into()]];
    let socket = MockSocket::new(&[response], fds);
    let mut conn = Connection::new(socket);

    let (result, mut received_fds) = conn
        .open_files(vec![
            "/etc/config.txt".to_string(),
            "/var/data.bin".to_string(),
        ])
        .await
        .unwrap();
    let file_list = result.unwrap();

    // Use the fd indexes to access and read from the actual FDs
    // Process in reverse order to avoid index shifting when removing
    assert_eq!(file_list[1].path, "/var/data.bin");
    let data_fd_idx = file_list[1].fd as usize;
    let data_fd = received_fds.remove(data_fd_idx);
    let mut data_stream = unsafe { UnixStream::from_raw_fd(data_fd.into_raw_fd()) };
    let mut data_buf = Vec::new();
    data_stream.read_to_end(&mut data_buf).unwrap();
    assert_eq!(data_buf, b"binary data");

    assert_eq!(file_list[0].path, "/etc/config.txt");
    let config_fd_idx = file_list[0].fd as usize;
    let config_fd = received_fds.remove(config_fd_idx);
    let mut config_stream = unsafe { UnixStream::from_raw_fd(config_fd.into_raw_fd()) };
    let mut config_buf = Vec::new();
    config_stream.read_to_end(&mut config_buf).unwrap();
    assert_eq!(config_buf, b"config data");
}

#[tokio::test]
async fn chain_methods_with_fds() {
    let (stdin1, mut stdin1_write) = UnixStream::pair().unwrap();
    let (_stdout1_read, stdout1) = UnixStream::pair().unwrap();
    let (stdin2, mut stdin2_write) = UnixStream::pair().unwrap();
    let (_stdout2_read, stdout2) = UnixStream::pair().unwrap();

    // Write test data to verify FDs are valid before chaining
    stdin1_write.write_all(b"input1").unwrap();
    stdin2_write.write_all(b"input2").unwrap();

    let reply1 = r#"{"parameters":{"pid":100}}"#;
    let reply2 = r#"{"parameters":{"pid":200}}"#;
    let socket = MockSocket::new(&[reply1, reply2], vec![vec![]]);
    let mut conn = Connection::new(socket);

    let replies = conn
        .chain_spawn_process(
            "/bin/cat".to_string(),
            0,
            1,
            vec![stdin1.into(), stdout1.into()],
        )
        .unwrap()
        .spawn_process(
            "/bin/ls".to_string(),
            0,
            1,
            vec![stdin2.into(), stdout2.into()],
        )
        .unwrap()
        .send::<ProcessInfo, FileError>()
        .await
        .unwrap();

    pin_mut!(replies);

    let (reply1, _fds1) = replies.next().await.unwrap().unwrap();
    let reply1 = reply1.unwrap();
    let process1 = reply1.parameters().unwrap();
    assert_eq!(process1.pid, 100);

    let (reply2, _fds2) = replies.next().await.unwrap().unwrap();
    let reply2 = reply2.unwrap();
    let process2 = reply2.parameters().unwrap();
    assert_eq!(process2.pid, 200);
}

#[tokio::test]
async fn send_empty_fd_vec() {
    let response = r#"{"parameters":{"pid":999}}"#;
    let socket = MockSocket::with_responses(&[response]);
    let mut conn = Connection::new(socket);

    // Sending empty FD vec should work
    // FD indexes still reference the empty vector
    let result = conn
        .spawn_process("/bin/true".to_string(), 0, 0, Vec::new())
        .await
        .unwrap();
    let process_info = result.unwrap();
    assert_eq!(process_info.pid, 999);
}

#[tokio::test]
async fn receive_empty_fd_vec() {
    let response = r#"{"parameters":[]}"#;
    let fds = vec![vec![]];
    let socket = MockSocket::new(&[response], fds);
    let mut conn = Connection::new(socket);

    let (result, received_fds) = conn.open_files(vec![]).await.unwrap();
    let file_list = result.unwrap();

    // When no files are opened, both lists are empty
    assert!(file_list.is_empty());
    assert!(received_fds.is_empty());
}
