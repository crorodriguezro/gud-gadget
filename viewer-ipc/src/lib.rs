use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const RUNTIME_DIR_NAME: &str = "gud-viewer";
pub const CONTROL_SOCKET_NAME: &str = "control.sock";
pub const FRAMEBUFFER_FILE_NAME: &str = "framebuffer.bin";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    Rgb565,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Hello {
        version: u32,
    },
    SessionStart {
        width: u32,
        height: u32,
        stride: u32,
        pixel_format: PixelFormat,
        shm_path: String,
    },
    FrameReady {
        width: u32,
        height: u32,
        stride: u32,
        generation: u64,
        full_frame: bool,
    },
    Disconnected,
    Shutdown,
}

pub fn default_runtime_dir(uid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}/{RUNTIME_DIR_NAME}"))
}

pub fn default_socket_path(uid: u32) -> PathBuf {
    default_runtime_dir(uid).join(CONTROL_SOCKET_NAME)
}

pub fn default_framebuffer_path(uid: u32) -> PathBuf {
    default_runtime_dir(uid).join(FRAMEBUFFER_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{default_framebuffer_path, default_socket_path, PixelFormat, ServerMessage};

    #[test]
    fn default_paths_include_runtime_dir() {
        assert_eq!(
            default_socket_path(10000),
            PathBuf::from("/run/user/10000/gud-viewer/control.sock")
        );
        assert_eq!(
            default_framebuffer_path(10000),
            PathBuf::from("/run/user/10000/gud-viewer/framebuffer.bin")
        );
    }

    #[test]
    fn server_messages_round_trip_as_json() {
        let message = ServerMessage::SessionStart {
            width: 1080,
            height: 2280,
            stride: 2160,
            pixel_format: PixelFormat::Rgb565,
            shm_path: "/run/user/10000/gud-viewer/framebuffer.bin".into(),
        };

        let json = serde_json::to_string(&message).expect("serialize");
        let parsed: ServerMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, message);
    }
}
