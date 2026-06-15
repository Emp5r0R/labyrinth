use labyrinth::protocol::Message;
use labyrinth::agent::command_executor::{CommandExecutor, OperatingSystem};
use labyrinth::Result;

#[test]
fn test_bof_protocol_serialization() {
    let bof_data = vec![0xDE, 0xAD, 0xBE, 0xEF];
    let args = vec![0x01, 0x02];
    let entry_point = "go".to_string();

    let msg = Message::BofExecutionRequest {
        bof_data: bof_data.clone(),
        args: args.clone(),
        entry_point: entry_point.clone(),
    };

    let serialized = serde_json::to_string(&msg).expect("Failed to serialize");
    let deserialized: Message = serde_json::from_str(&serialized).expect("Failed to deserialize");

    if let Message::BofExecutionRequest { bof_data: d, args: a, entry_point: e } = deserialized {
        assert_eq!(d, bof_data);
        assert_eq!(a, args);
        assert_eq!(e, entry_point);
    } else {
        panic!("Wrong message type after deserialization");
    }
}

#[test]
fn test_reflective_protocol_serialization() {
    let pe_data = vec![0x4D, 0x5A]; // MZ header
    let args = "test args".to_string();

    let msg = Message::ReflectiveLoadRequest {
        pe_data: pe_data.clone(),
        args: args.clone(),
    };

    let serialized = serde_json::to_string(&msg).expect("Failed to serialize");
    let deserialized: Message = serde_json::from_str(&serialized).expect("Failed to deserialize");

    if let Message::ReflectiveLoadRequest { pe_data: d, args: a } = deserialized {
        assert_eq!(d, pe_data);
        assert_eq!(a, args);
    } else {
        panic!("Wrong message type after deserialization");
    }
}

#[tokio::test]
async fn test_linux_bof_rejection() {
    let executor = CommandExecutor::new(&OperatingSystem::Linux);
    let result: Result<String> = executor.execute_bof(vec![], vec![], "go").await;
    
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not supported on Linux"));
}

#[tokio::test]
async fn test_linux_reflective_rejection() {
    let executor = CommandExecutor::new(&OperatingSystem::Linux);
    let result: Result<String> = executor.execute_reflective(vec![], "args").await;
    
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not supported on Linux"));
}

#[tokio::test]
async fn test_windows_bof_placeholder() {
    let executor = CommandExecutor::new(&OperatingSystem::Windows);
    // Use x64 COFF header mock
    let mut bof_data = vec![0u8; 100];
    bof_data[0] = 0x64; bof_data[1] = 0x86; // IMAGE_FILE_MACHINE_AMD64

    let result: Result<String> = executor.execute_bof(bof_data, vec![], "go").await;
    
    if cfg!(target_os = "windows") {
        assert!(result.is_ok());
        assert!(result.unwrap().contains("BOF loaded at"));
    } else {
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Not supported on this OS"));
    }
}

#[tokio::test]
async fn test_windows_reflective_placeholder() {
    let executor = CommandExecutor::new(&OperatingSystem::Windows);
    // Use MZ header mock
    let mut pe_data = vec![0u8; 1024];
    pe_data[0] = 0x4D; pe_data[1] = 0x5A; // MZ
    pe_data[60] = 0x40; // e_lfanew
    pe_data[0x40] = 0x50; pe_data[0x41] = 0x45; // PE\0\0

    let result: Result<String> = executor.execute_reflective(pe_data, "args").await;
    
    if cfg!(target_os = "windows") {
        assert!(result.is_ok());
        assert!(result.unwrap().contains("Reflectively mapped PE at"));
    } else {
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Not supported on this OS"));
    }
}
