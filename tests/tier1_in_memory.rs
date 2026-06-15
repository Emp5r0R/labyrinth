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
    let result: Result<String> = executor.execute_bof(vec![], vec![], "go").await;
    
    assert!(result.is_ok());
    assert!(result.unwrap().contains("Windows BOF loader placeholder"));
}

#[tokio::test]
async fn test_windows_reflective_placeholder() {
    let executor = CommandExecutor::new(&OperatingSystem::Windows);
    let result: Result<String> = executor.execute_reflective(vec![], "args").await;
    
    assert!(result.is_ok());
    assert!(result.unwrap().contains("Windows reflective loader placeholder"));
}
