//! Unit tests for async I/O streaming with data streams

use crate::cell::streams;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn test_bidirectional_async_streaming() {
    // Create channel pair for bidirectional communication
    let (host_to_guest_tx, host_to_guest_rx, guest_to_host_tx, mut guest_to_host_rx) =
        streams::create_channel_pair();

    // Create stream adapters
    let mut input_reader = streams::Reader::new(host_to_guest_rx);
    let mut output_writer = streams::Writer::new(guest_to_host_tx);

    // Test data
    let test_data = b"Hello, async world!";

    // Spawn async task to write from host side
    let host_tx = host_to_guest_tx.clone();
    let host_write_task = tokio::spawn(async move {
        host_tx.send(test_data.to_vec()).unwrap();
        // Send EOF by closing the channel
        drop(host_tx);
    });

    // Spawn async task to read on guest side (simulated)
    let guest_read_task = tokio::spawn(async move {
        let mut buffer = vec![0u8; 1024];
        let n = input_reader.read(&mut buffer).await.unwrap();
        assert_eq!(n, test_data.len());
        assert_eq!(&buffer[..n], test_data);
        n
    });

    // Spawn async task to write from guest side (simulated)
    let guest_write_data = b"Response from guest!";
    let guest_write_task = tokio::spawn(async move {
        output_writer.write_all(guest_write_data).await.unwrap();
        output_writer.flush().await.unwrap();
    });

    // Spawn async task to read on host side
    let host_read_task = tokio::spawn(async move {
        let data = guest_to_host_rx.recv().await.unwrap();
        assert_eq!(data, guest_write_data);
        data
    });

    // Wait for all tasks to complete
    let (_, read_result, _, host_read_result) = tokio::join!(
        host_write_task,
        guest_read_task,
        guest_write_task,
        host_read_task
    );

    assert_eq!(read_result.unwrap(), test_data.len());
    assert_eq!(host_read_result.unwrap(), guest_write_data);
}

#[tokio::test]
async fn test_concurrent_async_streaming() {
    // Test multiple concurrent read/write operations
    let (host_tx, host_rx, guest_tx, mut guest_rx) = streams::create_channel_pair();

    let mut reader = streams::Reader::new(host_rx);
    let mut writer = streams::Writer::new(guest_tx);

    // Send multiple messages from host
    let messages = vec![
        b"Message 1".to_vec(),
        b"Message 2".to_vec(),
        b"Message 3".to_vec(),
    ];

    let host_tx_clone = host_tx.clone();
    let send_task = tokio::spawn(async move {
        for msg in messages.iter() {
            host_tx_clone.send(msg.clone()).unwrap();
        }
        drop(host_tx_clone);
    });

    // Read all messages on guest side
    let read_task = tokio::spawn(async move {
        let mut received = Vec::new();
        let mut buffer = vec![0u8; 1024];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break, // EOF
                Ok(n) => {
                    received.push(buffer[..n].to_vec());
                    if received.len() >= 3 {
                        break;
                    }
                }
                Err(e) => panic!("Read error: {:?}", e),
            }
        }
        received
    });

    // Write responses from guest
    let responses = vec![
        b"Response 1".to_vec(),
        b"Response 2".to_vec(),
        b"Response 3".to_vec(),
    ];
    let write_task = tokio::spawn(async move {
        for response in responses.iter() {
            writer.write_all(response).await.unwrap();
        }
        writer.flush().await.unwrap();
    });

    // Read responses on host side
    let recv_task = tokio::spawn(async move {
        let mut received = Vec::new();
        for _ in 0..3 {
            if let Some(data) = guest_rx.recv().await {
                received.push(data);
            }
        }
        received
    });

    let (_, read_results, _, recv_results) =
        tokio::join!(send_task, read_task, write_task, recv_task);

    let read_results = read_results.unwrap();
    assert_eq!(read_results.len(), 3);
    assert_eq!(read_results[0], b"Message 1");
    assert_eq!(read_results[1], b"Message 2");
    assert_eq!(read_results[2], b"Message 3");

    let recv_results = recv_results.unwrap();
    assert_eq!(recv_results.len(), 3);
    assert_eq!(recv_results[0], b"Response 1");
    assert_eq!(recv_results[1], b"Response 2");
    assert_eq!(recv_results[2], b"Response 3");
}

#[tokio::test]
async fn test_stream_handles_integration() {
    // Test that stream handles work correctly with channel creation
    let (host_tx, _host_rx, _guest_tx, _guest_rx) = streams::create_channel_pair();

    // Verify we can send data
    assert!(host_tx.send(b"test".to_vec()).is_ok());
}
