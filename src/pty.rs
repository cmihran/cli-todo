use crate::db::{Status, Task};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize, Child};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct ClaudePane {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub session_id: String,
    pub task_id: i64,
    _reader_handle: JoinHandle<()>,
    pub exited: bool,
}

impl ClaudePane {
    pub fn spawn(task: &Task, subtasks: &[Task], cols: u16, rows: u16) -> Result<Self, String> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let prompt = build_prompt(task, subtasks);

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Failed to open PTY: {}", e))?;

        let mut cmd = CommandBuilder::new("claude");
        cmd.arg("--session-id");
        cmd.arg(&session_id);
        cmd.arg(&prompt);
        // Run in the current working directory so Claude sees the project
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        // Pass through MCP config if user has it set
        if let Ok(config) = std::env::var("CLAUDE_MCP_CONFIG") {
            cmd.arg("--mcp-config");
            cmd.arg(&config);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("Failed to spawn claude: {}", e))?;

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("Failed to clone PTY reader: {}", e))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("Failed to take PTY writer: {}", e))?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 1000)));
        let parser_clone = Arc::clone(&parser);

        let handle = std::thread::spawn(move || {
            reader_thread(reader, parser_clone);
        });

        Ok(ClaudePane {
            master: pair.master,
            child,
            writer,
            parser,
            session_id,
            task_id: task.id,
            _reader_handle: handle,
            exited: false,
        })
    }

    pub fn resume(session_id: &str, task_id: i64, cols: u16, rows: u16) -> Result<Self, String> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Failed to open PTY: {}", e))?;

        let mut cmd = CommandBuilder::new("claude");
        cmd.arg("--resume");
        cmd.arg(session_id);
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("Failed to spawn claude: {}", e))?;

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("Failed to clone PTY reader: {}", e))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("Failed to take PTY writer: {}", e))?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 1000)));
        let parser_clone = Arc::clone(&parser);

        let handle = std::thread::spawn(move || {
            reader_thread(reader, parser_clone);
        });

        Ok(ClaudePane {
            master: pair.master,
            child,
            writer,
            parser,
            session_id: session_id.to_string(),
            task_id,
            _reader_handle: handle,
            exited: false,
        })
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
    }

    pub fn write(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    pub fn try_wait(&mut self) -> bool {
        if self.exited {
            return true;
        }
        match self.child.try_wait() {
            Ok(Some(_)) => {
                self.exited = true;
                true
            }
            _ => false,
        }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        self.exited = true;
    }
}

fn reader_thread(mut reader: Box<dyn std::io::Read + Send>, parser: Arc<Mutex<vt100::Parser>>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Ok(mut p) = parser.lock() {
                    p.process(&buf[..n]);
                }
            }
            Err(_) => break,
        }
    }
}

fn build_prompt(task: &Task, subtasks: &[Task]) -> String {
    let status_str = task.status.as_str();
    let priority_str = task.priority.as_str();
    let tags_str = if task.tags.is_empty() {
        String::new()
    } else {
        format!("\nTags: {}", task.tags.join(", "))
    };

    let desc_str = if task.description.is_empty() {
        String::new()
    } else {
        format!("\n\nDescription:\n{}", task.description)
    };

    let subtask_str = if subtasks.is_empty() {
        String::new()
    } else {
        let items: Vec<String> = subtasks
            .iter()
            .map(|st| {
                let check = if st.status == Status::Done {
                    "x"
                } else {
                    " "
                };
                format!("  - [{}] #{}: {}", check, st.id, st.title)
            })
            .collect();
        format!("\n\nSubtasks:\n{}", items.join("\n"))
    };

    include_str!("prompt.md")
        .replace("{id}", &task.id.to_string())
        .replace("{title}", &task.title)
        .replace("{status}", status_str)
        .replace("{priority}", priority_str)
        .replace("{tags}", &tags_str)
        .replace("{description}", &desc_str)
        .replace("{subtasks}", &subtask_str)
}

/// Convert a crossterm KeyEvent to raw terminal bytes for the PTY.
pub fn key_to_bytes(key: &KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::Char(c) if ctrl => {
            // Ctrl+A = 0x01, Ctrl+B = 0x02, ..., Ctrl+Z = 0x1A
            // Ctrl+\ = 0x1C, Ctrl+] = 0x1D
            let byte = match c {
                'a'..='z' => c as u8 - b'a' + 1,
                '\\' => 0x1C,
                ']' => 0x1D,
                '[' => 0x1B,
                _ => return vec![],
            };
            vec![byte]
        }
        KeyCode::Char(c) if alt => {
            let mut bytes = vec![0x1B]; // ESC prefix
            let mut buf = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            bytes
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf);
            buf[..c.len_utf8()].to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7F],
        KeyCode::Esc => vec![0x1B],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => vec![0x1B, b'[', b'Z'],
        KeyCode::Up => vec![0x1B, b'[', b'A'],
        KeyCode::Down => vec![0x1B, b'[', b'B'],
        KeyCode::Right => vec![0x1B, b'[', b'C'],
        KeyCode::Left => vec![0x1B, b'[', b'D'],
        KeyCode::Home => vec![0x1B, b'[', b'H'],
        KeyCode::End => vec![0x1B, b'[', b'F'],
        KeyCode::PageUp => vec![0x1B, b'[', b'5', b'~'],
        KeyCode::PageDown => vec![0x1B, b'[', b'6', b'~'],
        KeyCode::Delete => vec![0x1B, b'[', b'3', b'~'],
        KeyCode::Insert => vec![0x1B, b'[', b'2', b'~'],
        KeyCode::F(n) => match n {
            1 => vec![0x1B, b'O', b'P'],
            2 => vec![0x1B, b'O', b'Q'],
            3 => vec![0x1B, b'O', b'R'],
            4 => vec![0x1B, b'O', b'S'],
            5 => vec![0x1B, b'[', b'1', b'5', b'~'],
            6 => vec![0x1B, b'[', b'1', b'7', b'~'],
            7 => vec![0x1B, b'[', b'1', b'8', b'~'],
            8 => vec![0x1B, b'[', b'1', b'9', b'~'],
            9 => vec![0x1B, b'[', b'2', b'0', b'~'],
            10 => vec![0x1B, b'[', b'2', b'1', b'~'],
            11 => vec![0x1B, b'[', b'2', b'3', b'~'],
            12 => vec![0x1B, b'[', b'2', b'4', b'~'],
            _ => vec![],
        },
        _ => vec![],
    }
}
